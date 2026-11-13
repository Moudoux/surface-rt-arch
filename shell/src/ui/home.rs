//! Home for style "rt" (DESKTOP-SHELL.md 3.2, Phase 3): a Start screen of tile groups that scrolls sideways,
//! and an All-apps sheet (alphabetical columns, letter jump, type-to-search) that slides up over it.
//!
//! Home is shown while no app is (the core tells us with `set_apps_shown`) and then takes all touch, mouse
//! and keyboard input; while an app is shown Home is hidden and the app gets everything (edge gestures that
//! reach Home from an app come in Phase 4; until then the Super key toggles Home).
//!
//! Touch: tap a tile to launch (it grows into a splash until the app's window appears); drag sideways to
//! scroll, with springs (flick coasting, rubber-band ends); swipe up (or "All apps") for the sheet, swipe
//! down to close it; tap a letter for the letter jump; long-press an app in the list to pin/unpin it.
//! Long-press a tile for edit mode: drag tiles (between groups, or past the last group for a new one),
//! resize, unpin, name the group (typing), Done. Keyboard: typing searches, Enter launches the first hit,
//! Escape goes back. All sizes and colours come from theme tokens (`[home]`, colours, fonts).

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay::reexports::calloop::ping::Ping;
use smithay::input::keyboard::keysyms as k;

use super::{
    Align, Layout, Node, NodeId, Paint, Shadow, Size, Tree,
    pins::{self, Group, Pins, TileSize, TileSpec},
    search,
    shell::Scene,
};
use crate::{
    anim::{Spring, SpringParams, VelocityTracker},
    apps::desktop::{self, DesktopApp},
    services::{Charge, Services},
    overlay::{CANCEL_ALL, KeyEvent, Phase, PointerEvent, Request},
    render::{
        batch::{Rect, Rgba},
        gl::Texture,
        text::TextStyle,
    },
    theme::Theme,
};

/// Finger travel before a touch counts as a drag (px).
const SLOP: f32 = 10.0;
/// Scale of the Start screen while an app covers it (app switch animations).
const REVEAL_SCALE: f32 = 0.92;

/// A launch splash gives up if no window appeared by then.
const SPLASH_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the "Pinned to Start" note stays.
const TOAST: Duration = Duration::from_millis(1600);

/// Kinetic scrolling on one axis: follows the finger with rubber-band ends, coasts after a flick and
/// settles on a soft spring.
pub(super) struct Scroller {
    pub(super) s: Spring,
    pub(super) min: f32,
    pub(super) max: f32,
    pub(super) rubber: f32,
    pub(super) flick: f32,
}

impl Scroller {
    pub(super) fn new(params: SpringParams, rubber: f32, flick: f32) -> Scroller {
        Scroller { s: Spring::new(0.0, params).with_eps(0.3), min: 0.0, max: 0.0, rubber, flick }
    }
    fn band(&self, v: f32) -> f32 {
        if v < self.min {
            self.min - (self.min - v) * self.rubber
        } else if v > self.max {
            self.max + (v - self.max) * self.rubber
        } else {
            v
        }
    }
    pub(super) fn drag(&mut self, raw: f32, vel: f32) {
        let v = self.band(raw);
        self.s.set(v, vel);
    }
    /// Undo the rubber band to get the raw position a drag continues from.
    pub(super) fn unband(&self) -> f32 {
        let v = self.s.value;
        if v < self.min {
            self.min - (self.min - v) / self.rubber
        } else if v > self.max {
            self.max + (v - self.max) / self.rubber
        } else {
            v
        }
    }
    pub(super) fn fling(&mut self, vel: f32) {
        self.s.velocity = vel;
        self.s.set_target((self.s.value + vel * self.flick).clamp(self.min, self.max));
    }
    pub(super) fn nudge(&mut self, d: f32) {
        self.s.set_target((self.s.target + d).clamp(self.min, self.max));
    }
    pub(super) fn to(&mut self, v: f32) {
        self.s.set_target(v.clamp(self.min, self.max));
    }
    pub(super) fn bounds(&mut self, min: f32, max: f32) {
        self.min = min;
        self.max = max.max(min);
        let t = self.s.target.clamp(self.min, self.max);
        self.s.set_target(t);
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Tile(usize),
    AllApps,
    Back,
    /// The search box of All apps.
    Search,
    Entry(usize),
    Letter,
    Jump(char),
    JumpBackground,
    EditUnpin,
    EditResize,
    EditName,
    EditDone,
    PromptWait,
    PromptKill,
    GroupLabel(usize),
    Sheet,
    Background,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Pending,
    ScrollStart,
    ScrollList,
    Sheet,
    DragTile(usize),
    /// Consumed (long press fired, or a direction we ignore): wait for the finger to lift.
    Done,
}

struct Gesture {
    id: i32,
    start: (f32, f32),
    t0: Duration,
    target: Target,
    mode: Mode,
    vel: VelocityTracker,
    origin: f32,
    long_fired: bool,
}

/// A tile on screen.
struct TileView {
    node: NodeId,
    group: usize,
    index: usize,
    color: Rgba,
    icon: String,
    /// What a tap does (None for live tiles).
    launch: Option<Request>,
    press: Spring,
}

struct GroupView {
    label: NodeId,
    x: f32,
    w: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum EntryKind {
    Letter(char),
    App(usize),
}

struct EntryView {
    node: NodeId,
    kind: EntryKind,
}

struct Splash {
    from: Rect,
    p: Spring,
    started: Duration,
    closing: bool,
}

struct Edit {
    selected: usize,
    /// Typing renames this group.
    renaming: Option<usize>,
}

struct Nodes {
    root: NodeId,
    start: NodeId,
    time: NodeId,
    date: NodeId,
    battery: NodeId,
    strip: NodeId,
    all_button: NodeId,
    edit_bar: NodeId,
    edit_buttons: [NodeId; 4],
    sheet: NodeId,
    search: NodeId,
    search_text: NodeId,
    list: NodeId,
    back_button: NodeId,
    toast: NodeId,
    jump: NodeId,
    jump_cells: Vec<(NodeId, char)>,
    splash: NodeId,
    splash_icon: NodeId,
    prompt: NodeId,
    prompt_text: NodeId,
    prompt_buttons: [NodeId; 2],
    backdrop: NodeId,
}

pub struct Home {
    theme: Theme,
    tree: Tree,
    n: Nodes,
    w: f32,
    h: f32,
    incoming: Arc<Mutex<Option<Vec<DesktopApp>>>>,
    /// Battery from UPower (services thread); without it, sysfs.
    services: Option<Services>,
    sys_gen: u64,
    last_battery: Option<(u32, String)>,
    apps: Vec<DesktopApp>,
    apps_loaded: bool,
    /// None until the apps are scanned on first run (defaults depend on them).
    pins: Option<Pins>,
    tiles: Vec<TileView>,
    groups: Vec<GroupView>,
    entries: Vec<EntryView>,
    letters: Vec<char>,
    scroll: Scroller,
    list_scroll: Scroller,
    sheet: Spring,
    jump: Spring,
    splash: Option<Splash>,
    gesture: Option<Gesture>,
    query: String,
    /// The search box was tapped (the on-screen keyboard is up for it while All apps is open).
    search_focus: bool,
    edit: Option<Edit>,
    requests: Vec<Request>,
    apps_shown: bool,
    /// How far an app switch has uncovered Home (1 = no app over it); the Start screen grows with it.
    reveal: f32,
    toast_until: Option<Duration>,
    /// The open "not responding" question (its id and the app's name).
    prompt: Option<(u64, String)>,
    /// Draw only the Overview backdrop (for the core's snapshot of it).
    backdrop: bool,
    now: Duration,
    clock_minute: i64,
    next_minute: Option<Duration>,
    background: Rgba,
    wallpaper: Option<Texture>,
    springs: SpringParams,
    dirty_start: bool,
    dirty_list: bool,
}

fn text(s: &str, size: f32, weight: u16, color: Rgba) -> Paint {
    Paint::Text { text: s.into(), style: TextStyle { size, weight, ..TextStyle::default() }, color, wrap: false, layout: None }
}

pub(super) fn local_time() -> (String, String, i64, u64) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        const MONTHS: [&str; 12] =
            ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
        (
            format!("{}:{:02}", tm.tm_hour, tm.tm_min),
            format!("{}, {} {}", DAYS[tm.tm_wday as usize % 7], tm.tm_mday, MONTHS[tm.tm_mon as usize % 12]),
            t as i64 / 60,
            60 - tm.tm_sec.clamp(0, 59) as u64,
        )
    }
}

