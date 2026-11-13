//! Edge gestures -> app switches (DESKTOP-SHELL.md 3.3, Phase 4). Core layer: the recognizer (gesture.rs)
//! says which edge and where the finger is; this file turns that into a `Switch` the finger drives, and
//! into policy calls when the finger lets go.
//!
//! - Bottom edge, swipe up: Home. The app shrinks under the finger (the grabbed point stays under it) and
//!   heads for its tile on release, or back to full size, by distance and velocity. Also catches an app
//!   that is growing back in.
//! - Left edge, swipe right: the previous app (most recently used) slides in over the current one, its
//!   right edge under the finger.
//!   Resting the finger (`PAUSE`) after pulling up opens the Overview with that app as its first card. On
//!   Home (no app shown) the bottom-edge swipe opens the Overview at once.
//! - Top edge, long pull down: close the app (Windows 8 style). The app shrinks to a card under the finger;
//!   released low enough (or flicked down) it drops off the bottom and is asked to close.

use std::time::Duration;

use smithay::{
    desktop::Window,
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Point},
};
use tracing::{info, warn};

use crate::{
    anim::SpringParams,
    gesture::{Edge, Edges, Track},
    overlay::EdgePhase,
    state::{State, window_app_id},
    switch::{Backdrop, CLOSE_HELD, Style, Switch},
};

/// Home gesture: p rises with the distance pulled up, quickly at first and then levelling off at `HOME_PMAX`
/// (the app stays a large card while the finger is down). `HOME_DIST` (fraction of the screen height) is
/// the distance for p = 1 at the initial slope.
const HOME_PMAX: f32 = 0.55;
const HOME_DIST: f32 = 0.45;
/// Release: go Home if flicked up faster than this (px/s), or pulled at least `HOME_COMMIT` of the height
/// and not flicked back down.
const HOME_FLICK: f32 = 350.0;
const HOME_COMMIT: f32 = 0.15;
/// Left edge: commit if flicked right faster than this (px/s), or dragged past this fraction of the width.
const BACK_FLICK: f32 = 400.0;
const BACK_COMMIT: f32 = 0.35;
/// Top edge: p = `CLOSE_HELD` (smallest card) after pulling this fraction of the height; close when released below
/// `CLOSE_LINE` of the height (not flicked up), or flicked down faster than `CLOSE_FLICK` after pulling
/// at least `CLOSE_MIN`.
const CLOSE_DIST: f32 = 0.5;
const CLOSE_LINE: f32 = 0.6;
const CLOSE_FLICK: f32 = 1200.0;
const CLOSE_MIN: f32 = 0.25;
/// Bottom edge: a finger that rests (slower than `PAUSE_SPEED` px/s, or no motion events) for `PAUSE` after
/// pulling at least `PAUSE_MIN` of the height opens the Overview.
const PAUSE: Duration = Duration::from_millis(220);
const PAUSE_SPEED: f32 = 120.0;
const PAUSE_MIN: f32 = 0.1;
/// How long a closed app gets before Home asks whether to force quit it (`RTSHELL_CLOSE_WAIT_MS` overrides,
/// for tests).
const CLOSE_WAIT_DEFAULT: Duration = Duration::from_secs(5);

/// A running edge gesture (the finger is down).
#[derive(Debug, Clone)]
pub struct EdgeRun {
    edge: Edge,
    /// Distance added to the finger's so a caught animation continues from where it was (bottom edge).
    d_off: f32,
    /// The app that was shown before (left edge: comes back if the swipe is abandoned).
    before: Option<Window>,
    /// Bottom edge: since when the finger has been nearly still, and the distance pulled (for the pause
    /// that opens the Overview).
    slow_since: Option<Duration>,
    last_d: f32,
}

fn home_p(d: f32, h: f32) -> (f32, f32) {
    let d = d.max(0.0);
    let k = HOME_PMAX * HOME_DIST * h;
    let e = (-d / k).exp();
    (HOME_PMAX * (1.0 - e), e / (HOME_DIST * h))
}

