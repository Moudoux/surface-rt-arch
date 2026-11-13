//! The interface between the compositor core and the shell UI (DESKTOP-SHELL.md 3.6): the core asks the
//! overlay for a render element each frame, feeds it touch/pointer input first, and keeps rendering while it
//! animates. The core never sees `ui/` types; `main.rs` plugs an implementation in.

use std::{path::PathBuf, time::Duration};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Physical, Size},
};

use crate::render::{batch::Rect, layer::UiElement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Down,
    Motion,
    Up,
    Cancel,
}

/// A touch point or the mouse (id `MOUSE`), in output pixels, timestamped on the monotonic clock.
#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    pub id: i32,
    pub phase: Phase,
    pub x: f32,
    pub y: f32,
    pub time: Duration,
}

pub const MOUSE: i32 = -1;
/// Id of a `Phase::Cancel` that ends every touch (libinput's touch cancel has no slot).
pub const CANCEL_ALL: i32 = i32::MIN;

/// A key press for the overlay (only while it is visible and no app has keyboard focus), or one the
/// on-screen keyboard typed (`Request::OskKey`). Its Debug output hides the text (it may be a password).
#[derive(Clone, PartialEq)]
pub struct KeyEvent {
    /// xkb keysym after modifiers.
    pub sym: u32,
    /// The text the key types, if any (control characters excluded).
    pub text: Option<String>,
    pub ctrl: bool,
    pub alt: bool,
}

impl std::fmt::Debug for KeyEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self.text.as_ref().map(|_| "..");
        f.debug_struct("KeyEvent").field("text", &text).field("ctrl", &self.ctrl).field("alt", &self.alt).finish_non_exhaustive()
    }
}

/// What kind of text the focused field takes (from text-input-v3's content purpose), for the on-screen
/// keyboard's first page and whether it shows key previews.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextPurpose {
    Text,
    Password,
    Number,
    Terminal,
}

/// A right-edge swipe as the core's gesture recognizer reports it (the panel follows the finger).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgePhase {
    Begin,
    Move,
    End,
    Cancel,
}

/// A volume key (the Surface's side buttons, the Type Cover's media keys). Up and Down repeat while held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKey {
    Up,
    Down,
    Mute,
}

/// What the overlay asks the core to do (collected with `take_requests`).
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Bring the app forward if one of its windows exists (xdg app_id equal to one of `app_ids`, compared
    /// lowercased), else start `cmd` in `cwd`; `terminal` runs it inside the configured terminal.
    /// `from`: the rectangle that was tapped (screen pixels); a running app grows from it.
    Launch { cmd: Vec<String>, app_ids: Vec<String>, cwd: Option<PathBuf>, terminal: bool, from: Option<Rect> },
    /// The answer to `Overlay::prompt_not_responding` number `id`: force quit the app, or wait longer.
    ForceQuit { id: u64, kill: bool },
    /// Show the UI in this orientation (auto-rotate; the core keeps it normal while a keyboard is attached).
    Rotate(crate::rotate::Orientation),
    /// A key from the on-screen keyboard for the focused app: text through text-input-v3 when the app has
    /// it enabled, otherwise (and for special keys and Ctrl/Alt combinations) as wl_keyboard key events.
    OskKey(KeyEvent),
    /// Keep this many pixels at the bottom of the screen free of apps (the on-screen keyboard; 0 = none).
    /// Fullscreen apps are not resized.
    Reserve(i32),
    /// Lock the session now (the Control Center's Lock button, logind's Lock signal, before sleep).
    Lock(LockReason),
    /// The lock screen was unlocked (a correct password, or logind's Unlock signal).
    Unlock,
    /// End rtshell (exit 0): the greeter after greetd accepted the session start.
    Quit,
    /// End rtshell with exit status `RESTART_EXIT` so `rtshell-session` starts it again at once (Settings'
    /// "Restart rtshell", e.g. after a theme that changes the style).
    Restart,
    /// Count as user activity: wake a dimmed or blank screen (resume from sleep, power plugged in or out).
    Wake,
    /// The user's config.toml changed (the Settings app wrote it): apply what can change at runtime
    /// (keyboard layout and repeat, `[input]`, `[idle]`, `[apps]`, `[osk] resize_apps`).
    ConfigChanged(Box<crate::config::Config>),
}

