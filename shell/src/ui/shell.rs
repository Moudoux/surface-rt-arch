//! `ShellUi`: owns the render kit's GPU resources (programs, atlases, text, icons, wallpaper and glass
//! textures) and one `Scene` (the widget tree and its behaviour), and implements the core's `Overlay`.
//!
//! Per frame: step the scene's animations to the predicted presentation time, lay out and paint the tree
//! into a draw list, and hand the core a `UiElement` whose damage is only what changed.

use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::gles::{GlesRenderer, ffi::Gles2},
    reexports::calloop::ping::Ping,
    utils::{Physical, Size},
};
use tracing::{error, info, warn};

use super::{Tree, control::ControlCenter, lock::LockScreen, osk::Keyboard, settings::Settings};
use crate::{
    overlay::{EdgePhase, IdleStage, KeyEvent, LockReason, Overlay, Phase, PointerEvent, Request, TextPurpose, VolumeKey},
    services::{Charge, Command, Services},
    render::{
        Painter,
        batch::{DrawList, Rect},
        blur::{self, GlassBlur},
        gl::{self, Format, Target, Texture},
        icons::Icons,
        kit::Kit,
        layer::{LayerStats, UiElement, UiLayer},
        text::{SharedAtlas, Text},
        wallpaper::{self, WallpaperImage},
    },
    theme::Theme,
};

/// Costs of the last frame, for the demo's readout and the log.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameCost {
    /// CPU time to step, lay out, paint and batch (ms).
    pub build_ms: f32,
    pub layer: LayerStats,
}

/// The base scene of greeter mode: nothing (only the lock layer is ever drawn; there is no Home).
pub struct Blank(Tree);

impl Blank {
    pub fn new() -> Blank {
        Blank(Tree::new(super::Node::new().size(super::Size::Fill(1.0), super::Size::Fill(1.0))))
    }
}

impl Scene for Blank {
    fn tree(&mut self) -> &mut Tree {
        &mut self.0
    }
    fn update(&mut self, _now: Duration, _dt: f32) -> bool {
        false
    }
    fn input(&mut self, _ev: PointerEvent) -> bool {
        false
    }
    fn visible(&self) -> bool {
        false
    }
}

/// What a screen built from widgets implements.
pub trait Scene {
    fn tree(&mut self) -> &mut Tree;
    /// Step animations to `now` (`dt` seconds since the last frame). Returns true while animating.
    fn update(&mut self, now: Duration, dt: f32) -> bool;
    fn input(&mut self, ev: PointerEvent) -> bool;
    fn next_wakeup(&self) -> Option<Duration> {
        None
    }
    /// The wallpaper texture (None until loaded) and the last frame's costs.
    fn set_wallpaper(&mut self, _tex: Option<Texture>) {}
    fn set_cost(&mut self, _cost: FrameCost) {}
    /// True if the scene covers the whole output opaquely (lets the compositor skip what is below).
    fn opaque(&self) -> bool {
        false
    }
    /// The output size (before the first `update`, and when it changes).
    fn set_size(&mut self, _w: f32, _h: f32) {}
    /// A worker thread woke the loop (e.g. the app list was scanned): true if the scene needs a frame.
    fn poll(&mut self) -> bool {
        false
    }
    fn set_apps_shown(&mut self, _shown: bool) {}
    fn visible(&self) -> bool {
        true
    }
    fn axis(&mut self, _x: f32, _y: f32, _dx: f32, _dy: f32) -> bool {
        false
    }
    fn key(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take_requests(&mut self) -> Vec<Request> {
        Vec::new()
    }
    /// The theme file changed (live reload): rebuild with the new tokens, keeping state.
    fn set_theme(&mut self, _theme: &Theme) {}
    /// See `Overlay::app_rect` / `Overlay::set_reveal`.
    fn app_rect(&self, _app_id: &str) -> Option<Rect> {
        None
    }
    fn set_reveal(&mut self, _r: f32) {}
    /// See `Overlay::prompt_not_responding` / `Overlay::dismiss_prompt`.
    fn prompt_not_responding(&mut self, _id: u64, _app_id: &str) -> bool {
        false
    }
    fn dismiss_prompt(&mut self, _id: u64) {}
    /// See `Overlay::show_backdrop`.
    fn show_backdrop(&mut self, _on: bool) -> bool {
        false
    }
    /// A text field of the scene has focus (the on-screen keyboard comes up for it).
    fn text_field(&self) -> bool {
        false
    }
}

struct Gpu {
    kit: Rc<Kit>,
    blur: Option<GlassBlur>,
    atlas: SharedAtlas,
    text: Text,
    icons: Icons,
    wallpaper: Option<Texture>,
    /// The lock screen's own image (`lock_wallpaper`); None = it shows `wallpaper`.
    lock_wallpaper: Option<Texture>,
    glass: Option<Texture>,
    glass_target: Option<Target>,
    atlas_generation: u64,
}

impl Gpu {
    /// What the lock curtain shows.
    fn lock_texture(&self) -> Option<Texture> {
        self.lock_wallpaper.or(self.wallpaper)
    }
}

/// A config wallpaper value as a path (`~/` expanded); None when empty.
fn wall_path(s: &str) -> Option<PathBuf> {
    (!s.is_empty()).then(|| match (s.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(s),
    })
}

/// The lock screen's own image path: None when unset or the same as the desktop's (one texture serves both).
fn lock_wall_path(lock: &str, desktop: &Option<PathBuf>) -> Option<PathBuf> {
    wall_path(lock).filter(|p| Some(p) != desktop.as_ref())
}

/// The Control Center's own layer: drawn as a second element above everything (apps included), with its own
/// damage tracking, so it can move over an app without Home being drawn at all.
struct PanelLayer {
    cc: ControlCenter,
    layer: UiLayer,
    list: DrawList,
    full: bool,
    atlas_seen: u64,
    last_t: Option<Duration>,
    animating: bool,
    /// Drawn last frame (a panel that comes back redraws in full).
    shown: bool,
    /// Laid out and painted once off screen (text shaped, glyphs and icons loaded), so the first open
    /// does not pay for it.
    warm: bool,
    /// The services snapshot it was warmed with (warmed again once when real values arrive).
    warm_gen: u64,
    size: Option<(i32, i32)>,
}

/// The Settings app's layer: a full-screen sheet over apps and Home, under the Control Center's layer
/// (banners, the volume popup) and the keyboard; never drawn while locked.
struct SettingsLayer {
    s: Settings,
    layer: UiLayer,
    list: DrawList,
    full: bool,
    atlas_seen: u64,
    last_t: Option<Duration>,
    animating: bool,
    shown: bool,
    size: Option<(i32, i32)>,
}

/// The lock screen's layer (Phase 8): drawn below the keyboard and the volume popup while locked; the core
/// draws nothing else then.
struct LockLayer {
    scene: LockScreen,
    layer: UiLayer,
    list: DrawList,
    full: bool,
    atlas_seen: u64,
    last_t: Option<Duration>,
    animating: bool,
    shown: bool,
    warm: bool,
    size: Option<(i32, i32)>,
}

/// Where the on-screen keyboard's keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OskTarget {
    /// The lock screen's password box.
    Lock,
    /// The Control Center's text field (Wi-Fi password).
    Panel,
    /// Home's text field (All apps search, group names).
    Home,
    /// The focused app (`Request::OskKey`).
    App,
}