/// The distance that gives progress `p` (capped below `HOME_PMAX`).
fn home_d(p: f32, h: f32) -> f32 {
    let p = p.clamp(0.0, HOME_PMAX * 0.98);
    -(HOME_PMAX * HOME_DIST * h) * (1.0 - p / HOME_PMAX).ln()
}

static CLOSE_WAIT_CELL: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();

fn close_wait() -> Duration {
    *CLOSE_WAIT_CELL.get_or_init(|| {
        std::env::var("RTSHELL_CLOSE_WAIT_MS").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis).unwrap_or(CLOSE_WAIT_DEFAULT)
    })
}

impl State {
    /// Which edges have an action now.
    pub(super) fn armed_edges(&self) -> Edges {
        if self.overview_takes_touches() || self.overlay.as_ref().is_some_and(|o| o.panel_active()) {
            return Edges::default();
        }
        // The right edge pulls the Control Center over anything that is not moving.
        let right = self.overlay.is_some() && self.switch.is_none();
        let app_shown = self.space.elements().next().is_some();
        let apps = self.toplevels.iter().filter(|t| t.mapped && !crate::winconf::has_parent(&t.window)).count();
        match self.switch.as_ref() {
            // On Home (no app shown) the bottom edge opens the Overview if any app runs; a swipe up anywhere
            // else on Home still opens All apps.
            None => Edges { bottom: app_shown || apps >= 1, left: app_shown && apps >= 2, top: app_shown, right },
            // Catch an app growing back in; an app -> app switch is replaced.
            Some(s) if s.held => Edges::default(),
            Some(s) => Edges { bottom: s.backdrop == Backdrop::App || (s.style == Style::Tile && s.entering()), ..Edges::default() },
        }
    }

    fn screen_size(&self) -> (f32, f32) {
        self.output_geometry().map(|g| (g.size.w as f32, g.size.h as f32)).unwrap_or((1366.0, 768.0))
    }

    fn finger(t: &Track) -> Point<f64, Logical> {
        Point::from((t.pos.0 as f64, t.pos.1 as f64))
    }

    /// A locked edge touch: start the switch it drives. False if there is nothing to do.
    pub(super) fn gesture_begin(&mut self, t: Track) -> bool {
        let (_, h) = self.screen_size();
        let first = |v: &[(Window, Point<i32, Logical>)]| v.first().map(|(w, _)| w.clone());
        let run = match t.edge {
            Edge::Bottom => {
                let catching = self.switch.as_ref().is_some_and(|s| s.backdrop == Backdrop::Home && s.style == Style::Tile);
                if !catching && self.switch.is_none() && self.space.elements().next().is_none() {
                    // From Home: straight to the Overview; the rest of this finger is swallowed.
                    info!(edge = ?t.edge, "edge gesture on Home: overview");
                    return self.open_overview(None);
                }
                if !catching {
                    self.switch_with(None, |p, ctx| p.set_home(ctx, true));
                }
                let Some(s) = self.switch.as_mut().filter(|s| s.backdrop == Backdrop::Home) else { return false };
                let d_off = home_d(s.p.value, h) - t.inward();
                s.grab(Self::finger(&t), true);
                EdgeRun { edge: t.edge, d_off, before: None, slow_since: None, last_d: 0.0 }
            }
            Edge::Left => {
                let before = self.shown_windows();
                self.with_policy_raw(|p, ctx| p.cycle(ctx, true));
                let after = self.shown_windows();
                let (b, a) = (first(&before), first(&after));
                let (Some(a), Some(area)) = (a, self.area()) else { return false };
                // Same unit still shown (e.g. only a split pair runs): nothing to go back to.
                if b.as_ref().is_some_and(|b| after.iter().any(|(w, _)| w == b)) {
                    return false;
                }
                let mut s = Switch::new(after, Backdrop::App, window_app_id(&a), area, self.spring_params(), 1.0, 1.0);
                s.style = Style::Slide;
                s.under = before;
                s.grab(Self::finger(&t), false);
                self.switch = Some(s);
                EdgeRun { edge: t.edge, d_off: 0.0, before: b, slow_since: None, last_d: 0.0 }
            }
            Edge::Top => {
                let shown = self.shown_windows();
                let (Some(a), Some(area)) = (first(&shown), self.area()) else { return false };
                let mut s = Switch::new(shown, Backdrop::Home, window_app_id(&a), area, self.spring_params(), 0.0, 0.0);
                s.style = Style::Close;
                s.grab(Self::finger(&t), true);
                self.switch = Some(s);
                EdgeRun { edge: t.edge, d_off: 0.0, before: Some(a), slow_since: None, last_d: 0.0 }
            }
            Edge::Right => {
                let x = t.pos.0;
                if !self.overlay.as_mut().is_some_and(|o| o.panel_edge(EdgePhase::Begin, x, t.velocity.0)) {
                    return false;
                }
                info!(x = t.pos.0, y = t.pos.1, "edge gesture: control center");
                self.gesture = Some(EdgeRun { edge: t.edge, d_off: 0.0, before: None, slow_since: None, last_d: 0.0 });
                self.queue_redraw();
                return true;
            }
        };
        info!(edge = ?t.edge, x = t.pos.0, y = t.pos.1, "edge gesture");
        self.gesture = Some(run);
        self.sync_apps_shown();
        self.gesture_move(t);
        true
    }

