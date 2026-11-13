//! Home for style "ipad" (DESKTOP-SHELL.md Phase 10, "Springboard design"): pages of app icons with names that
//! swipe sideways, page dots, a floating dock (pinned apps, then recently launched ones), and a search sheet
//! pulled down from the pages (with an empty query it lists every app A-Z, so apps removed from Home stay
//! reachable).
//!
//! Like the RT Home (`home.rs`) it is shown while no app is, takes all input then, and reuses the app scan,
//! search, launch requests, the launch splash, the "not responding" prompt and the Overview backdrop.
//!
//! Touch: tap = launch; swipe sideways = pages (snap, a flick moves one page); pull down = search; long-press
//! an icon = edit mode (icons wobble, a remove badge on page icons, Done top right) with the icon picked up at
//! once; drag it to a slot (the others make room), to the left/right edge to flip pages, or onto the dock.
//! Keyboard: typing searches, Enter launches the first hit, Escape goes back. Every look value is a
//! `[springboard]` theme token; the layout is saved by `board.rs`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay::input::keyboard::keysyms as k;
use smithay::reexports::calloop::ping::Ping;

use super::{
    Align, Layout, Node, NodeId, Paint, Shadow, Size, Tree,
    board::{self, Board, Slot},
    home::{Scroller, battery, local_time, upower_battery},
    search,
    shell::Scene,
};
use crate::{
    anim::{Spring, SpringParams, VelocityTracker},
    apps::desktop::{self, DesktopApp},
    overlay::{CANCEL_ALL, KeyEvent, Phase, PointerEvent, Request},
    render::{
        batch::{Rect, Rgba},
        gl::Texture,
        text::TextStyle,
    },
    services::{Charge, Link, Services, Sys},
    theme::Theme,
};

/// Finger travel before a touch counts as a drag (px).
const SLOP: f32 = 10.0;
/// A launch splash gives up if no window appeared by then.
const SPLASH_TIMEOUT: Duration = Duration::from_secs(15);
const TOAST: Duration = Duration::from_millis(1600);
/// A flick faster than this (px/s) opens or closes search whatever the distance.
const FLICK: f32 = 500.0;
/// Edit mode: how close to the left/right edge a held icon flips the page (px).
const EDGE: f32 = 48.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Icon(usize),
    Badge(usize),
    Recent(usize),
    Dot(usize),
    Done,
    Result(usize),
    Field,
    SheetHeader,
    Results,
    PromptWait,
    PromptKill,
    Prompt,
    Background,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Pending,
    Pages,
    /// Pulling search down from the pages.
    Pull,
    /// Pushing the search sheet back up.
    Push,
    Results,
    Drag,
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
    /// The page the touch started on (a flick moves at most one page from it).
    page: usize,
    long_fired: bool,
}

/// An icon on a page or in the dock.
struct IconView {
    node: NodeId,
    badge: Option<NodeId>,
    id: String,
    slot: Slot,
    press: Spring,
    /// Reflow offset while another icon is dragged.
    tx: Spring,
    ty: Spring,
    phase: f32,
}

struct ResultView {
    node: NodeId,
    app: usize,
}

struct Drag {
    id: String,
    from: Slot,
    at: (f32, f32),
    to: Option<Slot>,
    /// Since when the finger has been at a left/right edge.
    edge_since: Option<Duration>,
}

struct Splash {
    from: Rect,
    p: Spring,
    started: Duration,
    closing: bool,
}

struct Nodes {
    root: NodeId,
    main: NodeId,
    time: NodeId,
    battery: NodeId,
    strip: NodeId,
    dots: NodeId,
    dock: NodeId,
    done: NodeId,
    ghost: NodeId,
    ghost_icon: NodeId,
    sheet: NodeId,
    field: NodeId,
    field_text: NodeId,
    viewport: NodeId,
    results: NodeId,
    toast: NodeId,
    toast_text: NodeId,
    splash: NodeId,
    splash_icon: NodeId,
    prompt: NodeId,
    prompt_text: NodeId,
    prompt_buttons: [NodeId; 2],
    backdrop: NodeId,
}

pub struct Springboard {
    theme: Theme,
    tree: Tree,
    n: Nodes,
    w: f32,
    h: f32,
    incoming: Arc<Mutex<Option<Vec<DesktopApp>>>>,
    services: Option<Services>,
    sys_gen: u64,
    last_battery: Option<(u32, String)>,
    apps: Vec<DesktopApp>,
    by_id: HashMap<String, usize>,
    /// None until the apps are scanned on first run (defaults depend on them).
    board: Option<Board>,
    /// What is shown: pages of desktop ids, the pinned dock, recent apps.
    pages: Vec<Vec<String>>,
    dock: Vec<String>,
    recent: Vec<String>,
    icons: Vec<IconView>,
    page_nodes: Vec<NodeId>,
    /// The widget column on the first page (landscape).
    today: Option<NodeId>,
    /// What the status bar and widgets last showed from the services (redraw when it changes).
    sys_key: String,
    dot_nodes: Vec<NodeId>,
    recent_nodes: Vec<(NodeId, String)>,
    results: Vec<ResultView>,
    scroll: Scroller,
    search_scroll: Scroller,
    search: Spring,
    search_focus: bool,
    query: String,
    edit: bool,
    drag: Option<Drag>,
    gesture: Option<Gesture>,
    splash: Option<Splash>,
    requests: Vec<Request>,
    apps_shown: bool,
    reveal: f32,
    toast_until: Option<Duration>,
    prompt: Option<(u64, String)>,
    backdrop: bool,
    now: Duration,
    next_minute: Option<Duration>,
    background: Rgba,
    wallpaper: Option<Texture>,
    springs: SpringParams,
    dirty: bool,
    dirty_results: bool,
}

fn text(s: &str, size: f32, weight: u16, color: Rgba) -> Paint {
    Paint::Text { text: s.into(), style: TextStyle { size, weight, ..TextStyle::default() }, color, wrap: false, layout: None }
}

/// A name shortened to about what fits in `width` px (labels are one line under the icon).
fn fit_label(name: &str, width: f32, size: f32) -> String {
    let max = ((width / (size * 0.56)).floor() as usize).max(4);
    if name.chars().count() <= max {
        name.to_string()
    } else {
        let mut s: String = name.chars().take(max - 1).collect();
        s = s.trim_end().to_string();
        s.push('…');
        s
    }
}

/// The pages while `drag` is lifted out of `from` and held over `to`: the dragged icon's place is an empty
/// string, overflow ripples forward, no page is dropped (indices stay stable while dragging).
fn preview(pages: &[Vec<String>], from: Slot, to: Option<Slot>, per_page: usize) -> Vec<Vec<String>> {
    let mut v: Vec<Vec<String>> = pages.to_vec();
    if let Slot::Page(p, i) = from
        && p < v.len()
        && i < v[p].len()
    {
        v[p].remove(i);
    }
    if let Some(Slot::Page(p, i)) = to {
        while v.len() <= p {
            v.push(Vec::new());
        }
        let i = i.min(v[p].len());
        v[p].insert(i, String::new());
        let mut q = p;
        while v[q].len() > per_page.max(1) {
            let last = v[q].pop().unwrap();
            if q + 1 >= v.len() {
                v.push(Vec::new());
            }
            v[q + 1].insert(0, last);
            q += 1;
        }
    }
    v
}

impl Springboard {
    pub fn with_services(mut self, services: Services) -> Springboard {
        self.services = Some(services);
        self
    }

    pub fn new(theme: &Theme, ping: Ping) -> Springboard {
        let incoming = Arc::new(Mutex::new(None));
        let slot = incoming.clone();
        desktop::watch(move |apps| {
            *slot.lock().unwrap() = Some(apps);
            ping.ping();
        });
        let t = &theme.springboard;
        let springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        let page_springs = SpringParams { stiffness: t.page_stiffness, damping: 1.0 };
        let background = theme.colors.background.premultiplied();
        let (tree, n) = Self::build(theme, background);
        let mut sb = Springboard {
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
            by_id: HashMap::new(),
            board: Board::load(),
            pages: vec![Vec::new()],
            dock: Vec::new(),
            recent: Vec::new(),
            icons: Vec::new(),
            page_nodes: Vec::new(),
            today: None,
            sys_key: String::new(),
            dot_nodes: Vec::new(),
            recent_nodes: Vec::new(),
            results: Vec::new(),
            scroll: Scroller::new(page_springs, t.rubber_band, t.flick_time),
            search_scroll: Scroller::new(SpringParams { stiffness: theme.home.scroll_stiffness, damping: 1.0 }, t.rubber_band, theme.home.flick_time),
            search: Spring::new(0.0, springs).with_eps(0.001),
            search_focus: false,
            query: String::new(),
            edit: false,
            drag: None,
            gesture: None,
            splash: None,
            requests: Vec::new(),
            apps_shown: false,
            reveal: 1.0,
            toast_until: None,
            prompt: None,
            backdrop: false,
            now: Duration::ZERO,
            next_minute: None,
            background,
            wallpaper: None,
            springs,
            dirty: true,
            dirty_results: true,
        };
        sb.apply();
        sb
    }

