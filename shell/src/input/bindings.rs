//! Compositor shortcuts (Phase 1 set; DESKTOP-SHELL.md 3.3 lists the full plan).
//!
//! | Keys                          | Action                                   |
//! |-------------------------------|------------------------------------------|
//! | Ctrl+Alt+Backspace            | quit rtshell                             |
//! | Ctrl+Alt+F1..F12              | switch VT                                |
//! | Super+Return                  | terminal (`apps.terminal`)               |
//! | Super+1..9                    | launch `apps.launch[n-1]`                |
//! | Super+Q                       | close the focused app                    |
//! | Alt+Tab (+Shift)              | next / previous app                      |
//! | Super+Tab                     | Overview (toggle)                        |
//! | Super+A                       | Control Center (toggle)                  |
//! | Super+L                       | lock the session                         |
//! | Super+Left / Super+Right      | split view: focused app to the left/right half (swaps a pair) |
//! | Super+Up                      | split view: focused app back to the whole screen |
//! | Super (tap alone)             | Home / back to the last app (input/mod.rs) |
//! | Volume up / down / mute keys  | change the volume, show the level (any modifiers; up/down repeat) |
//! | Super+Shift+S                 | screenshot to the Pictures folder        |
//! | Super+I                       | Settings (toggle)                        |
//! | Power + Volume down (chord)   | screenshot (input/mod.rs: volume-down while power is held) |

use smithay::input::keyboard::{Keysym, KeysymHandle, ModifiersState, keysyms as k};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Quit,
    VtSwitch(i32),
    Terminal,
    Launch(usize),
    CloseFocused,
    Cycle { forward: bool },
    Overview,
    Home,
    ControlCenter,
    /// Split view: snap the focused app to a half, or back to the whole screen (None).
    Snap(Option<crate::policy::SnapSide>),
    /// A volume key (the side buttons, the Type Cover's media keys).
    Volume(crate::overlay::VolumeKey),
    Lock,
    /// The power button (gpio-keys KEY_POWER), handled on release: a tap sleeps. rtshell holds logind's
    /// handle-power-key block lock while its services run, so logind does not power off.
    Power,
    /// Save a screenshot to the Pictures folder (Super+Shift+S, or Volume-down while Power is held).
    Screenshot,
    /// Open or close the Settings app (Super+I).
    Settings,
}

pub fn lookup(mods: &ModifiersState, handle: &KeysymHandle<'_>) -> Option<Action> {
    // xkb maps Ctrl+Alt+Fn to XF86Switch_VT_n in its default keymap.
    for sym in handle.modified_syms() {
        let raw = sym.raw();
        if (k::KEY_XF86Switch_VT_1..=k::KEY_XF86Switch_VT_12).contains(&raw) {
            return Some(Action::VtSwitch((raw - k::KEY_XF86Switch_VT_1 + 1) as i32));
        }
    }
    let sym = handle.raw_latin_sym_or_raw_current_sym().unwrap_or(Keysym::from(0));
    lookup_sym(mods, sym.raw())
}

fn lookup_sym(mods: &ModifiersState, raw: u32) -> Option<Action> {
    use crate::overlay::VolumeKey;
    match raw {
        k::KEY_XF86AudioRaiseVolume => return Some(Action::Volume(VolumeKey::Up)),
        k::KEY_XF86AudioLowerVolume => return Some(Action::Volume(VolumeKey::Down)),
        k::KEY_XF86AudioMute => return Some(Action::Volume(VolumeKey::Mute)),
        k::KEY_XF86PowerOff => return Some(Action::Power),
        _ => {}
    }
    if mods.ctrl && mods.alt {
        if raw == k::KEY_BackSpace {
            return Some(Action::Quit);
        }
        if (k::KEY_F1..=k::KEY_F12).contains(&raw) {
            return Some(Action::VtSwitch((raw - k::KEY_F1 + 1) as i32));
        }
    }
    if raw == k::KEY_Tab && mods.logo && !mods.ctrl && !mods.alt {
        return Some(Action::Overview);
    }
    if raw == k::KEY_Tab && mods.alt && !mods.ctrl {
        return Some(Action::Cycle { forward: !mods.shift });
    }
    if mods.logo && !mods.ctrl && !mods.alt {
        return match raw {
            k::KEY_Return | k::KEY_KP_Enter => Some(Action::Terminal),
            k::KEY_q | k::KEY_Q => Some(Action::CloseFocused),
            k::KEY_a | k::KEY_A => Some(Action::ControlCenter),
            k::KEY_l | k::KEY_L => Some(Action::Lock),
            k::KEY_1..=k::KEY_9 => Some(Action::Launch((raw - k::KEY_1) as usize)),
            k::KEY_Left => Some(Action::Snap(Some(crate::policy::SnapSide::Start))),
            k::KEY_Right => Some(Action::Snap(Some(crate::policy::SnapSide::End))),
            k::KEY_Up => Some(Action::Snap(None)),
            k::KEY_s | k::KEY_S if mods.shift => Some(Action::Screenshot),
            k::KEY_i | k::KEY_I => Some(Action::Settings),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool, logo: bool) -> ModifiersState {
        ModifiersState {
            ctrl,
            alt,
            shift,
            logo,
            ..Default::default()
        }
    }

    #[test]
    fn bindings() {
        assert_eq!(lookup_sym(&mods(true, true, false, false), k::KEY_BackSpace), Some(Action::Quit));
        assert_eq!(lookup_sym(&mods(true, true, false, false), k::KEY_F3), Some(Action::VtSwitch(3)));
        assert_eq!(lookup_sym(&mods(false, false, false, true), k::KEY_3), Some(Action::Launch(2)));
        assert_eq!(
            lookup_sym(&mods(false, true, true, false), k::KEY_Tab),
            Some(Action::Cycle { forward: false })
        );
        assert_eq!(lookup_sym(&mods(false, false, false, true), k::KEY_Tab), Some(Action::Overview));
        assert_eq!(lookup_sym(&mods(false, false, false, false), k::KEY_q), None);
        assert_eq!(lookup_sym(&mods(false, false, false, true), k::KEY_a), Some(Action::ControlCenter));
        assert_eq!(lookup_sym(&mods(false, false, false, true), k::KEY_l), Some(Action::Lock));
        assert_eq!(lookup_sym(&mods(false, false, false, true), k::KEY_Left), Some(Action::Snap(Some(crate::policy::SnapSide::Start))));
        assert_eq!(lookup_sym(&mods(false, false, false, false), k::KEY_Left), None);
        assert_eq!(lookup_sym(&mods(true, false, false, true), k::KEY_Return), None);
        use crate::overlay::VolumeKey;
        assert_eq!(lookup_sym(&mods(false, false, false, false), k::KEY_XF86AudioRaiseVolume), Some(Action::Volume(VolumeKey::Up)));
        assert_eq!(lookup_sym(&mods(false, false, true, false), k::KEY_XF86AudioLowerVolume), Some(Action::Volume(VolumeKey::Down)));
        assert_eq!(lookup_sym(&mods(false, false, false, false), k::KEY_XF86AudioMute), Some(Action::Volume(VolumeKey::Mute)));
        assert_eq!(lookup_sym(&mods(false, false, false, false), k::KEY_XF86PowerOff), Some(Action::Power));
        assert_eq!(lookup_sym(&mods(false, false, true, true), k::KEY_s), Some(Action::Screenshot));
        assert_eq!(lookup_sym(&mods(false, false, false, true), k::KEY_s), None);
        assert_eq!(lookup_sym(&mods(false, false, false, true), k::KEY_i), Some(Action::Settings));
    }
}
