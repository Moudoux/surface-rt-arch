//! Startup work that does not need the GPU, started at the top of `run()` (before DRM/EGL init, ~0.6 s on the
//! Surface) so it is done by the time the UI builds its first frame: font loading (`Text::new`) and the icon
//! theme index + icon disk cache (`icons::prepare`). `ShellUi` takes the results when it creates its GPU state;
//! if the theme's font or icon names changed in between, it starts fresh as before.
//!
//! Also the process start time, for "first UI frame" / "home complete" timings in the log.

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    thread::JoinHandle,
    time::Instant,
};

use super::{icons, text::Text};

static START: OnceLock<Instant> = OnceLock::new();

type FontsJob = (String, String, JoinHandle<Text>);
type IconsJob = (String, JoinHandle<icons::Prepared>);
static FONTS: Mutex<Option<FontsJob>> = Mutex::new(None);
static ICONS: Mutex<Option<IconsJob>> = Mutex::new(None);

/// Call first thing in `main`.
pub fn mark_start() {
    let _ = START.set(Instant::now());
}

/// Milliseconds since `mark_start`.
pub fn since_start_ms() -> f32 {
    START.get().map(|s| s.elapsed().as_secs_f32() * 1000.0).unwrap_or(0.0)
}

/// Start loading fonts and the icon theme on worker threads.
pub fn start(ui_font: &str, mono_font: &str, icon_theme: &str) {
    let (ui, mono) = (ui_font.to_string(), mono_font.to_string());
    if let Ok(h) = std::thread::Builder::new().name("fonts".into()).spawn(move || Text::new(&ui, &mono)) {
        *FONTS.lock().unwrap() = Some((ui_font.to_string(), mono_font.to_string(), h));
    }
    let theme = icon_theme.to_string();
    if let Ok(h) = std::thread::Builder::new().name("icons-prep".into()).spawn(move || icons::prepare(&theme)) {
        *ICONS.lock().unwrap() = Some((icon_theme.to_string(), h));
    }
}

/// The preloaded fonts if they were started for these families.
pub fn take_fonts(ui: &str, mono: &str) -> Option<JoinHandle<Text>> {
    let (u, m, h) = FONTS.lock().unwrap().take()?;
    (u == ui && m == mono).then_some(h)
}

/// The prepared icon theme if it was started for this theme.
pub fn take_icons(theme: &str) -> Option<JoinHandle<icons::Prepared>> {
    let (t, h) = ICONS.lock().unwrap().take()?;
    (t == theme).then_some(h)
}

/// `$XDG_CACHE_HOME/rtshell` (or `~/.cache/rtshell`); not created here.
pub fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|b| b.join("rtshell"))
}

/// Write `data` to `path` atomically (temp file + rename), creating the directory.
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}