    pub(super) fn gesture_move(&mut self, t: Track) {
        let Some(run) = self.gesture.clone() else { return };
        let (w, h) = self.screen_size();
        let (p, v) = match run.edge {
            Edge::Bottom => {
                let (p, dp) = home_p(t.inward() + run.d_off, h);
                (p, dp * t.inward_velocity())
            }
            Edge::Left => ((1.0 - t.pos.0 / w).clamp(0.0, 1.0), -t.velocity.0 / w),
            Edge::Top => {
                let raw = t.inward() / (CLOSE_DIST * h);
                let v = if (0.0..1.0).contains(&raw) { t.velocity.1 / (CLOSE_DIST * h) } else { 0.0 };
                (raw.clamp(0.0, 1.0) * CLOSE_HELD, v * CLOSE_HELD)
            }
            Edge::Right => {
                if let Some(o) = self.overlay.as_mut() {
                    o.panel_edge(EdgePhase::Move, t.pos.0, t.velocity.0);
                }
                self.queue_redraw();
                return;
            }
        };
        if let Some(s) = self.switch.as_mut().filter(|s| s.held) {
            s.drag(Self::finger(&t), p, v);
        }
        self.queue_redraw();
        if run.edge == Edge::Bottom {
            self.watch_pause(&t, run.d_off, h);
        }
    }

    /// Bottom edge: open the Overview when the finger rests. Slow motion is seen here; a finger that sends
    /// no events at all is caught by a timer re-armed on every move.
    fn watch_pause(&mut self, t: &Track, d_off: f32, h: f32) {
        let now: Duration = self.clock.now().into();
        let d = t.inward() + d_off;
        let speed = t.velocity.0.hypot(t.velocity.1);
        let Some(run) = self.gesture.as_mut() else { return };
        run.last_d = d;
        if d > PAUSE_MIN * h && speed < PAUSE_SPEED {
            let since = *run.slow_since.get_or_insert(now);
            if now.saturating_sub(since) >= PAUSE {
                info!(d = d as i32, "edge gesture paused: overview");
                self.cancel_pause_timer();
                self.overview_from_gesture();
                return;
            }
        } else {
            run.slow_since = None;
        }
        self.cancel_pause_timer();
        let res = self.loop_handle.insert_source(Timer::from_duration(PAUSE), move |_, _, state| {
            state.pause_timer = None;
            let (_, h) = state.screen_size();
            if state.gesture.as_ref().is_some_and(|r| r.edge == Edge::Bottom && r.last_d > PAUSE_MIN * h) {
                info!("edge gesture rested: overview");
                state.overview_from_gesture();
            }
            TimeoutAction::Drop
        });
        match res {
            Ok(token) => self.pause_timer = Some(token),
            Err(err) => warn!(%err, "cannot arm pause timer"),
        }
    }