/// The on-screen keyboard's layer: drawn above the Control Center, with its own damage tracking.
struct OskLayer {
    kb: Keyboard,
    layer: UiLayer,
    list: DrawList,
    full: bool,
    atlas_seen: u64,
    last_t: Option<Duration>,
    animating: bool,
    shown: bool,
    warm: bool,
    size: Option<(i32, i32)>,
    /// The focused app's text input (from the core).
    app: Option<TextPurpose>,
    /// Shown by hand (`toggle_osk`) for an app without text input.
    forced: bool,
    /// Hidden by its hide key until the field changes.
    dismissed: bool,
    target: Option<OskTarget>,
    hw_keyboard: bool,
    /// The last `Request::Reserve` sent.
    reserved: i32,
}

pub struct ShellUi<S: Scene> {
    theme: Theme,
    panel: Option<PanelLayer>,
    settings: Option<SettingsLayer>,
    osk: Option<OskLayer>,
    lock: Option<LockLayer>,
    /// Requests of the UI layers themselves (keyboard keys for apps, the reserved strip).
    requests: Vec<Request>,
    /// Fonts load on a thread from startup (fc-match + parsing take ~0.4 s on the Surface).
    fonts: Option<std::thread::JoinHandle<Text>>,
    wallpaper_path: Option<PathBuf>,
    /// The lock screen's own image; None = the desktop wallpaper.
    lock_wallpaper_path: Option<PathBuf>,
    gpu: Option<Gpu>,
    failed: bool,
    wall_slot: Arc<Mutex<Option<WallpaperImage>>>,
    lock_wall_slot: Arc<Mutex<Option<WallpaperImage>>>,
    /// The lock screen's own texture must go (its image was unset): it shows the desktop wallpaper again.
    drop_lock_wall: bool,
    wall_size: Option<(i32, i32)>,
    wall_pending: bool,
    /// Bumped whenever the Overview's backdrop (frosted wallpaper + scrim) would look different.
    backdrop_serial: u64,
    /// The session (not the greeter): share the lock image with the login screen (`render::login_wall`).
    login_sync: bool,
    ping: Ping,
    layer: UiLayer,
    list: DrawList,
    scene: S,
    last_t: Option<Duration>,
    size: Option<(i32, i32)>,
    animating: bool,
    cost: FrameCost,
    /// Full damage needed on the next frame (a texture the list samples changed).
    full: bool,
    /// RTSHELL_DEBUG_FPS: per-2 s summary of UI costs in the log.
    log: Option<CostLog>,
    /// Live reload (`watch_config`): set by the watcher thread when config.toml or the theme file changed.
    reload: Arc<std::sync::atomic::AtomicBool>,
    config_path: Option<PathBuf>,
    /// The icon theme changed: make a new icon cache on the next frame (needs the GL context).
    new_icons: bool,
    /// Warm-ups (Control Center, keyboard, lock) wait until this time, set by the first frame: they cost
    /// ~65 ms on the Surface and must not delay the first Home frame. `warmed` once they ran.
    warm_at: Option<Duration>,
    warmed: bool,
    /// "home complete" (wallpaper and every icon of the first screen drawn) was logged.
    complete_logged: bool,
    /// System services (idle dimming, sleep, the power source); None without a bus.
    services: Option<Services>,
}

#[derive(Default)]
struct CostLog {
    since: Option<Instant>,
    frames: u32,
    build_ms: f32,
    max_build_ms: f32,
    draws: usize,
    quads: usize,
    damage_px: i64,
}

impl CostLog {
    fn record(&mut self, c: &FrameCost) {
        let since = *self.since.get_or_insert_with(Instant::now);
        self.frames += 1;
        self.build_ms += c.build_ms;
        self.max_build_ms = self.max_build_ms.max(c.build_ms);
        self.draws = self.draws.max(c.layer.draws);
        self.quads = self.quads.max(c.layer.quads);
        self.damage_px += c.layer.damaged_px;
        if since.elapsed() >= Duration::from_secs(2) {
            use std::sync::atomic::Ordering::Relaxed;
            let (us, n) = (crate::render::layer::GPU_US.swap(0, Relaxed), crate::render::layer::GPU_DRAWS.swap(0, Relaxed));
            let n_f = self.frames as f32;
            info!(
                frames = self.frames,
                build_ms = format!("{:.2}", self.build_ms / n_f),
                max_build_ms = format!("{:.2}", self.max_build_ms),
                gpu_ms = if n > 0 { format!("{:.2}", us as f32 / 1000.0 / n as f32) } else { "-".into() },
                max_draws = self.draws,
                max_quads = self.quads,
                avg_damage_kpx = format!("{:.0}", self.damage_px as f32 / n_f / 1000.0),
                "ui cost"
            );
            let mut t = crate::render::kit::BATCH_TIMES.lock().unwrap().clone();
            if !t.is_empty() {
                let total: u32 = t.iter().map(|b| b.3).sum();
                t.sort_by_key(|b| std::cmp::Reverse(b.3));
                t.truncate(8);
                info!(total_us = total, slowest = ?t, "ui batches (last frame)");
            }
            *self = CostLog::default();
        }
    }
}

impl<S: Scene> ShellUi<S> {
    /// `wallpaper` and `lock_wallpaper` are the config values (empty lock = the same image as the desktop).
    pub fn new(theme: Theme, wallpaper: &str, lock_wallpaper: &str, ping: Ping, scene: S) -> ShellUi<S> {
        let wallpaper_path = wall_path(wallpaper);
        let lock_wallpaper_path = lock_wall_path(lock_wallpaper, &wallpaper_path);
        let (ui, mono) = (theme.font.ui.clone(), theme.font.mono.clone());
        let fonts = crate::render::preload::take_fonts(&ui, &mono)
            .or_else(|| std::thread::Builder::new().name("fonts".into()).spawn(move || Text::new(&ui, &mono)).ok());
        ShellUi {
            theme,
            panel: None,
            settings: None,
            osk: None,
            lock: None,
            requests: Vec::new(),
            fonts,
            wallpaper_path,
            lock_wallpaper_path,
            gpu: None,
            failed: false,
            wall_slot: Arc::new(Mutex::new(None)),
            lock_wall_slot: Arc::new(Mutex::new(None)),
            drop_lock_wall: false,
            wall_size: None,
            wall_pending: false,
            backdrop_serial: 0,
            login_sync: false,
            ping,
            layer: UiLayer::new(),
            list: DrawList::default(),
            scene,
            last_t: None,
            size: None,
            animating: true,
            cost: FrameCost::default(),
            full: true,
            log: std::env::var_os("RTSHELL_DEBUG_FPS").map(|_| CostLog::default()),
            reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            config_path: None,
            new_icons: false,
            warm_at: None,
            warmed: false,
            complete_logged: false,
            services: None,
        }
    }

    /// System services for idle dimming, sleep and the power source.
    /// Keep the login screen's copy of the lock image up to date (the user's session only).
    pub fn with_login_sync(mut self) -> Self {
        self.login_sync = true;
        self
    }

    pub fn with_services(mut self, services: Services) -> Self {
        self.services = Some(services);
        self
    }

