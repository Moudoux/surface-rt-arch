//! Keys from the on-screen keyboard to the focused app (core side; the keyboard itself is `ui/osk.rs`).
//!
//! The compositor is the seat's input method (text-input-v3 without an input-method-v2 client, state.rs).
//! - Plain text while the app has text input enabled: `commit_string` + `done` on its active text-input.
//! - Everything else (Backspace, Enter, Tab, Escape, arrows, Ctrl/Alt combinations, and text for apps
//!   without text input, e.g. a terminal that does not enable it): key presses on the seat keyboard, with
//!   the keycode found in the seat's own keymap (Shift added when the keysym is on level 2). Characters the
//!   keymap does not have (accented letters on a US map) go through Smithay's `inject_text_keysyms`
//!   (a throwaway keymap for that one key).
//! Typed text is never logged. `RTSHELL_OSK_KEYS_ONLY=1` sends text as keys even to apps with text input.

use smithay::{
    backend::input::{InputTime, KeyState},
    input::keyboard::{FilterResult, Keycode, Keysym, xkb},
    utils::SERIAL_COUNTER,
    wayland::text_input::TextInputSeat,
};
use tracing::debug;

use crate::{overlay::KeyEvent, state::State};

/// evdev codes + 8 (xkb keycodes) of the modifiers we press around a key.
const LEFT_SHIFT: u32 = 42 + 8;
const LEFT_CTRL: u32 = 29 + 8;
const LEFT_ALT: u32 = 56 + 8;

impl State {
    /// Deliver one on-screen keyboard key to the app with keyboard focus.
    pub fn osk_key(&mut self, ev: KeyEvent) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        if keyboard.current_focus().is_none() {
            return;
        }
        self.kick_boost();
        let plain = ev.text.as_ref().filter(|_| !ev.ctrl && !ev.alt);
        // RTSHELL_OSK_KEYS_ONLY=1: never commit_string (tests the key path with apps that have text input).
        static KEYS_ONLY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let keys_only = *KEYS_ONLY.get_or_init(|| std::env::var_os("RTSHELL_OSK_KEYS_ONLY").is_some_and(|v| v == "1"));
        if let Some(text) = plain
            && self.app_text_input.is_some()
            && !keys_only
        {
            let ti = self.seat.text_input();
            let mut sent = false;
            ti.with_active_text_input(|t, _| {
                t.commit_string(Some(text.clone()));
                sent = true;
            });
            if sent {
                ti.done(false);
                return;
            }
        }
        match self.keycode_for(ev.sym) {
            Some((code, shift)) => {
                let mut mods = Vec::new();
                if ev.ctrl {
                    mods.push(LEFT_CTRL);
                }
                if ev.alt {
                    mods.push(LEFT_ALT);
                }
                if shift {
                    mods.push(LEFT_SHIFT);
                }
                self.tap_key(code, &mods);
            }
            None => {
                if let Some(text) = plain {
                    let syms: Vec<Keysym> = text.chars().map(|c| xkb::utf32_to_keysym(c as u32)).collect();
                    keyboard.inject_text_keysyms(self, &syms, InputTime::now());
                } else {
                    debug!(sym = ev.sym, "osk: no keycode for this key in the keymap");
                }
            }
        }
    }

    /// Press `mods`, tap `code`, release `mods` (all xkb keycodes), as if typed on a keyboard. The keys go
    /// straight to the focused client (no shell shortcuts).
    fn tap_key(&mut self, code: u32, mods: &[u32]) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let time = InputTime::now();
        let send = |state: &mut State, code: u32, st: KeyState| {
            keyboard.input::<(), _>(state, Keycode::new(code), st, SERIAL_COUNTER.next_serial(), time, |_, _, _| FilterResult::Forward);
        };
        for &m in mods {
            send(self, m, KeyState::Pressed);
        }
        send(self, code, KeyState::Pressed);
        send(self, code, KeyState::Released);
        for &m in mods.iter().rev() {
            send(self, m, KeyState::Released);
        }
    }

    /// The keycode that types `sym` in the seat keymap's first layout, and whether Shift is needed (level 2).
    fn keycode_for(&mut self, sym: u32) -> Option<(u32, bool)> {
        let keyboard = self.seat.get_keyboard()?;
        let want = Keysym::new(sym);
        keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            // SAFETY: the keymap reference is used only inside this closure, while the Xkb is locked.
            let keymap = unsafe { xkb.keymap() };
            let (min, max) = (keymap.min_keycode().raw(), keymap.max_keycode().raw());
            for level in 0..2u32 {
                for code in min..=max {
                    if keymap.key_get_syms_by_level(Keycode::new(code), 0, level).contains(&want) {
                        return Some((code, level == 1));
                    }
                }
            }
            None
        })
    }
}
