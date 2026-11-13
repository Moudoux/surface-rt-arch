//! The Settings app (DESKTOP-SHELL.md "Settings app design"): a full-screen sheet drawn as a top UI layer by
//! `ShellUi`, above apps and Home, below the Control Center's banners and the on-screen keyboard. Default
//! look: Windows 8.1 PC settings (a category column on the left, the page on the right, flat square choice
//! buttons); every look value is a `[settings]` theme token, and another style can bring its own scene.
//!
//! Each page is a list of groups, each a label and a row (or grid) of choices. A choice writes config.toml
//! through `config::edit` (comments kept) and then sends `Request::ConfigChanged` so the core applies it;
//! theme, accent and wallpaper reach the UI through the existing config watcher. The default browser goes
//! through `xdg-mime` (`~/.config/mimeapps.list`). Nothing here reads or shows secrets.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay::{input::keyboard::keysyms as k, reexports::calloop::ping::Ping};
use tracing::{info, warn};

use super::{Align, Layout, Node, NodeId, Paint, Size, Tree, shell::Scene};
use crate::{
    apps::desktop::DesktopApp,
    config::{
        Config,
        edit::{self, Edit, Val},
    },
    overlay::{CANCEL_ALL, KeyEvent, Phase, PointerEvent, Request},
    render::{batch::Rgba, text::TextStyle},
    theme::{Color, Theme},
};

/// Finger travel that turns a tap into a drag (nothing here scrolls: a drag just cancels the tap).
const SLOP: f32 = 14.0;
/// At most this many wallpaper thumbnails (besides "None") on Personalize, below theme and accent.
const MAX_WALLPAPERS: usize = 11;
/// The Lock screen page has only its picker: three rows of seven in landscape (besides "Same as desktop").
const MAX_LOCK_WALLPAPERS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Personalize,
    Lock,
    Keyboard,
    Touch,
    Power,
    Apps,
    About,
}

const PAGES: [Page; 7] = [Page::Personalize, Page::Lock, Page::Keyboard, Page::Touch, Page::Power, Page::Apps, Page::About];

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Personalize => "Personalize",
            Page::Lock => "Lock screen",
            Page::Keyboard => "Keyboard",
            Page::Touch => "Touch and touchpad",
            Page::Power => "Power and sleep",
            Page::Apps => "Default apps",
            Page::About => "About",
        }
    }
}

/// What a choice does.
#[derive(Debug, Clone, PartialEq)]
enum Act {
    Config(Vec<Edit>),
    /// `xdg-mime default <id>.desktop` for web links and HTML.
    Browser(String),
    /// Restart rtshell (a second tap confirms: open apps close).
    Restart,
}

#[derive(Debug, Clone, PartialEq)]
enum Look {
    Text,
    /// A colour swatch (None = the theme's own accent).
    Swatch(Option<Color>),
    /// A wallpaper thumbnail (None = no image: the theme gradient).
    Thumb(Option<PathBuf>),
}

#[derive(Debug, Clone, PartialEq)]
struct Opt {
    label: String,
    act: Act,
    selected: bool,
    look: Look,
}

#[derive(Debug, Clone, PartialEq)]
struct Group {
    label: String,
    opts: Vec<Opt>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Back,
    Nav(Page),
    Opt(usize, usize),
}

/// Scanned on a thread when the Default apps page opens.
#[derive(Default)]
struct AppScan {
    terminals: Vec<DesktopApp>,
    browsers: Vec<DesktopApp>,
    /// The current default for https links (desktop id without `.desktop`).
    browser: Option<String>,
}

pub struct Settings {
    theme: Theme,
    tree: Tree,
    hits: Vec<(NodeId, Hit)>,
    open: bool,
    /// Opacity (fades in and out over `fade_ms`).
    o: f32,
    page: Page,
    w: f32,
    h: f32,
    config_path: Option<PathBuf>,
    /// The user's config as last read or written.
    config: Config,
    groups: Vec<Group>,
    touch: Option<(i32, (f32, f32), Option<Hit>)>,
    requests: Vec<Request>,
    /// The first tap on "Restart rtshell" (a second one within `RESTART_CONFIRM` restarts).
    restart_armed: Option<std::time::Instant>,
    /// A write failed (shown at the bottom of the page).
    status: Option<String>,
    apps: Arc<Mutex<Option<AppScan>>>,
    scanning: bool,
    ping: Ping,
    /// Rebuild the tree before the next frame.
    dirty: bool,
}

fn text(s: &str, size: f32, weight: u16, color: Rgba) -> Paint {
    Paint::Text { text: s.into(), style: TextStyle { size, weight, ..TextStyle::default() }, color, wrap: false, layout: None }
}

