//! The Overview's glue (DESKTOP-SHELL.md 3.3, Phase 4): opening it (pause during the bottom swipe, Super+Tab),
//! its touches, and what its actions do to the policy. The Overview itself is `crate::overview`.

use smithay::{
    desktop::Window,
    utils::{Logical, Point},
};
use tracing::info;

use crate::{
    anim::SpringParams,
    overview::{Action, Overview},
    render::batch::Rect,
    policy::FocusChange,
    state::{State, window_app_id},
    switch::{Pose, Style},
};

impl State {
    /// The running apps, most recently used first, from the policy.
    pub fn app_families(&mut self) -> Vec<Vec<(Window, Point<i32, Logical>)>> {
        let Some(output) = self.output() else { return Vec::new() };
        let Some(output_geometry) = self.space.output_geometry(&output) else { return Vec::new() };
        let _ = output_geometry;
        self.policy_ctx(|p, ctx| p.families(ctx)).unwrap_or_default()
    }

    /// True while the Overview is open and takes every touch.
    pub fn overview_takes_touches(&self) -> bool {
        self.overview.as_ref().is_some_and(|o| !o.leaving)
    }

    /// Open the Overview. `from`: where the most recent app is drawn now (it moves from there to its card).
    pub fn open_overview(&mut self, from: Option<Pose>) -> bool {
        let families = self.app_families();
        let Some(area) = self.area() else { return false };
        if families.is_empty() {
            return false;
        }
        info!(apps = families.len(), from_app = from.is_some(), "overview open");
        self.kick_boost();
        self.with_policy_raw(|p, ctx| p.set_home(ctx, true));
        let params = SpringParams { stiffness: self.theme.motion.spring_stiffness, damping: self.theme.motion.spring_damping };
        let mut ov = Overview::new(families, area, params, from);
        let mut bg = self.theme.colors.background.premultiplied();
        bg[3] = 1.0;
        ov.scrim = bg;
        ov.drop_color = self.theme.split.drop_color.premultiplied();
        ov.drop_zone = self.theme.split.drop_zone;
        self.overview = Some(ov);
        self.sync_apps_shown();
        self.queue_redraw();
        true
    }

    /// Super+Tab: open (from the app shown, or from Home), or leave to Home.
    pub fn toggle_overview(&mut self) {
        if self.overview_takes_touches() {
            self.leave_overview();
            return;
        }
        let from = match self.switch.take() {
            Some(sw) => Some(sw.pose()),
            None => self.area().filter(|_| self.space.elements().next().is_some()).map(|a| {
                let a = a.to_f64();
                Pose { scale: 1.0, center: Point::from((a.loc.x + a.size.w / 2.0, a.loc.y + a.size.h / 2.0)), alpha: 1.0 }
            }),
        };
        self.open_overview(from);
    }

    /// The finger paused during the bottom swipe: the app it holds becomes the Overview's first card.
    pub(super) fn overview_from_gesture(&mut self) {
        self.gesture = None;
        let from = self.switch.take().map(|sw| sw.pose());
        if !self.open_overview(from) {
            // Nothing to show (cannot happen with an app held): go Home.
            self.with_policy_raw(|p, ctx| p.set_home(ctx, true));
            self.sync_apps_shown();
        }
    }

    pub fn leave_overview(&mut self) {
        self.kick_boost();
        if let Some(ov) = self.overview.as_mut() {
            ov.leave();
            self.queue_redraw();
        }
    }

    pub(super) fn overview_touch_down(&mut self, slot: i32, pos: Point<f64, Logical>) {
        let t = self.clock.now().into();
        if let Some(ov) = self.overview.as_mut() {
            ov.touch_down(slot, pos.x as f32, pos.y as f32, t);
        }
    }

    pub(super) fn overview_touch_motion(&mut self, slot: i32, pos: Point<f64, Logical>) {
        let t = self.clock.now().into();
        if let Some(ov) = self.overview.as_mut() {
            ov.touch_motion(slot, pos.x as f32, pos.y as f32, t);
        }
        self.queue_redraw();
    }