/// How idle the session is (DESKTOP-SHELL.md Phase 8 "Idle"); later stages include the earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdleStage {
    Active,
    /// The backlight is dimmed.
    Dim,
    /// The output is off.
    Blank,
    /// The system was asked to sleep.
    Sleep,
}

/// Why the session is being locked (the lock screen may show it differently, e.g. no curtain animation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockReason {
    /// Super+L, the Control Center, logind's Lock signal.
    User,
    Idle,
    /// Lid closed or the system is going to sleep: draw the lock at once.
    Sleep,
}

pub trait Overlay {
    /// The UI layer for the frame that will be presented at `present_at` (CLOCK_MONOTONIC). Steps
    /// animations to that time. None draws nothing (the overlay is hidden).
    fn element(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> Option<UiElement>;
    /// True while something moves: the core then renders every vblank.
    fn animating(&self) -> bool;
    /// Super+I: open (or close) the Settings app. False without one.
    fn open_settings(&mut self) -> bool {
        false
    }
    /// Close a full-screen UI sheet (Settings) if one is open: Super and the Home gesture do that first.
    fn close_sheet(&mut self) -> bool {
        false
    }
    /// A user screenshot was saved (`Ok(path as shown)`) or failed (`Err(reason)`): feedback (flash, banner).
    fn screenshot_taken(&mut self, _result: &Result<String, String>) {}
    /// The power button was tapped: sleep (locking first). False if the overlay cannot (no services).
    fn power_key(&mut self) -> bool {
        false
    }
    /// The output's size in pixels, as soon as the overlay exists (before the first frame): lets it start
    /// size-dependent work such as the wallpaper early.
    fn size_hint(&mut self, _w: i32, _h: i32) {}
    /// When the overlay next needs a frame although nothing animates (a clock tick, a timed state change).
    fn next_wakeup(&self) -> Option<Duration>;
    /// Input goes here before clients. Returns true if the overlay consumed it.
    fn input(&mut self, ev: PointerEvent) -> bool;
    /// Called from a worker-thread wakeup (e.g. the wallpaper finished loading); true if a redraw is needed.
    fn poll(&mut self) -> bool;
    /// Whether an app is on screen. Home hides behind apps (and gets no input then); a debug overlay may stay.
    fn set_apps_shown(&mut self, _shown: bool) {}
    /// A touch the side-panel layer takes although the panel is not open (e.g. on a notification banner, or
    /// one it already tracks). Checked before `panel_active` lets input through to Home and apps.
    fn panel_wants(&self, _id: i32, _x: f32, _y: f32) -> bool {
        false
    }
    /// A hardware keyboard (the Type Cover) was attached or removed (auto-rotate and the on-screen keyboard
    /// are off while attached).
    fn set_hw_keyboard(&mut self, _attached: bool) {}
    /// The focused app enabled text input (Some, with the field's purpose) or disabled it / lost focus.
    fn set_text_input(&mut self, _purpose: Option<TextPurpose>) {}
    /// Show or hide the on-screen keyboard by hand (for apps without text input). False if there is none.
    fn toggle_osk(&mut self) -> bool {
        false
    }
    /// Where the on-screen keyboard key (or long-press accent) named `name` is now, in output pixels (for
    /// scripted tests). None if the keyboard is not up or has no such key.
    fn osk_point(&self, _name: &str) -> Option<(f32, f32)> {
        None
    }
    /// Draw it and route input to it now?
    fn visible(&self) -> bool {
        true
    }
    /// Scroll wheel / touchpad scrolling, in pixels (positive = right/down). True if consumed.
    fn axis(&mut self, _x: f32, _y: f32, _dx: f32, _dy: f32) -> bool {
        false
    }
    /// A key press while visible and no app is focused. True if consumed.
    fn key(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take_requests(&mut self) -> Vec<Request> {
        Vec::new()
    }
    /// Where the app with this xdg app_id (lowercased) lives on screen now (its tile), for the switch
    /// animation to shrink into / grow from. None if it has no visible tile.
    fn app_rect(&self, _app_id: &str) -> Option<Rect> {
        None
    }
    /// How far an app switch over the overlay has gone: 0 = an app covers it, 1 = nothing does (the default
    /// outside switches). Called before `element` each frame.
    fn set_reveal(&mut self, _r: f32) {}
    /// An app (xdg app_id, lowercased) was asked to close and is still there: ask whether to force quit it.
    /// The answer comes back as `Request::ForceQuit { id, .. }`. Returns false if the overlay cannot ask.
    fn prompt_not_responding(&mut self, _id: u64, _app_id: &str) -> bool {
        false
    }
    /// The question `id` no longer applies (the app closed after all).
    fn dismiss_prompt(&mut self, _id: u64) {}
    /// Draw only the Overview's backdrop (on) or the normal UI again (off), for the core to render the
    /// backdrop once into a texture. Returns false if it cannot be drawn yet (wallpaper not ready).
    fn show_backdrop(&mut self, _on: bool) -> bool {
        false
    }
    /// Changes whenever the backdrop would look different (new wallpaper, theme): the core then renders it
    /// again.
    fn backdrop_serial(&self) -> u64 {
        0
    }

    // ---- Top layers: the side panel (Control Center) and the on-screen keyboard, drawn above everything,
    // apps included, each as its own element. The `panel_*` input methods cover both.

    /// The right-edge swipe: `x` is the finger (output px), `vx` its velocity (px/s). Begin returns false
    /// if there is no panel (the core then refuses the gesture).
    fn panel_edge(&mut self, _phase: EdgePhase, _x: f32, _vx: f32) -> bool {
        false
    }
    /// Open or close the panel (Super+A). False if there is none.
    fn toggle_panel(&mut self) -> bool {
        false
    }
    /// The panel is open (or held): it gets touches, clicks and keys before anyone else.
    fn panel_active(&self) -> bool {
        false
    }
    /// A touch or click while the panel is active. True if the panel took it (a touch down always is).
    fn panel_input(&mut self, _ev: PointerEvent) -> bool {
        false
    }
    /// A key press while the panel is active. True if consumed.
    fn panel_key(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    /// A volume key press (or a repeat while held): change the volume and show the level. False if the
    /// overlay has no volume control.
    fn volume_key(&mut self, _key: VolumeKey) -> bool {
        false
    }
    /// The top layers' elements for this frame, topmost first (drawn above everything); empty while none
    /// is up.
    fn top_elements(&mut self, _renderer: &mut GlesRenderer, _size: Size<i32, Physical>, _present_at: Duration) -> Vec<UiElement> {
        Vec::new()
    }

    // ---- Lock screen (Phase 8). While the core is locked it draws only the cursor and `top_elements`
    // (which then hold the lock scene), and sends every touch, click and key here.

    /// Lock now (the core already counts the session as locked when this returns true). False if the
    /// overlay has no lock screen: the core then refuses to lock (nothing could unlock it).
    fn lock(&mut self, _reason: LockReason) -> bool {
        false
    }
    /// A touch or click while locked.
    fn lock_input(&mut self, _ev: PointerEvent) {}
    /// A key press while locked.
    fn lock_key(&mut self, _key: &KeyEvent) {}
    /// Scroll while locked (ignored by default).
    fn lock_axis(&mut self, _dx: f32, _dy: f32) {}
    /// A frame showing the lock was presented (vblank): e.g. sleep may go ahead now.
    fn lock_presented(&mut self) {}
    /// Where the lock screen's control `name` is now (PIN pad digit, "ok", "back", "field"), for scripted
    /// tests. None if it is not shown.
    fn lock_point(&self, _name: &str) -> Option<(f32, f32)> {
        None
    }

    // ---- Idle (Phase 8). The core times the stages and turns the output off itself.

    /// The idle stage changed: `Dim` = dim the backlight to `dim_level` x its brightness, `Active` = bring
    /// it back, `Blank` = the output is now off, `Sleep` = suspend the system.
    fn idle(&mut self, _stage: IdleStage, _dim_level: f32) {}
    /// On AC power (Some(true)), on battery, or unknown (None = battery times). The overlay pings when it
    /// changes.
    fn on_ac_power(&self) -> Option<bool> {
        None
    }
}