/// A colour token that may be empty (= the accent).
fn or_accent(s: &str, theme: &Theme) -> Rgba {
    Color::parse(s).unwrap_or(theme.colors.accent).premultiplied()
}

fn secs_label(s: f64) -> String {
    match s as u64 {
        0 => "Never".into(),
        n if n < 60 => format!("{n} s"),
        n if n % 3600 == 0 => format!("{} h", n / 3600),
        n => format!("{} min", n / 60),
    }
}

/// Dim shortly before the screen turns off (Settings offers only "turn off" and "sleep", like Windows).
fn dim_for(blank: f64) -> f64 {
    if blank <= 0.0 { 0.0 } else { (blank - 30.0).max(blank * 0.66).round() }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/x` for paths in the home folder (as config.toml keeps them), else the path.
fn tilde(p: &Path) -> String {
    match home().as_deref().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rel) => format!("~/{}", rel.display()),
        None => p.display().to_string(),
    }
}

fn expand(s: &str) -> PathBuf {
    match (s.strip_prefix("~/"), home()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(s),
    }
}

fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
}

/// Wallpaper candidates: images in ~/Pictures (not screenshots), then one image per wallpaper package in
/// /usr/share/wallpapers (KDE layout `<name>/contents/images/<w>x<h>.<ext>`: the smallest landscape one at
/// least `screen_w` wide, else the widest).
fn wallpapers(screen_w: u32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(pics) = crate::render::screenshot::pictures_dir()
        && let Ok(rd) = std::fs::read_dir(&pics)
    {
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_image(p) && !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("Screenshot_")))
            .collect();
        v.sort();
        out.extend(v);
    }
    if let Ok(rd) = std::fs::read_dir("/usr/share/wallpapers") {
        let mut pkgs: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("contents/images")).filter(|p| p.is_dir()).collect();
        pkgs.sort();
        for dir in pkgs {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut sized: Vec<(u32, u32, PathBuf)> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| is_image(p))
                .filter_map(|p| {
                    let stem = p.file_stem()?.to_str()?;
                    let (w, h) = stem.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?, p))
                })
                .collect();
            sized.sort();
            let pick = sized.iter().find(|(w, h, _)| w >= h && *w >= screen_w).or_else(|| sized.iter().filter(|(w, h, _)| w >= h).last()).or(sized.last());
            if let Some((_, _, p)) = pick {
                out.push(p.clone());
            }
        }
    }
    out
}

/// Keyboard layouts offered (xkb name, label); "" = the system's.
const LAYOUTS: [(&str, &str); 10] = [
    ("us", "English (US)"),
    ("gb", "English (UK)"),
    ("se", "Swedish"),
    ("fi", "Finnish"),
    ("no", "Norwegian"),
    ("dk", "Danish"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("it", "Italian"),
];

const IDLE_CHOICES: [f64; 7] = [30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 0.0];

impl Settings {
    pub fn new(theme: &Theme, config_path: Option<PathBuf>, ping: Ping) -> Settings {
        let config = Config::load_quiet(config_path.clone());
        let mut s = Settings {
            theme: theme.clone(),
            tree: Tree::new(Node::new().size(Size::Fill(1.0), Size::Fill(1.0))),
            hits: Vec::new(),
            open: false,
            o: 0.0,
            page: Page::Personalize,
            w: 1366.0,
            h: 768.0,
            config_path,
            config,
            groups: Vec::new(),
            touch: None,
            requests: Vec::new(),
            status: None,
            restart_armed: None,
            apps: Arc::new(Mutex::new(None)),
            scanning: false,
            ping,
            dirty: true,
        };
        s.build();
        s
    }

    /// Open (re-reading config.toml: it may have been edited by hand) or close.
    pub fn set_open(&mut self, open: bool) {
        if open == self.open {
            return;
        }
        self.open = open;
        info!(open, page = ?self.page, "settings");
        if open {
            self.config = Config::load_quiet(self.config_path.clone());
            self.status = None;
            self.show_page(self.page);
        }
        self.touch = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// On screen (open, or fading out).
    pub fn active(&self) -> bool {
        self.open || self.o > 0.0
    }

    fn show_page(&mut self, page: Page) {
        self.page = page;
        if page == Page::Apps && !self.scanning {
            self.scan_apps();
        }
        self.dirty = true;
    }

    fn scan_apps(&mut self) {
        self.scanning = true;
        let (slot, ping) = (self.apps.clone(), self.ping.clone());
        let spawned = std::thread::Builder::new().name("settings-apps".into()).spawn(move || {
            let all = crate::apps::desktop::scan();
            let has = |a: &DesktopApp, c: &str| a.listed && a.categories.iter().any(|x| x == c);
            let terminals = all.iter().filter(|a| has(a, "TerminalEmulator")).cloned().collect();
            let browsers = all.iter().filter(|a| has(a, "WebBrowser")).cloned().collect();
            let browser = std::process::Command::new("xdg-mime")
                .args(["query", "default", "x-scheme-handler/https"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_end_matches(".desktop").to_string())
                .filter(|s| !s.is_empty());
            *slot.lock().unwrap() = Some(AppScan { terminals, browsers, browser });
            ping.ping();
        });
        if let Err(err) = spawned {
            warn!(%err, "settings: no app scan thread");
        }
    }

    /// The image picker of one config key (`wallpaper`, `lock_wallpaper`): a first "empty value" thumb
    /// labelled `none`, then every image found.
    /// `max`: images shown (nothing scrolls, so it is what fits the page).
    fn wall_picker(&self, key: &'static str, value: &str, none: &str, max: usize) -> Vec<Opt> {
        let mut images = wallpapers(self.w.max(self.h) as u32);
        images.truncate(max);
        let current = (!value.is_empty()).then(|| expand(value));
        let mut opts = vec![Opt {
            label: none.into(),
            act: Act::Config(vec![edit::edit(key, Val::Str(String::new()))]),
            selected: current.is_none(),
            look: Look::Thumb(None),
        }];
        for p in &images {
            opts.push(Opt {
                label: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                act: Act::Config(vec![edit::edit(key, Val::Str(tilde(p)))]),
                selected: current.as_deref() == Some(p.as_path()),
                look: Look::Thumb(Some(p.clone())),
            });
        }
        if let Some(w) = current.filter(|w| !images.contains(w)) {
            // An image set by hand somewhere else: shown (selected) so the choice is visible.
            opts.insert(1, Opt {
                label: tilde(&w),
                act: Act::Config(vec![edit::edit(key, Val::Str(tilde(&w)))]),
                selected: true,
                look: Look::Thumb(Some(w)),
            });
        }
        opts
    }

    /// The groups of the current page, from the config (and the scanned apps).
    fn page_groups(&self) -> Vec<Group> {
        let c = &self.config;
        let cfg = |edits: Vec<Edit>| Act::Config(edits);
        let text_opt = |label: &str, act: Act, selected: bool| Opt { label: label.into(), act, selected, look: Look::Text };
        let on_off = |key: &'static str, on: bool| {
            vec![text_opt("On", cfg(vec![edit::edit(key, Val::Bool(true))]), on), text_opt("Off", cfg(vec![edit::edit(key, Val::Bool(false))]), !on)]
        };
        match self.page {
            Page::Personalize => {
                // Every theme found (built-in + theme files); one made for another layout switches the style
                // too, which takes effect at the next sign-in (Scenes are chosen at start).
                let mut mode = Vec::new();
                for t in crate::theme::available(&crate::theme::theme_dirs()) {
                    let mut edits = vec![edit::edit("theme", Val::Str(t.spec.clone()))];
                    // Only a layout this build has (a theme for a newer style still applies its looks).
                    if let Some(style) = t.style.as_ref().filter(|s| **s != c.style && super::STYLES.contains(&s.as_str())) {
                        edits.push(edit::edit("style", Val::Str(style.clone())));
                    }
                    let label = match t.name.as_str() {
                        "dark" => "Windows dark".to_string(),
                        "light" => "Windows light".to_string(),
                        n => n.to_string(),
                    };
                    mode.push(text_opt(&label, cfg(edits), c.theme == t.spec));
                }
                let current = Color::parse(&c.accent);
                let mut accents = vec![Opt {
                    label: "Theme default".into(),
                    act: cfg(vec![edit::edit("accent", Val::Str(String::new()))]),
                    selected: current.is_none(),
                    look: Look::Swatch(None),
                }];
                for col in &self.theme.settings.accents {
                    accents.push(Opt {
                        label: col.hex(),
                        act: cfg(vec![edit::edit("accent", Val::Str(col.hex()))]),
                        selected: current.as_ref().is_some_and(|x| x.hex() == col.hex()),
                        look: Look::Swatch(Some(*col)),
                    });
                }
                let walls = self.wall_picker("wallpaper", &c.wallpaper, "None", MAX_WALLPAPERS);
                let mut groups = vec![
                    Group { label: "Theme".into(), opts: mode },
                    Group { label: "Accent colour".into(), opts: accents },
                    Group { label: "Wallpaper".into(), opts: walls },
                ];
                // Only under rtshell-session (which starts it again); a bare rtshell would just quit.
                if supervised() {
                    let armed = self.restart_armed.is_some_and(|t| t.elapsed() < RESTART_CONFIRM);
                    let label = if armed { "Confirm restart" } else { "Restart rtshell" };
                    groups.push(Group { label: "Shell".into(), opts: vec![text_opt(label, Act::Restart, false)] });
                }
                groups
            }
            Page::Keyboard => {
                let sys = crate::config::Keyboard { layout: String::new(), ..c.keyboard.clone() }.resolved().layout;
                let layout_edit = |name: &str| cfg(vec![edit::edit("keyboard.layout", Val::Str(name.into())), edit::edit("keyboard.variant", Val::Str(String::new()))]);
                let mut layouts = vec![text_opt(&format!("System ({sys})"), layout_edit(""), c.keyboard.layout.is_empty())];
                for (name, label) in LAYOUTS {
                    layouts.push(text_opt(label, layout_edit(name), c.keyboard.layout == name));
                }
                let delay = [250, 400, 600]
                    .into_iter()
                    .map(|ms| text_opt(&format!("{ms} ms"), cfg(vec![edit::edit("keyboard.repeat_delay", Val::Int(ms))]), c.keyboard.repeat_delay as i64 == ms))
                    .collect();
                let rate = [15, 25, 40]
                    .into_iter()
                    .map(|n| text_opt(&format!("{n} per second"), cfg(vec![edit::edit("keyboard.repeat_rate", Val::Int(n))]), c.keyboard.repeat_rate as i64 == n))
                    .collect();
                vec![
                    Group { label: "Layout (Type Cover and on-screen keyboard)".into(), opts: layouts },
                    Group { label: "Key repeat starts after".into(), opts: delay },
                    Group { label: "Key repeat speed".into(), opts: rate },
                    Group { label: "On-screen keyboard (applies after signing in again)".into(), opts: on_off("osk.enabled", c.osk.enabled) },
                ]
            }
            Page::Touch => {
                let zones = [("Narrow", 10.0), ("Normal", 16.0), ("Wide", 28.0)]
                    .into_iter()
                    .map(|(l, px)| text_opt(l, cfg(vec![edit::edit("input.edge_zone", Val::Float(px))]), (c.input.edge_zone - px as f32).abs() < 0.5))
                    .collect();
                vec![
                    Group { label: "Edge swipe area".into(), opts: zones },
                    Group { label: "Touchpad: tap to click".into(), opts: on_off("input.tap_to_click", c.input.tap_to_click) },
                    Group { label: "Touchpad: natural scrolling".into(), opts: on_off("input.natural_scroll", c.input.natural_scroll) },
                ]
            }
            Page::Power => {
                let blank = |src: &'static str, cur: f64| -> Vec<Opt> {
                    IDLE_CHOICES
                        .iter()
                        .map(|&s| {
                            let (b, d): (&'static str, &'static str) = if src == "ac" { ("idle.ac.blank", "idle.ac.dim") } else { ("idle.battery.blank", "idle.battery.dim") };
                            text_opt(&secs_label(s), cfg(vec![edit::edit(b, Val::Float(s)), edit::edit(d, Val::Float(dim_for(s)))]), cur == s)
                        })
                        .collect()
                };
                let sleep = |key: &'static str, cur: f64| -> Vec<Opt> {
                    IDLE_CHOICES.iter().map(|&s| text_opt(&secs_label(s), cfg(vec![edit::edit(key, Val::Float(s))]), cur == s)).collect()
                };
                vec![
                    Group { label: "On battery, turn off the screen after".into(), opts: blank("battery", c.idle.battery.blank) },
                    Group { label: "On battery, sleep after".into(), opts: sleep("idle.battery.sleep", c.idle.battery.sleep) },
                    Group { label: "Plugged in, turn off the screen after".into(), opts: blank("ac", c.idle.ac.blank) },
                    Group { label: "Plugged in, sleep after".into(), opts: sleep("idle.ac.sleep", c.idle.ac.sleep) },
                    Group { label: "Lock when the screen turns off".into(), opts: on_off("idle.lock", c.idle.lock) },
                ]
            }
            Page::Apps => {
                let scan = self.apps.lock().unwrap();
                let Some(scan) = scan.as_ref() else {
                    return vec![Group { label: "Looking for apps...".into(), opts: Vec::new() }];
                };
                let terminals = scan
                    .terminals
                    .iter()
                    .map(|a| {
                        // The whole command: `foot` and Foot Server's `foot --server` share the program.
                        let selected = c.apps.terminal == a.exec;
                        text_opt(&a.name, cfg(vec![edit::edit("apps.terminal", Val::StrList(a.exec.clone()))]), selected)
                    })
                    .collect();
                let browsers = scan.browsers.iter().map(|a| text_opt(&a.name, Act::Browser(a.id.clone()), scan.browser.as_deref() == Some(a.id.as_str()))).collect();
                vec![Group { label: "Terminal (Super+Return)".into(), opts: terminals }, Group { label: "Web browser (links)".into(), opts: browsers }]
            }
            Page::Lock => vec![Group {
                label: "Lock screen image".into(),
                opts: self.wall_picker("lock_wallpaper", &c.lock_wallpaper, "Same as desktop", MAX_LOCK_WALLPAPERS),
            }],
            Page::About => Vec::new(),
        }
    }

    /// Rebuild the whole tree (pages are small: a rebuild is cheaper than tracking changes).
    fn build(&mut self) {
        self.dirty = false;
        self.groups = self.page_groups();
        let theme = &self.theme;
        let t = &theme.settings;
        let mut tree = Tree::new(Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Rect { color: t.fill.premultiplied(), radius: 0.0 }).hit());
        let mut hits = Vec::new();
        let root = tree.root;
        tree[root].opacity = self.o;
        let row = tree.add(root, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).layout(Layout::Row { gap: 0.0 }));

        // Category column.
        let nav = tree.add(
            row,
            Node::new().size(Size::Fixed(t.nav_width), Size::Fill(1.0)).layout(Layout::Column { gap: 0.0 }).paint(Paint::Rect { color: t.nav_fill.premultiplied(), radius: 0.0 }),
        );
        let header = tree.add(
            nav,
            Node::new().size(Size::Fill(1.0), Size::Fixed(t.padding * 2.0 + t.title_size * 1.3)).layout(Layout::Row { gap: t.padding * 0.4 }).padding(t.padding * 0.5),
        );
        let back = tree.add(
            header,
            Node::new().size(Size::Fixed(t.title_size * 1.2), Size::Fixed(t.title_size * 1.2)).align(Align::Start, Align::Center).hit(),
        );
        tree.add(back, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: "go-previous".into(), size: (t.title_size * 0.8) as u32 }));
        hits.push((back, Hit::Back));
        tree.add(header, Node::new().align(Align::Start, Align::Center).paint(text("Settings", t.title_size, t.title_weight, t.text.premultiplied())));
        for page in PAGES {
            let sel = page == self.page;
            let fill = if sel { or_accent(&t.nav_selected, theme) } else { [0.0; 4] };
            let fg = if sel { t.nav_selected_text } else { t.text };
            let r = tree.add(
                nav,
                Node::new().size(Size::Fill(1.0), Size::Fixed(t.nav_row_height)).padding(t.padding * 0.5).paint(Paint::Rect { color: fill, radius: 0.0 }).hit(),
            );
            tree.add(r, Node::new().align(Align::Start, Align::Center).paint(text(page.title(), t.label_size * 1.1, theme.font.weight_body, fg.premultiplied())));
            hits.push((r, Hit::Nav(page)));
        }

        // Page.
        let content_w = (self.w - t.nav_width - 2.0 * t.padding).max(100.0);
        let content = tree.add(row, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).layout(Layout::Column { gap: t.gap }).padding(t.padding).clip());
        tree.add(content, Node::new().paint(text(self.page.title(), t.heading_size, t.heading_weight, t.text.premultiplied())));
        let selected = or_accent(&t.chip_selected, theme);
        // The accent of the configured theme without the user's override ("theme default" swatch), read once.
        let has_default_swatch = self.groups.iter().any(|g| g.opts.iter().any(|o| o.look == Look::Swatch(None)));
        let theme_accent = has_default_swatch.then(|| Theme::load(&self.config.theme).colors.accent);
        for (gi, g) in self.groups.iter().enumerate() {
            let col = tree.add(content, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: t.label_gap }));
            tree.add(col, Node::new().paint(text(&g.label, t.label_size, theme.font.weight_body, t.text_dim.premultiplied())));
            if g.opts.is_empty() {
                continue;
            }
            let cell = match g.opts[0].look {
                Look::Text => t.chip_min_width,
                Look::Swatch(_) => t.swatch + 2.0 * t.ring,
                Look::Thumb(_) => t.thumb as f32 + 2.0 * t.ring,
            };
            let columns = (((content_w + t.chip_gap) / (cell + t.chip_gap)).floor() as usize).max(1);
            let grid = tree.add(col, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Grid { columns, gap: t.chip_gap }));
            for (oi, o) in g.opts.iter().enumerate() {
                let node = match &o.look {
                    Look::Text => {
                        let (fill, fg) = if o.selected { (selected, t.chip_selected_text.premultiplied()) } else { (t.chip_fill.premultiplied(), t.chip_text.premultiplied()) };
                        let n = tree.add(
                            grid,
                            Node::new().size(Size::Fill(1.0), Size::Fixed(t.chip_height)).paint(Paint::Rect { color: fill, radius: t.chip_radius }).clip().hit(),
                        );
                        tree.add(n, Node::new().align(Align::Center, Align::Center).paint(text(&o.label, t.label_size, theme.font.weight_body, fg)));
                        n
                    }
                    Look::Swatch(c) => {
                        let ring = if o.selected { t.text.premultiplied() } else { [0.0; 4] };
                        let side = t.swatch + 2.0 * t.ring;
                        let n = tree.add(grid, Node::new().size(Size::Fixed(side), Size::Fixed(side)).paint(Paint::Rect { color: ring, radius: t.chip_radius }).hit());
                        let fill = c.or(theme_accent).unwrap_or(theme.colors.accent).premultiplied();
                        let sw = tree.add(n, Node::new().size(Size::Fixed(t.swatch), Size::Fixed(t.swatch)).align(Align::Center, Align::Center).paint(Paint::Rect { color: fill, radius: t.chip_radius }));
                        if c.is_none() {
                            // "Theme default": the theme's accent, marked with a small "A".
                            tree.add(sw, Node::new().align(Align::Center, Align::Center).paint(text("A", t.label_size, theme.font.weight_title, theme.colors.on_accent.premultiplied())));
                        }
                        n
                    }
                    Look::Thumb(p) => {
                        let ring = if o.selected { selected } else { [0.0; 4] };
                        let (tw, th) = (t.thumb as f32, (t.thumb as f32 * 9.0 / 16.0).round());
                        let n = tree.add(
                            grid,
                            Node::new().size(Size::Fixed(tw + 2.0 * t.ring), Size::Fixed(th + 2.0 * t.ring)).paint(Paint::Rect { color: ring, radius: t.chip_radius }).hit(),
                        );
                        let inner = tree.add(
                            n,
                            Node::new()
                                .size(Size::Fixed(tw), Size::Fixed(th))
                                .align(Align::Center, Align::Center)
                                .paint(Paint::Rect { color: theme.colors.background.premultiplied(), radius: t.chip_radius })
                                .clip(),
                        );
                        match p {
                            // Rasterized on the icon worker (aspect kept, longest side `thumb`), cached on disk.
                            Some(p) => {
                                tree.add(inner, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: p.display().to_string(), size: t.thumb }));
                            }
                            None => {
                                tree.add(inner, Node::new().align(Align::Center, Align::Center).paint(text(&o.label, t.label_size, theme.font.weight_body, t.text.premultiplied())));
                            }
                        }
                        n
                    }
                };
                hits.push((node, Hit::Opt(gi, oi)));
            }
        }
        if self.page == Page::About {
            for (k, v) in about() {
                let r = tree.add(content, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Row { gap: t.gap }));
                let kc = tree.add(r, Node::new().size(Size::Fixed(t.chip_min_width * 1.6), Size::Fit));
                tree.add(kc, Node::new().paint(text(&k, t.label_size, theme.font.weight_body, t.text_dim.premultiplied())));
                tree.add(r, Node::new().paint(text(&v, t.label_size, theme.font.weight_body, t.text.premultiplied())));
            }
        }
        if let Some(s) = &self.status {
            tree.add(content, Node::new().paint(text(s, t.label_size, theme.font.weight_body, t.text_dim.premultiplied())));
        }
        self.tree = tree;
        self.hits = hits;
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        let n = self.tree.hit_test(x, y)?;
        self.hits.iter().find(|(id, _)| *id == n).map(|(_, h)| *h)
    }

    fn activate(&mut self, hit: Hit) {
        match hit {
            Hit::Back => self.set_open(false),
            Hit::Nav(page) => {
                self.status = None;
                self.show_page(page);
            }
            Hit::Opt(gi, oi) => {
                let Some(opt) = self.groups.get(gi).and_then(|g| g.opts.get(oi)).cloned() else { return };
                if opt.selected {
                    return;
                }
                self.choose(opt);
            }
        }
        self.dirty = true;
    }

    fn choose(&mut self, opt: Opt) {
        match opt.act {
            Act::Config(edits) => {
                let Some(path) = self.config_path.clone().or_else(crate::config::default_path) else {
                    self.status = Some("No config folder (HOME is not set).".into());
                    return;
                };
                info!(edits = ?edits.iter().map(|e| e.path.join(".")).collect::<Vec<_>>(), "settings: writing config.toml");
                match edit::apply(&path, &edits) {
                    Ok(()) => {
                        let style_changed = edits.iter().any(|e| e.path == ["style"]);
                        self.status = style_changed.then(|| {
                            if supervised() {
                                "This theme comes with its own layout: restart rtshell (Shell, below) to see it.".to_string()
                            } else {
                                "This theme comes with its own layout: sign out and in again to see it.".to_string()
                            }
                        });
                        self.config = Config::load_quiet(Some(path));
                        self.requests.push(Request::ConfigChanged(Box::new(self.config.clone())));
                    }
                    Err(err) => {
                        warn!(%err, "settings: could not write config.toml");
                        self.status = Some(format!("Could not save: {err}"));
                    }
                }
            }
            Act::Restart => {
                if self.restart_armed.is_some_and(|t| t.elapsed() < RESTART_CONFIRM) {
                    info!("settings: restart rtshell");
                    self.requests.push(Request::Restart);
                } else {
                    self.restart_armed = Some(std::time::Instant::now());
                    self.status = Some("Restarting rtshell closes open apps. Tap \u{201c}Confirm restart\u{201d} to go ahead.".into());
                }
            }
            Act::Browser(id) => {
                info!(%id, "settings: default browser");
                if let Some(scan) = self.apps.lock().unwrap().as_mut() {
                    scan.browser = Some(id.clone());
                }
                let file = format!("{id}.desktop");
                let _ = std::thread::Builder::new().name("xdg-mime".into()).spawn(move || {
                    let res = std::process::Command::new("xdg-mime").args(["default", &file, "x-scheme-handler/http", "x-scheme-handler/https", "text/html"]).status();
                    match res {
                        Ok(s) if s.success() => info!(%file, "settings: default browser set"),
                        Ok(s) => warn!(%file, status = ?s.code(), "settings: xdg-mime failed"),
                        Err(err) => warn!(%file, %err, "settings: xdg-mime missing"),
                    }
                });
            }
        }
    }

    /// The theme the UI now uses (after a reload).
    pub fn set_theme_tokens(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.dirty = true;
    }
}

const RESTART_CONFIRM: std::time::Duration = std::time::Duration::from_secs(4);

/// Running under `rtshell-session` (which restarts rtshell): it exports RTSHELL_SUPERVISED; older copies of
/// the script do not, so the parent's name counts too.
fn supervised() -> bool {
    std::env::var_os("RTSHELL_SUPERVISED").is_some()
        || std::fs::read_to_string(format!("/proc/{}/comm", std::os::unix::process::parent_id())).is_ok_and(|c| c.trim() == "rtshell-session")
}

/// About: device, system and battery facts read from /proc and /sys (no secrets).
fn about() -> Vec<(String, String)> {
    let read = |p: &str| std::fs::read_to_string(p).ok().map(|s| s.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string()).filter(|s| !s.is_empty());
    let mut v = Vec::new();
    if let Some(m) = read("/sys/firmware/devicetree/base/model").or_else(|| read("/sys/class/dmi/id/product_name")) {
        v.push(("Device".into(), m));
    }
    if let Some(os) = read("/etc/os-release").and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|s| s.trim_matches('"').to_string()))) {
        v.push(("System".into(), os));
    }
    if let Some(k) = read("/proc/sys/kernel/osrelease") {
        v.push(("Kernel".into(), k));
    }
    v.push(("Shell".into(), format!("rtshell {}", env!("CARGO_PKG_VERSION"))));
    if let Some(cpu) = read("/proc/cpuinfo") {
        let n = cpu.lines().filter(|l| l.starts_with("processor")).count();
        v.push(("Processor".into(), format!("{n} cores")));
    }
    if let Some(mem) = read("/proc/meminfo") {
        let kb = |key: &str| mem.lines().find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse::<u64>().ok());
        if let (Some(total), Some(avail)) = (kb("MemTotal:"), kb("MemAvailable:")) {
            v.push(("Memory".into(), format!("{} MB in use of {} MB", (total - avail) / 1024, total / 1024)));
        }
    }
    // Safety: statvfs on a fixed C string into a zeroed struct.
    let st = unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(c"/".as_ptr(), &mut st) == 0).then_some(st)
    };
    if let Some(st) = st {
        let gb = |blocks: u64| blocks as f64 * st.f_frsize as f64 / 1e9;
        v.push(("Storage".into(), format!("{:.1} GB free of {:.1} GB", gb(st.f_bavail as u64), gb(st.f_blocks as u64))));
    }
    if let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") {
        for e in rd.flatten() {
            let p = e.path();
            let get = |n: &str| read(&p.join(n).to_string_lossy()).and_then(|s| s.parse::<f64>().ok());
            if read(&p.join("type").to_string_lossy()).as_deref() != Some("Battery") {
                continue;
            }
            let full = get("energy_full").or_else(|| get("charge_full"));
            let design = get("energy_full_design").or_else(|| get("charge_full_design"));
            let cap = get("capacity");
            let mut s = cap.map(|c| format!("{c:.0} %")).unwrap_or_default();
            if let (Some(f), Some(d)) = (full, design)
                && d > 0.0
            {
                s.push_str(&format!(", holds {:.0} % of its design capacity", f / d * 100.0));
            }
            if !s.is_empty() {
                v.push(("Battery".into(), s));
            }
        }
    }
    if let Some(up) = read("/proc/uptime").and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok()) {
        let m = (up / 60.0) as u64;
        v.push(("Up for".into(), if m >= 60 { format!("{} h {} min", m / 60, m % 60) } else { format!("{m} min") }));
    }
    v
}

impl Scene for Settings {
    fn tree(&mut self) -> &mut Tree {
        &mut self.tree
    }

    fn update(&mut self, _now: Duration, dt: f32) -> bool {
        let target = if self.open { 1.0 } else { 0.0 };
        let ms = self.theme.settings.fade_ms.max(1) as f32;
        let fading = self.o != target;
        if fading {
            let d = dt * 1000.0 / ms;
            self.o = if target > self.o { (self.o + d).min(1.0) } else { (self.o - d).max(0.0) };
        }
        if self.dirty {
            self.build();
        }
        let root = self.tree.root;
        self.tree[root].opacity = self.o;
        fading
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        if !self.open {
            return false;
        }
        match ev.phase {
            Phase::Down => {
                if self.touch.is_none() {
                    self.touch = Some((ev.id, (ev.x, ev.y), self.hit_at(ev.x, ev.y)));
                }
            }
            Phase::Motion => {
                if let Some((id, (x0, y0), hit)) = self.touch.as_mut()
                    && *id == ev.id
                    && hit.is_some()
                    && (ev.x - *x0).abs().max((ev.y - *y0).abs()) > SLOP
                {
                    *hit = None;
                }
            }
            Phase::Up | Phase::Cancel => {
                let all = ev.phase == Phase::Cancel && ev.id == CANCEL_ALL;
                // A touch up has no position of its own: the target from the down counts, unless the finger
                // moved off it (Motion beyond SLOP cleared it).
                if let Some((_, _, hit)) = self.touch.take_if(|t| all || t.0 == ev.id)
                    && ev.phase == Phase::Up
                    && let Some(h) = hit
                {
                    self.activate(h);
                }
            }
        }
        // Everything under the sheet is covered: it takes every touch while open.
        true
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if !self.open {
            return false;
        }
        match key.sym {
            k::KEY_Escape => self.set_open(false),
            k::KEY_Up | k::KEY_Down => {
                let i = PAGES.iter().position(|p| *p == self.page).unwrap_or(0);
                let j = if key.sym == k::KEY_Up { i.saturating_sub(1) } else { (i + 1).min(PAGES.len() - 1) };
                self.show_page(PAGES[j]);
            }
            _ => {}
        }
        true
    }

    fn poll(&mut self) -> bool {
        if self.scanning && self.apps.lock().unwrap().is_some() {
            self.scanning = false;
            if self.page == Page::Apps {
                self.dirty = true;
                return self.open;
            }
        }
        false
    }

    fn opaque(&self) -> bool {
        self.open && self.o >= 1.0 && self.theme.settings.fill.a >= 1.0
    }

    fn set_size(&mut self, w: f32, h: f32) {
        if (w, h) != (self.w, self.h) {
            self.w = w;
            self.h = h;
            self.dirty = true;
        }
    }

    fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.set_theme_tokens(theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_labels_and_dim() {
        assert_eq!(secs_label(30.0), "30 s");
        assert_eq!(secs_label(120.0), "2 min");
        assert_eq!(secs_label(0.0), "Never");
        assert_eq!(dim_for(0.0), 0.0);
        assert_eq!(dim_for(60.0), 40.0);
        assert_eq!(dim_for(300.0), 270.0);
    }
}
