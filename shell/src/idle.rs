//! Idle, core side (DESKTOP-SHELL.md Phase 8 "Idle"): the time of the last user input and the stage
//! reached since (Active -> Dim -> Blank -> Sleep). Input only stores the time; one calloop timer fires at
//! the next stage's deadline and re-arms itself from the stored time. Blank turns the output off here (the
//! backend); dim and sleep are asked of the overlay (its services thread talks to logind). Apps can hold
//! idle off with `zwp_idle_inhibit_v1` while their surface is shown.

use std::time::{Duration, Instant};

use smithay::{
    reexports::{
        calloop::{
            RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{Resource, protocol::wl_surface::WlSurface},
    },
    wayland::{compositor::get_parent, idle_inhibit::IdleInhibitHandler},
};
use tracing::{info, warn};

use crate::{
    config,
    overlay::{IdleStage, LockReason},
    state::State,
};

/// When each stage begins, as time without input (None = never).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Times {
    pub dim: Option<Duration>,
    pub blank: Option<Duration>,
    pub sleep: Option<Duration>,
}

impl Times {
    pub fn from_config(c: &config::Idle, on_ac: bool, locked: bool) -> Times {
        let t = if on_ac { c.ac } else { c.battery };
        let secs = |s: f64| (s > 0.0 && s.is_finite()).then(|| Duration::from_secs_f64(s));
        let mut blank = secs(t.blank);
        if locked && let Some(lb) = secs(c.lock_blank) {
            blank = Some(blank.map_or(lb, |b| b.min(lb)));
        }
        Times { dim: secs(t.dim), blank, sleep: secs(t.sleep) }
    }

    fn at(&self, s: IdleStage) -> Option<Duration> {
        match s {
            IdleStage::Active => Some(Duration::ZERO),
            IdleStage::Dim => self.dim,
            IdleStage::Blank => self.blank,
            IdleStage::Sleep => self.sleep,
        }
    }

    /// The stage reached after `elapsed` without input (a later stage whose time is earlier wins: e.g. the
    /// lock screen blanks before the dim time).
    pub fn stage(&self, elapsed: Duration) -> IdleStage {
        [IdleStage::Sleep, IdleStage::Blank, IdleStage::Dim]
            .into_iter()
            .find(|s| self.at(*s).is_some_and(|t| elapsed >= t))
            .unwrap_or(IdleStage::Active)
    }

    /// The time without input at which the first stage after `current` begins.
    pub fn next(&self, current: IdleStage) -> Option<Duration> {
        [IdleStage::Dim, IdleStage::Blank, IdleStage::Sleep].into_iter().filter(|s| *s > current).filter_map(|s| self.at(s)).min()
    }
}

pub struct IdleState {
    /// The last user input (or anything that counts as activity).
    pub last: Instant,
    pub stage: IdleStage,
    timer: Option<RegistrationToken>,
    /// Surfaces with a zwp_idle_inhibitor_v1.
    inhibitors: Vec<WlSurface>,
    /// Touch slots and pointer buttons whose press woke a blank screen: the rest of them is dropped.
    swallowed_touches: Vec<i32>,
    swallowed_buttons: Vec<u32>,
    /// AC power as the overlay last reported it (None = unknown: battery times).
    on_ac: Option<bool>,
    /// The session is in the background (VT switched away): no input reaches us, so no idle stages at all
    /// (another session may be in use; 2026-11-12 a background rtshell suspended the system that way).
    paused: bool,
}

impl Default for IdleState {
    fn default() -> Self {
        IdleState {
            last: Instant::now(),
            stage: IdleStage::Active,
            timer: None,
            inhibitors: Vec::new(),
            swallowed_touches: Vec::new(),
            swallowed_buttons: Vec::new(),
            on_ac: None,
            paused: false,
        }
    }
}

impl IdleState {
    /// The output is off (Blank or Sleep).
    pub fn blank(&self) -> bool {
        self.stage >= IdleStage::Blank
    }
}

impl State {
    fn idle_times(&self) -> Times {
        Times::from_config(&self.config.idle, self.idle.on_ac == Some(true), self.locked())
    }

    /// User input happened. True if it woke a blank screen: the caller then drops the event.
    pub fn idle_activity(&mut self) -> bool {
        self.idle.last = Instant::now();
        match self.idle.stage {
            IdleStage::Active => false,
            IdleStage::Dim => {
                self.idle_wake();
                false
            }
            IdleStage::Blank | IdleStage::Sleep => {
                self.idle_wake();
                true
            }
        }
    }

