//! Policy-owned handles (DESKTOP-SHELL.md 3.6 / Phase 7): a touch or left click that starts on something the
//! window policy draws itself (the split divider; title bars in a later floating policy) goes to the policy
//! instead of the app, until it is lifted. Core layer: it only asks `WindowPolicy::handle_at` and forwards.

use std::time::Duration;

use smithay::utils::{Logical, Point};

use crate::{
    policy::{FocusChange, HandlePhase},
    state::State,
};

impl State {
    /// A finger or the left button went down at `pos`: true if a policy handle took it.
    pub(super) fn handle_down(&mut self, id: i32, pos: Point<f64, Logical>) -> bool {
        if self.handle_slot.is_some() || self.switch.is_some() || self.overview.is_some() {
            return false;
        }
        if !self.policy_ctx(|p, ctx| p.handle_at(ctx, pos)).unwrap_or(false) {
            return false;
        }
        self.handle_slot = Some(id);
        self.handle_event(HandlePhase::Down, pos);
        true
    }

    /// Motion or lift of the touch/button holding a handle: true if it was one.
    pub(super) fn handle_moved(&mut self, id: i32, phase: HandlePhase, pos: Point<f64, Logical>) -> bool {
        if self.handle_slot != Some(id) {
            return false;
        }
        if matches!(phase, HandlePhase::Up | HandlePhase::Cancel) {
            self.handle_slot = None;
        }
        self.handle_event(phase, pos);
        true
    }

    /// Cancel a held handle (touch cancel, rotation, a switch starting).
    pub(crate) fn handle_cancel(&mut self) {
        if let Some(id) = self.handle_slot {
            self.handle_moved(id, HandlePhase::Cancel, Point::default());
        }
    }

    fn handle_event(&mut self, phase: HandlePhase, pos: Point<f64, Logical>) {
        self.kick_boost();
        let now: Duration = self.clock.now().into();
        let change = self.policy_ctx(|p, ctx| p.handle_drag(ctx, phase, pos, now)).unwrap_or(FocusChange::Keep);
        if change != FocusChange::Keep {
            self.with_policy(|_, _| change);
        }
        self.queue_redraw();
    }
}