/// Battery percentage and status words (as sysfs writes them) from a UPower snapshot.
pub(super) fn upower_battery(s: &Services) -> Option<(u32, String)> {
    let b = s.snapshot().1.battery?;
    let status = match b.charge {
        Charge::Charging => "Charging",
        Charge::Discharging => "Discharging",
        Charge::Full => "Full",
        Charge::Pending => "Not charging",
        Charge::Empty => "Empty",
        Charge::Unknown => "",
    };
    Some((b.percent.round() as u32, status.to_string()))
}

/// Battery percentage and status from sysfs (fallback without the services thread).
pub(super) fn battery() -> Option<(u32, String)> {
    let rd = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for e in rd.flatten() {
        let p = e.path();
        let read = |f: &str| std::fs::read_to_string(p.join(f)).ok().map(|s| s.trim().to_string());
        if read("type").as_deref() != Some("Battery") {
            continue;
        }
        let cap = read("capacity")?.parse().ok()?;
        return Some((cap, read("status").unwrap_or_default()));
    }
    None
}

/// Stable palette slot for a tile.
fn hash(s: &str) -> usize {
    s.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619)) as usize
}

fn letter_of(name: &str) -> char {
    match name.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some(c) if c.is_ascii_alphabetic() => c,
        _ => '#',
    }
}

impl Home {
    /// Read the battery from the services thread (UPower) instead of sysfs.
    pub fn with_services(mut self, services: Services) -> Home {
        self.services = Some(services);
        self
    }

    pub fn new(theme: &Theme, ping: Ping) -> Home {
        let incoming = Arc::new(Mutex::new(None));
        let slot = incoming.clone();
        desktop::watch(move |apps| {
            *slot.lock().unwrap() = Some(apps);
            ping.ping();
        });
        let t = &theme.home;
        let springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        let scroll_springs = SpringParams { stiffness: t.scroll_stiffness, damping: 1.0 };
        let background = theme.colors.background.premultiplied();
        let (tree, n) = Self::build(theme, background);
        let mut home = Home {
            theme: theme.clone(),
            tree,
            n,
            w: 1366.0,
            h: 768.0,
            incoming,
            services: None,
            sys_gen: u64::MAX,
            last_battery: None,
            apps: Vec::new(),
            apps_loaded: false,
            pins: Pins::load(),
            tiles: Vec::new(),
            groups: Vec::new(),
            entries: Vec::new(),
            letters: Vec::new(),
            scroll: Scroller::new(scroll_springs, t.rubber_band, t.flick_time),
            list_scroll: Scroller::new(scroll_springs, t.rubber_band, t.flick_time),
            sheet: Spring::new(0.0, springs).with_eps(0.001),
            jump: Spring::new(0.0, springs).with_eps(0.001),
            splash: None,
            gesture: None,
            query: String::new(),
            search_focus: false,
            edit: None,
            requests: Vec::new(),
            apps_shown: false,
            reveal: 1.0,
            toast_until: None,
            prompt: None,
            backdrop: false,
            now: Duration::ZERO,
            clock_minute: -1,
            next_minute: None,
            background,
            wallpaper: None,
            springs,
            dirty_start: true,
            dirty_list: true,
        };
        home.apply();
        home
    }

    /// The fixed part of the tree: header, empty strip and list, buttons, sheet, jump grid, splash.
    fn build(theme: &Theme, background: Rgba) -> (Tree, Nodes) {
        let t = &theme.home;
        let c = &theme.colors;
        let f = &theme.font;
        let unit = theme.spacing.unit as f32;
        let fg = c.text.premultiplied();
        let dim = c.text_dim.premultiplied();
        let mut tree = Tree::new(Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Rect { color: background, radius: 0.0 }));
        let root = tree.root;
        let full = || Node::new().size(Size::Fill(1.0), Size::Fill(1.0));