    fn cancel_pause_timer(&mut self) {
        if let Some(token) = self.pause_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    /// The finger lifted (or a second finger cancelled): decide and let the switch finish on its own.
    pub(super) fn gesture_end(&mut self, t: Track, cancel: bool) {
        self.cancel_pause_timer();
        let Some(run) = self.gesture.take() else { return };
        if run.edge == Edge::Right {
            let phase = if cancel { EdgePhase::Cancel } else { EdgePhase::End };
            if let Some(o) = self.overlay.as_mut() {
                o.panel_edge(phase, t.pos.0, t.velocity.0);
            }
            self.queue_redraw();
            return;
        }
        if !self.switch.as_ref().is_some_and(|s| s.held) {
            return;
        }
        let (w, h) = self.screen_size();
        let (vi, d) = (t.inward_velocity(), t.inward() + run.d_off);
        let commit = !cancel
            && match run.edge {
                Edge::Bottom => vi > HOME_FLICK || (vi > -HOME_FLICK && d > HOME_COMMIT * h),
                Edge::Left => vi > BACK_FLICK || (vi > -BACK_FLICK && t.pos.0 > BACK_COMMIT * w),
                Edge::Top => (t.pos.1 > CLOSE_LINE * h && vi > -HOME_FLICK) || (vi > CLOSE_FLICK && d > CLOSE_MIN * h),
                Edge::Right => false,
            };
        info!(edge = ?run.edge, cancel, commit, d = d as i32, v = vi as i32, "edge gesture end");
        match run.edge {
            Edge::Bottom => {
                if let Some(s) = self.switch.as_mut() {
                    s.release(!commit);
                }
                // The policy may still show the app (caught while growing in) or show Home (pulled up).
                let policy_home = self.space.elements().next().is_none();
                if commit != policy_home {
                    self.with_policy_raw(|p, ctx| p.set_home(ctx, commit));
                    if let Some(s) = self.switch.as_mut().filter(|_| !commit) {
                        s.windows = self.space.elements().filter_map(|w| Some((w.clone(), self.space.element_location(w)?))).collect();
                    }
                }
            }
            Edge::Left => {
                if let Some(s) = self.switch.as_mut() {
                    s.release(commit);
                }
                if !commit && let Some(b) = run.before {
                    // Back to the app that was shown; the new one slides out over it.
                    self.with_policy_raw(|p, ctx| p.activate(ctx, &b));
                }
            }
            Edge::Top => {
                if let Some(s) = self.switch.as_mut() {
                    s.release(!commit);
                }
                if commit && let Some(app) = run.before {
                    // The whole unit closes (both apps of a split pair).
                    let apps = self.policy.unit_apps(&app);
                    self.with_policy_raw(|p, ctx| p.set_home(ctx, true));
                    for app in &apps {
                        self.close_app(app);
                    }
                }
            }
            Edge::Right => {}
        }
        self.sync_apps_shown();
        self.queue_redraw();
    }

    /// Ask an app to close (xdg_toplevel.close). If its window is still there after 5 s (`RTSHELL_CLOSE_WAIT_MS`) and it is
    /// not on screen again, Home asks whether to force quit it (`Request::ForceQuit`).
    pub(crate) fn close_app(&mut self, app: &Window) {
        info!(app = window_app_id(app).as_deref().unwrap_or("?"), "closing app");
        crate::winconf::close(app);
        let app = app.clone();
        let res = self.loop_handle.insert_source(Timer::from_duration(close_wait()), move |_, _, state| {
            state.close_timed_out(&app);
            TimeoutAction::Drop
        });
        if let Err(err) = res {
            warn!(%err, "cannot arm close timer");
        }
    }

    fn close_timed_out(&mut self, app: &Window) {
        if !self.toplevels.iter().any(|t| &t.window == app) {
            return;
        }
        if self.space.elements().any(|w| w == app) {
            info!(app = window_app_id(app).as_deref().unwrap_or("?"), "app did not close but is shown again: not asking");
            return;
        }
        let app_id = window_app_id(app).unwrap_or_default();
        self.next_prompt += 1;
        let id = self.next_prompt;
        let asked = self.overlay.as_mut().is_some_and(|o| o.prompt_not_responding(id, &app_id));
        warn!(app = %app_id, id, asked, wait_ms = close_wait().as_millis() as u64, "app did not close");
        if asked {
            self.prompts.push((id, app.clone()));
            self.queue_redraw();
        }
    }

    /// Home's answer to a "not responding" prompt.
    pub(crate) fn answer_prompt(&mut self, id: u64, kill: bool) {
        let Some(i) = self.prompts.iter().position(|(p, _)| *p == id) else { return };
        let (_, app) = self.prompts.remove(i);
        if !self.toplevels.iter().any(|t| t.window == app) {
            return;
        }
        if kill {
            self.force_quit(&app);
        } else {
            info!(app = window_app_id(&app).as_deref().unwrap_or("?"), "waiting longer for the app to close");
            self.close_app(&app);
        }
    }

    /// SIGKILL the app's process group (apps started by rtshell lead their own group), or only its process
    /// if it shares rtshell's group.
    fn force_quit(&mut self, app: &Window) {
        // An X11 app: its own process (_NET_WM_PID), never Xwayland (the wl client of every X app).
        let pid = match crate::winconf::x11_pid(app) {
            Some(pid) => pid,
            None if crate::winconf::is_x11(app) => return warn!("force quit: the X11 app has no _NET_WM_PID"),
            None => {
                let Some(client) = app.toplevel().and_then(|t| smithay::reexports::wayland_server::Resource::client(t.wl_surface())) else { return };
                let Ok(creds) = client.get_credentials(&self.display_handle) else {
                    return warn!("force quit: no client credentials");
                };
                creds.pid
            }
        };
        let (pgid, own) = unsafe { (libc::getpgid(pid), libc::getpgrp()) };
        let target = if pgid > 1 && pgid != own { -pgid } else { pid };
        warn!(app = window_app_id(app).as_deref().unwrap_or("?"), pid, pgid, "force quit (SIGKILL)");
        if pid > 1 {
            unsafe {
                libc::kill(target, libc::SIGKILL);
            }
        }
    }

    /// A window went away: drop questions about it.
    pub(crate) fn forget_prompts(&mut self, w: &Window) {
        let gone: Vec<u64> = self.prompts.iter().filter(|(_, x)| x == w).map(|(id, _)| *id).collect();
        self.prompts.retain(|(_, x)| x != w);
        for id in gone {
            if let Some(o) = self.overlay.as_mut() {
                o.dismiss_prompt(id);
            }
            self.queue_redraw();
        }
    }

    fn spring_params(&self) -> SpringParams {
        SpringParams { stiffness: self.theme.motion.spring_stiffness, damping: self.theme.motion.spring_damping }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_curve() {
        let h = 768.0;
        let (p0, v0) = home_p(0.0, h);
        assert_eq!(p0, 0.0);
        assert!((v0 - 1.0 / (HOME_DIST * h)).abs() < 1e-6);
        let (p, _) = home_p(300.0, h);
        assert!(p > 0.35 && p < HOME_PMAX, "{p}");
        assert!((home_d(p, h) - 300.0).abs() < 0.5);
        assert!(home_p(10_000.0, h).0 <= HOME_PMAX);
    }
}
