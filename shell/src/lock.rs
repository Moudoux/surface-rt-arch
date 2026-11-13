//! Session lock, core side (DESKTOP-SHELL.md Phase 8 design). The core owns the fact "locked": while it is
//! set, frames hold only the cursor and the overlay's top layers (the lock scene), apps get no input, no
//! keyboard focus and no frame callbacks, and every touch, click and key goes to the overlay. The overlay
//! owns the lock screen's look and the password check; it alone ends the lock (`Request::Unlock`).

use smithay::{
    desktop::Window,
    input::pointer::MotionEvent,
    utils::SERIAL_COUNTER,
};
use tracing::{info, warn};

use crate::{overlay::LockReason, state::State};

#[derive(Default)]
pub struct LockState {
    pub locked: bool,
    /// The app that had keyboard focus (given back on unlock if it still exists).
    focus: Option<Window>,
    /// A frame with the lock drawn was queued; its vblank is reported to the overlay.
    pub frame_queued: bool,
    /// The lock was drawn and presented at least once since locking.
    pub presented: bool,
}

impl LockState {
    /// The app that gets keyboard focus on unlock (a window focused by the policy while locked).
    pub fn set_focus_after(&mut self, w: Option<Window>) {
        self.focus = w;
    }
}

impl State {
    pub fn locked(&self) -> bool {
        self.lock.locked
    }

    /// Lock the session. Refused (with a warning) when the overlay has no lock screen.
    pub fn lock_session(&mut self, reason: LockReason) {
        if self.lock.locked {
            // Already locked: let the overlay react (e.g. drop the curtain again before sleep).
            if let Some(o) = self.overlay.as_mut() {
                o.lock(reason);
            }
            self.queue_redraw();
            self.lock_blank_presented();
            return;
        }
        if !self.overlay.as_mut().is_some_and(|o| o.lock(reason)) {
            warn!(?reason, "lock refused: the shell UI has no lock screen");
            return;
        }
        info!(?reason, "session locked");
        // Every touch in progress ends (apps get a cancel), the Overview closes, focus goes away. Done before
        // `locked` is set: from then on all input goes to the lock screen.
        self.touch_cancel();
        if self.overview_takes_touches() {
            self.leave_overview();
        }
        self.lock.locked = true;
        self.lock.presented = false;
        self.lock.frame_queued = false;
        self.lock.focus = self.focused_window();
        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
        let pointer = self.pointer.clone();
        let event = MotionEvent { location: pointer.current_location(), serial: SERIAL_COUNTER.next_serial(), time: smithay::backend::input::InputTime::now() };
        pointer.motion(self, None, &event);
        pointer.frame(self);
        self.set_app_text_input(None);
        self.handle_overlay_requests();
        self.queue_redraw();
        // The lock screen blanks sooner (`[idle] lock_blank`).
        self.idle_arm();
        self.lock_blank_presented();
    }

    /// While the output is off nothing can be presented and nothing unlocked is shown: a lock waiting for
    /// its frame (sleep) may go ahead at once.
    fn lock_blank_presented(&mut self) {
        if self.idle.blank() && let Some(o) = self.overlay.as_mut() {
            o.lock_presented();
            self.handle_overlay_requests();
        }
    }

    /// End the lock (the overlay checked the password, or logind said so).
    pub fn unlock_session(&mut self) {
        if !self.lock.locked {
            return;
        }
        if self.greeter {
            // Fail-closed: the login screen has nothing behind it to show.
            tracing::warn!("greeter mode: unlock refused");
            return;
        }
        info!("session unlocked");
        self.lock.locked = false;
        self.lock.frame_queued = false;
        let focus = self.lock.focus.take().filter(|w| self.toplevels.iter().any(|t| &t.window == w));
        if let Some(w) = focus
            && self.space.elements().any(|e| e == &w)
        {
            let surface = crate::winconf::surface(&w);
            if let Some(kb) = self.seat.get_keyboard() {
                kb.set_focus(self, surface, SERIAL_COUNTER.next_serial());
            }
        }
        self.refresh_pointer_focus();
        self.sync_apps_shown();
        self.queue_redraw();
        self.idle_arm();
    }

    /// After a vblank: if the frame it showed had the lock in it, tell the overlay (once per lock).
    pub fn lock_frame_presented(&mut self) {
        if !std::mem::take(&mut self.lock.frame_queued) || !self.lock.locked {
            return;
        }
        if !self.lock.presented {
            self.lock.presented = true;
            info!("lock screen presented");
        }
        if let Some(o) = self.overlay.as_mut() {
            o.lock_presented();
        }
        self.handle_overlay_requests();
    }
}