    /// A touch down: false if it woke the screen (the whole touch is dropped then).
    pub fn idle_touch_down(&mut self, slot: i32) -> bool {
        if self.idle_activity() {
            self.idle.swallowed_touches.push(slot);
            return false;
        }
        true
    }

    /// A touch motion or up: false if its touch is being dropped (`up` ends that).
    pub fn idle_touch(&mut self, slot: i32, up: bool) -> bool {
        if let Some(i) = self.idle.swallowed_touches.iter().position(|s| *s == slot) {
            if up {
                self.idle.swallowed_touches.remove(i);
            }
            return false;
        }
        self.idle_activity();
        true
    }

    pub fn idle_touch_cancel(&mut self) {
        self.idle.swallowed_touches.clear();
    }

    /// A pointer button: false if it is dropped (the press woke the screen, and then its release).
    pub fn idle_button(&mut self, button: u32, pressed: bool) -> bool {
        if pressed {
            if self.idle_activity() {
                self.idle.swallowed_buttons.push(button);
                return false;
            }
            return true;
        }
        if let Some(i) = self.idle.swallowed_buttons.iter().position(|b| *b == button) {
            self.idle.swallowed_buttons.remove(i);
            return false;
        }
        self.idle_activity();
        true
    }

    /// Back to Active (input, resume, power plugged): output on, brightness back, timer from now.
    pub fn idle_wake(&mut self) {
        self.idle.last = Instant::now();
        let was = std::mem::replace(&mut self.idle.stage, IdleStage::Active);
        if was == IdleStage::Active {
            return;
        }
        info!(from = ?was, "idle: awake");
        if was >= IdleStage::Blank {
            crate::backend::set_output_power(self, true);
        }
        let level = self.config.idle.dim_level;
        if let Some(o) = self.overlay.as_mut() {
            o.idle(IdleStage::Active, level);
        }
        self.queue_redraw();
        self.handle_overlay_requests();
        self.idle_arm();
    }

    /// The session went to the background (VT switch away): stop the idle timer until it is active again.
    pub fn idle_pause(&mut self) {
        info!("idle: paused (session in the background)");
        self.idle.paused = true;
        if let Some(t) = self.idle.timer.take() {
            self.loop_handle.remove(t);
        }
    }

    /// The session is in the foreground again: counts as activity, timer from now.
    pub fn idle_resume(&mut self) {
        self.idle.paused = false;
        self.idle_wake();
        self.idle_arm();
    }

    /// (Re)start the stage timer from the last input (at start, after waking, when the lock or the power
    /// source changes the times).
    pub fn idle_arm(&mut self) {
        if let Some(t) = self.idle.timer.take() {
            self.loop_handle.remove(t);
        }
        if self.idle.paused {
            return;
        }
        let Some(at) = self.idle_times().next(self.idle.stage) else { return };
        let due = (self.idle.last + at).saturating_duration_since(Instant::now());
        match self.loop_handle.insert_source(Timer::from_duration(due), |_, _, state| state.idle_tick()) {
            Ok(t) => self.idle.timer = Some(t),
            Err(err) => warn!(%err, "idle timer"),
        }
    }

    /// The overlay's AC state may have changed (services update): new times, and plugging in or out counts
    /// as activity.
    pub fn idle_power_source(&mut self) {
        let ac = self.overlay.as_ref().and_then(|o| o.on_ac_power());
        if ac == self.idle.on_ac {
            return;
        }
        info!(on_ac = ?ac, "idle: power source");
        // The first answer after start is not a plug event.
        let plugged = matches!((self.idle.on_ac, ac), (Some(a), Some(b)) if a != b);
        self.idle.on_ac = ac;
        if plugged {
            self.idle_activity();
        }
        self.idle_arm();
    }

    fn idle_tick(&mut self) -> TimeoutAction {
        // This timer's token: anything below that re-arms (locking) replaces it, and this one then ends.
        let me = self.idle.timer.take();
        if self.idle.paused {
            return TimeoutAction::Drop;
        }
        let now = Instant::now();
        if self.idle_inhibited() {
            self.idle.last = now;
        }
        let times = self.idle_times();
        let target = times.stage(now.saturating_duration_since(self.idle.last));
        if target > self.idle.stage {
            self.idle_enter(target);
        }
        if self.idle.timer.is_some() {
            return TimeoutAction::Drop;
        }
        let times = self.idle_times();
        match times.next(self.idle.stage) {
            Some(at) => {
                self.idle.timer = me;
                let due = (self.idle.last + at).saturating_duration_since(Instant::now());
                TimeoutAction::ToDuration(due.max(Duration::from_millis(1)))
            }
            None => TimeoutAction::Drop,
        }
    }

