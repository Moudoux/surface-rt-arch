//! Scripted input for testing the shell UI without a finger (`RTSHELL_INPUT_SCRIPT=file`), nested or on
//! the device. Touches take the same path as libinput touches (slot 0: edge gestures, shell UI, apps); keys
//! go to the overlay only.
//!
//! One command per line (`#` comments):
//!   wait MS                      pause
//!   tap X Y                      touch down and up 60 ms later
//!   hold X Y MS                  touch down, stay MS, up
//!   drag X0 Y0 X1 Y1 MS          touch down, move in 16 ms steps over MS, up (a flick if MS is short)
//!   down X Y [S] / move X Y [S] / up [S]   single touch events (slot S, default 0: two fingers = two slots)
//!   wheel DX DY                  scroll wheel / touchpad amounts in px
//!   type TEXT                    key presses for each character (rest of the line)
//!   key NAME                     Return, Escape, BackSpace, Left, Right, Tab
//!   home                         the Super key tap (toggle Home)
//!   cycle                        Alt+Tab (next app)
//!   snap left|right|full         Super+Left / Super+Right / Super+Up (split view)
//!   overview                     Super+Tab (toggle the Overview)
//!   panel                        Super+A (toggle the Control Center)
//!   volume up|down|mute [MS]     a volume key held MS (default 60; up/down repeat after the key delay)
//!   osk                          show / hide the on-screen keyboard by hand
//!   osktap NAME [MS]             tap the on-screen key NAME (a character, or space, backspace, enter, shift,
//!                                tab, esc, ctrl, alt, left, right, up, down, home, end, 123, abc, more, hide;
//!                                `\s` = a space character), held MS (default 60)
//!   oskdown NAME / oskmove NAME  finger down on / slide to key NAME (or a long-press accent), then `up`
//!   rotate NAME                  as if the accelerometer said NAME (normal, left-up, right-up, bottom-up)
//!   lock                         Super+L (lock the session)
//!   locktap NAME [MS]            tap the lock screen's control NAME (PIN digit 0-9, ok, back, field,
//!                                submit; greeter: user:LOGIN = a user tile, switch = "Switch user"), held MS
//!                                (default 60); `type` and `key` go to the lock while locked
//!   lid close|open               the lid switch
//!   shot                         screenshot (as SIGUSR1)
//!   rawkey CODE down|up          a key event through the real key path (evdev code: 116 power, 114 volume
//!                                down, 125 Super, 42 Shift, 31 S); unlike `key`, shortcuts act
//!   quit                         stop rtshell

use std::time::Duration;

use smithay::{
    input::keyboard::keysyms as k,
    backend::input::InputTime,
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::Point,
};
use tracing::{info, warn};

use crate::{
    overlay::{KeyEvent, Phase},
    state::State,
};

#[derive(Debug, Clone)]
enum Step {
    Wait(u64),
    /// Phase, x, y, slot.
    Touch(Phase, f32, f32, i32),
    Wheel(f32, f32),
    Key(u32, Option<String>),
    Home,
    Cycle,
    Snap(Option<crate::policy::SnapSide>),
    Overview,
    Panel,
    /// A volume key down (Some) or up (None).
    Volume(Option<crate::overlay::VolumeKey>),
    Osk,
    /// Touch down (true) or move (false) on the on-screen key NAME, slot 0.
    OskTouch(String, bool),
    Rotate(crate::rotate::Orientation),
    Lock,
    /// Touch down on the lock screen's control NAME, slot 0.
    LockTouch(String),
    /// Lid closed (true) or opened.
    Lid(bool),
    Shot,
    /// A raw key event through the real key path (xkb keycode = evdev + 8; true = pressed).
    RawKey(u32, bool),
    Quit,
}

fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let nums: Vec<f32> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        let need = |k: usize| if nums.len() >= k { Ok(()) } else { Err(format!("line {}: {cmd} needs {k} numbers", n + 1)) };
        match cmd {
            "wait" => {
                need(1)?;
                out.push(Step::Wait(nums[0] as u64));
            }
            "tap" => {
                need(2)?;
                out.extend([Step::Touch(Phase::Down, nums[0], nums[1], 0), Step::Wait(60), Step::Touch(Phase::Up, nums[0], nums[1], 0)]);
            }
            "hold" => {
                need(3)?;
                out.extend([
                    Step::Touch(Phase::Down, nums[0], nums[1], 0),
                    Step::Wait(nums[2] as u64),
                    Step::Touch(Phase::Up, nums[0], nums[1], 0),
                ]);
            }
            "drag" => {
                need(5)?;
                let (x0, y0, x1, y1, ms) = (nums[0], nums[1], nums[2], nums[3], nums[4].max(16.0));
                out.push(Step::Touch(Phase::Down, x0, y0, 0));
                let steps = (ms / 16.0).ceil() as u32;
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    out.push(Step::Wait(16));
                    out.push(Step::Touch(Phase::Motion, x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, 0));
                }
                out.push(Step::Touch(Phase::Up, x1, y1, 0));
            }
            "down" | "move" => {
                need(2)?;
                let ph = if cmd == "down" { Phase::Down } else { Phase::Motion };
                out.push(Step::Touch(ph, nums[0], nums[1], nums.get(2).map_or(0, |s| *s as i32)));
            }
            "up" => out.push(Step::Touch(Phase::Up, 0.0, 0.0, nums.first().map_or(0, |s| *s as i32))),
            "wheel" => {
                need(2)?;
                out.push(Step::Wheel(nums[0], nums[1]));
            }
            "type" => {
                for c in rest.chars() {
                    out.push(Step::Key(c as u32, Some(c.to_string())));
                    out.push(Step::Wait(30));
                }
            }
            "key" => {
                let sym = match rest.trim() {
                    "Return" => k::KEY_Return,
                    "Escape" => k::KEY_Escape,
                    "BackSpace" => k::KEY_BackSpace,
                    "Left" => k::KEY_Left,
                    "Right" => k::KEY_Right,
                    "Tab" => k::KEY_Tab,
                    other => return Err(format!("line {}: unknown key {other}", n + 1)),
                };
                out.push(Step::Key(sym, None));
            }
            "home" => out.push(Step::Home),
            "cycle" => out.push(Step::Cycle),
            "snap" => out.push(Step::Snap(match Some(rest.trim()) {
                Some("left") | Some("top") => Some(crate::policy::SnapSide::Start),
                Some("right") | Some("bottom") => Some(crate::policy::SnapSide::End),
                _ => None,
            })),
            "overview" => out.push(Step::Overview),
            "panel" => out.push(Step::Panel),
            "volume" => {
                use crate::overlay::VolumeKey;
                let mut parts = rest.split_whitespace();
                let key = match parts.next() {
                    Some("up") => VolumeKey::Up,
                    Some("down") => VolumeKey::Down,
                    Some("mute") => VolumeKey::Mute,
                    _ => return Err(format!("line {}: volume up|down|mute [MS]", n + 1)),
                };
                let ms = parts.next().and_then(|v| v.parse().ok()).unwrap_or(60);
                out.extend([Step::Volume(Some(key)), Step::Wait(ms), Step::Volume(None)]);
            }
            "osk" => out.push(Step::Osk),
            "osktap" | "oskdown" | "oskmove" => {
                let mut parts = rest.split_whitespace();
                let Some(name) = parts.next() else { return Err(format!("line {}: {cmd} needs a key name", n + 1)) };
                let name = name.to_string();
                match cmd {
                    "osktap" => {
                        let ms = parts.next().and_then(|v| v.parse().ok()).unwrap_or(60);
                        out.extend([Step::OskTouch(name, true), Step::Wait(ms), Step::Touch(Phase::Up, 0.0, 0.0, 0)]);
                    }
                    "oskdown" => out.push(Step::OskTouch(name, true)),
                    _ => out.push(Step::OskTouch(name, false)),
                }
            }
            "rotate" => match crate::rotate::Orientation::parse(rest.trim()) {
                Some(o) => out.push(Step::Rotate(o)),
                None => return Err(format!("line {}: unknown orientation {}", n + 1, rest.trim())),
            },
            "lock" => out.push(Step::Lock),
            "locktap" => {
                let mut it = rest.split_whitespace();
                let name = it.next().ok_or(format!("line {}: locktap NAME [MS]", n + 1))?.to_string();
                let ms = it.next().and_then(|v| v.parse().ok()).unwrap_or(60);
                out.push(Step::LockTouch(name));
                out.push(Step::Wait(ms));
                out.push(Step::Touch(Phase::Up, 0.0, 0.0, 0));
            }
            "lid" => out.push(Step::Lid(match rest.trim() {
                "close" => true,
                "open" => false,
                _ => return Err(format!("line {}: lid close|open", n + 1)),
            })),
            "shot" => out.push(Step::Shot),
            "rawkey" => {
                let mut it = rest.split_whitespace();
                let code = it.next().and_then(|c| c.parse::<u32>().ok());
                let pressed = match it.next() {
                    Some("down") => Some(true),
                    Some("up") => Some(false),
                    _ => None,
                };
                match (code, pressed) {
                    (Some(c), Some(p)) => out.push(Step::RawKey(c + 8, p)),
                    _ => return Err(format!("line {}: rawkey EVDEV_CODE down|up", n + 1)),
                }
            }
            "quit" => out.push(Step::Quit),
            other => return Err(format!("line {}: unknown command {other}", n + 1)),
        }
    }
    Ok(out)
}

