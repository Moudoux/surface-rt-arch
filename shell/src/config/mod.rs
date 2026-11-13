//! User configuration: `assets/config.toml` (built-in defaults) merged with `~/.config/rtshell/config.toml`.

use std::{collections::BTreeMap, path::PathBuf};

use serde::Deserialize;
use tracing::{info, warn};

pub mod edit;

const DEFAULTS: &str = include_str!("../../assets/config.toml");

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    /// Which screens and behaviour the shell UI uses (DESKTOP-SHELL.md 3.6): "rt" (Start screen of tiles)
    /// or "none" (no shell UI; apps only).
    pub style: String,
    pub theme: String,
    /// "#rrggbb" over the theme's accent colour; empty = the theme's.
    pub accent: String,
    /// Image file (PNG/JPEG); empty = a gradient from theme colours. `~/` is expanded.
    pub wallpaper: String,
    /// The lock screen's own image; empty = the same as `wallpaper`. `~/` is expanded.
    pub lock_wallpaper: String,
    pub keyboard: Keyboard,
    pub osk: Osk,
    pub lock: Lock,
    pub greeter: Greeter,
    pub idle: Idle,
    pub input: Input,
    pub apps: Apps,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Keyboard {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
}

/// On-screen keyboard behaviour (its looks are `[osk]` theme tokens).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Osk {
    pub enabled: bool,
    /// Shrink the apps' area while it is up (fullscreen apps are overlaid).
    pub resize_apps: bool,
}

/// Lock screen behaviour (its looks are `[lock]` theme tokens).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Lock {
    /// "password" (a text box: Type Cover or the on-screen keyboard) or "pin" (the shell's digit pad, for a
    /// numeric password). Both check the account password with PAM.
    pub entry: String,
    /// PAM service (a file in /etc/pam.d).
    pub pam_service: String,
}

/// Greeter mode (`rtshell --greeter` under greetd): who is preselected and what session starts.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Greeter {
    /// Login name picked at start (empty = the first person, lowest uid).
    pub default_user: String,
    /// The session command greetd runs for the user.
    pub session: Vec<String>,
    /// Environment for the session ("NAME=value"), set before the PAM session opens.
    pub env: Vec<String>,
}

/// Idle behaviour (DESKTOP-SHELL.md Phase 8 "Idle"): stage times without input, on battery and on AC.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Idle {
    pub battery: IdleTimes,
    pub ac: IdleTimes,
    /// Dimmed brightness as a fraction of the brightness before dimming.
    pub dim_level: f32,
    /// Lock the session when the screen blanks.
    pub lock: bool,
    /// While locked, the screen blanks after this many seconds without input (0 = as when unlocked).
    pub lock_blank: f64,
}

/// Seconds without input before each stage (0 = never; fractions are fine).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct IdleTimes {
    pub dim: f64,
    pub blank: f64,
    pub sleep: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Input {
    pub tap_to_click: bool,
    pub natural_scroll: bool,
    pub pointer_accel: f64,
    /// Touchscreen edge gestures: zone width and the inward travel that starts a gesture (px).
    pub edge_zone: f32,
    pub edge_lock: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Apps {
    /// Start Xwayland for X11 apps (`xwayland` build feature).
    pub xwayland: bool,
    pub terminal: Vec<String>,
    pub launch: Vec<Vec<String>>,
    pub autostart: Vec<Vec<String>>,
}

/// The greeter's config (it cannot read anyone's home).
pub const GREETER_PATH: &str = "/etc/rtshell/config.toml";

pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("rtshell/config.toml"))
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

impl Keyboard {
    /// With `layout` empty, take rules/model/layout/variant/options from the system (what `localectl
    /// set-x11-keymap` writes); fields set in config.toml win. Falls back to "us".
    pub fn resolved(&self) -> Keyboard {
        let mut k = self.clone();
        if !k.layout.is_empty() {
            return k;
        }
        let sys = system_keyboard();
        for (field, key) in [(&mut k.rules, "rules"), (&mut k.model, "model"), (&mut k.layout, "layout"), (&mut k.variant, "variant"), (&mut k.options, "options")] {
            if field.is_empty()
                && let Some(v) = sys.get(key)
            {
                *field = v.clone();
            }
        }
        if k.layout.is_empty() {
            k.layout = "us".into();
        }
        info!(layout = %k.layout, model = %k.model, variant = %k.variant, from_system = sys.contains_key("layout"), "keyboard layout");
        k
    }
}