    fn idle_enter(&mut self, target: IdleStage) {
        let from = self.idle.stage;
        info!(?from, to = ?target, idle_s = format!("{:.1}", self.idle.last.elapsed().as_secs_f32()), "idle");
        let level = self.config.idle.dim_level;
        if target >= IdleStage::Blank && from < IdleStage::Blank {
            if self.config.idle.lock && !self.locked() {
                self.lock_session(LockReason::Idle);
            }
            self.idle.stage = IdleStage::Blank;
            crate::backend::set_output_power(self, false);
            if self.locked() && let Some(o) = self.overlay.as_mut() {
                // Nothing is shown now, so a sleep waiting for the lock may go ahead.
                o.lock_presented();
            }
        }
        self.idle.stage = target;
        if let Some(o) = self.overlay.as_mut() {
            if target == IdleStage::Sleep && from < IdleStage::Blank {
                o.idle(IdleStage::Blank, level);
            }
            o.idle(target, level);
        }
        self.handle_overlay_requests();
    }

    /// An app holds idle off: one of its inhibiting surfaces is shown and the session is not locked.
    fn idle_inhibited(&mut self) -> bool {
        self.idle.inhibitors.retain(|s| s.is_alive());
        if self.locked() || self.idle.inhibitors.is_empty() {
            return false;
        }
        self.idle.inhibitors.iter().any(|s| {
            let mut root = s.clone();
            while let Some(p) = get_parent(&root) {
                root = p;
            }
            self.space.elements().any(|w| crate::winconf::surface(w).is_some_and(|t| t == root))
        })
    }
}

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        info!(surface = ?surface.id(), "idle inhibitor added");
        self.idle.inhibitors.push(surface);
        if self.idle.stage == IdleStage::Dim && self.idle_inhibited() {
            self.idle_wake();
        }
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        info!(surface = ?surface.id(), "idle inhibitor removed");
        let was = self.idle_inhibited();
        self.idle.inhibitors.retain(|s| s != &surface);
        if was && !self.idle_inhibited() {
            // Idle counts from now, not from the timer's last look.
            self.idle.last = Instant::now();
            self.idle_arm();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use IdleStage::*;

    fn cfg() -> config::Idle {
        config::Config::parse_over_defaults(
            "[idle]\nbattery = { dim = 10, blank = 20, sleep = 60 }\nac = { dim = 0, blank = 30, sleep = 0 }\nlock_blank = 5\n",
        )
        .unwrap()
        .idle
    }

    fn s(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    #[test]
    fn stages_on_battery() {
        let t = Times::from_config(&cfg(), false, false);
        assert_eq!(t.stage(s(0.0)), Active);
        assert_eq!(t.stage(s(9.9)), Active);
        assert_eq!(t.stage(s(10.0)), Dim);
        assert_eq!(t.stage(s(25.0)), Blank);
        assert_eq!(t.stage(s(61.0)), Sleep);
        assert_eq!(t.next(Active), Some(s(10.0)));
        assert_eq!(t.next(Dim), Some(s(20.0)));
        assert_eq!(t.next(Blank), Some(s(60.0)));
        assert_eq!(t.next(Sleep), None);
    }

    #[test]
    fn ac_never_dims_or_sleeps() {
        let t = Times::from_config(&cfg(), true, false);
        assert_eq!(t.stage(s(29.0)), Active);
        assert_eq!(t.stage(s(1000.0)), Blank);
        assert_eq!(t.next(Active), Some(s(30.0)));
        assert_eq!(t.next(Blank), None);
    }

    #[test]
    fn locked_blanks_early_and_skips_dim() {
        let t = Times::from_config(&cfg(), false, true);
        assert_eq!(t.next(Active), Some(s(5.0)));
        assert_eq!(t.stage(s(5.0)), Blank);
        assert_eq!(t.stage(s(12.0)), Blank);
        assert_eq!(t.next(Blank), Some(s(60.0)));
        // On AC (blank 30) the lock screen still blanks after 5 s.
        assert_eq!(Times::from_config(&cfg(), true, true).next(Active), Some(s(5.0)));
    }

    #[test]
    fn all_zero_never_idles() {
        let c = config::Config::parse_over_defaults("[idle]\nbattery = { dim = 0, blank = 0, sleep = 0 }\nlock_blank = 0\n").unwrap().idle;
        let t = Times::from_config(&c, false, true);
        assert_eq!(t.stage(s(1e6)), Active);
        assert_eq!(t.next(Active), None);
    }
}
