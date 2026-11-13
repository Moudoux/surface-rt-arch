//! Design tokens (DESKTOP-SHELL.md 3.1, 3.6).
//!
//! All visual values (colours, radii, spacing, motion, cursor theme) live in a theme TOML file. The built-in
//! themes are `assets/themes/*.toml`; a user theme file is merged over the built-in one it names (or over
//! "dark"), so it only needs the keys it changes. No other module may contain literal colours or radii.

// Most tokens are for the Phase 2+ UI (render kit, motion); they are parsed and validated already.
#![allow(dead_code)]

use std::path::Path;

use serde::Deserialize;
use tracing::{info, warn};

const DARK: &str = include_str!("../../assets/themes/dark.toml");
const LIGHT: &str = include_str!("../../assets/themes/light.toml");

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Theme {
    pub name: String,
    pub colors: Colors,
    pub shape: Shape,
    pub spacing: Spacing,
    pub motion: Motion,
    pub cursor: CursorTokens,
    pub font: FontTokens,
    pub glass: GlassTokens,
    pub shadow: ShadowTokens,
    pub icons: IconTokens,
    pub wallpaper: WallpaperTokens,
    pub home: HomeTokens,
    pub springboard: SpringboardTokens,
    pub control: ControlTokens,
    pub banner: BannerTokens,
    pub screenshot: ScreenshotTokens,
    pub settings: SettingsTokens,
    pub volume: VolumeTokens,
    pub lock: LockTokens,
    pub osk: OskTokens,
    pub demo: DemoTokens,
    pub split: SplitTokens,
}

/// On-screen keyboard (ui/osk.rs).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OskTokens {
    pub fill: Color,
    pub key: Color,
    pub key_fn: Color,
    pub key_pressed: Color,
    pub key_latched: Color,
    pub key_locked: Color,
    pub text: Color,
    pub text_fn: Color,
    pub preview_fill: Color,
    pub preview_text: Color,
    pub accent_fill: Color,
    pub accent_selected: Color,
    pub key_height: f32,
    pub key_height_portrait: f32,
    pub coding_height: f32,
    pub coding_row: bool,
    pub gap: f32,
    pub padding: f32,
    pub max_width: f32,
    pub radius: f32,
    pub label_size: f32,
    pub label_weight: u16,
    pub fn_label_size: f32,
    pub glyph_size: f32,
    pub preview: bool,
    pub preview_height: f32,
    pub preview_size: f32,
    pub preview_radius: f32,
    pub shadow: bool,
    pub label_shift: String,
    pub label_backspace: String,
    pub label_enter: String,
    pub label_symbols: String,
    pub label_letters: String,
    pub label_hide: String,
    pub label_left: String,
    pub label_right: String,
    pub label_up: String,
    pub label_down: String,
    pub long_press_ms: u32,
    pub repeat_delay_ms: u32,
    pub repeat_ms: u32,
    pub double_tap_ms: u32,
}

/// Notification banners (drawn by the Control Center's layer, above apps).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BannerTokens {
    pub anchor: String,
    pub slide_from: String,
    pub width: f32,
    pub margin: f32,
    pub padding: f32,
    pub gap: f32,
    pub radius: f32,
    pub glass: bool,
    pub fill: Color,
    pub text: Color,
    pub text_dim: Color,
    pub shadow: bool,
    pub icon: u32,
    pub title_size: f32,
    pub body_size: f32,
    pub timeout_ms: u32,
    pub max_shown: usize,
}

/// Feedback for a user screenshot (Super+Shift+S, Power + Volume-down), drawn by the Control Center's layer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScreenshotTokens {
    /// A full-screen flash that fades out from this colour (alpha = its starting strength); 0 ms = no flash.
    pub flash: Color,
    pub flash_ms: u32,
    /// A notification banner naming the file (failures always get one).
    pub banner: bool,
    /// Icon of that banner (icon theme name).
    pub icon: String,
}