    fn shadow(theme: &Theme) -> Shadow {
        Shadow { sigma: theme.shadow.small_sigma, dx: 0.0, dy: theme.shadow.small_offset_y, color: theme.shadow.color.premultiplied() }
    }

    fn plate_paint(theme: &Theme, edge: f32) -> Paint {
        let t = &theme.springboard;
        let radius = t.plate_radius.min(edge * 0.5);
        match t.plate.as_str() {
            "solid" => Paint::Rect { color: t.plate_fill.premultiplied(), radius },
            "glass" => Paint::Glass { radius, tint: t.plate_fill.premultiplied() },
            _ => Paint::None,
        }
    }

    /// Plate edge for an icon of `icon` px.
    fn plate_edge(&self, icon: u32) -> f32 {
        let t = &self.theme.springboard;
        if t.plate == "none" { icon as f32 } else { icon as f32 + 2.0 * t.plate_padding }
    }

    /// The fixed part of the tree: status strip, empty page strip, dots, dock, Done, ghost, search sheet,
    /// toast, splash, prompt, backdrop.
    fn build(theme: &Theme, background: Rgba) -> (Tree, Nodes) {
        let t = &theme.springboard;
        let c = &theme.colors;
        let f = &theme.font;
        let unit = theme.spacing.unit as f32;
        let mut tree = Tree::new(Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Rect { color: background, radius: 0.0 }));
        let root = tree.root;
        let full = || Node::new().size(Size::Fill(1.0), Size::Fill(1.0));
        let status_text = t.status_text.premultiplied();

        let main = tree.add(root, full());
        let strip = tree.add(main, Node::new().size(Size::Fixed(1.0), Size::Fill(1.0)));
        let status = tree.add(main, Node::new().size(Size::Fill(1.0), Size::Fixed(t.status_height)));
        let time = tree.add(
            status,
            Node::new().align(Align::Start, Align::Center).offset(t.status_margin, 0.0).paint(text("", t.status_size, t.status_weight, status_text)),
        );
        let battery = tree.add(
            status,
            Node::new().layout(Layout::Row { gap: 6.0 }).align(Align::End, Align::Center).offset(-t.status_margin, 0.0),
        );
        let dots = tree.add(main, Node::new().layout(Layout::Row { gap: t.dots_gap }).align(Align::Center, Align::End));
        let mut dock = Node::new()
            .layout(Layout::Row { gap: t.dock_gap })
            .size(Size::Fit, Size::Fixed(t.dock_height))
            .padding(t.dock_padding)
            .align(Align::Center, Align::End)
            .offset(0.0, -t.dock_margin)
            .paint(if t.dock_glass {
                Paint::Glass { radius: t.dock_radius, tint: t.dock_fill.premultiplied() }
            } else {
                Paint::Rect { color: t.dock_fill.premultiplied(), radius: t.dock_radius }
            })
            .hit();
        if t.dock_shadow {
            dock = dock.shadow(Self::shadow(theme));
        }
        let dock = tree.add(main, dock);
        let done_fill = theme_color(&t.done_fill, c.accent.premultiplied());
        let done = tree.add(
            main,
            Node::new()
                .size(Size::Fit, Size::Fixed(theme.spacing.touch_target as f32 - 12.0))
                .padding(1.5 * unit)
                .align(Align::End, Align::Start)
                .offset(-t.status_margin, t.status_height + unit)
                .paint(Paint::Rect { color: done_fill, radius: (theme.spacing.touch_target as f32 - 12.0) * 0.5 })
                .hit(),
        );
        tree.add(done, Node::new().align(Align::Center, Align::Center).paint(text(&t.label_done, f.size_body, f.weight_title, t.done_text.premultiplied())));
        tree[done].visible = false;