/// Start replaying the script named by RTSHELL_INPUT_SCRIPT (if set), after `delay`.
pub fn start(state: &mut State, delay: Duration) {
    let Some(path) = std::env::var_os("RTSHELL_INPUT_SCRIPT") else { return };
    let steps = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
        Ok(s) => s,
        Err(err) => return warn!(path = ?path, %err, "input script not usable"),
    };
    info!(path = ?path, steps = steps.len(), "input script");
    let mut i = 0;
    let res = state.loop_handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
        while let Some(step) = steps.get(i).cloned() {
            i += 1;
            // Every step is user activity like real input (touches count inside `touch_down` & co.); a key
            // or scroll that wakes a blank screen is dropped there too.
            let input = !matches!(step, Step::Wait(_) | Step::Shot | Step::Quit | Step::Touch(..) | Step::OskTouch(..) | Step::LockTouch(..));
            if input && state.idle_activity() && matches!(step, Step::Key(..) | Step::Wheel(..)) {
                info!("input script: step woke the screen (dropped)");
                continue;
            }
            match step {
                Step::Wait(ms) => return TimeoutAction::ToDuration(Duration::from_millis(ms)),
                Step::Touch(phase, x, y, slot) => {
                    // The same routing as libinput touches: edge gestures, then the shell UI, then apps.
                    let pos = Point::from((x as f64, y as f64));
                    let t = InputTime::now();
                    match phase {
                        Phase::Down => state.touch_down(slot, pos, t),
                        Phase::Motion => state.touch_motion(slot, pos, t),
                        Phase::Up => state.touch_up(slot, t),
                        Phase::Cancel => state.touch_cancel(),
                    }
                    state.touch_frame();
                }
                Step::Wheel(dx, dy) => {
                    let pos = state.pointer.current_location();
                    if let Some(o) = state.visible_overlay() {
                        o.axis(pos.x as f32, pos.y as f32, dx, dy);
                    }
                    state.queue_redraw();
                }
                Step::Key(sym, text) if state.locked() => {
                    let ev = KeyEvent { sym, text, ctrl: false, alt: false };
                    if let Some(o) = state.overlay.as_mut() {
                        o.lock_key(&ev);
                    }
                    state.handle_overlay_requests();
                    state.queue_redraw();
                }
                Step::Key(sym, text) => {
                    let ev = KeyEvent { sym, text, ctrl: false, alt: false };
                    let panel = state.overlay.as_mut().is_some_and(|o| o.panel_active() && o.panel_key(&ev));
                    if !panel && let Some(o) = state.visible_overlay() {
                        o.key(&ev);
                    }
                    state.handle_overlay_requests();
                    state.queue_redraw();
                }
                Step::Home => state.toggle_home(),
                Step::Cycle => state.cycle(true),
                Step::Snap(side) => state.run_action(crate::input::bindings::Action::Snap(side)),
                Step::Overview => state.toggle_overview(),
                Step::Panel => state.run_action(crate::input::bindings::Action::ControlCenter),
                Step::Volume(Some(key)) => state.volume_press(key, None),
                Step::Volume(None) => state.volume_release(),
                Step::Osk => {
                    if state.overlay.as_mut().is_some_and(|o| o.toggle_osk()) {
                        state.handle_overlay_requests();
                        state.queue_redraw();
                    }
                }
                Step::OskTouch(name, down) => {
                    let Some((x, y)) = state.overlay.as_ref().and_then(|o| o.osk_point(&name)) else {
                        warn!(key = %name, "input script: no such on-screen key now");
                        continue;
                    };
                    let pos = Point::from((x as f64, y as f64));
                    if down {
                        state.touch_down(0, pos, InputTime::now());
                    } else {
                        state.touch_motion(0, pos, InputTime::now());
                    }
                    state.touch_frame();
                }
                Step::Rotate(o) => {
                    state.want_orientation = o;
                    state.apply_rotation();
                }
                Step::Lock => state.run_action(crate::input::bindings::Action::Lock),
                Step::LockTouch(name) => {
                    let Some((x, y)) = state.overlay.as_ref().and_then(|o| o.lock_point(&name)) else {
                        warn!(control = %name, "input script: no such lock-screen control now");
                        continue;
                    };
                    state.touch_down(0, Point::from((x as f64, y as f64)), InputTime::now());
                    state.touch_frame();
                }
                Step::Lid(closed) => {
                    if closed {
                        state.lock_session(crate::overlay::LockReason::Sleep);
                    }
                }
                Step::Shot => state.take_screenshot(),
                Step::RawKey(code, pressed) => {
                    let ks = if pressed { smithay::backend::input::KeyState::Pressed } else { smithay::backend::input::KeyState::Released };
                    state.key_input(code.into(), ks, InputTime::now());
                }
                Step::Quit => {
                    state.loop_signal.stop();
                    state.loop_signal.wakeup();
                }
            }
        }
        info!("input script done");
        TimeoutAction::Drop
    });
    if let Err(err) = res {
        warn!(%err, "cannot start input script");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let s = parse("tap 1 2\n# c\ndrag 0 0 100 0 32 # flick\ntype ab\nkey Return\nshot").unwrap();
        assert_eq!(s.len(), 3 + (1 + 2 * 2 + 1) + 4 + 1 + 1);
        assert!(parse("tap 1").is_err());
        assert!(parse("jump").is_err());
    }
}