    /// Reload the theme when config.toml (`path`, or the default) or the theme file it names changes, so
    /// styles can be tried on a running shell (DESKTOP-SHELL.md 3.6). Colours, sizes, glass, wallpaper and
    /// the icon theme apply at once; fonts and `style` take effect on the next start.
    pub fn watch_config(mut self, path: Option<PathBuf>) -> Self {
        self.config_path = path.clone();
        let flag = self.reload.clone();
        let ping = self.ping.clone();
        let dirs = move || {
            let cfg = path.clone().or_else(crate::config::default_path);
            let mut v: Vec<PathBuf> = cfg.iter().filter_map(|p| p.parent().map(PathBuf::from)).collect();
            let theme = crate::config::Config::load_quiet(cfg.clone()).theme;
            if theme.contains('/')
                && let Some(d) = std::path::Path::new(&theme).parent()
            {
                v.push(d.to_path_buf());
            }
            v
        };
        crate::watch::spawn("config", dirs, move || {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            ping.ping();
        });
        self
    }

    /// Add the Control Center (right-edge panel, Super+A).
    pub fn with_panel(mut self, cc: ControlCenter) -> Self {
        self.panel = Some(PanelLayer {
            cc,
            layer: UiLayer::new(),
            list: DrawList::default(),
            full: true,
            atlas_seen: 0,
            last_t: None,
            animating: false,
            shown: false,
            warm: false,
            warm_gen: 0,
            size: None,
        });
        self
    }

    /// Add the Settings app (after `watch_config`, whose config path it edits).
    pub fn with_settings(mut self) -> Self {
        let s = Settings::new(&self.theme, self.config_path.clone(), self.ping.clone());
        self.settings = Some(SettingsLayer { s, layer: UiLayer::new(), list: DrawList::default(), full: true, atlas_seen: 0, last_t: None, animating: false, shown: false, size: None });
        self
    }