/// The Settings app (ui/settings.rs): a full-screen sheet over apps. Default look: Windows 8.1 PC settings
/// (category column on the left, content on the right, flat square controls).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SettingsTokens {
    pub fill: Color,
    pub nav_fill: Color,
    pub nav_width: f32,
    /// The selected category's row (empty = the accent colour) and its text.
    pub nav_selected: String,
    pub nav_selected_text: Color,
    pub nav_row_height: f32,
    pub padding: f32,
    pub title_size: f32,
    pub title_weight: u16,
    pub heading_size: f32,
    pub heading_weight: u16,
    pub label_size: f32,
    pub text: Color,
    pub text_dim: Color,
    /// Space between setting groups, and between a group's label and its choices.
    pub gap: f32,
    pub label_gap: f32,
    /// Choice buttons: minimum width (sets how many share a row), height, gap, corners, fills.
    pub chip_min_width: f32,
    pub chip_height: f32,
    pub chip_gap: f32,
    pub chip_radius: f32,
    pub chip_fill: Color,
    pub chip_text: Color,
    /// The selected choice (empty = the accent colour) and its text.
    pub chip_selected: String,
    pub chip_selected_text: Color,
    /// Accent swatches: size and the colours offered (besides "theme default").
    pub swatch: f32,
    pub accents: Vec<Color>,
    /// Wallpaper thumbnails (longest side, px) and the ring around the selected one.
    pub thumb: u32,
    pub ring: f32,
    pub fade_ms: u32,
}

/// The volume popup shown by the volume keys (drawn by the Control Center's layer, above apps).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VolumeTokens {
    /// "left", "center" or "right" (horizontally) and "top", "center" or "bottom" (vertically).
    pub anchor: String,
    pub anchor_y: String,
    /// "vertical": number on top, bar, speaker icon at the bottom; "horizontal": icon, bar, number in a row.
    pub orientation: String,
    pub margin: f32,
    pub width: f32,
    pub height: f32,
    pub padding: f32,
    pub gap: f32,
    pub radius: f32,
    pub glass: bool,
    pub fill: Color,
    pub shadow: bool,
    pub text: Color,
    pub number_size: f32,
    pub number_weight: u16,
    pub track: f32,
    pub track_fill: Color,
    pub track_radius: f32,
    /// The level fill (empty = the accent colour), and the same while muted.
    pub level_fill: String,
    pub muted_fill: Color,
    pub knob: Color,
    /// Knob length along the bar (0 = no knob) and across it.
    pub knob_length: f32,
    pub knob_thickness: f32,
    pub knob_radius: f32,
    pub icon: u32,
    pub timeout_ms: u32,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
    /// Behaviour: change per key press (0-1).
    pub step: f32,
}

/// Lock screen (Phase 8): the curtain with the clock, and the sign-in screen under it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LockTokens {
    pub curtain_scrim: Color,
    /// Clock block: "left", "center", "right" and "top", "center", "bottom".
    pub anchor: String,
    pub anchor_y: String,
    pub margin_x: f32,
    pub margin_y: f32,
    pub text: Color,
    pub clock_size: f32,
    pub clock_weight: u16,
    pub date_size: f32,
    pub date_weight: u16,
    pub status_icon: u32,
    pub status_gap: f32,
    pub lift_commit: f32,
    pub lift_flick: f32,
    pub curtain_timeout_ms: u32,
    pub signin_glass: bool,
    pub signin_fill: Color,
    pub signin_text: Color,
    pub gap: f32,
    pub name_size: f32,
    pub name_weight: u16,
    pub field_width: f32,
    pub field_height: f32,
    pub field_radius: f32,
    pub field_fill: Color,
    pub field_text: Color,
    pub field_placeholder: Color,
    /// Empty = the accent colour.
    pub field_focus: String,
    pub field_border: f32,
    pub bullet_size: f32,
    pub submit_fill: String,
    pub submit_text: Color,
    pub message_size: f32,
    pub message_text: Color,
    pub error_text: Color,
    pub pin_key: f32,
    pub pin_gap: f32,
    pub pin_radius: f32,
    pub pin_fill: Color,
    pub pin_pressed: String,
    pub pin_text: Color,
    pub pin_size: f32,
    pub pin_weight: u16,
    pub label_password: String,
    pub label_pin: String,
    pub label_submit: String,
    pub label_back: String,
    pub label_ok: String,
    pub label_checking: String,
    /// Greeter (login screen) user tiles: square size, corner radius, fill, the picked tile's fill (empty =
    /// accent), initial letter size/weight, name size, gap between tiles.
    pub user_tile: f32,
    pub user_tile_radius: f32,
    pub user_tile_fill: Color,
    pub user_tile_selected: String,
    pub user_initial_size: f32,
    pub user_initial_weight: u16,
    pub user_name_size: f32,
    pub user_gap: f32,
    pub label_switch_user: String,
    pub label_signing_in: String,
    pub label_no_users: String,
}