        // The icon being dragged (edit mode), above the pages and the dock.
        let pe = if t.plate == "none" { t.icon as f32 } else { t.icon as f32 + 2.0 * t.plate_padding };
        let ghost = tree.add(main, Node::new().size(Size::Fixed(pe), Size::Fixed(pe)).paint(Self::plate_paint(theme, pe)).shadow(Self::shadow(theme)));
        let ghost_icon = tree.add(ghost, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: String::new(), size: t.icon }));
        tree[ghost].visible = false;

        // Search sheet: frosted wallpaper, the field, and a vertically scrolling grid of results.
        let sheet = tree.add(root, full().paint(Paint::Glass { radius: 0.0, tint: t.search_tint.premultiplied() }).hit());
        let field = tree.add(
            sheet,
            Node::new()
                .size(Size::Fixed(t.search_width), Size::Fixed(t.search_height))
                .layout(Layout::Row { gap: unit })
                .padding(1.5 * unit)
                .align(Align::Center, Align::Start)
                .offset(0.0, t.status_height + 2.0 * unit)
                .paint(Paint::Rect { color: t.search_fill.premultiplied(), radius: t.search_radius })
                .hit(),
        );
        tree.add(field, Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: "search".into(), size: 20 }));
        let field_text = tree.add(field, Node::new().align(Align::Start, Align::Center).paint(text(&t.label_search, f.size_body, f.weight_body, t.search_placeholder.premultiplied())));
        let top = t.status_height + 4.0 * unit + t.search_height;
        let viewport = tree.add(sheet, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).offset(0.0, top).clip());
        let results = tree.add(viewport, Node::new().size(Size::Fill(1.0), Size::Fixed(1.0)));
        tree[sheet].visible = false;

        let toast = tree.add(
            root,
            Node::new()
                .align(Align::Center, Align::End)
                .offset(0.0, -(t.dock_height + t.dock_margin + 3.0 * unit))
                .padding(1.5 * unit)
                .paint(Paint::Rect { color: t.toast_fill.premultiplied(), radius: t.toast_radius }),
        );
        let toast_text = tree.add(toast, Node::new().paint(text("", f.size_body, f.weight_body, t.toast_text.premultiplied())));
        tree[toast].visible = false;

        let splash = tree.add(root, Node::new().size(Size::Fixed(1.0), Size::Fixed(1.0)).paint(Paint::Rect { color: background, radius: t.plate_radius }));
        let splash_icon = tree.add(splash, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: String::new(), size: 96 }));
        tree[splash].visible = false;

        let fg = c.text.premultiplied();
        let prompt = tree.add(
            root,
            Node::new()
                .layout(Layout::Row { gap: unit })
                .align(Align::Center, Align::End)
                .offset(0.0, -2.0 * unit)
                .padding(unit)
                .paint(Paint::Glass { radius: theme.shape.radius_panel as f32, tint: theme.glass.tint.premultiplied() })
                .shadow(Self::shadow(theme))
                .hit(),
        );
        let prompt_text = tree.add(prompt, Node::new().align(Align::Start, Align::Center).padding(unit).paint(text("", f.size_body, f.weight_body, fg)));
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

        let backdrop = tree.add(root, full().paint(Paint::Glass { radius: 0.0, tint: theme.glass.tint.premultiplied() }));
        tree.add(backdrop, full().paint(Paint::Scrim { color: c.scrim.premultiplied() }));
        tree[backdrop].visible = false;

        let n = Nodes {
            root,
            main,
            time,
            battery,
            strip,
            dots,
            dock,
            done,
            ghost,
            ghost_icon,
            sheet,
            field,
            field_text,
            viewport,
            results,
            toast,
            toast_text,
            splash,
            splash_icon,
            prompt,
            prompt_text,
            prompt_buttons,
            backdrop,
        };
        (tree, n)
    }

    // ---- geometry

    fn grid(&self) -> (usize, usize) {
        let t = &self.theme.springboard;
        if self.h > self.w { (t.columns_portrait.max(1), t.rows_portrait.max(1)) } else { (t.columns.max(1), t.rows.max(1)) }
    }

    fn per_page(&self) -> usize {
        let (c, r) = self.grid();
        c * r
    }

    fn dock_top(&self) -> f32 {
        let t = &self.theme.springboard;
        self.h - t.dock_margin - t.dock_height
    }

    /// The widget column is shown (landscape, enabled, widgets listed).
    fn today_on(&self) -> bool {
        self.theme.springboard.today && self.w >= self.h && self.board.as_ref().is_some_and(|b| !b.widgets.is_empty())
    }

    fn today_width(&self) -> f32 {
        let t = &self.theme.springboard;
        2.0 * t.widget_small + t.widget_gap
    }

    /// Left edge of the icon grid (right of the widget column when it is shown).
    fn grid_x0(&self) -> f32 {
        let t = &self.theme.springboard;
        if self.today_on() { t.today_x + self.today_width() + t.grid_gap_x } else { t.grid_margin_x }
    }

    /// Cell width and row height of the page grid.
    fn cell(&self) -> (f32, f32) {
        let t = &self.theme.springboard;
        let (cols, rows) = self.grid();
        let cw = (self.w - t.grid_margin_x - self.grid_x0()).max(1.0) / cols as f32;
        let rh = (self.dock_top() - t.grid_bottom - t.grid_top).max(1.0) / rows as f32;
        (cw, rh)
    }

    /// Top-left of slot `i` inside its page.
    fn slot_pos(&self, i: usize) -> (f32, f32) {
        let t = &self.theme.springboard;
        let (cols, _) = self.grid();
        let (cw, rh) = self.cell();
        (self.grid_x0() + (i % cols) as f32 * cw, t.grid_top + (i / cols) as f32 * rh)
    }

    /// Pages drawn: the shown ones plus an empty one at the end while editing (somewhere to drag to).
    fn page_count(&self) -> usize {
        self.pages.len() + usize::from(self.edit && self.pages.last().is_some_and(|p| !p.is_empty()))
    }

    fn current_page(&self) -> usize {
        ((self.scroll.s.target / self.w.max(1.0)).round().max(0.0) as usize).min(self.page_count().saturating_sub(1))
    }

    // ---- model

    /// Shown pages, dock and recent apps from the board.
    fn refresh(&mut self) {
        let per_page = self.per_page();
        let t = &self.theme.springboard;
        let Some(b) = self.board.as_ref() else { return };
        let by_id = &self.by_id;
        self.pages = b.shown_pages(|id| by_id.contains_key(id), per_page);
        self.dock = b.dock.iter().filter(|id| by_id.contains_key(id.as_str())).cloned().collect();
        self.recent = b
            .recent
            .iter()
            .filter(|id| by_id.contains_key(id.as_str()) && !self.dock.contains(id))
            .take(t.dock_recent)
            .cloned()
            .collect();
        self.dirty = true;
    }

    /// Write the shown pages and dock back to the board and save it.
    fn commit(&mut self) {
        let by_id = self.by_id.clone();
        let (pages, dock) = (self.pages.clone(), self.dock.clone());
        let Some(b) = self.board.as_mut() else { return };
        b.set_pages(pages, |id| by_id.contains_key(id));
        let missing: Vec<String> = b.dock.iter().filter(|id| !by_id.contains_key(id.as_str())).cloned().collect();
        b.dock = dock.into_iter().chain(missing).collect();
        b.recent.retain(|r| !b.dock.contains(r));
        b.save();
        self.refresh();
    }

    // ---- tree

    /// One icon cell: plate with the icon, the name below (pages only), the remove badge (edit mode).
    fn add_icon(&mut self, parent: NodeId, app: usize, x: f32, y: f32, w: f32, h: f32, icon: u32, label: Option<Rgba>, badge: bool) -> (NodeId, Option<NodeId>) {
        let th = &self.theme;
        let t = &th.springboard;
        let pe = if t.plate == "none" { icon as f32 } else { icon as f32 + 2.0 * t.plate_padding };
        let label_h = if label.is_some() { t.label_gap + t.label_size * 1.35 } else { 0.0 };
        let top = ((h - pe - label_h) * 0.5).max(0.0);
        let a = &self.apps[app];
        let name = a.icon.clone().unwrap_or_else(|| super::FALLBACK_ICON.into());
        let cell = self.tree.add(parent, Node::new().size(Size::Fixed(w), Size::Fixed(h)).offset(x, y).hit());
        let plate = self.tree.add(
            cell,
            Node::new().size(Size::Fixed(pe), Size::Fixed(pe)).align(Align::Center, Align::Start).offset(0.0, top).paint(Self::plate_paint(th, pe)),
        );
        self.tree.add(plate, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name, size: icon }));
        if let Some(color) = label {
            let s = fit_label(&a.name, w - 4.0, t.label_size);
            self.tree.add(
                cell,
                Node::new().align(Align::Center, Align::Start).offset(0.0, top + pe + t.label_gap).paint(text(&s, t.label_size, t.label_weight, color)),
            );
        }
        let badge = badge.then(|| {
            let bs = t.badge_size;
            let b = self.tree.add(
                cell,
                Node::new()
                    .size(Size::Fixed(bs), Size::Fixed(bs))
                    .offset((w - pe) * 0.5 - bs * 0.35, top - bs * 0.35)
                    .paint(Paint::Rect { color: t.badge_fill.premultiplied(), radius: bs * 0.5 })
                    .hit(),
            );
            self.tree.add(b, Node::new().align(Align::Center, Align::Center).paint(text(&t.badge_label, bs * 0.75, 600, t.badge_text.premultiplied())));
            b
        });
        (cell, badge)
    }

    fn rebuild(&mut self) {
        self.dirty = false;
        let (strip, dots, dock) = (self.n.strip, self.n.dots, self.n.dock);
        self.tree.clear_children(strip);
        self.tree.clear_children(dots);
        self.tree.clear_children(dock);
        self.icons.clear();
        self.page_nodes.clear();
        self.dot_nodes.clear();
        self.recent_nodes.clear();
        let t = self.theme.springboard.clone();
        let (cw, rh) = self.cell();
        let n_pages = self.page_count();
        let springs = self.springs;
        let mut views = Vec::new();
        for p in 0..n_pages {
            let page = self.tree.add(strip, Node::new().size(Size::Fixed(self.w), Size::Fill(1.0)).offset(p as f32 * self.w, 0.0));
            self.page_nodes.push(page);
            if p == 0 {
                self.today = self.today_on().then(|| self.tree.add(page, Node::new().offset(t.today_x, t.today_top)));
            }
            let ids = self.pages.get(p).cloned().unwrap_or_default();
            for (i, id) in ids.iter().enumerate() {
                let Some(&app) = self.by_id.get(id) else { continue };
                let (x, y) = self.slot_pos(i);
                let (node, badge) = self.add_icon(page, app, x, y, cw, rh, t.icon, Some(t.label_color.premultiplied()), true);
                views.push((node, badge, id.clone(), Slot::Page(p, i)));
            }
        }
        self.tree[strip].width = Size::Fixed((n_pages as f32 * self.w).max(1.0));
        self.scroll.bounds(0.0, (n_pages.saturating_sub(1)) as f32 * self.w);
        // Page dots (only with more than one page).
        if n_pages > 1 {
            for _ in 0..n_pages {
                let d = self.tree.add(
                    dots,
                    Node::new().size(Size::Fixed(t.dots_size), Size::Fixed(t.dots_size)).paint(Paint::Rect { color: t.dot.premultiplied(), radius: t.dots_size * 0.5 }).hit(),
                );
                self.dot_nodes.push(d);
            }
        }
        self.tree[dots].offset = (0.0, -(t.dock_height + t.dock_margin + t.dots_offset - t.dots_size * 0.5));
        // Dock: pinned, a divider, recent.
        let edge = t.dock_height - 2.0 * t.dock_padding;
        for (i, id) in self.dock.clone().iter().enumerate() {
            let Some(&app) = self.by_id.get(id) else { continue };
            let (node, _) = self.add_icon(dock, app, 0.0, 0.0, edge, edge, t.dock_icon, None, false);
            self.tree[node].valign = Align::Center;
            views.push((node, None, id.clone(), Slot::Dock(i)));
        }
        if !self.recent.is_empty() {
            self.tree.add(dock, Node::new().size(Size::Fixed(1.0), Size::Fixed(edge * 0.8)).align(Align::Start, Align::Center).paint(Paint::Rect { color: t.dock_divider.premultiplied(), radius: 0.0 }));
            for id in self.recent.clone() {
                let Some(&app) = self.by_id.get(&id) else { continue };
                let (node, _) = self.add_icon(dock, app, 0.0, 0.0, edge, edge, t.dock_icon, None, false);
                self.tree[node].valign = Align::Center;
                self.recent_nodes.push((node, id));
            }
        }
        // An empty dock keeps a minimum width.
        self.tree[dock].width = if self.dock.is_empty() && self.recent.is_empty() { Size::Fixed(edge * 3.0) } else { Size::Fit };
        for (i, (node, badge, id, slot)) in views.into_iter().enumerate() {
            if let Some(b) = badge {
                self.tree[b].visible = self.edit;
            }
            self.icons.push(IconView {
                node,
                badge,
                id,
                slot,
                press: Spring::new(1.0, springs).with_eps(0.0005),
                tx: Spring::new(0.0, springs).with_eps(0.05),
                ty: Spring::new(0.0, springs).with_eps(0.05),
                phase: (i as f32 * 2.399) % std::f32::consts::TAU,
            });
        }
        if let Some(d) = self.drag.as_ref()
            && let Some(v) = self.icons.iter().find(|v| v.slot == d.from)
        {
            self.tree[v.node].visible = false;
        }
        self.update_clock();
        self.tree.mark_layout();
    }

    /// Search results: hits for the query, or every listed app A-Z.
    fn rebuild_results(&mut self) {
        self.dirty_results = false;
        let results = self.n.results;
        self.tree.clear_children(results);
        self.results.clear();
        let t = self.theme.springboard.clone();
        let q = self.query.trim().to_string();
        let hits: Vec<usize> = if q.is_empty() {
            (0..self.apps.len()).filter(|&i| self.apps[i].listed).collect()
        } else {
            search::search(&self.apps, &q)
        };
        let searching = !q.is_empty();
        self.tree.set_text(self.n.field_text, if searching { self.query.as_str() } else { t.label_search.as_str() });
        if let Paint::Text { color, .. } = &mut self.tree[self.n.field_text].paint {
            *color = if searching { t.search_text.premultiplied() } else { t.search_placeholder.premultiplied() };
        }
        let cols = t.search_columns.max(1);
        let width = (self.w - 2.0 * t.grid_margin_x).max(1.0);
        let cw = width / cols as f32;
        if hits.is_empty() {
            let f = &self.theme.font;
            self.tree.add(results, Node::new().align(Align::Center, Align::Start).offset(0.0, t.search_row * 0.3).paint(text(&t.label_no_match, f.size_body, f.weight_body, t.search_placeholder.premultiplied())));
        }
        for (k, &app) in hits.iter().enumerate() {
            let (x, y) = (t.grid_margin_x + (k % cols) as f32 * cw, (k / cols) as f32 * t.search_row);
            let (node, _) = self.add_icon(results, app, x, y, cw, t.search_row, t.search_icon, Some(t.search_label.premultiplied()), false);
            self.results.push(ResultView { node, app });
        }
        let rows = hits.len().div_ceil(cols);
        let content = rows as f32 * t.search_row + t.search_row * 0.5;
        self.tree[results].height = Size::Fixed(content.max(1.0));
        let view_h = self.h - (t.status_height + 4.0 * self.theme.spacing.unit as f32 + t.search_height);
        self.search_scroll.bounds(0.0, (content - view_h).max(0.0));
        if searching {
            self.search_scroll.s.set(0.0, 0.0);
        }
        self.tree.mark_layout();
    }

    fn update_clock(&mut self) {
        let (hm, date, _, secs_left) = local_time();
        self.next_minute = Some(self.now + Duration::from_secs(secs_left));
        let sys = self.services.as_ref().map(|s| s.snapshot().1);
        let bat = match self.services.as_ref() {
            Some(s) => upower_battery(s),
            None => battery(),
        };
        self.last_battery = bat.clone();
        self.sys_key = sys_key(sys.as_ref(), &bat);
        // The time is in the widget column when it is shown, else at the left of the status bar.
        let today = self.today_on();
        self.tree.set_text(self.n.time, if today { "" } else { &hm });
        let _ = date;
        self.build_status(sys.as_ref(), &bat);
        self.build_today(sys.as_ref(), &bat);
    }

    /// Status bar, right: Wi-Fi bars, battery percentage, battery glyph (drawn from token colours: the icon
    /// theme's symbolic icons cannot be tinted to the status colour).
    fn build_status(&mut self, sys: Option<&Sys>, bat: &Option<(u32, String)>) {
        let row = self.n.battery;
        self.tree.clear_children(row);
        let t = self.theme.springboard.clone();
        let g = t.status_glyph;
        let fg = t.status_text.premultiplied();
        let dim = fg.map(|v| v * 0.35);
        if let Some(w) = sys.and_then(|s| s.wifi.as_ref()).filter(|w| w.enabled) {
            let on = if w.link == Link::Connected { (w.strength as f32 / 25.0).ceil().clamp(1.0, 4.0) as usize } else { 0 };
            let bars = self.tree.add(row, Node::new().layout(Layout::Row { gap: g * 0.18 }).align(Align::Start, Align::Center));
            for i in 0..4 {
                let h = g * (0.4 + 0.2 * i as f32);
                let c = if i < on { fg } else { dim };
                self.tree.add(bars, Node::new().size(Size::Fixed(g * 0.27), Size::Fixed(h)).align(Align::Start, Align::End).paint(Paint::Rect { color: c, radius: g * 0.1 }));
            }
        }
        if let Some((p, status)) = bat {
            self.tree.add(row, Node::new().align(Align::Start, Align::Center).paint(text(&format!("{p}%"), t.status_size, t.status_weight, fg)));
            let (w, h) = (g * 2.2, g);
            let body = self.tree.add(row, Node::new().size(Size::Fixed(w), Size::Fixed(h)).align(Align::Start, Align::Center).paint(Paint::Rect { color: dim, radius: h * 0.3 }));
            let level = if status == "Charging" { t.status_battery_charging.premultiplied() } else { fg };
            let lw = ((w - 3.0) * (*p as f32 / 100.0)).max(1.0);
            self.tree.add(body, Node::new().size(Size::Fixed(lw), Size::Fixed(h - 3.0)).offset(1.5, 1.5).paint(Paint::Rect { color: level, radius: h * 0.2 }));
            self.tree.add(row, Node::new().size(Size::Fixed(1.5), Size::Fixed(h * 0.4)).align(Align::Start, Align::Center).offset(-4.5, 0.0).paint(Paint::Rect { color: dim, radius: 0.0 }));
        }
        self.tree.mark_layout();
    }

    /// The widget column: a large clock and the date, then widget cards (small ones two per row, wide ones a
    /// row each) until the space above the page dots runs out.
    fn build_today(&mut self, sys: Option<&Sys>, bat: &Option<(u32, String)>) {
        let Some(col) = self.today else { return };
        self.tree.clear_children(col);
        let t = self.theme.springboard.clone();
        let tm = tm_now();
        let fg = t.status_text.premultiplied();
        let (hm, _, _, _) = local_time();
        self.tree.add(col, Node::new().paint(text(&hm, t.clock_size, t.clock_weight, fg)));
        let date = format!("{}, {} {}", DAYS[tm.tm_wday as usize % 7], tm.tm_mday, MONTHS[tm.tm_mon as usize % 12]);
        self.tree.add(col, Node::new().offset(2.0, t.clock_size * 1.12).paint(text(&date, t.date_size, t.date_weight, fg)));
        let small = t.widget_small;
        let wide = self.today_width();
        let mut y = t.clock_size * 1.12 + t.date_size * 1.4 + t.widget_gap * 1.5;
        let bottom = self.dock_top() - t.dots_offset - t.dots_size - t.today_top;
        let mut x_slot = 0;
        let widgets = self.board.as_ref().map(|b| b.widgets.clone()).unwrap_or_default();
        for kind in widgets {
            let is_wide = matches!(kind.as_str(), "month" | "network");
            if is_wide && x_slot == 1 {
                y += small + t.widget_gap;
                x_slot = 0;
            }
            if y + small > bottom {
                break;
            }
            let x = if x_slot == 1 { small + t.widget_gap } else { 0.0 };
            let w = if is_wide { wide } else { small };
            let mut card = Node::new().size(Size::Fixed(w), Size::Fixed(small)).offset(x, y).padding(t.widget_padding).paint(Paint::Rect { color: t.widget_fill.premultiplied(), radius: t.widget_radius });
            if t.widget_shadow {
                card = card.shadow(Self::shadow(&self.theme));
            }
            let card = self.tree.add(col, card);
            let inner = (w - 2.0 * t.widget_padding, small - 2.0 * t.widget_padding);
            match kind.as_str() {
                "date" => self.widget_date(card, &tm),
                "battery" => self.widget_battery(card, bat, sys, inner),
                "month" => self.widget_month(card, &tm, inner),
                "network" => self.widget_network(card, sys, inner),
                _ => {}
            }
            if is_wide || x_slot == 1 {
                y += small + t.widget_gap;
                x_slot = 0;
            } else {
                x_slot = 1;
            }
        }
        self.tree.mark_layout();
    }

    fn widget_date(&mut self, card: NodeId, tm: &libc::tm) {
        let t = self.theme.springboard.clone();
        let wd = DAYS[tm.tm_wday as usize % 7].to_uppercase();
        self.tree.add(card, Node::new().paint(text(&wd, t.widget_title_size, 700, t.widget_accent.premultiplied())));
        let big = t.widget_big_size * 1.45;
        self.tree.add(card, Node::new().offset(-2.0, t.widget_title_size * 1.2).paint(text(&tm.tm_mday.to_string(), big, t.widget_big_weight, t.widget_text.premultiplied())));
        let my = format!("{} {}", MONTHS[tm.tm_mon as usize % 12], 1900 + tm.tm_year);
        self.tree.add(card, Node::new().align(Align::Start, Align::End).paint(text(&my, t.widget_body_size, 400, t.widget_dim.premultiplied())));
    }

    fn widget_battery(&mut self, card: NodeId, bat: &Option<(u32, String)>, sys: Option<&Sys>, inner: (f32, f32)) {
        let t = self.theme.springboard.clone();
        let (pct, status) = bat.clone().unwrap_or((0, String::new()));
        let charge = sys.and_then(|s| s.battery.as_ref()).map(|b| b.charge).unwrap_or(Charge::Unknown);
        let icon = super::lock::battery_icon(pct as f32, charge);
        self.tree.add(card, Node::new().paint(Paint::Icon { name: icon, size: t.widget_icon }));
        let label = if bat.is_some() { format!("{pct}%") } else { "--".into() };
        self.tree.add(card, Node::new().offset(0.0, t.widget_icon as f32 + 6.0).paint(text(&label, t.widget_big_size, t.widget_big_weight, t.widget_text.premultiplied())));
        let words = match status.as_str() {
            "Charging" => "Charging",
            "Full" => "Fully charged",
            "Discharging" => "On battery",
            "" => "Battery",
            s => s,
        };
        let bar_h = 6.0;
        self.tree.add(card, Node::new().align(Align::Start, Align::End).offset(0.0, -(bar_h + 8.0)).paint(text(words, t.widget_body_size, 400, t.widget_dim.premultiplied())));
        let track = self.tree.add(card, Node::new().size(Size::Fixed(inner.0), Size::Fixed(bar_h)).align(Align::Start, Align::End).paint(Paint::Rect { color: t.widget_track.premultiplied(), radius: bar_h * 0.5 }));
        let fill = if pct <= 20 && status != "Charging" { t.widget_accent } else { t.widget_good };
        let fw = (inner.0 * pct as f32 / 100.0).max(bar_h);
        self.tree.add(track, Node::new().size(Size::Fixed(fw), Size::Fixed(bar_h)).paint(Paint::Rect { color: fill.premultiplied(), radius: bar_h * 0.5 }));
    }

    fn widget_month(&mut self, card: NodeId, tm: &libc::tm, inner: (f32, f32)) {
        let t = self.theme.springboard.clone();
        let accent = t.widget_accent.premultiplied();
        let title = MONTHS[tm.tm_mon as usize % 12].to_uppercase();
        self.tree.add(card, Node::new().paint(text(&title, t.widget_title_size, 700, accent)));
        let cols = 7.0;
        let cw = inner.0 / cols;
        let top = t.widget_title_size * 1.5;
        let size = t.widget_title_size - 1.0;
        for (i, l) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
            let c = self.tree.add(card, Node::new().size(Size::Fixed(cw), Size::Fixed(size * 1.4)).offset(i as f32 * cw, top));
            self.tree.add(c, Node::new().align(Align::Center, Align::Center).paint(text(l, size, 600, t.widget_dim.premultiplied())));
        }
        let year = 1900 + tm.tm_year;
        let days = days_in_month(year, tm.tm_mon as u32 + 1);
        // Monday-first column of the 1st.
        let first = ((tm.tm_wday - (tm.tm_mday - 1)).rem_euclid(7) + 6) % 7;
        let y0 = top + size * 1.6;
        let rows = ((first as u32 + days).div_ceil(7)).max(5) as f32;
        let rh = (inner.1 - y0) / rows;
        let d = rh.min(cw) * 0.95;
        for day in 1..=days {
            let k = first as u32 + day - 1;
            let (cx, cy) = ((k % 7) as f32 * cw, y0 + (k / 7) as f32 * rh);
            let cell = self.tree.add(card, Node::new().size(Size::Fixed(cw), Size::Fixed(rh)).offset(cx, cy));
            let today = day as i32 == tm.tm_mday;
            let color = if today {
                self.tree.add(cell, Node::new().size(Size::Fixed(d), Size::Fixed(d)).align(Align::Center, Align::Center).paint(Paint::Rect { color: accent, radius: d * 0.5 }));
                [1.0, 1.0, 1.0, 1.0]
            } else {
                t.widget_text.premultiplied()
            };
            self.tree.add(cell, Node::new().align(Align::Center, Align::Center).paint(text(&day.to_string(), size, if today { 700 } else { 500 }, color)));
        }
    }

    fn widget_network(&mut self, card: NodeId, sys: Option<&Sys>, inner: (f32, f32)) {
        let t = self.theme.springboard.clone();
        let wifi = match sys.and_then(|s| s.wifi.as_ref()) {
            Some(w) if !w.enabled => "Off".to_string(),
            Some(w) if w.link == Link::Connected => w.ssid.clone().unwrap_or_else(|| "Connected".into()),
            Some(_) => "Not connected".into(),
            None => "--".into(),
        };
        let bt = match sys.and_then(|s| s.bluetooth.as_ref()) {
            Some(b) if !b.powered => "Off".to_string(),
            Some(b) => {
                let names: Vec<&str> = b.devices.iter().filter(|d| d.connected).map(|d| d.name.as_str()).collect();
                if names.is_empty() { "On".into() } else { names.join(", ") }
            }
            None => "--".into(),
        };
        let row_h = inner.1 / 2.0;
        for (i, (icon, title, value)) in [("network-wireless", "Wi-Fi", wifi), ("network-bluetooth", "Bluetooth", bt)].into_iter().enumerate() {
            let row = self.tree.add(card, Node::new().size(Size::Fixed(inner.0), Size::Fixed(row_h)).offset(0.0, i as f32 * row_h).layout(Layout::Row { gap: 12.0 }));
            self.tree.add(row, Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: icon.into(), size: t.widget_icon + 6 }));
            let txt = self.tree.add(row, Node::new().layout(Layout::Column { gap: 2.0 }).align(Align::Start, Align::Center));
            self.tree.add(txt, Node::new().paint(text(title, t.widget_body_size, 600, t.widget_text.premultiplied())));
            let v = fit_label(&value, inner.0 - 60.0, t.widget_body_size);
            self.tree.add(txt, Node::new().paint(text(&v, t.widget_body_size, 400, t.widget_dim.premultiplied())));
        }
    }

    /// Copy animation state into the tree.
    fn apply(&mut self) {
        let p = self.search.value.clamp(0.0, 1.0);
        let sheet_on = p > 0.001;
        self.tree.set_visible(self.n.sheet, sheet_on);
        self.tree[self.n.sheet].opacity = p;
        let unit = self.theme.spacing.unit as f32;
        self.tree[self.n.field].translate = (0.0, (p - 1.0) * 8.0 * unit);
        self.tree[self.n.viewport].translate = (0.0, (1.0 - p) * 4.0 * unit);
        self.tree[self.n.results].translate = (0.0, -self.search_scroll.s.value);
        let covered = p >= 0.999;
        self.tree.set_visible(self.n.main, !covered);
        let root = self.n.root;
        self.tree[root].paint = match (covered, self.wallpaper) {
            (true, _) => Paint::None,
            (false, Some(tex)) => Paint::Backdrop { tex },
            (false, None) => Paint::Rect { color: self.background, radius: 0.0 },
        };
        // App switches: Home sits back a little while an app covers it; search pushes it back too.
        let rs = self.theme.springboard.reveal_scale;
        let r = self.reveal;
        let s = rs + (1.0 - rs) * r * r * (3.0 - 2.0 * r);
        self.tree[self.n.main].scale = s * (1.0 - 0.04 * p);
        self.tree[self.n.main].opacity = 1.0 - 0.5 * p;
        self.tree[self.n.strip].translate = (-self.scroll.s.value, 0.0);
        // Only pages that overlap the screen are drawn.
        let x0 = self.scroll.s.value;
        for (i, &node) in self.page_nodes.iter().enumerate() {
            let px = i as f32 * self.w;
            let on = px < x0 + self.w && px + self.w > x0;
            self.tree.set_visible(node, on);
        }
        let cur = ((x0 / self.w.max(1.0)).round().max(0.0)) as usize;
        let t = &self.theme.springboard;
        for (i, &d) in self.dot_nodes.iter().enumerate() {
            if let Paint::Rect { color, .. } = &mut self.tree[d].paint {
                *color = if i == cur { t.dot_active.premultiplied() } else { t.dot.premultiplied() };
            }
        }
        // Icons: press, reflow, wobble.
        let wob = if self.edit { t.jiggle } else { 0.0 };
        let w = std::f32::consts::TAU * t.jiggle_hz * self.now.as_secs_f32();
        for v in &self.icons {
            let n = &mut self.tree[v.node];
            n.scale = v.press.value;
            let (jx, jy) = if wob > 0.0 { (wob * (w + v.phase).sin(), wob * 0.7 * (w * 1.13 + v.phase).cos()) } else { (0.0, 0.0) };
            n.translate = (v.tx.value + jx, v.ty.value + jy);
        }
        self.tree.set_visible(self.n.done, self.edit && self.prompt.is_none());
        match self.drag.as_ref() {
            Some(d) => {
                self.tree.set_visible(self.n.ghost, true);
                let pe = self.plate_edge(t.icon);
                self.tree[self.n.ghost].translate = (d.at.0 - pe * 0.5, d.at.1 - pe * 0.5);
                self.tree[self.n.ghost].scale = 1.1;
            }
            None => self.tree.set_visible(self.n.ghost, false),
        }
        self.tree.set_visible(self.n.toast, self.toast_until.is_some_and(|t| self.now < t));
        self.tree.set_visible(self.n.prompt, self.prompt.is_some());
        self.tree.set_visible(self.n.backdrop, self.backdrop);
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

    // ---- actions

    fn show_toast(&mut self, msg: &str) {
        self.tree.set_text(self.n.toast_text, msg);
        self.toast_until = Some(self.now + TOAST);
    }

    fn launch(&mut self, app: usize, from: Rect) {
        let a = &self.apps[app];
        let req = Request::Launch { cmd: a.exec.clone(), app_ids: a.window_ids(), cwd: a.path.clone(), terminal: a.terminal, from: Some(from) };
        let icon = a.icon.clone().unwrap_or_else(|| super::FALLBACK_ICON.into());
        let id = a.id.clone();
        self.requests.push(req);
        let mut p = Spring::new(0.0, self.springs).with_eps(0.001);
        p.set_target(1.0);
        self.splash = Some(Splash { from, p, started: self.now, closing: false });
        if let Paint::Icon { name, .. } = &mut self.tree[self.n.splash_icon].paint {
            *name = icon;
        }
        let keep = self.theme.springboard.dock_recent;
        if keep > 0
            && let Some(b) = self.board.as_mut()
            && b.launched(&id, keep)
        {
            b.save();
            // The dock changes after the app is up (`set_apps_shown`), not under the splash.
        }
    }

    fn open_search(&mut self, open: bool) {
        self.search.set_target(if open { 1.0 } else { 0.0 });
        if open {
            self.search_focus = true;
            self.end_edit();
        } else {
            self.search_focus = false;
        }
    }

    fn search_open(&self) -> bool {
        self.search.target > 0.5
    }

    fn set_query(&mut self, q: String) {
        if q != self.query {
            self.query = q;
            self.dirty_results = true;
        }
    }

    fn begin_edit(&mut self) {
        if !self.edit {
            self.edit = true;
            self.dirty = true;
        }
    }

    fn end_edit(&mut self) {
        if self.edit {
            self.edit = false;
            self.drag = None;
            self.dirty = true;
            // The extra page is gone: stay on a real one.
            let last = self.pages.len().saturating_sub(1) as f32 * self.w;
            if self.scroll.s.target > last {
                self.scroll.to(last);
            }
        }
    }

    /// Pick up the icon in `slot` (edit mode on).
    fn start_drag(&mut self, slot: Slot, at: (f32, f32)) {
        let Some(id) = self.icons.iter().find(|v| v.slot == slot).map(|v| v.id.clone()) else { return };
        self.begin_edit();
        if let Some(&app) = self.by_id.get(&id)
            && let Paint::Icon { name, .. } = &mut self.tree[self.n.ghost_icon].paint
        {
            *name = self.apps[app].icon.clone().unwrap_or_else(|| super::FALLBACK_ICON.into());
        }
        self.drag = Some(Drag { id, from: slot, at, to: Some(slot), edge_since: None });
        // Rebuild now (badges, the extra page) so the picked-up icon's node is hidden.
        self.rebuild();
    }

    /// Where a held icon at screen point (x, y) would go.
    fn drop_slot(&self, x: f32, y: f32, from: Slot) -> Option<Slot> {
        let t = &self.theme.springboard;
        if y >= self.dock_top() - t.dots_offset * 0.5 {
            // Dock: the number of other pinned icons whose centre is left of the point.
            let i = self
                .icons
                .iter()
                .filter(|v| matches!(v.slot, Slot::Dock(_)) && v.slot != from)
                .filter(|v| {
                    let r = self.tree.screen_rect(v.node);
                    r.x + r.w * 0.5 < x
                })
                .count();
            return Some(Slot::Dock(i));
        }
        let (cols, rows) = self.grid();
        let (cw, rh) = self.cell();
        let page = self.current_page();
        let c = ((x - self.grid_x0()) / cw).floor().clamp(0.0, cols as f32 - 1.0) as usize;
        let r = ((y - t.grid_top) / rh).floor().clamp(0.0, rows as f32 - 1.0) as usize;
        let len = self.pages.get(page).map_or(0, |p| p.len()).saturating_sub(usize::from(matches!(from, Slot::Page(p, _) if p == page)));
        Some(Slot::Page(page, (r * cols + c).min(len)))
    }

    /// Move the other page icons to where they would be with the held icon dropped at `to`.
    fn reflow(&mut self) {
        let Some(d) = self.drag.as_ref() else { return };
        let per_page = self.per_page();
        let v = preview(&self.pages, d.from, d.to.filter(|s| matches!(s, Slot::Page(..))), per_page);
        let mut place: HashMap<&str, (usize, usize)> = HashMap::new();
        for (p, page) in v.iter().enumerate() {
            for (i, id) in page.iter().enumerate() {
                if !id.is_empty() {
                    place.insert(id.as_str(), (p, i));
                }
            }
        }
        let mut moves = Vec::new();
        for (k, icon) in self.icons.iter().enumerate() {
            let Slot::Page(p, i) = icon.slot else { continue };
            let Some(&(np, ni)) = place.get(icon.id.as_str()) else { continue };
            let (bx, by) = self.slot_pos(i);
            let (nx, ny) = self.slot_pos(ni);
            moves.push((k, (np as f32 - p as f32) * self.w + nx - bx, ny - by));
        }
        for (k, dx, dy) in moves {
            self.icons[k].tx.set_target(dx);
            self.icons[k].ty.set_target(dy);
        }
    }

    fn drop_held(&mut self) {
        let Some(d) = self.drag.take() else { return };
        let t = self.theme.springboard.clone();
        let Some(to) = d.to else {
            self.dirty = true;
            return;
        };
        if matches!(to, Slot::Dock(_)) && !matches!(d.from, Slot::Dock(_)) && self.dock.len() >= t.dock_max {
            self.show_toast(&t.label_dock_full);
            self.dirty = true;
            return;
        }
        let per_page = self.per_page();
        let (mut pages, mut dock) = (self.pages.clone(), self.dock.clone());
        // The extra (empty) edit page is a real target.
        while pages.len() < self.page_count() {
            pages.push(Vec::new());
        }
        if board::move_icon(&mut pages, &mut dock, d.from, to, per_page, t.dock_max) {
            self.pages = pages;
            self.dock = dock;
            self.commit();
        }
        self.rebuild();
        // The dropped icon glides from the finger into its slot.
        let pe = self.plate_edge(t.icon);
        let page_x = -self.scroll.s.target;
        if let Some(i) = self.icons.iter().position(|v| v.id == d.id && matches!(v.slot, Slot::Page(..))) {
            let Slot::Page(p, s) = self.icons[i].slot else { return };
            let (sx, sy) = self.slot_pos(s);
            let (cw, rh) = self.cell();
            let label_h = t.label_gap + t.label_size * 1.35;
            let cx = page_x + p as f32 * self.w + sx + cw * 0.5;
            let cy = sy + ((rh - pe - label_h) * 0.5).max(0.0) + pe * 0.5;
            let v = &mut self.icons[i];
            v.tx.set(d.at.0 - cx, 0.0);
            v.ty.set(d.at.1 - cy, 0.0);
            v.tx.set_target(0.0);
            v.ty.set_target(0.0);
        }
    }

    fn hide_app(&mut self, id: &str) {
        let Some(b) = self.board.as_mut() else { return };
        b.hide(id);
        b.save();
        self.refresh();
    }

    fn tap(&mut self, target: Target) {
        match target {
            Target::Icon(i) => {
                if self.edit {
                    return;
                }
                let Some(v) = self.icons.get(i) else { return };
                let Some(&app) = self.by_id.get(&v.id) else { return };
                let from = self.tree.screen_rect(v.node);
                self.launch(app, from);
            }
            Target::Recent(i) => {
                let Some((node, id)) = self.recent_nodes.get(i).cloned() else { return };
                if self.edit {
                    return;
                }
                if let Some(&app) = self.by_id.get(&id) {
                    let from = self.tree.screen_rect(node);
                    self.launch(app, from);
                }
            }
            Target::Badge(i) => {
                if let Some(id) = self.icons.get(i).map(|v| v.id.clone()) {
                    self.hide_app(&id);
                }
            }
            Target::Dot(i) => self.scroll.to(i as f32 * self.w),
            Target::Done => self.end_edit(),
            Target::Result(i) => {
                if let Some(r) = self.results.get(i) {
                    let from = self.tree.screen_rect(r.node);
                    self.launch(r.app, from);
                }
            }
            Target::Field => self.search_focus = true,
            // A tap on empty space closes search (as tapping outside it).
            Target::SheetHeader | Target::Results => self.open_search(false),
            Target::PromptWait | Target::PromptKill => {
                if let Some((id, _)) = self.prompt.take() {
                    self.requests.push(Request::ForceQuit { id, kill: target == Target::PromptKill });
                }
                self.apply();
            }
            Target::Prompt => {}
            Target::Background => self.end_edit(),
        }
    }

    fn long_press(&mut self, target: Target, at: (f32, f32)) -> bool {
        match target {
            Target::Icon(i) => {
                let Some(slot) = self.icons.get(i).map(|v| v.slot) else { return false };
                if let Some(v) = self.icons.get_mut(i) {
                    v.press.set_target(1.0);
                }
                self.start_drag(slot, at);
                true
            }
            Target::Result(i) => {
                let Some(app) = self.results.get(i).map(|r| r.app) else { return false };
                let id = self.apps[app].id.clone();
                let per_page = self.per_page();
                let msg = self.theme.springboard.label_added.clone();
                if let Some(b) = self.board.as_mut()
                    && b.unhide(&id, per_page)
                {
                    b.save();
                    self.refresh();
                    self.show_toast(&msg);
                }
                true
            }
            Target::Background if !self.search_open() => {
                self.begin_edit();
                true
            }
            _ => false,
        }
    }

    fn results_top(&self) -> f32 {
        let t = &self.theme.springboard;
        t.status_height + 4.0 * self.theme.spacing.unit as f32 + t.search_height
    }

    fn target_at(&self, x: f32, y: f32) -> Target {
        let hit = self.tree.hit_test(x, y);
        let pb = &self.n.prompt_buttons;
        match hit {
            Some(h) if h == pb[0] => return Target::PromptWait,
            Some(h) if h == pb[1] => return Target::PromptKill,
            Some(h) if h == self.n.prompt => return Target::Prompt,
            _ => {}
        }
        if self.search_open() {
            if y < self.results_top() {
                return if hit == Some(self.n.field) { Target::Field } else { Target::SheetHeader };
            }
            return match hit.and_then(|h| self.results.iter().position(|r| r.node == h)) {
                Some(i) => Target::Result(i),
                None => Target::Results,
            };
        }
        let Some(hit) = hit else { return Target::Background };
        if hit == self.n.done {
            return Target::Done;
        }
        if let Some(i) = self.icons.iter().position(|v| v.badge == Some(hit)) {
            return Target::Badge(i);
        }
        if let Some(i) = self.icons.iter().position(|v| v.node == hit) {
            return Target::Icon(i);
        }
        if let Some(i) = self.recent_nodes.iter().position(|(n, _)| *n == hit) {
            return Target::Recent(i);
        }
        if let Some(i) = self.dot_nodes.iter().position(|&n| n == hit) {
            return Target::Dot(i);
        }
        Target::Background
    }

    fn press(&mut self, target: Target, down: bool) {
        if let Target::Icon(i) = target
            && !self.edit
            && let Some(v) = self.icons.get_mut(i)
        {
            v.press.set_target(if down { self.theme.springboard.pressed_scale } else { 1.0 });
        }
    }

    fn on_screen(&self, r: Rect) -> Option<Rect> {
        (r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= self.w && r.y + r.h <= self.h && r.w > 1.0).then_some(r)
    }

    fn release_pages(&mut self, vx: f32, from_page: usize) {
        let w = self.w.max(1.0);
        let last = self.page_count().saturating_sub(1);
        let proj = (self.scroll.s.value - vx * self.theme.springboard.flick_time) / w;
        let page = (proj.round().max(0.0) as usize).clamp(from_page.saturating_sub(1), (from_page + 1).min(last));
        self.scroll.s.velocity = -vx;
        self.scroll.to(page as f32 * w);
    }

    /// Edit mode: a held icon at the left/right edge flips the page after a short wait.
    fn edge_flip(&mut self, now: Duration) {
        let ms = Duration::from_millis(self.theme.springboard.edge_flip_ms as u64);
        let (w, n) = (self.w, self.page_count());
        let cur = self.current_page();
        let Some(d) = self.drag.as_mut() else { return };
        let dir = if d.at.0 < EDGE { -1 } else if d.at.0 > w - EDGE { 1 } else { 0 };
        if dir == 0 || d.at.1 >= self.h - self.theme.springboard.dock_height - self.theme.springboard.dock_margin {
            d.edge_since = None;
            return;
        }
        let since = *d.edge_since.get_or_insert(now);
        if now.saturating_sub(since) < ms {
            return;
        }
        d.edge_since = Some(now);
        let next = cur as i64 + dir;
        if next >= 0 && (next as usize) < n {
            self.scroll.to(next as f32 * w);
            let (x, y, from) = (d.at.0, d.at.1, d.from);
            let to = self.drop_slot(x, y, from).map(|s| match s {
                Slot::Page(_, i) => {
                    let len = self.pages.get(next as usize).map_or(0, |p| p.len());
                    Slot::Page(next as usize, i.min(len))
                }
                s => s,
            });
            if let Some(d) = self.drag.as_mut() {
                d.to = to;
            }
            self.reflow();
        }
    }
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn tm_now() -> libc::tm {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// What the status bar and widgets show from the services: they are rebuilt only when this changes.
fn sys_key(sys: Option<&Sys>, bat: &Option<(u32, String)>) -> String {
    let wifi = sys.and_then(|s| s.wifi.as_ref()).map(|w| (w.enabled, w.link == Link::Connected, w.ssid.clone(), (w.strength as f32 / 25.0).ceil() as u8));
    let bt = sys.and_then(|s| s.bluetooth.as_ref()).map(|b| (b.powered, b.devices.iter().filter(|d| d.connected).map(|d| d.name.clone()).collect::<Vec<_>>()));
    format!("{bat:?}{wifi:?}{bt:?}")
}

/// A colour token that may be empty (= `default`).
fn theme_color(s: &str, default: Rgba) -> Rgba {
    crate::theme::Color::parse(s).map(|c| c.premultiplied()).unwrap_or(default)
}

impl Scene for Springboard {
    fn tree(&mut self) -> &mut Tree {
        &mut self.tree
    }

    fn update(&mut self, now: Duration, dt: f32) -> bool {
        self.now = now;
        if self.next_minute.is_none_or(|t| now >= t) {
            self.update_clock();
        }
        if self.dirty {
            self.rebuild();
        }
        if self.dirty_results && self.search.value > 0.001 {
            self.rebuild_results();
        }
        let long = Duration::from_millis(self.theme.springboard.long_press_ms as u64);
        if let Some(g) = self.gesture.as_ref()
            && g.mode == Mode::Pending
            && !g.long_fired
            && now.saturating_sub(g.t0) >= long
        {
            let (target, at) = (g.target, g.vel.last().unwrap_or(g.start));
            if self.long_press(target, at) {
                let g = self.gesture.as_mut().unwrap();
                g.long_fired = true;
                if self.drag.is_some() {
                    g.mode = Mode::Drag;
                }
            }
        }
        if self.drag.is_some() {
            self.edge_flip(now);
        }
        if let Some(s) = self.splash.as_mut()
            && !s.closing
            && now.saturating_sub(s.started) > SPLASH_TIMEOUT
        {
            s.closing = true;
            s.p.set_target(0.0);
        }
        let mut moving = false;
        moving |= self.scroll.s.step(dt);
        moving |= self.search_scroll.s.step(dt);
        moving |= self.search.step(dt);
        for v in &mut self.icons {
            moving |= v.press.step(dt);
            moving |= v.tx.step(dt);
            moving |= v.ty.step(dt);
        }
        if let Some(s) = self.splash.as_mut() {
            moving |= s.p.step(dt);
            if s.closing && s.p.is_resting() {
                self.splash = None;
            }
        }
        if self.search.is_resting() && self.search.value < 0.001 && !self.query.is_empty() {
            self.set_query(String::new());
        }
        self.apply();
        let toast = self.toast_until.is_some_and(|t| now < t);
        let wobble = self.edit && self.theme.springboard.jiggle > 0.0;
        moving || wobble || toast || self.gesture.as_ref().is_some_and(|g| g.mode == Mode::Pending && !g.long_fired)
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        match ev.phase {
            Phase::Down => {
                if self.gesture.is_some() {
                    return true;
                }
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
                let page = self.current_page();
                self.gesture = Some(Gesture { id: ev.id, start: (ev.x, ev.y), t0: ev.time, target, mode: Mode::Pending, vel, origin: 0.0, page, long_fired: false });
                true
            }
            Phase::Motion => {
                let open = self.search_open();
                let edit = self.edit;
                let pull = self.theme.springboard.search_pull.max(1.0) * 2.0;
                // Results that cannot scroll further up: a swipe up closes search instead.
                let at_end = self.search_scroll.s.value >= self.search_scroll.max - 1.0;
                let Some(g) = self.gesture.as_mut().filter(|g| g.id == ev.id) else { return false };
                g.vel.add(ev.time, ev.x, ev.y);
                let (dx, dy) = (ev.x - g.start.0, ev.y - g.start.1);
                let (vx, vy) = g.vel.velocity();
                let mut pick_up = None;
                if g.mode == Mode::Pending && dx.abs().max(dy.abs()) > SLOP {
                    let target = g.target;
                    g.mode = match target {
                        _ if g.long_fired => Mode::Done,
                        Target::Icon(_) if edit => Mode::Drag,
                        Target::Result(_) | Target::Results if open => match dy.abs() > dx.abs() {
                            true if dy < 0.0 && at_end => Mode::Push,
                            true => Mode::Results,
                            false => Mode::Done,
                        },
                        Target::SheetHeader | Target::Field if open && dy < 0.0 => Mode::Push,
                        _ if open => Mode::Done,
                        _ if dx.abs() > dy.abs() => Mode::Pages,
                        _ if dy > 0.0 && !edit => Mode::Pull,
                        _ => Mode::Done,
                    };
                    match g.mode {
                        Mode::Pages => g.origin = self.scroll.unband() + dx,
                        Mode::Results => g.origin = self.search_scroll.unband() + dy,
                        Mode::Pull | Mode::Push => g.origin = self.search.value,
                        Mode::Drag => {
                            if let Target::Icon(i) = target {
                                pick_up = self.icons.get(i).map(|v| v.slot);
                            }
                        }
                        _ => {}
                    }
                    if g.mode != Mode::Drag {
                        self.press(target, false);
                    }
                }
                if let Some(slot) = pick_up {
                    self.start_drag(slot, (ev.x, ev.y));
                }
                let Some(g) = self.gesture.as_ref() else { return true };
                match g.mode {
                    Mode::Pages => self.scroll.drag(g.origin - dx, -vx),
                    Mode::Results => self.search_scroll.drag(g.origin - dy, -vy),
                    Mode::Pull | Mode::Push => {
                        let p = (g.origin + dy / pull).clamp(0.0, 1.0);
                        self.search.set(p, vy / pull);
                        if p > 0.0 {
                            self.dirty_results |= self.results.is_empty();
                        }
                    }
                    Mode::Drag => {
                        let from = self.drag.as_ref().map(|d| d.from);
                        if let Some(from) = from {
                            let to = self.drop_slot(ev.x, ev.y, from);
                            let changed = self.drag.as_ref().is_some_and(|d| d.to != to);
                            if let Some(d) = self.drag.as_mut() {
                                d.at = (ev.x, ev.y);
                                d.to = to;
                            }
                            if changed {
                                self.reflow();
                            }
                        }
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
                let pull = self.theme.springboard.search_pull;
                match g.mode {
                    Mode::Pending if !g.long_fired && !cancel => self.tap(g.target),
                    Mode::Pending if g.long_fired && self.drag.is_some() => {
                        // Long-pressed and let go without moving: edit mode stays, the icon goes back.
                        self.drag = None;
                        self.dirty = true;
                    }
                    Mode::Pages => self.release_pages(vx, g.page),
                    Mode::Results => self.search_scroll.fling(-vy),
                    Mode::Pull => {
                        let dy = g.vel.last().map_or(0.0, |l| l.1 - g.start.1);
                        self.open_search(!cancel && (dy > pull || vy > FLICK) && vy > -FLICK);
                    }
                    Mode::Push => {
                        let dy = g.vel.last().map_or(0.0, |l| l.1 - g.start.1);
                        self.open_search(cancel || !(dy < -pull || vy < -FLICK));
                    }
                    Mode::Drag => {
                        if cancel {
                            self.drag = None;
                            self.dirty = true;
                        } else {
                            self.drop_held();
                        }
                    }
                    _ => {}
                }
                true
            }
        }
    }

    fn axis(&mut self, _x: f32, _y: f32, dx: f32, dy: f32) -> bool {
        if self.search_open() {
            self.search_scroll.nudge((dx + dy) * 3.0);
        } else {
            let d = dx + dy;
            if d.abs() > 0.5 {
                let next = (self.current_page() as i64 + d.signum() as i64).clamp(0, self.page_count() as i64 - 1);
                self.scroll.to(next as f32 * self.w);
            }
        }
        true
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if key.ctrl || key.alt {
            return false;
        }
        match key.sym {
            k::KEY_Escape => {
                if !self.query.is_empty() {
                    self.set_query(String::new());
                } else if self.search_open() {
                    self.open_search(false);
                } else if self.edit {
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
                if self.search_open() && !self.query.is_empty() {
                    if self.dirty_results {
                        self.rebuild_results();
                    }
                    if let Some(r) = self.results.first() {
                        let from = self.tree.screen_rect(r.node);
                        self.launch(r.app, from);
                    }
                }
                true
            }
            k::KEY_Left | k::KEY_Right if !self.search_open() => {
                let d = if key.sym == k::KEY_Left { -1 } else { 1 };
                let next = (self.current_page() as i64 + d).clamp(0, self.page_count() as i64 - 1);
                self.scroll.to(next as f32 * self.w);
                true
            }
            _ => match &key.text {
                Some(t) => {
                    if self.query.is_empty() && t.trim().is_empty() {
                        return true;
                    }
                    if !self.search_open() {
                        self.open_search(true);
                    }
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
        let mut at = |x: Duration| t = Some(t.map_or(x, |y: Duration| y.min(x)));
        if let Some(g) = self.gesture.as_ref().filter(|g| g.mode == Mode::Pending && !g.long_fired) {
            at(g.t0 + Duration::from_millis(self.theme.springboard.long_press_ms as u64));
        }
        if let Some(s) = self.splash.as_ref().filter(|s| !s.closing) {
            at(s.started + SPLASH_TIMEOUT);
        }
        if let Some(x) = self.toast_until {
            at(x);
        }
        if let Some(since) = self.drag.as_ref().and_then(|d| d.edge_since) {
            at(since + Duration::from_millis(self.theme.springboard.edge_flip_ms as u64));
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
        let page = self.current_page();
        self.w = w;
        self.h = h;
        self.refresh();
        self.dirty = true;
        self.dirty_results = true;
        self.rebuild();
        self.scroll.s.set(page.min(self.page_count().saturating_sub(1)) as f32 * w, 0.0);
        self.scroll.to(self.scroll.s.value);
        self.apply();
    }

    fn poll(&mut self) -> bool {
        let mut battery = false;
        if let Some(s) = self.services.as_ref()
            && s.generation() != self.sys_gen
        {
            self.sys_gen = s.generation();
            let key = sys_key(Some(&s.snapshot().1), &upower_battery(s));
            if key != self.sys_key {
                self.update_clock();
                battery = true;
            }
        }
        let Some(apps) = self.incoming.lock().unwrap().take() else { return battery };
        self.apps = apps;
        self.by_id = self.apps.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
        let per_page = self.per_page();
        match self.board.as_mut() {
            Some(b) => {
                b.reconcile(&self.apps, per_page);
            }
            None => self.board = Some(Board::defaults(&self.apps, per_page)),
        }
        // A drag in progress refers to the old layout.
        self.drag = None;
        self.gesture = None;
        self.refresh();
        self.dirty_results = true;
        true
    }

    fn set_apps_shown(&mut self, shown: bool) {
        self.apps_shown = shown;
        if shown {
            self.splash = None;
            self.gesture = None;
            self.drag = None;
            self.edit = false;
            self.search.set(0.0, 0.0);
            self.search_focus = false;
            if !self.query.is_empty() {
                self.query.clear();
                self.dirty_results = true;
            }
            // Recent apps changed with the launch: rebuild now, while the app covers Home.
            self.refresh();
            self.rebuild();
            let last = self.pages.len().saturating_sub(1) as f32 * self.w;
            if self.scroll.s.value > last {
                self.scroll.s.set(last, 0.0);
                self.scroll.to(last);
            }
        } else {
            self.update_clock();
        }
        self.apply();
    }

    fn visible(&self) -> bool {
        !self.apps_shown
    }

    fn app_rect(&self, app_id: &str) -> Option<Rect> {
        if self.search.value > 0.001 {
            return None;
        }
        let matches = |id: &str| self.by_id.get(id).is_some_and(|&a| self.apps[a].window_ids().iter().any(|x| x == app_id));
        let cur = self.current_page();
        let node = self
            .icons
            .iter()
            .filter(|v| match v.slot {
                Slot::Page(p, _) => p == cur,
                Slot::Dock(_) => true,
            })
            .find(|v| matches(&v.id))
            .map(|v| v.node)
            .or_else(|| self.recent_nodes.iter().find(|(_, id)| matches(id)).map(|(n, _)| *n))?;
        self.on_screen(self.tree.screen_rect(node))
    }

    fn set_reveal(&mut self, r: f32) {
        if r < 0.999 {
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

    fn dismiss_prompt(&mut self, id: u64) {
        if self.prompt.as_ref().is_some_and(|(p, _)| *p == id) {
            self.prompt = None;
            self.apply();
        }
    }

    fn show_backdrop(&mut self, on: bool) -> bool {
        self.backdrop = on;
        self.apply();
        true
    }

    fn text_field(&self) -> bool {
        self.search_focus && self.search_open()
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        let t = &theme.springboard;
        self.springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        self.scroll.s.params = SpringParams { stiffness: t.page_stiffness, damping: 1.0 };
        self.scroll.rubber = t.rubber_band;
        self.scroll.flick = t.flick_time;
        self.search_scroll.s.params = SpringParams { stiffness: theme.home.scroll_stiffness, damping: 1.0 };
        self.search.params = self.springs;
        self.background = theme.colors.background.premultiplied();
        let (tree, n) = Self::build(theme, self.background);
        self.tree = tree;
        self.n = n;
        self.icons.clear();
        self.results.clear();
        self.page_nodes.clear();
        // Node ids of the old tree (the widget column is recreated by the rebuild).
        self.today = None;
        self.dot_nodes.clear();
        self.recent_nodes.clear();
        self.gesture = None;
        self.drag = None;
        self.splash = None;
        self.refresh();
        self.dirty = true;
        self.dirty_results = true;
        self.update_clock();
        self.apply();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_makes_room() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let pages = vec![s(&["a", "b", "c"]), s(&["d"])];
        // c lifted, held before a: a and b move right.
        let v = preview(&pages, Slot::Page(0, 2), Some(Slot::Page(0, 0)), 3);
        assert_eq!(v, vec![s(&["", "a", "b"]), s(&["d"])]);
        // From the dock into a full page: the last icon ripples to the next page.
        let v = preview(&pages, Slot::Dock(0), Some(Slot::Page(0, 1)), 3);
        assert_eq!(v, vec![s(&["a", "", "b"]), s(&["c", "d"])]);
        // Held over the dock: the page gap closes.
        let v = preview(&pages, Slot::Page(0, 0), None, 3);
        assert_eq!(v, vec![s(&["b", "c"]), s(&["d"])]);
    }

    #[test]
    fn labels_fit() {
        assert_eq!(fit_label("Foot", 100.0, 13.0), "Foot");
        let l = fit_label("A very long application name indeed", 100.0, 13.0);
        assert!(l.ends_with('…') && l.chars().count() <= 13);
    }
}