        // Start screen.
        let start = tree.add(root, full());
        tree.add(start, Node::new().offset(t.margin_left, t.title_top).paint(text(&t.title, t.title_size, t.title_weight, fg)));
        let status = tree.add(
            start,
            Node::new().layout(Layout::Column { gap: 0.0 }).align(Align::End, Align::Start).offset(-t.margin_left, t.title_top),
        );
        let time = tree.add(status, Node::new().align(Align::End, Align::Start).paint(text("", f.size_title, f.weight_title, fg)));
        let date = tree.add(status, Node::new().align(Align::End, Align::Start).paint(text("", f.size_caption, f.weight_body, dim)));
        let battery = tree.add(status, Node::new().align(Align::End, Align::Start).paint(text("", f.size_caption, f.weight_body, dim)));
        let strip = tree.add(start, Node::new().size(Size::Fixed(1.0), Size::Fill(1.0)));
        let round_button = |tree: &mut Tree, parent: NodeId, icon: &str, label: &str| {
            let b = tree.add(
                parent,
                Node::new()
                    .layout(Layout::Row { gap: 1.5 * unit })
                    .align(Align::Start, Align::End)
                    .offset(t.margin_left - unit, -2.0 * unit)
                    .padding(unit)
                    .hit(),
            );
            let size = theme.spacing.touch_target as f32 - 8.0;
            let circle = tree.add(
                b,
                Node::new()
                    .size(Size::Fixed(size), Size::Fixed(size))
                    .align(Align::Start, Align::Center)
                    .paint(Paint::Rect { color: c.surface.premultiplied(), radius: size * 0.5 }),
            );
            tree.add(circle, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: icon.into(), size: 22 }));
            tree.add(b, Node::new().align(Align::Start, Align::Center).paint(text(label, f.size_body, f.weight_body, fg)));
            b
        };
        let all_button = round_button(&mut tree, start, "go-down", "All apps");
        // Edit bar (edit mode): a glass pill at the bottom centre.
        let edit_bar = tree.add(
            start,
            Node::new()
                .layout(Layout::Row { gap: unit })
                .align(Align::Center, Align::End)
                .offset(0.0, -2.0 * unit)
                .padding(unit)
                .paint(Paint::Glass { radius: theme.shape.radius_panel as f32, tint: theme.glass.tint.premultiplied() })
                .shadow(Shadow {
                    sigma: theme.shadow.small_sigma,
                    dx: 0.0,
                    dy: theme.shadow.small_offset_y,
                    color: theme.shadow.color.premultiplied(),
                }),
        );
        let mut edit_buttons = [0; 4];
        for (i, label) in ["Unpin", "Resize", "Name group", "Done"].iter().enumerate() {
            let color = if i == 3 { c.accent.premultiplied() } else { c.surface.premultiplied() };
            let fgc = if i == 3 { c.on_accent.premultiplied() } else { fg };
            let b = tree.add(
                edit_bar,
                Node::new()
                    .size(Size::Fit, Size::Fixed(theme.spacing.touch_target as f32))
                    .padding(1.5 * unit)
                    .paint(Paint::Rect { color, radius: theme.shape.radius_small as f32 * 1.5 })
                    .hit(),
            );
            tree.add(b, Node::new().align(Align::Center, Align::Center).paint(text(label, f.size_body, f.weight_body, fgc)));
            edit_buttons[i] = b;
        }
        tree[edit_bar].visible = false;

        // All-apps sheet.
        let sheet = tree.add(root, full().paint(Paint::Glass { radius: 0.0, tint: theme.glass.tint.premultiplied() }).hit());
        tree.add(sheet, Node::new().offset(t.margin_left, t.title_top).paint(text("Apps", t.title_size, t.title_weight, fg)));
        let search = tree.add(
            sheet,
            Node::new()
                .size(Size::Fixed(t.search_width), Size::Fixed(theme.spacing.touch_target as f32))
                .layout(Layout::Row { gap: unit })
                .padding(1.5 * unit)
                .align(Align::End, Align::Start)
                .offset(-t.margin_left, t.title_top)
                .paint(Paint::Rect { color: c.surface.premultiplied(), radius: theme.shape.radius_small as f32 * 1.5 })
                .hit(),
        );
        tree.add(search, Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: "search".into(), size: 20 }));
        let search_text = tree.add(search, Node::new().align(Align::Start, Align::Center).paint(text("Type to search", f.size_body, f.weight_body, dim)));
        let list = tree.add(sheet, Node::new().size(Size::Fixed(1.0), Size::Fill(1.0)));
        let back_button = round_button(&mut tree, sheet, "go-up", "Start");
        let toast = tree.add(
            sheet,
            Node::new()
                .align(Align::Center, Align::End)
                .offset(0.0, -3.0 * unit)
                .padding(1.5 * unit)
                .paint(Paint::Rect { color: c.surface.premultiplied(), radius: theme.shape.radius_small as f32 * 1.5 }),
        );
        tree.add(toast, Node::new().paint(text("Pinned to Start", f.size_body, f.weight_body, fg)));
        tree[toast].visible = false;
        tree[sheet].visible = false;

        // Letter jump: a black scrim (darkens the sheet) and a grid of letters.
        // The scrim is the jump node's own paint: as a child of the root it darkens the sheet and Start.
        let jump = tree.add(root, full().paint(Paint::Scrim { color: c.scrim.premultiplied() }).hit());
        let cell = t.list_row * 1.4;
        let grid = tree.add(
            jump,
            Node::new()
                .size(Size::Fixed(7.0 * cell + 6.0 * unit), Size::Fit)
                .layout(Layout::Grid { columns: 7, gap: unit })
                .align(Align::Center, Align::Center),
        );
        let mut jump_cells = Vec::new();
        for ch in std::iter::once('#').chain('A'..='Z') {
            let node = tree.add(
                grid,
                Node::new()
                    .size(Size::Fixed(cell), Size::Fixed(cell))
                    .paint(Paint::Rect { color: c.surface.premultiplied(), radius: theme.shape.radius_small as f32 })
                    .hit(),
            );
            tree.add(node, Node::new().align(Align::Center, Align::Center).paint(text(&ch.to_string(), t.letter_size, f.weight_title, fg)));
            jump_cells.push((node, ch));
        }
        tree[jump].visible = false;

        // Launch splash.
        let splash = tree.add(root, Node::new().size(Size::Fixed(1.0), Size::Fixed(1.0)).paint(Paint::Rect { color: background, radius: 0.0 }));
        let splash_icon = tree.add(splash, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: String::new(), size: 96 }));
        tree[splash].visible = false;

        // "App is not responding" prompt (after a close that did not work): a glass pill at the bottom,
        // above everything.
        let prompt = tree.add(
            root,
            Node::new()
                .layout(Layout::Row { gap: unit })
                .align(Align::Center, Align::End)
                .offset(0.0, -2.0 * unit)
                .padding(unit)
                .paint(Paint::Glass { radius: theme.shape.radius_panel as f32, tint: theme.glass.tint.premultiplied() })
                .shadow(Shadow {
                    sigma: theme.shadow.small_sigma,
                    dx: 0.0,
                    dy: theme.shadow.small_offset_y,
                    color: theme.shadow.color.premultiplied(),
                })
                .hit(),
        );
        let prompt_text = tree.add(
            prompt,
            Node::new().align(Align::Start, Align::Center).padding(unit).paint(text("App is not responding", f.size_body, f.weight_body, fg)),
        );
        let mut prompt_buttons = [0; 2];
        for (i, label) in ["Wait", "Force quit"].iter().enumerate() {
            let color = if i == 1 { c.accent.premultiplied() } else { c.surface.premultiplied() };
            let fgc = if i == 1 { c.on_accent.premultiplied() } else { fg };
            let b = tree.add(
                prompt,
                Node::new()
                    .size(Size::Fit, Size::Fixed(theme.spacing.touch_target as f32))
                    .padding(1.5 * unit)
                    .paint(Paint::Rect { color, radius: theme.shape.radius_small as f32 * 1.5 })
                    .hit(),
            );
            tree.add(b, Node::new().align(Align::Center, Align::Center).paint(text(label, f.size_body, f.weight_body, fgc)));
            prompt_buttons[i] = b;
        }
        tree[prompt].visible = false;

        // The Overview's backdrop: frosted wallpaper with a scrim over everything. Only shown for one frame
        // while the core renders it into a texture (`Overlay::show_backdrop`).
        let backdrop = tree.add(root, full().paint(Paint::Glass { radius: 0.0, tint: theme.glass.tint.premultiplied() }));
        tree.add(backdrop, full().paint(Paint::Scrim { color: c.scrim.premultiplied() }));
        tree[backdrop].visible = false;

        let n = Nodes {
            root,
            start,
            time,
            date,
            battery,
            strip,
            all_button,
            edit_bar,
            edit_buttons,
            sheet,
            search,
            search_text,
            list,
            back_button,
            toast,
            jump,
            jump_cells,
            splash,
            splash_icon,
            prompt,
            prompt_text,
            prompt_buttons,
            backdrop,
        };
        (tree, n)
    }

    fn launch_request(app: &DesktopApp) -> Request {
        Request::Launch { cmd: app.exec.clone(), app_ids: app.window_ids(), cwd: app.path.clone(), terminal: app.terminal, from: None }
    }

    /// Cell rows of the tile grid for the current height (even, so medium tiles line up).
    fn rows(&self) -> u32 {
        let t = &self.theme.home;
        let avail = self.h - t.tiles_top - t.tiles_bottom;
        let rows = ((avail + t.gap) / (t.cell + t.gap)).floor().max(2.0) as u32;
        rows & !1
    }

    fn rebuild_start(&mut self) {
        self.dirty_start = false;
        let strip = self.n.strip;
        self.tree.clear_children(strip);
        self.tiles.clear();
        self.groups.clear();
        let Some(pins) = self.pins.as_ref() else { return };
        let th = &self.theme;
        let t = &th.home;
        let f = &th.font;
        let rows = self.rows();
        let step = t.cell + t.gap;
        let label_color = t.tile_text.premultiplied();
        let glass = t.tile_style == "glass";
        let editing = self.edit.is_some();
        let mut x = t.margin_left;
        let mut views = Vec::new();
        let mut groups = Vec::new();
        for (gi, g) in pins.groups.iter().enumerate() {
            // Tiles of apps that are not installed (or not scanned yet) are kept in the file but not shown.
            let shown: Vec<(usize, &TileSpec)> = g
                .tiles
                .iter()
                .enumerate()
                .filter(|(_, s)| s.app.as_ref().is_none_or(|id| self.apps.iter().any(|a| &a.id == id)))
                .collect();
            let sizes: Vec<TileSize> = shown.iter().map(|(_, s)| s.size).collect();
            let (pos, wcells) = pins::pack(&sizes, rows);
            let gw = (wcells.max(2) as f32 * step - t.gap).max(0.0);
            let renaming = self.edit.as_ref().and_then(|e| e.renaming) == Some(gi);
            let label = match (g.name.is_empty(), editing, renaming) {
                (_, _, true) => format!("{}|", g.name),
                (true, true, false) => "Name group".to_string(),
                _ => g.name.clone(),
            };
            let label_node = self.tree.add(
                strip,
                Node::new()
                    .offset(x, t.tiles_top - t.group_label_size * 2.0)
                    .padding(2.0)
                    .paint(text(&label, t.group_label_size, f.weight_body, th.colors.text.premultiplied()))
                    .hit(),
            );
            groups.push(GroupView { label: label_node, x, w: gw });
            for ((index, spec), (cx, cy)) in shown.into_iter().zip(pos) {
                let (cw, ch) = spec.size.cells();
                let ch = ch.min(rows);
                let rect = Rect::new(x + cx as f32 * step, t.tiles_top + cy as f32 * step, cw as f32 * step - t.gap, ch as f32 * step - t.gap);
                let app = spec.app.as_ref().and_then(|id| self.apps.iter().find(|a| &a.id == id));
                let key = spec.app.clone().or(spec.live.clone()).unwrap_or_else(|| spec.exec.join(" "));
                let color = th.home.tile_palette.get(hash(&key) % th.home.tile_palette.len().max(1)).map(|c| c.premultiplied()).unwrap_or(self.background);
                let paint = if glass {
                    Paint::Glass { radius: t.tile_radius, tint: th.glass.tint.premultiplied() }
                } else {
                    Paint::Rect { color, radius: t.tile_radius }
                };
                let node = self.tree.add(
                    strip,
                    Node::new()
                        .size(Size::Fixed(rect.w), Size::Fixed(rect.h))
                        .offset(rect.x, rect.y)
                        .padding(th.spacing.unit as f32)
                        .paint(paint)
                        .hit(),
                );
                let name = app.map(|a| a.name.clone()).or(spec.name.clone()).unwrap_or_default();
                let icon = app.and_then(|a| a.icon.clone()).or(spec.icon.clone()).unwrap_or_else(|| super::FALLBACK_ICON.into());
                let launch = match (app, &spec.live) {
                    (Some(a), _) => Some(Self::launch_request(a)),
                    (None, None) if !spec.exec.is_empty() => Some(Request::Launch {
                        cmd: spec.exec.clone(),
                        app_ids: spec.exec.first().map(|p| vec![p.rsplit('/').next().unwrap_or(p).to_lowercase()]).unwrap_or_default(),
                        cwd: None,
                        terminal: false,
                        from: None,
                    }),
                    _ => None,
                };
                let big = (f.size_display * 0.6).round();
                match spec.live.as_deref() {
                    Some(kind @ ("clock" | "battery")) => {
                        let col = self.tree.add(node, Node::new().layout(Layout::Column { gap: 0.0 }).align(Align::Start, Align::End));
                        let (a, b) = if kind == "clock" { ("", "") } else { ("", "Battery") };
                        let size = if spec.size == TileSize::Small { f.size_body } else { big };
                        self.tree.add(col, Node::new().paint(text(a, size, f.weight_display, label_color)));
                        if spec.size != TileSize::Small {
                            self.tree.add(col, Node::new().paint(text(b, f.size_caption, f.weight_body, label_color)));
                        }
                    }
                    _ => {
                        let isz = match spec.size {
                            TileSize::Small => t.tile_icons[0],
                            TileSize::Medium | TileSize::Wide => t.tile_icons[1],
                            TileSize::Large => t.tile_icons[2],
                        };
                        let dy = if spec.size == TileSize::Small { 0.0 } else { -f.size_caption * 0.6 };
                        self.tree.add(
                            node,
                            Node::new().align(Align::Center, Align::Center).offset(0.0, dy).paint(Paint::Icon { name: icon.clone(), size: isz }),
                        );
                        if spec.size != TileSize::Small {
                            let l = self.tree.add(node, Node::new().align(Align::Start, Align::End).paint(text(&name, f.size_caption, f.weight_body, label_color)));
                            // Long names stay inside the tile.
                            self.tree[l].clip = true;
                            self.tree[l].width = Size::Fixed(rect.w - 2.0 * th.spacing.unit as f32);
                        }
                    }
                }
                views.push(TileView {
                    node,
                    group: gi,
                    index,
                    color,
                    icon,
                    launch,
                    press: Spring::new(1.0, self.springs).with_eps(0.0005),
                });
            }
            x += gw + t.group_gap;
        }
        let content = x - t.group_gap + t.margin_right;
        self.tree[strip].width = Size::Fixed(content.max(1.0));
        self.tiles = views;
        self.groups = groups;
        self.scroll.bounds(0.0, (content - self.w).max(0.0));
        self.update_live();
        self.tree.mark_layout();
    }

    /// Entries of the All-apps list: letters and apps, or search hits.
    fn rebuild_list(&mut self) {
        self.dirty_list = false;
        let list = self.n.list;
        self.tree.clear_children(list);
        self.entries.clear();
        self.letters.clear();
        let th = &self.theme;
        let t = &th.home;
        let f = &th.font;
        let unit = th.spacing.unit as f32;
        let fg = th.colors.text.premultiplied();
        let rows = (((self.h - t.tiles_top - t.tiles_bottom) / t.list_row).floor() as usize).max(2);
        let searching = !self.query.trim().is_empty();
        let mut seq: Vec<EntryKind> = Vec::new();
        if searching {
            seq.extend(search::search(&self.apps, &self.query).into_iter().map(EntryKind::App));
        } else {
            let mut last = None;
            for (i, a) in self.apps.iter().enumerate().filter(|(_, a)| a.listed) {
                let l = letter_of(&a.name);
                if last != Some(l) {
                    seq.push(EntryKind::Letter(l));
                    self.letters.push(l);
                    last = Some(l);
                }
                seq.push(EntryKind::App(i));
            }
        }
        self.tree.set_text(
            self.n.search_text,
            if searching { self.query.as_str() } else { "Type to search" },
        );
        if let Paint::Text { color, .. } = &mut self.tree[self.n.search_text].paint {
            *color = if searching { fg } else { th.colors.text_dim.premultiplied() };
        }
        if searching && seq.is_empty() {
            let node = self.tree.add(
                list,
                Node::new()
                    .offset(t.margin_left, t.tiles_top)
                    .paint(text(&format!("No apps match \u{201c}{}\u{201d}", self.query.trim()), f.size_body, f.weight_body, th.colors.text_dim.premultiplied())),
            );
            let _ = node;
        }
        let (mut col, mut row) = (0usize, 0usize);
        let item_w = t.list_column - 2.0 * unit;
        for kind in seq {
            // A letter never ends a column.
            if matches!(kind, EntryKind::Letter(_)) && row + 1 >= rows && row > 0 {
                col += 1;
                row = 0;
            }
            let (x, y) = (t.margin_left + col as f32 * t.list_column, t.tiles_top + row as f32 * t.list_row);
            let node = match kind {
                EntryKind::Letter(l) => {
                    let n = self.tree.add(
                        list,
                        Node::new().size(Size::Fixed(t.list_row), Size::Fixed(t.list_row)).offset(x, y).hit(),
                    );
                    self.tree.add(n, Node::new().align(Align::Start, Align::Center).padding(unit).paint(text(&l.to_string(), t.letter_size, f.weight_title, th.colors.accent.premultiplied())));
                    n
                }
                EntryKind::App(i) => {
                    let a = &self.apps[i];
                    let n = self.tree.add(
                        list,
                        Node::new()
                            .size(Size::Fixed(item_w), Size::Fixed(t.list_row))
                            .offset(x, y)
                            .layout(Layout::Row { gap: 1.5 * unit })
                            .padding(unit)
                            .hit(),
                    );
                    self.tree[n].clip = true;
                    let icon = a.icon.clone().unwrap_or_else(|| super::FALLBACK_ICON.into());
                    self.tree.add(n, Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: icon, size: t.list_icon }));
                    self.tree.add(n, Node::new().align(Align::Start, Align::Center).paint(text(&a.name, f.size_body, f.weight_body, fg)));
                    n
                }
            };
            self.entries.push(EntryView { node, kind });
            row += 1;
            if row >= rows {
                col += 1;
                row = 0;
            }
        }
        let cols = col + usize::from(row > 0);
        let content = t.margin_left + cols as f32 * t.list_column + t.margin_right;
        self.tree[list].width = Size::Fixed(content.max(1.0));
        self.list_scroll.bounds(0.0, (content - self.w).max(0.0));
        if searching {
            self.list_scroll.s.set(0.0, 0.0);
        }
        for (node, ch) in self.n.jump_cells.clone() {
            let on = self.letters.contains(&ch);
            for c in self.tree[node].children.clone() {
                if let Paint::Text { color, .. } = &mut self.tree[c].paint {
                    *color = if on { fg } else { th.colors.text_dim.premultiplied().map(|v| v * 0.5) };
                }
            }
        }
        self.tree.mark_layout();
    }

    /// Clock, date and battery text (header and live tiles).
    fn update_live(&mut self) {
        let (hm, date, minute, secs_left) = local_time();
        self.clock_minute = minute;
        self.next_minute = Some(self.now + Duration::from_secs(secs_left));
        let bat = match self.services.as_ref() {
            Some(s) => upower_battery(s),
            None => battery(),
        };
        self.last_battery = bat.clone();
        let bat_text = bat.as_ref().map(|(p, s)| if s == "Charging" { format!("{p} % charging") } else { format!("{p} %") });
        self.tree.set_text(self.n.time, &hm);
        self.tree.set_text(self.n.date, &date);
        self.tree.set_text(self.n.battery, bat_text.as_deref().map(|b| format!("Battery {b}")).as_deref().unwrap_or(""));
        let Some(pins) = self.pins.as_ref() else { return };
        for v in &self.tiles {
            let Some(spec) = pins.groups.get(v.group).and_then(|g| g.tiles.get(v.index)) else { continue };
            let Some(col) = self.tree[v.node].children.first().copied() else { continue };
            let kids = self.tree[col].children.clone();
            match spec.live.as_deref() {
                Some("clock") => {
                    self.tree.set_text(kids[0], &hm);
                    if let Some(&d) = kids.get(1) {
                        self.tree.set_text(d, &date);
                    }
                }
                Some("battery") => {
                    self.tree.set_text(kids[0], bat.as_ref().map(|(p, _)| format!("{p} %")).as_deref().unwrap_or("--"));
                    if let Some(&d) = kids.get(1) {
                        let s = bat.as_ref().map(|(_, s)| s.as_str()).filter(|s| !s.is_empty()).unwrap_or("Battery");
                        self.tree.set_text(d, s);
                    }
                }
                _ => {}
            }
        }
    }

    /// Copy animation state into the tree.
    fn apply(&mut self) {
        let p = self.sheet.value;
        let sheet_on = p > 0.001;
        self.tree.set_visible(self.n.sheet, sheet_on);
        self.tree[self.n.sheet].translate = (0.0, (1.0 - p) * self.h);
        // Under a fully open sheet the Start screen and the wallpaper are hidden (the glass is opaque).
        let covered = p >= 0.999;
        self.tree.set_visible(self.n.start, !covered);
        let root = self.n.root;
        self.tree[root].paint = match (covered, self.wallpaper) {
            (true, _) => Paint::None,
            (false, Some(tex)) => Paint::Backdrop { tex },
            (false, None) => Paint::Rect { color: self.background, radius: 0.0 },
        };
        self.tree[self.n.start].opacity = (1.0 - p * 0.6).clamp(0.0, 1.0);
        // An app switch: the Start screen sits back a little while an app covers it.
        let r = self.reveal;
        self.tree[self.n.start].scale = REVEAL_SCALE + (1.0 - REVEAL_SCALE) * r * r * (3.0 - 2.0 * r);
        self.tree[self.n.strip].translate = (-self.scroll.s.value, 0.0);
        self.tree[self.n.list].translate = (-self.list_scroll.s.value, 0.0);
        let j = self.jump.value;
        self.tree.set_visible(self.n.jump, j > 0.001);
        self.tree[self.n.jump].opacity = j.clamp(0.0, 1.0);
        self.tree.set_visible(self.n.toast, self.toast_until.is_some_and(|t| self.now < t));
        let editing = self.edit.as_ref().map(|e| e.selected);
        self.tree.set_visible(self.n.prompt, self.prompt.is_some());
        self.tree.set_visible(self.n.backdrop, self.backdrop);
        self.tree.set_visible(self.n.edit_bar, editing.is_some() && self.prompt.is_none());
        self.tree.set_visible(self.n.all_button, editing.is_none());
        let drag = match self.gesture.as_ref().map(|g| (g.mode, g.start)) {
            Some((Mode::DragTile(i), start)) => Some((i, start)),
            _ => None,
        };
        for (i, v) in self.tiles.iter().enumerate() {
            let n = &mut self.tree[v.node];
            n.scale = v.press.value;
            n.translate = (0.0, 0.0);
            n.opacity = match editing {
                Some(sel) if sel != i => 0.6,
                _ => 1.0,
            };
            if editing.is_some() && editing != Some(i) {
                n.scale *= 0.94;
            }
        }
        if let (Some((i, _)), Some(g)) = (drag, self.gesture.as_ref())
            && let Some(v) = self.tiles.get(i)
        {
            let (dx, dy) = self.drag_offset(g);
            let n = &mut self.tree[v.node];
            n.translate = (dx, dy);
            n.scale = 1.05;
        }
        // Splash: from the tile's rectangle to the whole screen.
        match &self.splash {
            Some(s) => {
                let k = s.p.value.clamp(0.0, 1.0);
                let lerp = |a: f32, b: f32| a + (b - a) * k;
                let r = Rect::new(lerp(s.from.x, 0.0), lerp(s.from.y, 0.0), lerp(s.from.w, self.w), lerp(s.from.h, self.h));
                let n = &mut self.tree[self.n.splash];
                n.offset = (r.x, r.y);
                n.width = Size::Fixed(r.w.max(1.0));
                n.height = Size::Fixed(r.h.max(1.0));
                n.opacity = if s.closing { k } else { 1.0 };
                let on = s.p.value > 0.01;
                self.tree.set_visible(self.n.splash, on);
                self.tree.mark_layout();
            }
            None => self.tree.set_visible(self.n.splash, false),
        }
    }

    fn drag_offset(&self, g: &Gesture) -> (f32, f32) {
        let last = g.vel.last().unwrap_or(g.start);
        (last.0 - g.start.0, last.1 - g.start.1)
    }

    fn open_sheet(&mut self, open: bool) {
        self.sheet.set_target(if open { 1.0 } else { 0.0 });
        if !open {
            self.search_focus = false;
        }
        if open {
            self.edit = None;
            self.dirty_start = true;
        } else {
            self.jump.set_target(0.0);
        }
    }

    fn set_query(&mut self, q: String) {
        if q != self.query {
            self.query = q;
            self.dirty_list = true;
        }
    }

    fn launch_from(&mut self, mut req: Request, from: Rect, color: Rgba, icon: &str) {
        if let Request::Launch { from: f, .. } = &mut req {
            *f = Some(from);
        }
        self.requests.push(req);
        let mut p = Spring::new(0.0, self.springs).with_eps(0.001);
        p.set_target(1.0);
        self.splash = Some(Splash { from, p, started: self.now, closing: false });
        if let Paint::Rect { color: c, .. } = &mut self.tree[self.n.splash].paint {
            *c = color;
        }
        if let Paint::Icon { name, .. } = &mut self.tree[self.n.splash_icon].paint {
            *name = icon.to_string();
        }
    }

    fn launch_entry(&mut self, i: usize) {
        let Some(EntryKind::App(a)) = self.entries.get(i).map(|e| e.kind) else { return };
        let app = &self.apps[a];
        let req = Self::launch_request(app);
        let icon = app.icon.clone().unwrap_or_else(|| super::FALLBACK_ICON.into());
        let pal = &self.theme.home.tile_palette;
        let color = pal.get(hash(&app.id) % pal.len().max(1)).map(|c| c.premultiplied()).unwrap_or(self.background);
        let from = self.tree.screen_rect(self.entries[i].node);
        self.launch_from(req, from, color, &icon);
    }

    fn save_pins(&mut self) {
        if let Some(p) = &self.pins {
            p.save();
        }
    }

    fn show_toast(&mut self, msg: &str) {
        if let Some(&c) = self.tree[self.n.toast].children.first() {
            self.tree.set_text(c, msg);
        }
        self.toast_until = Some(self.now + TOAST);
    }

    /// Tile `i` was dropped at screen point (x, y): move it to that place in the group order.
    fn drop_tile(&mut self, i: usize, x: f32, y: f32) {
        let Some(v) = self.tiles.get(i) else { return };
        let (from_g, from_i) = (v.group, v.index);
        let t = &self.theme.home;
        let sx = x + self.scroll.s.value;
        let step = t.cell + t.gap;
        let rows = self.rows();
        let Some(pins) = self.pins.as_mut() else { return };
        // Target group: the one under the point; past the last one (or before the first) a new group.
        let gi = self.groups.iter().position(|g| sx >= g.x - t.group_gap * 0.5 && sx < g.x + g.w + t.group_gap * 0.5);
        let spec = pins.groups[from_g].tiles.remove(from_i);
        let mut from_g = from_g;
        let (to_g, cell) = match gi {
            Some(g) => (g, ((sx - self.groups[g].x) / step, (y - t.tiles_top) / step)),
            None => {
                let at = if self.groups.first().is_some_and(|g| sx < g.x) { 0 } else { pins.groups.len() };
                pins.groups.insert(at, Group { name: String::new(), tiles: Vec::new() });
                if at <= from_g {
                    from_g += 1;
                }
                (at, (0.0, 0.0))
            }
        };
        // Insert before the first tile that comes after the point in packing order (block, row, column).
        let key = |cx: f32, cy: f32| ((cx / 4.0).floor() as i32, cy.floor() as i32, cx.floor() as i32);
        let point = key(cell.0.max(0.0), cell.1.max(0.0));
        let group = &pins.groups[to_g];
        let sizes: Vec<TileSize> = group.tiles.iter().map(|s| s.size).collect();
        let (pos, _) = pins::pack(&sizes, rows);
        let idx = pos.iter().position(|&(px, py)| key(px as f32, py as f32) > point).unwrap_or(group.tiles.len());
        pins.groups[to_g].tiles.insert(idx, spec);
        // The source group disappears when it is empty now; groups after it move down by one.
        let to_g = if pins.groups[from_g].tiles.is_empty() && from_g < to_g { to_g - 1 } else { to_g };
        pins.groups.retain(|g| !g.tiles.is_empty());
        self.after_edit();
        // Keep the moved tile selected.
        let spec_pos = self.tiles.iter().position(|v| v.group == to_g && v.index == idx);
        if let (Some(e), Some(p)) = (self.edit.as_mut(), spec_pos) {
            e.selected = p;
        }
    }

    fn after_edit(&mut self) {
        self.save_pins();
        self.rebuild_start();
    }

    fn selected_spec(&mut self) -> Option<(usize, usize)> {
        let sel = self.edit.as_ref()?.selected;
        let v = self.tiles.get(sel)?;
        Some((v.group, v.index))
    }

    fn tap(&mut self, target: Target) {
        match target {
            Target::Tile(i) => {
                if let Some(e) = self.edit.as_mut() {
                    e.selected = i;
                    e.renaming = None;
                    return;
                }
                let v = &self.tiles[i];
                if let Some(req) = v.launch.clone() {
                    let from = self.tree.screen_rect(v.node);
                    let (color, icon) = (v.color, v.icon.clone());
                    self.launch_from(req, from, color, &icon);
                }
            }
            Target::AllApps => self.open_sheet(true),
            Target::Back => self.open_sheet(false),
            Target::Search => self.search_focus = true,
            Target::Entry(i) => self.launch_entry(i),
            Target::Letter => self.jump.set_target(1.0),
            Target::Jump(ch) => {
                if let Some(e) = self.entries.iter().find(|e| e.kind == EntryKind::Letter(ch)) {
                    let x = self.tree[e.node].frame.x - self.theme.home.margin_left;
                    self.list_scroll.to(x);
                }
                self.jump.set_target(0.0);
            }
            Target::JumpBackground => self.jump.set_target(0.0),
            Target::EditUnpin => {
                if let Some((g, i)) = self.selected_spec()
                    && let Some(p) = self.pins.as_mut()
                {
                    p.groups[g].tiles.remove(i);
                    p.groups.retain(|g| !g.tiles.is_empty());
                    self.after_edit();
                    let n = self.tiles.len();
                    if n == 0 {
                        self.edit = None;
                    } else if let Some(e) = self.edit.as_mut() {
                        e.selected = e.selected.min(n - 1);
                    }
                    self.dirty_start = true;
                }
            }
            Target::EditResize => {
                if let Some((g, i)) = self.selected_spec()
                    && let Some(p) = self.pins.as_mut()
                {
                    let s = &mut p.groups[g].tiles[i];
                    s.size = s.size.next();
                    self.after_edit();
                }
            }
            Target::EditName => {
                if let Some((g, _)) = self.selected_spec()
                    && let Some(e) = self.edit.as_mut()
                {
                    e.renaming = Some(g);
                    self.show_rename_hint();
                }
            }
            Target::GroupLabel(g) => {
                if let Some(e) = self.edit.as_mut() {
                    e.renaming = Some(g);
                    self.show_rename_hint();
                }
            }
            Target::EditDone => self.end_edit(),
            Target::PromptWait | Target::PromptKill => {
                if let Some((id, _)) = self.prompt.take() {
                    self.requests.push(Request::ForceQuit { id, kill: target == Target::PromptKill });
                }
                self.apply();
            }
            Target::Sheet => {}
            Target::Background => {
                if self.edit.is_some() {
                    self.end_edit();
                }
            }
        }
    }

    fn show_rename_hint(&mut self) {
        // The group label shows the name as typed; an empty one shows the placeholder.
        self.dirty_start = true;
    }

    fn end_edit(&mut self) {
        self.edit = None;
        self.dirty_start = true;
    }

    fn long_press(&mut self, target: Target) -> bool {
        match target {
            Target::Tile(i) => {
                self.edit = Some(Edit { selected: i, renaming: None });
                for v in &mut self.tiles {
                    v.press.set_target(1.0);
                }
                self.dirty_start = true;
                true
            }
            Target::Entry(i) => {
                let Some(EntryKind::App(a)) = self.entries.get(i).map(|e| e.kind) else { return false };
                let id = self.apps[a].id.clone();
                let pins = self.pins.get_or_insert_with(Pins::default);
                let now = pins.toggle(&id);
                self.save_pins();
                self.dirty_start = true;
                self.show_toast(if now { "Pinned to Start" } else { "Unpinned from Start" });
                true
            }
            _ => false,
        }
    }

    fn target_at(&self, x: f32, y: f32) -> Target {
        let Some(hit) = self.tree.hit_test(x, y) else { return Target::Background };
        if let Some(i) = self.tiles.iter().position(|v| v.node == hit) {
            return Target::Tile(i);
        }
        if let Some(i) = self.entries.iter().position(|e| e.node == hit) {
            return match self.entries[i].kind {
                EntryKind::Letter(_) => Target::Letter,
                EntryKind::App(_) => Target::Entry(i),
            };
        }
        if let Some(&(_, ch)) = self.n.jump_cells.iter().find(|(n, _)| *n == hit) {
            return if self.letters.contains(&ch) { Target::Jump(ch) } else { Target::JumpBackground };
        }
        if let Some(g) = self.groups.iter().position(|g| g.label == hit) {
            return Target::GroupLabel(g);
        }
        let b = &self.n.edit_buttons;
        let pb = &self.n.prompt_buttons;
        match hit {
            h if h == pb[0] => Target::PromptWait,
            h if h == pb[1] => Target::PromptKill,
            h if h == self.n.prompt => Target::Sheet,
            h if h == self.n.all_button => Target::AllApps,
            h if h == self.n.back_button => Target::Back,
            h if h == self.n.search => Target::Search,
            h if h == self.n.jump => Target::JumpBackground,
            h if h == b[0] => Target::EditUnpin,
            h if h == b[1] => Target::EditResize,
            h if h == b[2] => Target::EditName,
            h if h == b[3] => Target::EditDone,
            h if h == self.n.sheet => Target::Sheet,
            _ => Target::Background,
        }
    }

    fn press(&mut self, target: Target, down: bool) {
        if let Target::Tile(i) = target
            && self.edit.is_none()
            && let Some(v) = self.tiles.get_mut(i)
        {
            v.press.set_target(if down { self.theme.home.pressed_scale } else { 1.0 });
        }
    }

    fn sheet_open(&self) -> bool {
        self.sheet.target > 0.5
    }

    /// A screen rectangle if it is fully on screen.
    fn on_screen(&self, r: Rect) -> Option<Rect> {
        (r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= self.w && r.y + r.h <= self.h && r.w > 1.0).then_some(r)
    }
}