/// Control Center (right-edge panel).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ControlTokens {
    pub width: f32,
    pub margin: f32,
    pub padding: f32,
    pub gap: f32,
    pub radius: f32,
    pub glass: bool,
    pub fill: Color,
    pub shadow: bool,
    pub dim: Color,
    pub title: String,
    pub title_size: f32,
    pub title_weight: u16,
    pub toggle_off: Color,
    pub toggle_layout: String,
    pub toggle_height: f32,
    pub toggle_columns: usize,
    pub toggle_gap: f32,
    pub toggle_padding: f32,
    pub toggle_radius: f32,
    pub toggle_icon: u32,
    pub toggle_label_size: f32,
    pub slider_height: f32,
    pub slider_track: f32,
    pub slider_knob: f32,
    pub knob_width: f32,
    pub slider_radius: f32,
    pub knob: Color,
    pub knob_radius: f32,
    pub power_button: f32,
    pub power_fill: bool,
    pub power_radius: f32,
    pub power_icon: u32,
    pub time_size: f32,
    pub time_weight: u16,
    pub section_size: f32,
    pub devices_max: usize,
    pub row_height: f32,
    pub row_icon: u32,
    pub row_selected: Color,
    pub row_radius: f32,
    pub row_padding: f32,
    pub networks_max: usize,
    pub notes_max: usize,
    pub field_height: f32,
    pub field_fill: Color,
    pub field_text: Color,
    pub field_radius: f32,
    pub button_height: f32,
    pub button_radius: f32,
    pub open_flick: f32,
    pub open_commit: f32,
    pub confirm_ms: u32,
}

/// Split view (Phase 7): the divider between two apps, its drag feedback and the Overview's drop zones.
/// Layout values (divider, grab margin, snaps, dismiss, double tap) go to the window policy via `main.rs`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SplitTokens {
    pub divider: u32,
    pub divider_color: Color,
    pub divider_active: Color,
    pub handle_length: u32,
    pub handle_thickness: u32,
    pub handle_color: Color,
    pub handle_active: Color,
    pub grab_margin: u32,
    pub snap_mark: u32,
    pub snap_color: Color,
    pub reveal_color: Color,
    pub dismiss_color: Color,
    pub drop_color: Color,
    pub drop_zone: f32,
    pub snaps: Vec<f64>,
    pub dismiss: f64,
    pub double_tap_ms: u32,
}

/// Sizes of the `--demo` test scene.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DemoTokens {
    pub card_width: f32,
    pub card_height: f32,
    pub panel_width: f32,
}

/// Home / Start screen and All-apps sheet (style "rt").
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HomeTokens {
    pub title: String,
    pub title_size: f32,
    pub title_weight: u16,
    pub margin_left: f32,
    pub margin_right: f32,
    pub title_top: f32,
    pub tiles_top: f32,
    pub tiles_bottom: f32,
    pub cell: f32,
    pub gap: f32,
    pub group_gap: f32,
    pub group_label_size: f32,
    pub tile_radius: f32,
    pub tile_style: String,
    pub tile_text: Color,
    pub tile_palette: Vec<Color>,
    pub pressed_scale: f32,
    pub tile_icons: [u32; 3],
    pub list_row: f32,
    pub list_column: f32,
    pub list_icon: u32,
    pub letter_size: f32,
    pub search_width: f32,
    pub scroll_stiffness: f32,
    pub flick_time: f32,
    pub rubber_band: f32,
    pub long_press_ms: u32,
}