    pub(super) fn overview_touch_up(&mut self, slot: i32) {
        let t = self.clock.now().into();
        let Some(ov) = self.overview.as_mut() else { return };
        let action = ov.touch_up(slot, t);
        self.overview_action(action);
        self.queue_redraw();
    }

    fn overview_action(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Home => info!("overview: home"),
            Action::Activate(card, pose) => {
                // The card's topmost window: the app (or dialog) of the unit that had focus last.
                let Some(w) = card.windows.last().map(|(w, _)| w.clone()) else { return };
                info!(app = window_app_id(&w).as_deref().unwrap_or("?"), windows = card.windows.len(), "overview: activate");
                let a = self.area().map(|a| a.size.to_f64()).unwrap_or_default();
                let (cw, ch) = (a.w * pose.scale, a.h * pose.scale);
                let rect = Rect::new((pose.center.x - cw / 2.0) as f32, (pose.center.y - ch / 2.0) as f32, cw as f32, ch as f32);
                self.switch = None;
                self.switch_with(None, |p, ctx| p.activate(ctx, &w));
                if let Some(s) = self.switch.as_mut() {
                    s.style = Style::Card;
                    s.tile = Some(rect);
                }
            }
            Action::Pair { first, second, side, pose } => {
                let Some(w) = first.windows.last().map(|(w, _)| w.clone()) else { return };
                let with = second.and_then(|c| c.windows.last().map(|(w, _)| w.clone()));
                info!(
                    app = window_app_id(&w).as_deref().unwrap_or("?"),
                    with = with.as_ref().and_then(window_app_id).as_deref().unwrap_or("-"),
                    ?side,
                    "overview: split"
                );
                let rect = Self::pose_rect(self.area(), pose);
                self.switch = None;
                self.switch_with(None, |p, ctx| match with.as_ref().map(|x| p.snap(ctx, &w, Some(side), Some(x))) {
                    Some(f @ (FocusChange::Focus(_) | FocusChange::Clear)) => f,
                    // Nothing to pair with (or it does not fit): just show the app.
                    _ => p.activate(ctx, &w),
                });
                if let Some(s) = self.switch.as_mut() {
                    s.style = Style::Card;
                    s.tile = Some(rect);
                }
            }
            Action::Close(w) => {
                for app in self.policy.unit_apps(&w) {
                    self.close_app(&app);
                }
                if self.overview.as_ref().is_some_and(|o| o.live_cards() == 0) {
                    self.leave_overview();
                }
            }
        }
    }

    fn pose_rect(area: Option<smithay::utils::Rectangle<i32, Logical>>, pose: Pose) -> Rect {
        let a = area.map(|a| a.size.to_f64()).unwrap_or_default();
        let (cw, ch) = (a.w * pose.scale, a.h * pose.scale);
        Rect::new((pose.center.x - cw / 2.0) as f32, (pose.center.y - ch / 2.0) as f32, cw as f32, ch as f32)
    }

    /// After a frame: keep drawing while it moves, end it when it has faded out, and leave it if an app
    /// came up by itself (a new window, xdg-activation).
    pub(crate) fn overview_after_frame(&mut self) {
        let app_shown = self.space.elements().next().is_some();
        let Some(ov) = self.overview.as_mut() else { return };
        if !ov.leaving && app_shown && self.switch.is_none() {
            ov.leave();
        }
        if ov.is_done() {
            info!(
                frames = ov.frames,
                late = ov.late,
                max_gap_ms = format!("{:.1}", ov.max_gap.as_secs_f32() * 1000.0),
                max_render_ms = format!("{:.1}", ov.max_render.as_secs_f32() * 1000.0),
                "overview closed"
            );
            self.overview = None;
            self.sync_apps_shown();
            self.queue_redraw();
        } else if ov.moving {
            self.queue_redraw();
        }
    }
}