impl Scene for Home {
    fn tree(&mut self) -> &mut Tree {
        &mut self.tree
    }

    fn update(&mut self, now: Duration, dt: f32) -> bool {
        self.now = now;
        if self.next_minute.is_none_or(|t| now >= t) {
            self.update_live();
        }
        if self.dirty_start {
            self.rebuild_start();
        }
        if self.dirty_list {
            self.rebuild_list();
        }
        // Long press: a finger that stayed put.
        let long = Duration::from_millis(self.theme.home.long_press_ms as u64);
        if let Some(g) = self.gesture.as_ref()
            && g.mode == Mode::Pending
            && !g.long_fired
            && now.saturating_sub(g.t0) >= long
        {
            let target = g.target;
            if self.long_press(target) {
                let g = self.gesture.as_mut().unwrap();
                g.long_fired = true;
            }
        }
        if let Some(s) = self.splash.as_mut() {
            if !s.closing && now.saturating_sub(s.started) > SPLASH_TIMEOUT {
                s.closing = true;
                s.p.set_target(0.0);
            }
        }
        let mut moving = false;
        moving |= self.scroll.s.step(dt);
        moving |= self.list_scroll.s.step(dt);
        moving |= self.sheet.step(dt);
        moving |= self.jump.step(dt);
        for v in &mut self.tiles {
            moving |= v.press.step(dt);
        }
        if let Some(s) = self.splash.as_mut() {
            moving |= s.p.step(dt);
            if s.closing && s.p.is_resting() {
                self.splash = None;
            }
        }
        if self.sheet.is_resting() && self.sheet.value < 0.001 && !self.query.is_empty() {
            self.set_query(String::new());
            self.rebuild_list();
        }
        self.apply();
        let toast = self.toast_until.is_some_and(|t| now < t);
        moving || self.gesture.as_ref().is_some_and(|g| g.mode == Mode::Pending && !g.long_fired) || toast
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        match ev.phase {
            Phase::Down => {
                if self.gesture.is_some() {
                    return true; // one finger at a time (pinch comes later)
                }
                // A splash still waiting after a second (an app that never opens a window): a touch
                // dismisses it.
                if let Some(sp) = self.splash.as_mut().filter(|s| !s.closing) {
                    if ev.time.saturating_sub(sp.started) > Duration::from_secs(1) {
                        sp.closing = true;
                        sp.p.set_target(0.0);
                    }
                    return true;
                }
                let target = self.target_at(ev.x, ev.y);
                self.press(target, true);
                let mut vel = VelocityTracker::default();
                vel.add(ev.time, ev.x, ev.y);
                self.gesture = Some(Gesture { id: ev.id, start: (ev.x, ev.y), t0: ev.time, target, mode: Mode::Pending, vel, origin: 0.0, long_fired: false });
                true
            }
            Phase::Motion => {
                let sheet_open = self.sheet_open();
                let editing = self.edit.is_some();
                let h = self.h;
                let Some(g) = self.gesture.as_mut().filter(|g| g.id == ev.id) else { return false };
                g.vel.add(ev.time, ev.x, ev.y);
                let (dx, dy) = (ev.x - g.start.0, ev.y - g.start.1);
                let (vx, vy) = g.vel.velocity();
                if g.mode == Mode::Pending && dx.abs().max(dy.abs()) > SLOP {
                    let target = g.target;
                    g.mode = match target {
                        Target::Tile(i) if editing || g.long_fired => Mode::DragTile(i),
                        _ if g.long_fired => Mode::Done,
                        Target::Jump(_) | Target::JumpBackground => Mode::Done,
                        _ if dx.abs() > dy.abs() => {
                            if sheet_open { Mode::ScrollList } else { Mode::ScrollStart }
                        }
                        _ if sheet_open && dy > 0.0 => Mode::Sheet,
                        _ if !sheet_open && dy < 0.0 && !editing => Mode::Sheet,
                        _ => Mode::Done,
                    };
                    match g.mode {
                        Mode::ScrollStart => g.origin = self.scroll.unband() + dx,
                        Mode::ScrollList => g.origin = self.list_scroll.unband() + dx,
                        Mode::Sheet => g.origin = self.sheet.value + dy / h,
                        _ => {}
                    }
                    let mode = g.mode;
                    if !matches!(mode, Mode::DragTile(_)) {
                        self.press(target, false);
                    }
                }
                let Some(g) = self.gesture.as_ref() else { return true };
                match g.mode {
                    Mode::ScrollStart => self.scroll.drag(g.origin - dx, -vx),
                    Mode::ScrollList => self.list_scroll.drag(g.origin - dx, -vx),
                    Mode::Sheet => {
                        let p = (g.origin - dy / self.h).clamp(0.0, 1.0);
                        self.sheet.set(p, -vy / self.h);
                    }
                    _ => {}
                }
                true
            }
            Phase::Up | Phase::Cancel => {
                let all = ev.phase == Phase::Cancel && ev.id == CANCEL_ALL;
                let Some(g) = self.gesture.take_if(|g| all || g.id == ev.id) else { return false };
                self.press(g.target, false);
                let (vx, vy) = g.vel.velocity();
                let cancel = ev.phase == Phase::Cancel;
                match g.mode {
                    Mode::Pending if !g.long_fired && !cancel => self.tap(g.target),
                    Mode::ScrollStart => self.scroll.fling(-vx),
                    Mode::ScrollList => self.list_scroll.fling(-vx),
                    Mode::Sheet => {
                        let coast = self.sheet.value - vy / self.h * 0.15;
                        self.open_sheet(coast > 0.5);
                    }
                    Mode::DragTile(i) if !cancel => {
                        let last = g.vel.last().unwrap_or(g.start);
                        self.drop_tile(i, last.0, last.1);
                    }
                    _ => {}
                }
                true
            }
        }
    }