/// Home for style "ipad" (ui/springboard.rs): pages of app icons, page dots, a dock, a search sheet.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpringboardTokens {
    pub status_height: f32,
    pub status_margin: f32,
    pub status_size: f32,
    pub status_weight: u16,
    pub status_text: Color,
    pub grid_top: f32,
    pub grid_bottom: f32,
    pub grid_margin_x: f32,
    /// Between the widget column and the icon grid.
    pub grid_gap_x: f32,
    /// Status bar glyphs (Wi-Fi bars, battery) height.
    pub status_glyph: f32,
    pub status_battery_charging: Color,
    /// The widget column (landscape, first page): clock, date, widget cards.
    pub today: bool,
    pub today_x: f32,
    pub today_top: f32,
    pub clock_size: f32,
    pub clock_weight: u16,
    pub date_size: f32,
    pub date_weight: u16,
    /// Small widget edge; wide widgets are two small ones plus the gap.
    pub widget_small: f32,
    pub widget_gap: f32,
    pub widget_radius: f32,
    pub widget_padding: f32,
    pub widget_fill: Color,
    pub widget_text: Color,
    pub widget_dim: Color,
    pub widget_accent: Color,
    pub widget_good: Color,
    pub widget_track: Color,
    pub widget_shadow: bool,
    pub widget_title_size: f32,
    pub widget_body_size: f32,
    pub widget_big_size: f32,
    pub widget_big_weight: u16,
    pub widget_icon: u32,
    pub columns: usize,
    pub rows: usize,
    pub columns_portrait: usize,
    pub rows_portrait: usize,
    pub icon: u32,
    pub label_size: f32,
    pub label_weight: u16,
    pub label_gap: f32,
    pub label_color: Color,
    /// "none", "solid" (plate_fill) or "glass" (frosted wallpaper tinted with plate_fill).
    pub plate: String,
    pub plate_fill: Color,
    pub plate_radius: f32,
    pub plate_padding: f32,
    pub pressed_scale: f32,
    pub dots_size: f32,
    pub dots_gap: f32,
    pub dots_offset: f32,
    pub dot: Color,
    pub dot_active: Color,
    pub dock_height: f32,
    pub dock_icon: u32,
    pub dock_radius: f32,
    pub dock_padding: f32,
    pub dock_gap: f32,
    pub dock_margin: f32,
    pub dock_glass: bool,
    pub dock_fill: Color,
    pub dock_shadow: bool,
    pub dock_divider: Color,
    pub dock_max: usize,
    pub dock_recent: usize,
    pub badge_size: f32,
    pub badge_fill: Color,
    pub badge_text: Color,
    pub badge_label: String,
    pub jiggle: f32,
    pub jiggle_hz: f32,
    pub done_fill: String,
    pub done_text: Color,
    pub search_pull: f32,
    pub search_width: f32,
    pub search_height: f32,
    pub search_radius: f32,
    pub search_fill: Color,
    pub search_text: Color,
    pub search_placeholder: Color,
    /// Names under the result icons (the sheet may be light while page labels sit on the wallpaper).
    pub search_label: Color,
    pub search_tint: Color,
    pub search_columns: usize,
    pub search_row: f32,
    pub search_icon: u32,
    pub toast_fill: Color,
    pub toast_text: Color,
    pub toast_radius: f32,
    pub page_stiffness: f32,
    pub flick_time: f32,
    pub rubber_band: f32,
    pub reveal_scale: f32,
    pub long_press_ms: u32,
    pub edge_flip_ms: u32,
    pub label_done: String,
    pub label_search: String,
    pub label_no_match: String,
    pub label_added: String,
    pub label_dock_full: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FontTokens {
    pub ui: String,
    pub mono: String,
    pub size_caption: f32,
    pub size_body: f32,
    pub size_title: f32,
    pub size_display: f32,
    pub weight_body: u16,
    pub weight_title: u16,
    pub weight_display: u16,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GlassTokens {
    pub tint: Color,
    pub blur_sigma: f32,
    pub blur_levels: u32,
    pub blur_passes: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ShadowTokens {
    pub color: Color,
    pub sigma: f32,
    pub offset_y: f32,
    pub small_sigma: f32,
    pub small_offset_y: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IconTokens {
    pub theme: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WallpaperTokens {
    pub gradient_end: Color,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Colors {
    pub background: Color,
    pub surface: Color,
    pub surface_glass: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub text: Color,
    pub text_dim: Color,
    pub scrim: Color,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Shape {
    pub radius_small: u32,
    pub radius_card: u32,
    pub radius_panel: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Spacing {
    pub unit: u32,
    pub touch_target: u32,
    pub edge_zone: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Motion {
    pub duration_short_ms: u32,
    pub duration_long_ms: u32,
    pub spring_stiffness: f32,
    pub spring_damping: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CursorTokens {
    pub theme: String,
    pub size: u32,
}

/// An sRGB colour with straight (non-premultiplied) alpha, parsed from "#rrggbb" or "#rrggbbaa".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// "#rrggbb" (or "#rrggbbaa" when not opaque), as config and theme files write colours.
    pub fn hex(&self) -> String {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if self.a >= 1.0 {
            format!("#{:02x}{:02x}{:02x}", b(self.r), b(self.g), b(self.b))
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", b(self.r), b(self.g), b(self.b), b(self.a))
        }
    }

    pub fn parse(s: &str) -> Option<Color> {
        let hex = s.strip_prefix('#')?;
        if !(hex.len() == 6 || hex.len() == 8) || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|v| v as f32 / 255.0);
        Some(Color {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: if hex.len() == 8 { byte(6)? } else { 1.0 },
        })
    }

    /// Premultiplied RGBA, the form every renderer path uses (DESKTOP-SHELL.md 2: blending).
    pub fn premultiplied(self) -> [f32; 4] {
        [self.r * self.a, self.g * self.a, self.b * self.a, self.a]
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad colour {s:?}, want #rrggbb[aa]")))
    }
}

fn builtin(name: &str) -> Option<&'static str> {
    match name {
        "dark" => Some(DARK),
        "light" => Some(LIGHT),
        _ => None,
    }
}

/// Recursively merge `over` into `base` (tables merge, other values replace).
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

impl Theme {
    pub fn builtin_dark() -> Theme {
        toml::from_str(DARK).expect("built-in dark theme is valid")
    }

    /// `spec` is a built-in theme name ("dark", "light") or a path to a TOML file. A file may set
    /// `base = "light"` to choose which built-in theme it extends.
    pub fn load(spec: &str) -> Theme {
        match Self::try_load(spec) {
            Ok(theme) => {
                info!(theme = %theme.name, "theme loaded");
                theme
            }
            Err(err) => {
                warn!(%spec, %err, "could not load theme, using built-in dark");
                Self::builtin_dark()
            }
        }
    }

    fn try_load(spec: &str) -> Result<Theme, String> {
        if let Some(src) = builtin(spec) {
            return toml::from_str(src).map_err(|e| e.to_string());
        }
        let text = std::fs::read_to_string(Path::new(spec)).map_err(|e| e.to_string())?;
        let over: toml::Table = toml::from_str(&text).map_err(|e| e.to_string())?;
        let base_name = over.get("base").and_then(|v| v.as_str()).unwrap_or("dark");
        let mut base: toml::Table =
            toml::from_str(builtin(base_name).ok_or(format!("unknown base theme {base_name:?}"))?)
                .map_err(|e| e.to_string())?;
        merge(&mut base, over);
        base.remove("base");
        // Which layout (style) the theme was made for: read by Settings (`available`), not a look token.
        base.remove("style");
        base.try_into().map_err(|e: toml::de::Error| e.to_string())
    }
}

/// A theme the Settings app can offer: a built-in one or a theme file.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeEntry {
    /// Shown name (`name` in the file, else the file name).
    pub name: String,
    /// What `theme =` in config.toml takes: a built-in name or the file's path.
    pub spec: String,
    /// The layout the theme is made for (`style = "ipad"` in the file); None = any.
    pub style: Option<String>,
}

/// Folders searched for theme files (`*.toml`): the user's, then the installed ones.
pub fn theme_dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")));
    if let Some(c) = config {
        v.push(c.join("rtshell/themes"));
    }
    v.push("/usr/share/rtshell/themes".into());
    v
}

/// The built-in themes, then every readable theme file in `dirs` (sorted by name within a folder). Files that
/// only repeat a built-in theme (the package installs copies of dark.toml and light.toml as examples) are
/// left out.
pub fn available(dirs: &[std::path::PathBuf]) -> Vec<ThemeEntry> {
    // The built-in themes are the default style's ("rt"): picking one from another style switches back.
    let mut out: Vec<ThemeEntry> =
        ["dark", "light"].iter().map(|n| ThemeEntry { name: n.to_string(), spec: n.to_string(), style: Some("rt".into()) }).collect();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut found: Vec<ThemeEntry> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let t: toml::Table = toml::from_str(&text).ok()?;
                let stem = p.file_stem()?.to_string_lossy().into_owned();
                let name = t.get("name").and_then(|v| v.as_str()).map(String::from).unwrap_or(stem);
                if builtin(&name).is_some() && t.get("base").is_none() && dir.starts_with("/usr/share") {
                    return None;
                }
                let style = t.get("style").and_then(|v| v.as_str()).map(String::from);
                Some(ThemeEntry { name, spec: p.display().to_string(), style })
            })
            .collect();
        found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        for e in found {
            if !out.iter().any(|o| o.spec == e.spec) {
                out.push(e);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_list() {
        let dir = std::env::temp_dir().join(format!("rtshell-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ocean.toml"), "name = \"Ocean\"\nbase = \"light\"\nstyle = \"ipad\"\n[colors]\naccent = \"#0088cc\"\n").unwrap();
        std::fs::write(dir.join("broken.toml"), "this is not = = toml").unwrap();
        let list = available(std::slice::from_ref(&dir));
        assert_eq!(list.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["dark", "light", "Ocean"]);
        let ocean = &list[2];
        assert_eq!(ocean.style.as_deref(), Some("ipad"));
        // The file loads as a theme (its `style` key is not a token).
        let t = Theme::try_load(&ocean.spec).unwrap();
        assert_eq!(t.colors.accent, Color::parse("#0088cc").unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The shipped iPad themes load, switch the style, and only set tokens that exist (a typo would be
    /// ignored silently by serde).
    #[test]
    fn ipad_themes() {
        fn unknown(base: &toml::Table, over: &toml::Table, path: &str, out: &mut Vec<String>) {
            for (k, v) in over {
                let p = format!("{path}{k}");
                match (base.get(k), v) {
                    (Some(toml::Value::Table(b)), toml::Value::Table(o)) => unknown(b, o, &format!("{p}."), out),
                    (Some(_), _) => {}
                    (None, _) if path.is_empty() && matches!(k.as_str(), "base" | "style") => {}
                    (None, _) => out.push(p),
                }
            }
        }
        for (file, base) in [("ipad-dark.toml", "dark"), ("ipad-light.toml", "light")] {
            let path = format!("{}/assets/themes/{file}", env!("CARGO_MANIFEST_DIR"));
            let t = Theme::try_load(&path).unwrap();
            assert!(t.name.starts_with("iPad"));
            assert_eq!(t.banner.slide_from, "top");
            let over: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(over.get("style").and_then(|v| v.as_str()), Some("ipad"));
            let base: toml::Table = toml::from_str(builtin(base).unwrap()).unwrap();
            let mut bad = Vec::new();
            unknown(&base, &over, "", &mut bad);
            assert!(bad.is_empty(), "{file}: unknown tokens {bad:?}");
        }
    }

    #[test]
    fn builtins_parse() {
        for name in ["dark", "light"] {
            let t = Theme::try_load(name).unwrap();
            assert_eq!(t.name, name);
        }
    }

    #[test]
    fn colour_parsing() {
        assert_eq!(Color::parse("#ff000080").unwrap().premultiplied()[0], 128.0 / 255.0);
        assert!(Color::parse("ff0000").is_none());
        assert!(Color::parse("#ff00").is_none());
    }

    #[test]
    fn user_file_merges_over_base() {
        let dir = std::env::temp_dir().join(format!("rtshell-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.toml");
        std::fs::write(&file, "base = \"light\"\nname = \"mine\"\n[colors]\naccent = \"#112233\"\n").unwrap();
        let t = Theme::try_load(file.to_str().unwrap()).unwrap();
        assert_eq!(t.name, "mine");
        assert_eq!(t.colors.accent, Color::parse("#112233").unwrap());
        assert_eq!(t.colors.background, Theme::try_load("light").unwrap().colors.background);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