    fn settings_open(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.s.is_open())
    }

    fn open_settings_sheet(&mut self, open: bool) -> bool {
        let Some(l) = self.settings.as_mut() else { return false };
        if open && let Some(p) = self.panel.as_mut() {
            p.cc.open(false);
            p.animating = true;
        }
        l.s.set_open(open);
        l.animating = true;
        self.sync_osk();
        true
    }

    /// Add the on-screen keyboard.
    pub fn with_osk(mut self, kb: Keyboard) -> Self {
        self.osk = Some(OskLayer {
            kb,
            layer: UiLayer::new(),
            list: DrawList::default(),
            full: true,
            atlas_seen: 0,
            last_t: None,
            animating: false,
            shown: false,
            warm: false,
            size: None,
            app: None,
            forced: false,
            dismissed: false,
            target: None,
            hw_keyboard: false,
            reserved: 0,
        });
        self
    }

    /// Add the lock screen.
    pub fn with_lock(mut self, scene: LockScreen) -> Self {
        self.lock = Some(LockLayer {
            scene,
            layer: UiLayer::new(),
            list: DrawList::default(),
            full: true,
            atlas_seen: 0,
            last_t: None,
            animating: false,
            shown: false,
            warm: false,
            size: None,
        });
        self
    }

    fn is_locked(&self) -> bool {
        self.lock.as_ref().is_some_and(|l| l.scene.locked())
    }

    /// Keep the other layers in step with the lock (the Control Center shuts and hides banners).
    fn sync_lock(&mut self) {
        let locked = self.is_locked();
        if let Some(p) = self.panel.as_mut() {
            let was = p.cc.active();
            p.cc.set_locked(locked);
            if was != p.cc.active() {
                p.animating = true;
            }
        }
        self.sync_osk();
    }

    /// Textures the layers sample changed: redraw them all in full.
    fn invalidate(&mut self) {
        self.full = true;
        if let Some(l) = self.lock.as_mut() {
            l.full = true;
        }
        if let Some(p) = self.panel.as_mut() {
            p.full = true;
        }
        if let Some(o) = self.osk.as_mut() {
            o.full = true;
        }
    }

    /// Who wants text now: the Control Center's field (it is on top), Home's, the focused app's.
    fn osk_target(&self) -> Option<(OskTarget, TextPurpose)> {
        let o = self.osk.as_ref()?;
        if let Some(l) = self.lock.as_ref().filter(|l| l.scene.locked()) {
            // Locked: only the lock screen's box may get text.
            return l.scene.wants_text().then_some((OskTarget::Lock, TextPurpose::Password));
        }
        if self.panel.as_ref().is_some_and(|p| p.cc.takes_input() && p.cc.text_field()) {
            return Some((OskTarget::Panel, TextPurpose::Password));
        }
        if self.scene.visible() && self.scene.text_field() {
            return Some((OskTarget::Home, TextPurpose::Text));
        }
        // An app's keyboard steps aside while Home or the Overview is on screen (no app shown) or the
        // Control Center comes in; it returns with the app (the app re-enables its field on focus).
        if self.scene.visible() || self.panel.as_ref().is_some_and(|p| p.cc.coming_in()) {
            return None;
        }
        match (o.app, o.forced) {
            (Some(p), _) => Some((OskTarget::App, p)),
            (None, true) => Some((OskTarget::App, TextPurpose::Text)),
            _ => None,
        }
    }

    /// Show or hide the keyboard for whoever wants text, and keep the apps' reserved strip in step.
    fn sync_osk(&mut self) {
        let target = self.osk_target();
        let Some(o) = self.osk.as_mut() else { return };
        if target.map(|t| t.0) != o.target {
            o.target = target.map(|t| t.0);
            o.dismissed = false;
        }
        if target.is_none() {
            o.forced = false;
        }
        let want = target.is_some() && !o.hw_keyboard && !o.dismissed;
        let purpose = target.map_or(TextPurpose::Text, |t| t.1);
        if o.kb.set_wanted(want, purpose) {
            o.animating = true;
        }
        // Hidden by hand while the field still wants text: a handle brings it back.
        if o.kb.set_handle(target.is_some() && o.dismissed && !o.hw_keyboard) {
            o.animating = true;
            o.full = true;
        }
        // Apps shrink once the keyboard is fully up (they redraw behind it), and grow back as soon as it
        // starts to leave (behind the keyboard sliding out).
        let up = o.kb.settled_up() || (o.reserved > 0 && want);
        let reserve = if up && o.target == Some(OskTarget::App) && o.size.is_some() { o.kb.height().round() as i32 } else { 0 };
        if reserve != o.reserved {
            o.reserved = reserve;
            self.requests.push(Request::Reserve(reserve));
        }
        // The lock screen's sign-in column moves up above the keyboard.
        let inset = if want && o.target == Some(OskTarget::Lock) { o.kb.height() } else { 0.0 };
        if let Some(l) = self.lock.as_mut()
            && l.scene.set_bottom_inset(inset)
        {
            l.animating = true;
        }
    }

    /// Hand the keyboard's keys to their target.
    fn drain_osk(&mut self) {
        let Some(o) = self.osk.as_mut() else { return };
        let keys = o.kb.take_keys();
        if o.kb.take_hide() {
            // Shown by hand for an app without text input: just gone. Otherwise the handle stays.
            if o.forced && o.app.is_none() {
                o.forced = false;
            } else {
                o.dismissed = true;
            }
        }
        if o.kb.take_show() {
            o.dismissed = false;
        }
        let target = o.target;
        for key in keys {
            match target {
                Some(OskTarget::Lock) => {
                    if let Some(l) = self.lock.as_mut() {
                        l.scene.key(&key);
                        l.animating = true;
                    }
                }
                Some(OskTarget::Panel) => {
                    if let Some(p) = self.panel.as_mut() {
                        p.cc.key(&key);
                        p.animating = true;
                    }
                }
                Some(OskTarget::Home) => {
                    self.scene.key(&key);
                    self.animating = true;
                }
                Some(OskTarget::App) => self.requests.push(Request::OskKey(key)),
                None => {}
            }
        }
        self.sync_osk();
    }

    /// GPU resources, wallpaper/glass and the icon theme, up to date for this frame. False if the UI cannot
    /// draw (render kit failed).
    fn prepare(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> bool {
        if self.failed {
            return false;
        }
        if self.gpu.is_none() {
            match renderer.with_context(|gl| unsafe { self.init_gpu(gl) }) {
                Ok(Ok(g)) => self.gpu = Some(g),
                Ok(Err(err)) => {
                    error!(%err, "render kit failed; shell UI disabled");
                    self.failed = true;
                    return false;
                }
                Err(err) => {
                    error!(?err, "render kit failed; shell UI disabled");
                    self.failed = true;
                    return false;
                }
            }
        }
        if self.wall_size != Some((size.w, size.h)) {
            self.request_wallpaper(size.w, size.h);
        }
        let loaded = self.wall_slot.lock().unwrap().take();
        // A lock image that arrives after the lock path was unset again is stale: dropped.
        let lock_loaded = self.lock_wall_slot.lock().unwrap().take().filter(|_| self.lock_wallpaper_path.is_some());
        let drop_lock = std::mem::take(&mut self.drop_lock_wall);
        let new_icons = std::mem::take(&mut self.new_icons);
        if loaded.is_none() && lock_loaded.is_none() && !drop_lock && !new_icons {
            return true;
        }
        let gpu = self.gpu.as_mut().unwrap();
        let ping = self.ping.clone();
        let theme = &self.theme;
        let scene = &mut self.scene;
        let panel = &mut self.panel;
        let lock = &mut self.lock;
        let had_wall = loaded.is_some();
        let res = renderer.with_context(|gl| unsafe {
            let mut lock_changed = false;
            if let Some(img) = loaded {
                Self::install_wallpaper(gpu, gl, img, theme);
                scene.set_wallpaper(gpu.wallpaper);
                scene.tree().mark_layout();
                lock_changed = true;
            }
            if drop_lock && let Some(t) = gpu.lock_wallpaper.take() {
                gl::delete_texture(gl, t);
                lock_changed = true;
            }
            if let Some(img) = lock_loaded {
                // Only the curtain draws it: no glass texture (the sign-in panel's glass is the desktop's).
                if let Some(t) = gpu.lock_wallpaper.take() {
                    gl::delete_texture(gl, t);
                }
                gpu.lock_wallpaper = Some(gl::texture(gl, img.w as i32, img.h as i32, Format::Rgba, true, Some(&img.px)));
                lock_changed = true;
            }
            if lock_changed && let Some(l) = lock.as_mut() {
                l.scene.set_wallpaper(gpu.lock_texture());
                l.scene.tree().mark_layout();
            }
            if new_icons {
                let old = std::mem::replace(&mut gpu.icons, Icons::new(gl, &theme.icons.theme, ping, None));
                old.delete(gl);
                scene.tree().mark_layout();
                if let Some(p) = panel.as_mut() {
                    p.cc.tree().mark_layout();
                }
                if let Some(l) = lock.as_mut() {
                    l.scene.tree().mark_layout();
                }
            }
        });
        if had_wall {
            self.wall_pending = false;
            self.backdrop_serial += 1;
        }
        self.invalidate();
        res.is_ok()
    }

    /// Lay out and paint one scene's tree into `list` (twice if the glyph atlas overflowed).
    unsafe fn paint(gl: &Gles2, gpu: &mut Gpu, list: &mut DrawList, tree: &mut Tree, screen: Rect) -> (Duration, Duration, u32) {
        let (mut t_layout, mut t_paint, mut passes) = (Duration::ZERO, Duration::ZERO, 0);
        gpu.icons.begin_frame();
        for _ in 0..2 {
            list.clear();
            let mut p = Painter {
                gl,
                list,
                text: &mut gpu.text,
                atlas: &mut gpu.atlas,
                icons: &mut gpu.icons,
                glass: gpu.glass_target.as_ref().map(|t| t.tex).or(gpu.glass),
                screen,
            };
            let t0 = Instant::now();
            tree.layout(&mut p, screen);
            t_layout += t0.elapsed();
            let t0 = Instant::now();
            tree.paint(&mut p);
            t_paint += t0.elapsed();
            passes += 1;
            if !gpu.atlas.take_overflow() {
                break;
            }
        }
        (t_layout, t_paint, passes)
    }

    /// Paint the closed panel once into its list (discarded) while Home is idle: shapes its text and puts
    /// its glyphs and icons in the atlases (the first open cost ~50 ms on the Surface without this).
    fn warm_panel(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) {
        let Some(p) = self.panel.as_mut().filter(|p| (!p.warm || (p.warm_gen <= 1 && p.cc.generation() > p.warm_gen)) && !p.cc.active())
        else {
            return;
        };
        p.warm_gen = p.cc.generation();
        let started = Instant::now();
        p.cc.set_size(size.w as f32, size.h as f32);
        p.size = Some((size.w, size.h));
        p.cc.update(present_at, 0.0);
        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let (cc, list) = (&mut p.cc, &mut p.list);
        cc.warm_pose(true);
        let _ = renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, cc.tree(), screen) });
        cc.warm_pose(false);
        p.list.clear();
        p.warm = true;
        p.full = true;
        info!(ms = format!("{:.1}", started.elapsed().as_secs_f32() * 1000.0), "control center warmed up");
    }

    /// Paint the hidden keyboard once (discarded) while Home is idle, so its first show does not pay for
    /// shaping and glyphs.
    fn warm_osk(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) {
        let Some(o) = self.osk.as_mut().filter(|o| !o.warm && !o.kb.active()) else { return };
        let started = Instant::now();
        o.kb.set_size(size.w as f32, size.h as f32);
        o.size = Some((size.w, size.h));
        o.kb.update(present_at, 0.0);
        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let (kb, list) = (&mut o.kb, &mut o.list);
        kb.warm_pose(true);
        let _ = renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, kb.tree(), screen) });
        kb.warm_pose(false);
        o.list.clear();
        o.warm = true;
        o.full = true;
        info!(ms = format!("{:.1}", started.elapsed().as_secs_f32() * 1000.0), "keyboard warmed up");
    }

    /// Paint the lock screen once (discarded) while Home is idle: the first lock (often right before sleep)
    /// then does not pay for shaping the clock and loading glyphs and icons.
    fn warm_lock(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) {
        let Some(l) = self.lock.as_mut().filter(|l| !l.warm && !l.scene.locked()) else { return };
        let started = Instant::now();
        l.scene.set_size(size.w as f32, size.h as f32);
        l.size = Some((size.w, size.h));
        l.scene.update(present_at, 0.0);
        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let (sc, list) = (&mut l.scene, &mut l.list);
        let _ = renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, sc.tree(), screen) });
        l.list.clear();
        l.warm = true;
        l.full = true;
        info!(ms = format!("{:.1}", started.elapsed().as_secs_f32() * 1000.0), "lock screen warmed up");
    }

    /// The lock screen's element for this frame (None while unlocked).
    fn lock_element(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> Option<UiElement> {
        let l = self.lock.as_mut()?;
        if !l.scene.active() {
            l.shown = false;
            l.animating = false;
            l.last_t = None;
            return None;
        }
        if !self.prepare(renderer, size) {
            return None;
        }
        let started = Instant::now();
        let l = self.lock.as_mut().unwrap();
        if l.size != Some((size.w, size.h)) {
            l.size = Some((size.w, size.h));
            l.scene.set_size(size.w as f32, size.h as f32);
            l.full = true;
        }
        let dt = match l.last_t {
            Some(last) if present_at > last => (present_at - last).as_secs_f32().min(0.1),
            Some(_) => 0.0,
            None => 1.0 / 60.0,
        };
        l.last_t = Some(present_at);
        l.animating = l.scene.update(present_at, dt);
        if !l.shown {
            l.shown = true;
            l.full = true;
        }
        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let (sc, list) = (&mut l.scene, &mut l.list);
        renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, sc.tree(), screen) }).ok()?;
        if gpu.atlas.generation != l.atlas_seen {
            l.atlas_seen = gpu.atlas.generation;
            l.full = true;
        }
        let opaque = if l.scene.opaque() { vec![screen] } else { Vec::new() };
        let el = l.layer.element(&gpu.kit, &l.list, size, std::mem::take(&mut l.full), &opaque);
        let ms = started.elapsed().as_secs_f32() * 1000.0;
        if ms > 30.0 {
            warn!(build_ms = format!("{ms:.1}"), "slow lock screen frame");
        }
        // The curtain timeout or a tap may have changed who wants the keyboard.
        self.sync_osk();
        Some(el)
    }

    /// The keyboard's element for this frame (None while hidden).
    fn osk_element(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> Option<UiElement> {
        let o = self.osk.as_mut()?;
        if o.size != Some((size.w, size.h)) {
            o.size = Some((size.w, size.h));
            o.kb.set_size(size.w as f32, size.h as f32);
            o.full = true;
            self.sync_osk();
        }
        let o = self.osk.as_mut().unwrap();
        if !o.kb.active() {
            o.shown = false;
            o.animating = false;
            o.last_t = None;
            return None;
        }
        if !self.prepare(renderer, size) {
            return None;
        }
        let started = Instant::now();
        let o = self.osk.as_mut().unwrap();
        let dt = match o.last_t {
            Some(last) if present_at > last => (present_at - last).as_secs_f32().min(0.1),
            Some(_) => 0.0,
            None => 1.0 / 60.0,
        };
        o.last_t = Some(present_at);
        o.animating = o.kb.update(present_at, dt);
        // Held keys repeat from `update`.
        self.drain_osk();
        let o = self.osk.as_mut().unwrap();
        if !o.kb.active() {
            o.shown = false;
            o.animating = false;
            o.last_t = None;
            return None;
        }
        if !o.shown {
            o.shown = true;
            o.full = true;
        }
        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let (kb, list) = (&mut o.kb, &mut o.list);
        renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, kb.tree(), screen) }).ok()?;
        if gpu.atlas.generation != o.atlas_seen {
            o.atlas_seen = gpu.atlas.generation;
            o.full = true;
        }
        let opaque = o.kb.opaque();
        let el = o.layer.element(&gpu.kit, &o.list, size, std::mem::take(&mut o.full), &opaque);
        let ms = started.elapsed().as_secs_f32() * 1000.0;
        if ms > 30.0 {
            warn!(build_ms = format!("{ms:.1}"), "slow keyboard frame");
        }
        Some(el)
    }

    /// Re-read config and theme; true if something changed.
    fn reload_theme(&mut self) -> bool {
        let config = crate::config::Config::load_quiet(self.config_path.clone());
        let theme = config.theme();
        let wall_path = wall_path(&config.wallpaper);
        let lock_path = lock_wall_path(&config.lock_wallpaper, &wall_path);
        if theme == self.theme && wall_path == self.wallpaper_path && lock_path == self.lock_wallpaper_path {
            return false;
        }
        info!(theme = %theme.name, "theme reloaded");
        self.backdrop_serial += 1;
        if theme.font != self.theme.font {
            warn!("font tokens changed: they apply on the next start");
        }
        self.new_icons |= theme.icons != self.theme.icons;
        let rewall = wall_path != self.wallpaper_path
            || theme.glass != self.theme.glass
            || theme.colors != self.theme.colors
            || theme.wallpaper != self.theme.wallpaper;
        let relock = rewall || lock_path != self.lock_wallpaper_path;
        if lock_path.is_none() && self.lock_wallpaper_path.is_some() {
            self.drop_lock_wall = true;
        }
        self.wallpaper_path = wall_path;
        self.lock_wallpaper_path = lock_path;
        self.theme = theme;
        self.scene.set_theme(&self.theme);
        if let Some(p) = self.panel.as_mut() {
            p.cc.set_theme(&self.theme);
            p.warm = false;
        }
        if let Some(o) = self.osk.as_mut() {
            o.kb.set_theme(&self.theme);
            o.warm = false;
            o.animating = true;
        }
        if let Some(l) = self.settings.as_mut() {
            l.s.set_theme(&self.theme);
            l.full = true;
            l.animating = true;
        }
        if let Some(l) = self.lock.as_mut() {
            l.scene.set_theme(&self.theme);
            l.warm = false;
            l.animating = true;
        }
        if let Some((w, h)) = self.wall_size {
            if rewall {
                self.request_wallpaper(w, h);
            } else if relock {
                self.request_lock_wallpaper(w, h);
            }
        }
        self.invalidate();
        self.animating = true;
        true
    }

    unsafe fn init_gpu(&mut self, gl: &Gles2) -> Result<Gpu, String> {
        unsafe {
            let kit = Rc::new(Kit::new(gl)?);
            let g = &self.theme.glass;
            let blur = match GlassBlur::new(gl, g.blur_sigma) {
                Ok(b) => Some(b),
                Err(err) => {
                    warn!(%err, "GPU blur unavailable, the glass texture will be blurred on the CPU");
                    None
                }
            };
            let f = &self.theme.font;
            let text = match self.fonts.take().map(|h| h.join()) {
                Some(Ok(t)) => t,
                _ => Text::new(&f.ui, &f.mono),
            };
            Ok(Gpu {
                kit,
                blur,
                atlas: SharedAtlas::new(gl),
                text,
                icons: Icons::new(gl, &self.theme.icons.theme, self.ping.clone(), crate::render::preload::take_icons(&self.theme.icons.theme)),
                wallpaper: None,
                lock_wallpaper: None,
                glass: None,
                glass_target: None,
                atlas_generation: 0,
            })
        }
    }

    fn request_wallpaper(&mut self, w: i32, h: i32) {
        self.wall_size = Some((w, h));
        self.wall_pending = true;
        let c = &self.theme.colors;
        let colors = [c.background.premultiplied(), self.theme.wallpaper.gradient_end.premultiplied(), c.accent.premultiplied()];
        let slot = self.wall_slot.clone();
        let ping = self.ping.clone();
        wallpaper::spawn(self.wallpaper_path.clone(), w as u32, h as u32, colors, move |img| {
            *slot.lock().unwrap() = Some(img);
            ping.ping();
        });
        self.request_lock_wallpaper(w, h);
    }

    /// Load the lock screen's own image, if it has one (and there is a lock screen).
    fn request_lock_wallpaper(&mut self, w: i32, h: i32) {
        if self.login_sync {
            let img = self.lock_wallpaper_path.clone().or_else(|| self.wallpaper_path.clone());
            crate::render::login_wall::sync(img, (w as u32, h as u32));
        }
        if self.lock.is_none() || self.lock_wallpaper_path.is_none() {
            return;
        }
        let c = &self.theme.colors;
        let colors = [c.background.premultiplied(), self.theme.wallpaper.gradient_end.premultiplied(), c.accent.premultiplied()];
        let slot = self.lock_wall_slot.clone();
        let ping = self.ping.clone();
        wallpaper::spawn(self.lock_wallpaper_path.clone(), w as u32, h as u32, colors, move |img| {
            *slot.lock().unwrap() = Some(img);
            ping.ping();
        });
    }

    /// Upload a loaded wallpaper and rebuild the glass texture from it.
    unsafe fn install_wallpaper(gpu: &mut Gpu, gl: &Gles2, img: WallpaperImage, theme: &Theme) {
        unsafe {
            if let Some(t) = gpu.wallpaper.take() {
                gl::delete_texture(gl, t);
            }
            if let Some(t) = gpu.glass_target.take() {
                gl::delete_target(gl, t);
            }
            if let Some(t) = gpu.glass.take() {
                gl::delete_texture(gl, t);
            }
            let tex = gl::texture(gl, img.w as i32, img.h as i32, Format::Rgba, true, Some(&img.px));
            gpu.wallpaper = Some(tex);
            let g = &theme.glass;
            let started = Instant::now();
            let gpu_result = gpu.blur.as_ref().map(|b| b.run(gl, tex, g.blur_levels, g.blur_passes));
            match gpu_result {
                Some(Ok(target)) => {
                    gl.Finish();
                    info!(ms = format!("{:.1}", started.elapsed().as_secs_f32() * 1000.0), "glass texture (GPU)");
                    gpu.glass_target = Some(target);
                }
                other => {
                    if let Some(Err(err)) = other {
                        error!(%err, "GPU glass blur failed");
                        gpu.blur = None;
                    }
                    // Same look on the CPU: a box blur of radius ~sigma, three times per pass.
                    let r = g.blur_sigma.round().max(1.0) as u32;
                    let (px, w, h) = blur::cpu_blur(&img.px, img.w, img.h, g.blur_levels, r, g.blur_passes);
                    gpu.glass = Some(gl::texture(gl, w as i32, h as i32, Format::Rgba, true, Some(&px)));
                    info!(ms = format!("{:.1}", started.elapsed().as_secs_f32() * 1000.0), "glass texture (CPU)");
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn scene(&mut self) -> &mut S {
        &mut self.scene
    }
}

impl<S: Scene> Overlay for ShellUi<S> {
    fn element(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> Option<UiElement> {
        let started = Instant::now();
        if !self.prepare(renderer, size) {
            return None;
        }
        if self.size != Some((size.w, size.h)) {
            self.size = Some((size.w, size.h));
            self.scene.set_size(size.w as f32, size.h as f32);
        }
        let dt = match self.last_t {
            Some(last) if present_at > last => (present_at - last).as_secs_f32().min(0.1),
            Some(_) => 0.0,
            None => 1.0 / 60.0,
        };
        self.last_t = Some(present_at);
        self.animating = self.scene.update(present_at, dt);
        let t_update = started.elapsed();

        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let scene = &mut self.scene;
        let list = &mut self.list;
        let built = renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, scene.tree(), screen) });
        let Ok((t_layout, t_paint, passes)) = built else { return None };
        let gpu = self.gpu.as_mut().unwrap();
        if gpu.atlas.generation != gpu.atlas_generation {
            gpu.atlas_generation = gpu.atlas.generation;
            self.full = true;
        }
        let first = self.warm_at.is_none();
        let warm_at = *self.warm_at.get_or_insert(present_at + Duration::from_millis(400));
        if !self.animating && present_at >= warm_at {
            self.warm_panel(renderer, size, present_at);
            self.warm_osk(renderer, size, present_at);
            self.warm_lock(renderer, size, present_at);
            self.warmed = true;
        }
        if first {
            info!(since_start_ms = format!("{:.0}", crate::render::preload::since_start_ms()), "first UI frame");
        }
        if !self.complete_logged {
            let gpu = self.gpu.as_ref().unwrap();
            if gpu.wallpaper.is_some() && gpu.icons.pending() == 0 {
                self.complete_logged = true;
                info!(since_start_ms = format!("{:.0}", crate::render::preload::since_start_ms()), "home complete (wallpaper + icons)");
            }
        }
        let opaque = if self.scene.opaque() { vec![screen] } else { Vec::new() };
        let gpu = self.gpu.as_mut().unwrap();
        let el = self.layer.element(&gpu.kit, &self.list, size, std::mem::take(&mut self.full), &opaque);
        self.cost = FrameCost { build_ms: started.elapsed().as_secs_f32() * 1000.0, layer: self.layer.stats };
        if self.cost.build_ms > 50.0 {
            let ms = |d: Duration| format!("{:.1}", d.as_secs_f32() * 1000.0);
            warn!(
                build_ms = format!("{:.1}", self.cost.build_ms),
                update_ms = ms(t_update),
                layout_ms = ms(t_layout),
                paint_ms = ms(t_paint),
                passes,
                "slow UI frame"
            );
        }
        self.scene.set_cost(self.cost);
        if let Some(l) = self.log.as_mut() {
            l.record(&self.cost);
        }
        Some(el)
    }

    fn animating(&self) -> bool {
        // Home's flag is from its last frame; while hidden it is not drawn and does not count.
        (self.animating && self.scene.visible())
            || self.panel.as_ref().is_some_and(|p| p.animating && p.cc.active())
            || self.settings.as_ref().is_some_and(|l| l.animating && l.s.active())
            || self.osk.as_ref().is_some_and(|o| o.animating && o.kb.active())
            || self.lock.as_ref().is_some_and(|l| l.animating && l.scene.active())
    }

    fn size_hint(&mut self, w: i32, h: i32) {
        if self.wall_size.is_none() {
            self.request_wallpaper(w, h);
        }
    }

    fn next_wakeup(&self) -> Option<Duration> {
        let home = self.scene.next_wakeup().filter(|_| self.scene.visible());
        let panel = self.panel.as_ref().and_then(|p| p.cc.next_wakeup());
        let osk = self.osk.as_ref().and_then(|o| o.kb.next_wakeup());
        let lock = self.lock.as_ref().and_then(|l| l.scene.next_wakeup());
        let warm = self.warm_at.filter(|_| !self.warmed);
        [home, panel, osk, lock, warm].into_iter().flatten().min()
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        let used = self.scene.input(ev);
        if used {
            self.animating = true;
            self.sync_osk();
        }
        used
    }

    fn poll(&mut self) -> bool {
        let reloaded = self.reload.swap(false, std::sync::atomic::Ordering::Relaxed) && self.reload_theme();
        let icons = self.gpu.as_mut().is_some_and(|g| g.icons.collect());
        let scene = self.scene.poll() || icons;
        if scene {
            self.animating = true;
        }
        let panel = self.panel.as_mut().is_some_and(|p| {
            let shown = p.cc.poll() || (icons && p.cc.active());
            p.animating |= shown;
            shown
        });
        let settings = self.settings.as_mut().is_some_and(|l| {
            let redraw = l.s.poll() || (icons && l.s.active());
            l.animating |= redraw;
            redraw
        });
        let lock = self.lock.as_mut().is_some_and(|l| {
            let redraw = l.scene.poll();
            l.animating |= redraw;
            redraw
        });
        self.sync_lock();
        self.wall_slot.lock().unwrap().is_some() || scene || reloaded || panel || settings || lock
    }

    fn set_apps_shown(&mut self, shown: bool) {
        let was = self.scene.visible();
        self.scene.set_apps_shown(shown);
        self.sync_osk();
        if !was && self.scene.visible() {
            // Back on screen after an app covered the output: redraw everything, and do not animate
            // across the time we were hidden.
            self.full = true;
            self.last_t = None;
            self.animating = true;
        }
    }

    fn visible(&self) -> bool {
        self.scene.visible()
    }

    fn app_rect(&self, app_id: &str) -> Option<Rect> {
        self.scene.app_rect(app_id)
    }

    fn set_reveal(&mut self, r: f32) {
        self.scene.set_reveal(r);
    }

    fn axis(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> bool {
        let used = self.scene.axis(x, y, dx, dy);
        if used {
            self.animating = true;
        }
        used
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        let used = self.scene.key(key);
        if used {
            self.animating = true;
            self.sync_osk();
        }
        used
    }

    fn take_requests(&mut self) -> Vec<Request> {
        self.sync_osk();
        let mut r = self.scene.take_requests();
        if let Some(p) = self.panel.as_mut() {
            r.extend(p.cc.take_requests());
        }
        if let Some(l) = self.settings.as_mut() {
            let sr = l.s.take_requests();
            for req in &sr {
                // A new keyboard layout: the on-screen letters follow it (the core swaps the keymap).
                if let Request::ConfigChanged(c) = req
                    && let Some(o) = self.osk.as_mut()
                {
                    o.kb.set_layout(&c.keyboard.resolved().layout);
                    o.warm = false;
                    o.animating = true;
                }
            }
            if !sr.is_empty() {
                l.animating = true;
            }
            r.extend(sr);
        }
        if let Some(l) = self.lock.as_mut() {
            let lr = l.scene.take_requests();
            if !lr.is_empty() {
                r.extend(lr);
                self.sync_lock();
            }
        }
        r.append(&mut self.requests);
        r
    }

    fn panel_wants(&self, id: i32, x: f32, y: f32) -> bool {
        self.osk.as_ref().is_some_and(|o| o.kb.wants(id, x, y)) || self.panel.as_ref().is_some_and(|p| p.cc.wants(id, x, y))
    }

    fn set_hw_keyboard(&mut self, attached: bool) {
        if let Some(p) = self.panel.as_mut() {
            p.cc.set_hw_keyboard(attached);
        }
        if let Some(l) = self.lock.as_mut() {
            l.scene.set_hw_keyboard(attached);
        }
        if let Some(o) = self.osk.as_mut() {
            o.hw_keyboard = attached;
        }
        self.sync_osk();
    }

    fn set_text_input(&mut self, purpose: Option<TextPurpose>) {
        let Some(o) = self.osk.as_mut() else { return };
        o.app = purpose;
        if purpose.is_some() {
            // Each enable from the app (a field tapped) brings a hand-hidden keyboard back.
            o.dismissed = false;
        }
        self.sync_osk();
    }

    fn toggle_osk(&mut self) -> bool {
        let Some(o) = self.osk.as_mut() else { return false };
        if o.kb.wanted() {
            if o.forced && o.app.is_none() {
                o.forced = false;
            } else {
                o.dismissed = true;
            }
        } else {
            o.forced = true;
            o.dismissed = false;
        }
        self.sync_osk();
        true
    }

    fn osk_point(&self, name: &str) -> Option<(f32, f32)> {
        self.osk.as_ref().filter(|o| o.kb.wanted()).and_then(|o| o.kb.point(name))
    }

    fn prompt_not_responding(&mut self, id: u64, app_id: &str) -> bool {
        let shown = self.scene.prompt_not_responding(id, app_id);
        self.animating = true;
        shown
    }

    fn dismiss_prompt(&mut self, id: u64) {
        self.scene.dismiss_prompt(id);
        self.animating = true;
    }

    fn backdrop_serial(&self) -> u64 {
        self.backdrop_serial
    }

    fn show_backdrop(&mut self, on: bool) -> bool {
        // Not before the frosted wallpaper exists (it is made on a worker after startup).
        let ready = self.gpu.as_ref().is_some_and(|g| g.glass.is_some() || g.glass_target.is_some());
        if on && !ready {
            return false;
        }
        let shown = self.scene.show_backdrop(on);
        self.full = true;
        shown
    }

    fn panel_edge(&mut self, phase: EdgePhase, x: f32, vx: f32) -> bool {
        let Some(p) = self.panel.as_mut() else { return false };
        p.cc.edge(phase, x, vx);
        p.animating = true;
        true
    }

    fn toggle_panel(&mut self) -> bool {
        let Some(p) = self.panel.as_mut() else { return false };
        p.cc.toggle();
        p.animating = true;
        true
    }

    fn panel_active(&self) -> bool {
        self.panel.as_ref().is_some_and(|p| p.cc.takes_input()) || self.settings_open()
    }

    fn panel_input(&mut self, ev: PointerEvent) -> bool {
        // The keyboard is drawn on top: it gets its touches first (a cancel goes to both).
        if let Some(o) = self.osk.as_mut()
            && (o.kb.wants(ev.id, ev.x, ev.y) || ev.phase == Phase::Cancel)
        {
            let used = o.kb.input(ev);
            if used {
                o.animating = true;
                self.drain_osk();
                if ev.phase != Phase::Cancel {
                    return true;
                }
            }
        }
        let mut used = false;
        if let Some(p) = self
            .panel
            .as_mut()
            .filter(|p| p.cc.takes_input() || p.cc.wants(ev.id, ev.x, ev.y) || ev.phase == crate::overlay::Phase::Cancel)
        {
            used = p.cc.input(ev);
            if used {
                p.animating = true;
                let settings = p.cc.take_open_settings();
                // A tap may have opened or closed the password box.
                self.sync_osk();
                if settings {
                    self.open_settings_sheet(true);
                }
                if ev.phase != crate::overlay::Phase::Cancel {
                    return true;
                }
            }
        }
        if let Some(l) = self.settings.as_mut().filter(|l| l.s.is_open() || ev.phase == crate::overlay::Phase::Cancel) {
            let u = l.s.input(ev);
            if u {
                l.animating = true;
            }
            used |= u;
        }
        used
    }

    fn panel_key(&mut self, key: &KeyEvent) -> bool {
        let Some(p) = self.panel.as_mut().filter(|p| p.cc.takes_input()) else {
            let Some(l) = self.settings.as_mut().filter(|l| l.s.is_open()) else { return false };
            let used = l.s.key(key);
            l.animating |= used;
            return used;
        };
        let used = p.cc.key(key);
        if used {
            p.animating = true;
            self.sync_osk();
        }
        used
    }

    fn volume_key(&mut self, key: VolumeKey) -> bool {
        let Some(p) = self.panel.as_mut() else { return false };
        p.cc.volume_key(key);
        p.animating = true;
        true
    }

    fn top_elements(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> Vec<UiElement> {
        // The lock first (its curtain timeout may take the keyboard away), then the keyboard (a held key
        // repeating into the panel's field shows in the same frame), then the panel. Topmost first.
        let lock = self.lock_element(renderer, size, present_at);
        let osk = self.osk_element(renderer, size, present_at);
        let panel = self.panel_element(renderer, size, present_at);
        let settings = self.settings_element(renderer, size, present_at);
        osk.into_iter().chain(panel).chain(settings).chain(lock).collect()
    }

    fn lock(&mut self, reason: LockReason) -> bool {
        // Settings never stays open behind the lock (it is drawn above apps, like the lock).
        if self.settings_open() {
            self.open_settings_sheet(false);
        }
        let Some(l) = self.lock.as_mut() else { return false };
        let ok = l.scene.lock(reason);
        l.animating = true;
        l.full = true;
        self.sync_lock();
        ok
    }

    fn lock_input(&mut self, ev: PointerEvent) {
        // The keyboard and the volume popup sit above the lock and take their touches first.
        if let Some(o) = self.osk.as_mut()
            && (o.kb.wants(ev.id, ev.x, ev.y) || ev.phase == Phase::Cancel)
            && o.kb.input(ev)
        {
            o.animating = true;
            self.drain_osk();
            if ev.phase != Phase::Cancel {
                return;
            }
        }
        if let Some(p) = self.panel.as_mut()
            && (p.cc.wants(ev.id, ev.x, ev.y) || ev.phase == Phase::Cancel)
            && p.cc.input(ev)
        {
            p.animating = true;
            if ev.phase != Phase::Cancel {
                return;
            }
        }
        if let Some(l) = self.lock.as_mut() {
            l.scene.input(ev);
            l.animating = true;
        }
        self.sync_osk();
    }

    fn lock_key(&mut self, key: &KeyEvent) {
        if let Some(l) = self.lock.as_mut() {
            l.scene.key(key);
            l.animating = true;
        }
        self.sync_osk();
    }

    fn lock_presented(&mut self) {
        if let Some(l) = self.lock.as_mut() {
            l.scene.presented();
        }
    }

    fn lock_point(&self, name: &str) -> Option<(f32, f32)> {
        self.lock.as_ref().and_then(|l| l.scene.point(name))
    }

    fn open_settings(&mut self) -> bool {
        let open = !self.settings_open();
        self.open_settings_sheet(open)
    }

    fn close_sheet(&mut self) -> bool {
        self.settings_open() && self.open_settings_sheet(false)
    }

    fn screenshot_taken(&mut self, result: &Result<String, String>) {
        let Some(p) = self.panel.as_mut() else { return };
        p.cc.screenshot_taken(result);
        p.animating = true;
    }

    fn power_key(&mut self) -> bool {
        let Some(s) = self.services.as_ref() else { return false };
        info!("power button: sleep");
        s.send(Command::PowerKey);
        true
    }

    fn idle(&mut self, stage: IdleStage, dim_level: f32) {
        let Some(s) = self.services.as_ref() else { return };
        match stage {
            IdleStage::Active => {
                s.send(Command::Dim(None));
                s.send(Command::SetIdleHint(false));
            }
            IdleStage::Dim => s.send(Command::Dim(Some(dim_level))),
            IdleStage::Blank => {
                s.send(Command::SetIdleHint(true));
                // Typed text on the lock screen is wiped; it wakes on its curtain.
                if let Some(l) = self.lock.as_mut() {
                    l.scene.blanked();
                    l.full = true;
                }
            }
            IdleStage::Sleep => s.send(Command::IdleSuspend),
        }
    }

    fn on_ac_power(&self) -> Option<bool> {
        let battery = self.services.as_ref()?.snapshot().1.battery?;
        match battery.charge {
            Charge::Charging | Charge::Full | Charge::Pending => Some(true),
            Charge::Discharging | Charge::Empty => Some(false),
            Charge::Unknown => None,
        }
    }
}

impl<S: Scene> ShellUi<S> {
    fn settings_element(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> Option<UiElement> {
        let locked = self.lock.as_ref().is_some_and(|l| l.scene.active());
        if locked || !self.settings.as_ref().is_some_and(|l| l.s.active()) {
            if let Some(l) = self.settings.as_mut() {
                l.shown = false;
                l.animating = false;
                l.last_t = None;
            }
            return None;
        }
        if !self.prepare(renderer, size) {
            return None;
        }
        let started = Instant::now();
        let l = self.settings.as_mut().unwrap();
        if l.size != Some((size.w, size.h)) {
            l.size = Some((size.w, size.h));
            l.s.set_size(size.w as f32, size.h as f32);
        }
        let dt = match l.last_t {
            Some(last) if present_at > last => (present_at - last).as_secs_f32().min(0.1),
            Some(_) => 0.0,
            None => 1.0 / 60.0,
        };
        l.last_t = Some(present_at);
        l.animating = l.s.update(present_at, dt);
        if !l.s.active() {
            l.shown = false;
            l.animating = false;
            l.last_t = None;
            return None;
        }
        if !l.shown {
            l.shown = true;
            l.full = true;
        }
        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let (s, list) = (&mut l.s, &mut l.list);
        renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, s.tree(), screen) }).ok()?;
        if gpu.atlas.generation != l.atlas_seen {
            l.atlas_seen = gpu.atlas.generation;
            l.full = true;
        }
        // Fully faded in over an opaque fill: the whole screen, so nothing below is drawn.
        let opaque: Vec<Rect> = if l.s.opaque() { vec![screen] } else { Vec::new() };
        let el = l.layer.element(&gpu.kit, &l.list, size, std::mem::take(&mut l.full), &opaque);
        let ms = started.elapsed().as_secs_f32() * 1000.0;
        if ms > 30.0 {
            warn!(build_ms = format!("{ms:.1}"), "slow settings frame");
        }
        Some(el)
    }

    fn panel_element(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>, present_at: Duration) -> Option<UiElement> {
        if !self.panel.as_ref().is_some_and(|p| p.cc.active()) {
            if let Some(p) = self.panel.as_mut() {
                p.shown = false;
                p.animating = false;
                p.last_t = None;
            }
            return None;
        }
        if !self.prepare(renderer, size) {
            return None;
        }
        let started = Instant::now();
        let p = self.panel.as_mut().unwrap();
        if p.size != Some((size.w, size.h)) {
            p.size = Some((size.w, size.h));
            p.cc.set_size(size.w as f32, size.h as f32);
        }
        let dt = match p.last_t {
            Some(last) if present_at > last => (present_at - last).as_secs_f32().min(0.1),
            Some(_) => 0.0,
            None => 1.0 / 60.0,
        };
        p.last_t = Some(present_at);
        p.animating = p.cc.update(present_at, dt);
        if !p.cc.active() {
            // Just finished closing: this frame is drawn without it.
            p.shown = false;
            p.animating = false;
            p.last_t = None;
            return None;
        }
        if !p.shown {
            p.shown = true;
            p.full = true;
        }
        let screen = Rect::new(0.0, 0.0, size.w as f32, size.h as f32);
        let gpu = self.gpu.as_mut().unwrap();
        let (cc, list) = (&mut p.cc, &mut p.list);
        renderer.with_context(|gl| unsafe { Self::paint(gl, gpu, list, cc.tree(), screen) }).ok()?;
        if gpu.atlas.generation != p.atlas_seen {
            p.atlas_seen = gpu.atlas.generation;
            p.full = true;
        }
        let opaque = p.cc.opaque();
        let el = p.layer.element(&gpu.kit, &p.list, size, std::mem::take(&mut p.full), &opaque);
        let ms = started.elapsed().as_secs_f32() * 1000.0;
        if ms > 30.0 {
            warn!(build_ms = format!("{ms:.1}"), "slow control center frame");
        }
        Some(el)
    }
}