    fn axis(&mut self, _x: f32, _y: f32, dx: f32, dy: f32) -> bool {
        // Wheel notches arrive as 15 px; a sideways list wants more per notch.
        let d = (dx + dy) * 3.0;
        if self.sheet_open() {
            self.list_scroll.nudge(d);
        } else {
            self.scroll.nudge(d);
        }
        true
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if key.ctrl || key.alt {
            return false;
        }
        // Renaming a group in edit mode.
        if let Some(g) = self.edit.as_ref().and_then(|e| e.renaming)
            && let Some(p) = self.pins.as_mut()
            && let Some(group) = p.groups.get_mut(g)
        {
            match key.sym {
                k::KEY_Return | k::KEY_KP_Enter | k::KEY_Escape => {
                    if let Some(e) = self.edit.as_mut() {
                        e.renaming = None;
                    }
                    self.save_pins();
                }
                k::KEY_BackSpace => {
                    group.name.pop();
                }
                _ => match &key.text {
                    Some(t) => group.name.push_str(t),
                    None => return false,
                },
            }
            self.dirty_start = true;
            return true;
        }
        match key.sym {
            k::KEY_Escape => {
                if self.jump.target > 0.5 {
                    self.jump.set_target(0.0);
                } else if !self.query.is_empty() {
                    self.set_query(String::new());
                } else if self.sheet_open() {
                    self.open_sheet(false);
                } else if self.edit.is_some() {
                    self.end_edit();
                } else {
                    return false;
                }
                true
            }
            k::KEY_BackSpace => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(q);
                true
            }
            k::KEY_Return | k::KEY_KP_Enter => {
                if self.sheet_open() && !self.query.is_empty() {
                    if self.dirty_list {
                        self.rebuild_list();
                    }
                    if let Some(i) = self.entries.iter().position(|e| matches!(e.kind, EntryKind::App(_))) {
                        self.launch_entry(i);
                    }
                }
                true
            }
            _ => match &key.text {
                Some(t) => {
                    if self.query.is_empty() && t.trim().is_empty() {
                        return true;
                    }
                    self.open_sheet(true);
                    let q = format!("{}{}", self.query, t);
                    self.set_query(q);
                    true
                }
                None => false,
            },
        }
    }

    fn next_wakeup(&self) -> Option<Duration> {
        let mut t = self.next_minute;
        if let Some(g) = self.gesture.as_ref().filter(|g| g.mode == Mode::Pending && !g.long_fired) {
            let at = g.t0 + Duration::from_millis(self.theme.home.long_press_ms as u64);
            t = Some(t.map_or(at, |x| x.min(at)));
        }
        if let Some(s) = self.splash.as_ref().filter(|s| !s.closing) {
            let at = s.started + SPLASH_TIMEOUT;
            t = Some(t.map_or(at, |x| x.min(at)));
        }
        if let Some(at) = self.toast_until {
            t = Some(t.map_or(at, |x| x.min(at)));
        }
        t
    }

    fn set_wallpaper(&mut self, tex: Option<Texture>) {
        self.wallpaper = tex;
        self.apply();
    }

    fn opaque(&self) -> bool {
        true
    }

    fn set_size(&mut self, w: f32, h: f32) {
        self.w = w;
        self.h = h;
        self.dirty_start = true;
        self.dirty_list = true;
        self.apply();
    }

    fn poll(&mut self) -> bool {
        // UPower changed: redraw only if the battery text would.
        let mut battery = false;
        if let Some(s) = self.services.as_ref()
            && s.generation() != self.sys_gen
        {
            self.sys_gen = s.generation();
            if upower_battery(s) != self.last_battery {
                self.update_live();
                battery = true;
            }
        }
        let Some(apps) = self.incoming.lock().unwrap().take() else { return battery };
        self.apps = apps;
        if self.pins.is_none() {
            self.pins = Some(Pins::defaults(&self.apps));
        }
        self.apps_loaded = true;
        self.dirty_start = true;
        self.dirty_list = true;
        true
    }

    fn set_apps_shown(&mut self, shown: bool) {
        self.apps_shown = shown;
        if shown {
            // The app is up: Home starts from the Start screen next time.
            self.splash = None;
            self.gesture = None;
            let was_editing = self.edit.take().is_some();
            self.sheet.set(0.0, 0.0);
            self.jump.set(0.0, 0.0);
            if !self.query.is_empty() {
                self.query.clear();
                self.dirty_list = true;
            }
            for v in &mut self.tiles {
                v.press.set(1.0, 0.0);
            }
            // Rebuild now, while the app is on screen, not in the first frame of the next switch to Home
            // (that frame also redraws everything and starts compositing the app).
            if was_editing || self.dirty_start {
                self.rebuild_start();
            }
            if self.dirty_list {
                self.rebuild_list();
            }
        } else {
            self.update_live();
        }
        self.apply();
    }

    fn visible(&self) -> bool {
        !self.apps_shown
    }

    fn app_rect(&self, app_id: &str) -> Option<Rect> {
        if self.sheet.value > 0.5 {
            let e = self.entries.iter().find(|e| match e.kind {
                EntryKind::App(a) => self.apps[a].window_ids().iter().any(|x| x == app_id),
                EntryKind::Letter(_) => false,
            })?;
            return self.on_screen(self.tree.screen_rect(e.node));
        }
        if self.sheet.value > 0.001 {
            return None;
        }
        let v = self.tiles.iter().find(|v| match &v.launch {
            Some(Request::Launch { app_ids, .. }) => app_ids.iter().any(|x| x == app_id),
            _ => false,
        })?;
        self.on_screen(self.tree.screen_rect(v.node))
    }

    fn set_reveal(&mut self, r: f32) {
        if r < 0.999 {
            // A running app is coming in or going out: no launch splash (that is for apps still starting).
            self.splash = None;
        }
        if r != self.reveal {
            self.reveal = r;
            self.apply();
        }
    }

    fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    fn prompt_not_responding(&mut self, id: u64, app_id: &str) -> bool {
        let name = self
            .apps
            .iter()
            .find(|a| a.window_ids().iter().any(|x| x == app_id))
            .map(|a| a.name.clone())
            .unwrap_or_else(|| app_id.to_string());
        self.tree.set_text(self.n.prompt_text, &format!("{name} is not responding"));
        self.prompt = Some((id, name));
        self.apply();
        true
    }

    fn show_backdrop(&mut self, on: bool) -> bool {
        self.backdrop = on;
        self.apply();
        true
    }

    /// The All-apps search box was tapped (while the sheet is open), or a group is being renamed.
    fn text_field(&self) -> bool {
        (self.search_focus && self.sheet_open()) || self.edit.as_ref().is_some_and(|e| e.renaming.is_some())
    }

    fn dismiss_prompt(&mut self, id: u64) {
        if self.prompt.as_ref().is_some_and(|(p, _)| *p == id) {
            self.prompt = None;
            self.apply();
        }
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        let t = &theme.home;
        self.springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        let scroll = SpringParams { stiffness: t.scroll_stiffness, damping: 1.0 };
        for sc in [&mut self.scroll, &mut self.list_scroll] {
            sc.s.params = scroll;
            sc.rubber = t.rubber_band;
            sc.flick = t.flick_time;
        }
        self.sheet.params = self.springs;
        self.jump.params = self.springs;
        self.background = theme.colors.background.premultiplied();
        let (tree, n) = Self::build(theme, self.background);
        self.tree = tree;
        self.n = n;
        // Node ids changed: everything that pointed into the old tree is rebuilt.
        self.tiles.clear();
        self.entries.clear();
        self.groups.clear();
        self.gesture = None;
        self.splash = None;
        self.dirty_start = true;
        self.dirty_list = true;
        self.update_live();
        self.apply();
    }
}