/// The system keyboard: X11 `00-keyboard.conf` (`Option "XkbLayout" "se"`), else vconsole.conf (`XKBLAYOUT=se`).
fn system_keyboard() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Ok(t) = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf") {
        for line in t.lines() {
            let parts: Vec<&str> = line.split('"').collect();
            if line.trim_start().starts_with("Option") && parts.len() >= 4 {
                let key = parts[1].strip_prefix("Xkb").unwrap_or(parts[1]).to_lowercase();
                m.insert(key, parts[3].to_string());
            }
        }
    }
    if !m.contains_key("layout")
        && let Ok(t) = std::fs::read_to_string("/etc/vconsole.conf")
    {
        for line in t.lines() {
            if let Some((k, v)) = line.split_once('=')
                && let Some(key) = k.trim().strip_prefix("XKB")
            {
                m.insert(key.to_lowercase(), v.trim().trim_matches('"').to_string());
            }
        }
    }
    m
}

impl Config {
    /// The configured theme with the `accent` override applied.
    pub fn theme(&self) -> crate::theme::Theme {
        let mut theme = crate::theme::Theme::load(&self.theme);
        if !self.accent.is_empty() {
            match crate::theme::Color::parse(&self.accent) {
                Some(c) => theme.colors.accent = c,
                None => warn!(accent = %self.accent, "accent is not a colour; using the theme's"),
            }
        }
        theme
    }

    pub fn builtin() -> Config {
        toml::from_str(DEFAULTS).expect("built-in config is valid")
    }

    /// Load the config; on any error, log it and use the built-in defaults.
    pub fn load(path: Option<PathBuf>) -> Config {
        let Some(path) = path.or_else(default_path) else {
            return Self::builtin();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path.display(), "no user config, using defaults");
                return Self::builtin();
            }
            Err(err) => {
                warn!(path = %path.display(), %err, "cannot read config, using defaults");
                return Self::builtin();
            }
        };
        match Self::parse_over_defaults(&text) {
            Ok(c) => {
                info!(path = %path.display(), "config loaded");
                c
            }
            Err(err) => {
                warn!(path = %path.display(), %err, "bad config, using defaults");
                Self::builtin()
            }
        }
    }

    /// `load` without the log lines (for re-reading on every change).
    pub fn load_quiet(path: Option<PathBuf>) -> Config {
        let Some(path) = path.or_else(default_path) else { return Self::builtin() };
        std::fs::read_to_string(&path).ok().and_then(|t| Self::parse_over_defaults(&t).ok()).unwrap_or_else(Self::builtin)
    }

    pub(crate) fn parse_over_defaults(text: &str) -> Result<Config, String> {
        let over: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut base: toml::Table = toml::from_str(DEFAULTS).map_err(|e| e.to_string())?;
        merge(&mut base, over);
        base.try_into().map_err(|e: toml::de::Error| e.to_string())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn greeter_config_parses() {
        let c = Config::parse_over_defaults(include_str!("../../../build/shell/greetd/rtshell-greeter.toml")).unwrap();
        assert_eq!(c.greeter.default_user, "alarm");
        assert_eq!(c.greeter.session, ["/usr/local/bin/rtshell-session"]);
        assert!(c.greeter.env.iter().any(|e| e == "XDG_SESSION_TYPE=wayland"));
    }

    use super::*;

    #[test]
    fn defaults_parse() {
        let c = Config::builtin();
        assert_eq!(c.keyboard.layout, "");
        assert_eq!(c.apps.terminal, vec!["foot".to_string()]);
        assert!(c.idle.battery.blank > 0.0 && c.idle.lock);
    }

    #[test]
    fn idle_override_keeps_other_times() {
        let c = Config::parse_over_defaults("[idle.battery]\ndim = 2.5\n").unwrap();
        assert_eq!(c.idle.battery.dim, 2.5);
        assert_eq!(c.idle.battery.blank, Config::builtin().idle.battery.blank);
    }

    #[test]
    fn override_merges() {
        let c = Config::parse_over_defaults("[keyboard]\nlayout = \"de\"\n").unwrap();
        assert_eq!(c.keyboard.layout, "de");
        assert_eq!(c.keyboard.repeat_rate, 25);
        assert!(c.env.contains_key("QT_QPA_PLATFORM"));
    }
}
