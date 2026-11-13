//! On-screen keyboard (DESKTOP-SHELL.md Phase 6): a UI scene drawn as its own top layer above apps and the
//! Control Center (`ShellUi`'s keyboard layer). Default look: the Windows 8/RT touch keyboard (full width,
//! flush with the bottom, flat keys); every look value is an `[osk]` theme token, the layout is data
//! (`osk_layout.rs`).
//!
//! It only produces `KeyEvent`s (`take_keys`); `ShellUi` sends them to the shell's own text field (Wi-Fi
//! password, Home search) or, as `Request::OskKey`, to the focused app. Nothing typed is logged.
//!
//! Touch: a key types on release (the finger may slide to a neighbour first); a second finger going down
//! types the first finger's key at once (fast two-thumb typing); Backspace and the arrows type on press and
//! repeat while held; a long press on a letter opens a strip of accented variants (slide, release).
//! Shift: once, or twice quickly for Caps lock; Ctrl and Alt latch the same way.

use std::time::Duration;

use smithay::input::keyboard::{keysyms as k, xkb};
use tracing::info;

use super::{
    Align, Node, NodeId, Paint, Shadow, Size, Tree,
    osk_layout::{self as lay, Act, Page, Row},
    shell::Scene,
};
use crate::{
    anim::{Spring, SpringParams},
    overlay::{CANCEL_ALL, KeyEvent, Phase, PointerEvent, TextPurpose},
    render::{
        batch::{Rect, Rgba},
        text::TextStyle,
    },
    theme::{OskTokens, Theme},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Latch {
    Off,
    Once,
    Locked,
}

struct KeyView {
    act: Act,
    /// Keyboard-local (origin at the keyboard's top-left).
    rect: Rect,
    row: usize,
    coding: bool,
    node: NodeId,
    label: NodeId,
}

struct Touch {
    id: i32,
    key: Option<usize>,
    /// When the finger reached this key (long press counts from here).
    since: Duration,
    /// Typed already (repeating keys type on press).
    fired: bool,
    repeat_at: Option<Duration>,
    /// The long press was checked for this key (a strip opened, or there are no variants).
    long_done: bool,
}

struct Strip {
    touch: i32,
    items: Vec<String>,
    /// Keyboard-local cell rectangles.
    cells: Vec<Rect>,
    nodes: Vec<NodeId>,
    bg: NodeId,
    sel: usize,
}

pub struct Keyboard {
    theme: Theme,
    springs: SpringParams,
    tree: Tree,
    panel: NodeId,
    preview: NodeId,
    preview_label: NodeId,
    /// The key the preview shows (None = hidden).
    preview_for: Option<usize>,
    keys: Vec<KeyView>,
    /// Row extents (keyboard-local top, bottom).
    rows: Vec<(f32, f32)>,
    w: f32,
    h: f32,
    kb_h: f32,
    page: Page,
    shift: Latch,
    ctrl: Latch,
    alt: Latch,
    /// The last tap on a latching key (for double taps).
    last_tap: Option<(Act, Duration)>,
    purpose: TextPurpose,
    want: bool,
    /// 0 = hidden below the screen, 1 = up.
    show: Spring,
    warming: bool,
    touches: Vec<Touch>,
    strip: Option<Strip>,
    out: Vec<KeyEvent>,
    hide_asked: bool,
    /// The small "show keyboard" handle (a field wants text, the keyboard was hidden by hand).
    handle_on: bool,
    handle: NodeId,
    handle_touch: Option<i32>,
    show_asked: bool,
    now: Duration,
    dirty: bool,
    /// First xkb layout name (picks the letter page in `osk_layout::rows`).
    layout: String,
}

fn text_paint(s: &str, size: f32, weight: u16, mono: bool, color: Rgba) -> Paint {
    Paint::Text { text: s.into(), style: TextStyle { size, weight, mono, ..TextStyle::default() }, color, wrap: false, layout: None }
}

fn set_rect_color(tree: &mut Tree, id: NodeId, c: Rgba) {
    if let Paint::Rect { color, .. } = &mut tree[id].paint {
        *color = c;
    }
}

/// Keyboard-local key rectangles for `rows` across `width`, and the keyboard's height.
fn geometry(rows: &[Row], width: f32, t: &OskTokens, key_h: f32) -> (Vec<Vec<Rect>>, f32) {
    let kb_w = if t.max_width > 0.0 { width.min(t.max_width) } else { width };
    let x0 = (width - kb_w) * 0.5 + t.padding;
    let avail = kb_w - 2.0 * t.padding;
    let mut y = t.padding;
    let mut out = Vec::new();
    for row in rows {
        let h = if row.coding { t.coding_height } else { key_h };
        let n = row.keys.len() as f32;
        let space = avail - t.gap * (n - 1.0).max(0.0);
        let widths: Vec<f32> = if row.coding {
            vec![space / n; row.keys.len()]
        } else {
            let unit = space / lay::ROW_UNITS;
            let fixed: f32 = row.keys.iter().map(|k| k.w).sum();
            row.keys.iter().map(|k| if k.w == lay::GROW { (lay::ROW_UNITS - fixed) * unit } else { k.w * unit }).collect()
        };
        let mut x = x0;
        out.push(
            widths
                .iter()
                .map(|&w| {
                    let r = Rect::new(x.round(), y, (x + w).round() - x.round(), h);
                    x += w + t.gap;
                    r
                })
                .collect(),
        );
        y += h + t.gap;
    }
    (out, y - t.gap + t.padding)
}

impl Keyboard {
    pub fn new(theme: &Theme) -> Keyboard {
        let springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        let mut kb = Keyboard {
            theme: theme.clone(),
            springs,
            tree: Tree::new(Node::new()),
            panel: 0,
            preview: 0,
            preview_label: 0,
            preview_for: None,
            keys: Vec::new(),
            rows: Vec::new(),
            w: 1366.0,
            h: 768.0,
            kb_h: 0.0,
            page: Page::Letters,
            shift: Latch::Off,
            ctrl: Latch::Off,
            alt: Latch::Off,
            last_tap: None,
            purpose: TextPurpose::Text,
            want: false,
            show: Spring::new(0.0, springs).with_eps(0.002),
            warming: false,
            touches: Vec::new(),
            strip: None,
            out: Vec::new(),
            hide_asked: false,
            handle_on: false,
            handle: 0,
            handle_touch: None,
            show_asked: false,
            now: Duration::ZERO,
            dirty: true,
            layout: "us".into(),
        };
        kb.build();
        kb
    }

    /// The xkb layout name of the hardware keyboard (`[keyboard] layout`, resolved): the on-screen keys
    /// follow it when `osk_layout` has that layout, else US English.
    pub fn with_layout(mut self, xkb: &str) -> Keyboard {
        self.set_layout(xkb);
        self
    }

    /// The hardware layout changed at runtime (Settings).
    pub fn set_layout(&mut self, xkb: &str) {
        self.layout = xkb.split(',').next().unwrap_or("us").trim().to_string();
        if !lay::LAYOUTS.contains(&self.layout.as_str()) {
            tracing::info!(layout = %self.layout, "no on-screen layout for this keyboard layout; using us");
        }
        self.build();
    }

    fn portrait(&self) -> bool {
        self.h > self.w
    }

    /// Rebuild the tree for the current page, size, shift state and theme.
    fn build(&mut self) {
        self.close_strip();
        let t = self.theme.osk.clone();
        let rows = lay::rows(&self.layout, self.page, t.coding_row);
        let key_h = if self.portrait() { t.key_height_portrait } else { t.key_height };
        let (rects, kb_h) = geometry(&rows, self.w, &t, key_h);
        self.kb_h = kb_h;
        let mut tree = Tree::new(Node::new());
        let panel = tree.add(
            0,
            Node::new()
                .size(Size::Fixed(self.w), Size::Fixed(kb_h))
                .align(Align::Start, Align::End)
                .paint(Paint::Rect { color: t.fill.premultiplied(), radius: 0.0 }),
        );
        self.keys.clear();
        self.rows.clear();
        for (ri, (row, rr)) in rows.iter().zip(&rects).enumerate() {
            if let Some(r) = rr.first() {
                self.rows.push((r.y, r.y + r.h));
            }
            for (key, r) in row.keys.iter().zip(rr) {
                let node = tree.add(
                    panel,
                    Node::new()
                        .size(Size::Fixed(r.w), Size::Fixed(r.h))
                        .offset(r.x, r.y)
                        .paint(Paint::Rect { color: t.key.premultiplied(), radius: t.radius }),
                );
                let fn_look = key.act.is_fn() || row.coding;
                let (s, mono) = self.label(key.act);
                let (size, color) = if fn_look { (t.fn_label_size, t.text_fn) } else { (t.label_size, t.text) };
                let size = if mono { t.glyph_size } else { size };
                let label = tree.add(node, Node::new().align(Align::Center, Align::Center).paint(text_paint(&s, size, t.label_weight, mono, color.premultiplied())));
                self.keys.push(KeyView { act: key.act, rect: *r, row: ri, coding: row.coding, node, label });
            }
        }
        let mut preview = Node::new().paint(Paint::Rect { color: t.preview_fill.premultiplied(), radius: t.preview_radius });
        if t.shadow {
            let s = &self.theme.shadow;
            preview = preview.shadow(Shadow { sigma: s.small_sigma, dx: 0.0, dy: s.small_offset_y, color: s.color.premultiplied() });
        }
        let preview = tree.add(panel, preview);
        let preview_label =
            tree.add(preview, Node::new().align(Align::Center, Align::Center).paint(text_paint("", t.preview_size, t.label_weight, false, t.preview_text.premultiplied())));
        tree[preview].visible = false;
        // The "show keyboard" handle: one coding-row key in the bottom-right corner.
        let hs = t.coding_height;
        let handle = tree.add(
            0,
            Node::new()
                .size(Size::Fixed(hs * 1.5), Size::Fixed(hs))
                .align(Align::End, Align::End)
                .offset(-t.padding, -t.padding)
                .paint(Paint::Rect { color: t.key_fn.premultiplied(), radius: t.radius }),
        );
        tree.add(handle, Node::new().align(Align::Center, Align::Center).paint(text_paint(&t.label_hide, t.glyph_size, t.label_weight, true, t.text_fn.premultiplied())));
        tree[handle].visible = self.handle_on;
        self.handle = handle;
        self.tree = tree;
        self.panel = panel;
        self.preview = preview;
        self.preview_label = preview_label;
        self.preview_for = None;
        // Keys under fingers may have moved: let go of them (no key types).
        self.touches.clear();
        self.dirty = false;
        self.apply();
    }

    /// A key's label and whether it is drawn in the monospace face (symbol glyphs such as ⌫ ⇧ ←).
    fn label(&self, act: Act) -> (String, bool) {
        let t = &self.theme.osk;
        let s = match act {
            Act::Letter(c) => return (if self.shifted() { c.to_uppercase().collect() } else { c.to_string() }, false),
            Act::Sym(a, b) => return ((if self.shifted() && self.page == Page::Letters { b } else { a }).to_string(), false),
            Act::Space => String::new(),
            Act::Back => t.label_backspace.clone(),
            Act::Enter => t.label_enter.clone(),
            Act::Shift => t.label_shift.clone(),
            Act::Tab => "Tab".into(),
            Act::Esc => "Esc".into(),
            Act::Ctrl => "Ctrl".into(),
            Act::Alt => "Alt".into(),
            Act::Left => t.label_left.clone(),
            Act::Right => t.label_right.clone(),
            Act::Up => t.label_up.clone(),
            Act::Down => t.label_down.clone(),
            Act::Home => "Home".into(),
            Act::End => "End".into(),
            Act::Page(Page::Letters) => t.label_letters.clone(),
            Act::Page(_) if self.page == Page::Letters => t.label_symbols.clone(),
            Act::Page(Page::Symbols2) => "1/2".into(),
            Act::Page(Page::Symbols1) => "2/2".into(),
            Act::Hide => t.label_hide.clone(),
        };
        let mono = !s.is_ascii();
        (s, mono)
    }

    fn shifted(&self) -> bool {
        self.shift != Latch::Off
    }

    // ---- what ShellUi asks

    /// Show (with the field's purpose) or hide. True if anything changed.
    pub fn set_wanted(&mut self, want: bool, purpose: TextPurpose) -> bool {
        let mut changed = false;
        if want && (!self.want || purpose != self.purpose) {
            // A new field: start fresh on the page that suits it (the tree is rebuilt only if that differs).
            self.purpose = purpose;
            let page = if purpose == TextPurpose::Number { Page::Symbols1 } else { Page::Letters };
            if page != self.page || self.shift != Latch::Off {
                self.page = page;
                self.shift = Latch::Off;
                self.dirty = true;
            }
            self.ctrl = Latch::Off;
            self.alt = Latch::Off;
            changed = true;
        }
        if want != self.want {
            info!(show = want, purpose = ?purpose, "osk");
            self.want = want;
            self.show.set_target(if want { 1.0 } else { 0.0 });
            if !want {
                self.touches.clear();
                self.close_strip();
            }
            changed = true;
        }
        if self.dirty {
            self.build();
        }
        if changed {
            self.apply();
        }
        changed
    }

    /// Show the small handle that brings the keyboard back (while hidden).
    pub fn set_handle(&mut self, on: bool) -> bool {
        if on == self.handle_on {
            return false;
        }
        self.handle_on = on;
        self.handle_touch = None;
        self.tree.set_visible(self.handle, on);
        true
    }

    /// The handle was tapped since the last call.
    pub fn take_show(&mut self) -> bool {
        std::mem::take(&mut self.show_asked)
    }

    fn handle_rect(&self) -> Option<Rect> {
        let hs = self.theme.osk.coding_height;
        let p = self.theme.osk.padding;
        (self.handle_on && !self.want).then(|| Rect::new(self.w - p - hs * 1.5, self.h - p - hs, hs * 1.5, hs))
    }

    /// Up (or coming up).
    pub fn wanted(&self) -> bool {
        self.want
    }

    /// On screen or moving.
    pub fn active(&self) -> bool {
        self.want || self.show.value > 0.001 || self.warming || self.handle_on
    }

    /// Fully up and at rest (apps are resized only then, so they redraw while covered).
    pub fn settled_up(&self) -> bool {
        self.want && self.show.is_resting() && self.show.value > 0.99
    }

    /// The keyboard's height when up (what apps should leave free).
    pub fn height(&self) -> f32 {
        self.kb_h
    }

    /// Where the keyboard is on screen now.
    pub fn rect(&self) -> Rect {
        Rect::new(0.0, self.h - self.kb_h * self.show.value.clamp(0.0, 1.0), self.w, self.kb_h)
    }

    /// A touch this layer should get: one it tracks, or a new one on the keyboard while it is up.
    pub fn wants(&self, id: i32, x: f32, y: f32) -> bool {
        self.touches.iter().any(|t| t.id == id)
            || self.handle_touch == Some(id)
            || (self.want && self.rect().contains(x, y))
            || self.handle_rect().is_some_and(|r| r.contains(x, y))
    }

    /// Keys typed since the last call.
    pub fn take_keys(&mut self) -> Vec<KeyEvent> {
        std::mem::take(&mut self.out)
    }

    /// The hide key was pressed since the last call.
    pub fn take_hide(&mut self) -> bool {
        std::mem::take(&mut self.hide_asked)
    }

    /// The opaque part (a solid fill, fully up), for the compositor to skip what is under it.
    pub fn opaque(&self) -> Vec<Rect> {
        if self.theme.osk.fill.a < 1.0 || !self.want || !self.show.is_resting() {
            return Vec::new();
        }
        vec![self.rect().round_out().inset(1.0)]
    }

    /// Lay out and paint in the "up" pose although hidden (glyphs loaded before the first show).
    pub fn warm_pose(&mut self, on: bool) {
        self.warming = on;
        self.apply();
    }

    /// The centre of the key named `name` (`Act::name`), or of a cell of the open accent strip with that
    /// text, in screen pixels.
    pub fn point(&self, name: &str) -> Option<(f32, f32)> {
        let top = self.rect().y;
        let r = match &self.strip {
            Some(s) => s.items.iter().position(|i| i == name).map(|i| s.cells[i]),
            None => None,
        }
        .or_else(|| self.keys.iter().find(|k| k.act.name() == name).map(|k| k.rect))?;
        Some((r.x + r.w * 0.5, top + r.y + r.h * 0.5))
    }

    // ---- touch

    /// The key under a screen point (nearest in the row; gaps belong to the nearer key). None above or
    /// beside the keyboard.
    fn key_at(&self, x: f32, y: f32) -> Option<usize> {
        let ly = y - self.rect().y;
        let half = self.theme.osk.gap * 0.5;
        if ly < -half || ly > self.kb_h || x < 0.0 || x > self.w {
            return None;
        }
        let last = self.rows.len().checked_sub(1)?;
        let row = self.rows.iter().position(|&(top, bottom)| ly < bottom + half && (ly >= top - half)).unwrap_or(if ly < self.rows[0].0 { 0 } else { last });
        self.keys
            .iter()
            .enumerate()
            .filter(|(_, k)| k.row == row)
            .min_by(|a, b| {
                let d = |r: &Rect| if x < r.x { r.x - x } else if x > r.right() { x - r.right() } else { 0.0 };
                d(&a.1.rect).total_cmp(&d(&b.1.rect))
            })
            .map(|(i, _)| i)
    }

    fn type_text(&mut self, s: &str) {
        let Some(c) = s.chars().next() else { return };
        let sym = xkb::utf32_to_keysym(c as u32).raw();
        self.out.push(KeyEvent { sym, text: Some(s.to_string()), ctrl: self.ctrl != Latch::Off, alt: self.alt != Latch::Off });
        self.after_key(true);
    }

    fn type_sym(&mut self, sym: u32) {
        self.out.push(KeyEvent { sym, text: None, ctrl: self.ctrl != Latch::Off, alt: self.alt != Latch::Off });
        self.after_key(false);
    }

    /// One-shot latches end after a key (Shift only after text).
    fn after_key(&mut self, text: bool) {
        if text && self.shift == Latch::Once {
            self.shift = Latch::Off;
            self.relabel();
        }
        if self.ctrl == Latch::Once {
            self.ctrl = Latch::Off;
        }
        if self.alt == Latch::Once {
            self.alt = Latch::Off;
        }
    }

    /// Off -> once -> (a quick second tap) locked -> off.
    fn latch(&mut self, act: Act, cur: Latch) -> Latch {
        let quick = self
            .last_tap
            .is_some_and(|(a, at)| a == act && self.now.saturating_sub(at) <= Duration::from_millis(self.theme.osk.double_tap_ms as u64));
        self.last_tap = Some((act, self.now));
        match cur {
            Latch::Off => Latch::Once,
            Latch::Once if quick => Latch::Locked,
            _ => Latch::Off,
        }
    }

    fn fire(&mut self, act: Act) {
        match act {
            Act::Letter(c) => {
                let s: String = if self.shifted() { c.to_uppercase().collect() } else { c.to_string() };
                self.type_text(&s);
            }
            Act::Sym(a, b) => {
                let s = if self.shifted() && self.page == Page::Letters { b } else { a };
                self.type_text(s);
            }
            Act::Space => self.type_text(" "),
            Act::Back => self.type_sym(k::KEY_BackSpace),
            Act::Enter => self.type_sym(k::KEY_Return),
            Act::Tab => self.type_sym(k::KEY_Tab),
            Act::Esc => self.type_sym(k::KEY_Escape),
            Act::Left => self.type_sym(k::KEY_Left),
            Act::Right => self.type_sym(k::KEY_Right),
            Act::Up => self.type_sym(k::KEY_Up),
            Act::Down => self.type_sym(k::KEY_Down),
            Act::Home => self.type_sym(k::KEY_Home),
            Act::End => self.type_sym(k::KEY_End),
            Act::Shift => {
                self.shift = self.latch(act, self.shift);
                self.relabel();
            }
            Act::Ctrl => self.ctrl = self.latch(act, self.ctrl),
            Act::Alt => self.alt = self.latch(act, self.alt),
            Act::Page(p) => {
                self.page = p;
                self.shift = Latch::Off;
                self.dirty = true;
            }
            Act::Hide => self.hide_asked = true,
        }
    }

    /// Letter labels follow Shift.
    fn relabel(&mut self) {
        for i in 0..self.keys.len() {
            if matches!(self.keys[i].act, Act::Letter(_) | Act::Sym(..)) && !self.keys[i].coding {
                let (s, _) = self.label(self.keys[i].act);
                let id = self.keys[i].label;
                self.tree.set_text(id, &s);
            }
        }
    }

    fn open_strip(&mut self, ti: usize) {
        self.touches[ti].long_done = true;
        let Some(ki) = self.touches[ti].key else { return };
        let base = match self.keys[ki].act {
            Act::Letter(c) => c.to_string(),
            Act::Sym(a, _) => a.to_string(),
            _ => return,
        };
        let vars = lay::variants(&base);
        if vars.is_empty() {
            return;
        }
        let up = self.shifted() && self.page == Page::Letters;
        let case = |s: &str| if up { s.to_uppercase() } else { s.to_string() };
        let mut items = vec![self.label(self.keys[ki].act).0];
        items.extend(vars.iter().map(|v| case(v)));
        let t = self.theme.osk.clone();
        let r = self.keys[ki].rect;
        let cw = r.w.max(r.h * 0.8);
        let n = items.len() as f32;
        let total = n * cw + (n - 1.0) * t.gap;
        let x0 = r.x.min(self.w - total - t.padding).max(t.padding);
        let y0 = r.y - r.h - 2.0 * t.gap;
        let mut bg = Node::new()
            .size(Size::Fixed(total + 2.0 * t.gap), Size::Fixed(r.h + 2.0 * t.gap))
            .offset(x0 - t.gap, y0 - t.gap)
            .paint(Paint::Rect { color: t.accent_fill.premultiplied(), radius: t.preview_radius });
        if t.shadow {
            let s = &self.theme.shadow;
            bg = bg.shadow(Shadow { sigma: s.small_sigma, dx: 0.0, dy: s.small_offset_y, color: s.color.premultiplied() });
        }
        let bg = self.tree.add(self.panel, bg);
        let mut cells = Vec::new();
        let mut nodes = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let c = Rect::new(x0 + i as f32 * (cw + t.gap), y0, cw, r.h);
            let node = self.tree.add(
                self.panel,
                Node::new().size(Size::Fixed(c.w), Size::Fixed(c.h)).offset(c.x, c.y).paint(Paint::Rect { color: t.key.premultiplied(), radius: t.radius }),
            );
            self.tree.add(node, Node::new().align(Align::Center, Align::Center).paint(text_paint(item, t.label_size, t.label_weight, false, t.text.premultiplied())));
            cells.push(c);
            nodes.push(node);
        }
        self.strip = Some(Strip { touch: self.touches[ti].id, items, cells, nodes, bg, sel: 0 });
    }

    fn close_strip(&mut self) {
        if let Some(s) = self.strip.take() {
            for n in s.nodes {
                self.tree.remove(n);
            }
            self.tree.remove(s.bg);
        }
    }

    /// Strip cell under screen x (nearest).
    fn strip_pick(&mut self, x: f32) {
        if let Some(s) = self.strip.as_mut() {
            let d = |r: &Rect| if x < r.x { r.x - x } else if x > r.right() { x - r.right() } else { 0.0 };
            if let Some((i, _)) = s.cells.iter().enumerate().min_by(|a, b| d(a.1).total_cmp(&d(b.1))) {
                s.sel = i;
            }
        }
    }

    /// Type a finger's key (or its strip choice) now and let go of it.
    fn finish(&mut self, t: Touch) {
        if self.strip.as_ref().is_some_and(|s| s.touch == t.id) {
            let s = self.strip.as_ref().unwrap();
            let item = s.items[s.sel].clone();
            self.close_strip();
            self.type_text(&item);
            return;
        }
        if let Some(ki) = t.key.filter(|_| !t.fired) {
            let act = self.keys[ki].act;
            self.fire(act);
        }
    }

    /// Colours, preview, slide position: everything that changes without a new layout.
    fn apply(&mut self) {
        let t = &self.theme.osk;
        for i in 0..self.keys.len() {
            let kv = &self.keys[i];
            // A finger on it, unless that finger is choosing in the accent strip.
            let strip_touch = self.strip.as_ref().map(|s| s.touch);
            let pressed = self.touches.iter().any(|t| t.key == Some(i) && Some(t.id) != strip_touch);
            let latch = match kv.act {
                Act::Shift => self.shift,
                Act::Ctrl => self.ctrl,
                Act::Alt => self.alt,
                _ => Latch::Off,
            };
            let c = if pressed {
                t.key_pressed
            } else if latch == Latch::Locked {
                t.key_locked
            } else if latch == Latch::Once {
                t.key_latched
            } else if kv.act.is_fn() || kv.coding {
                t.key_fn
            } else {
                t.key
            };
            let node = kv.node;
            set_rect_color(&mut self.tree, node, c.premultiplied());
        }
        if let Some(s) = &self.strip {
            for (i, &n) in s.nodes.iter().enumerate() {
                let c = if i == s.sel { t.accent_selected } else { t.key };
                set_rect_color(&mut self.tree, n, c.premultiplied());
            }
        }
        // The preview follows the newest finger on a character key.
        let show_preview = t.preview && self.purpose != TextPurpose::Password && self.strip.is_none();
        let pk = self
            .touches
            .last()
            .and_then(|t| t.key)
            .filter(|&i| show_preview && matches!(self.keys[i].act, Act::Letter(_) | Act::Sym(..)) && !self.keys[i].coding);
        if pk != self.preview_for {
            self.preview_for = pk;
            match pk {
                Some(i) => {
                    let r = self.keys[i].rect;
                    let (pw, ph) = (r.w, r.h * t.preview_height);
                    let x = (r.x + (r.w - pw) * 0.5).clamp(0.0, (self.w - pw).max(0.0));
                    let y = r.y - ph - t.gap;
                    let (s, _) = self.label(self.keys[i].act);
                    let n = &mut self.tree[self.preview];
                    n.width = Size::Fixed(pw);
                    n.height = Size::Fixed(ph);
                    n.offset = (x, y);
                    self.tree.set_text(self.preview_label, &s);
                    self.tree.set_visible(self.preview, true);
                    self.tree.mark_layout();
                }
                None => self.tree.set_visible(self.preview, false),
            }
        }
        let p = if self.warming { 1.0 } else { self.show.value.clamp(0.0, 1.0) };
        self.tree[self.panel].translate = (0.0, (1.0 - p) * self.kb_h);
    }
}

impl Scene for Keyboard {
    fn tree(&mut self) -> &mut Tree {
        &mut self.tree
    }

    fn update(&mut self, now: Duration, dt: f32) -> bool {
        self.now = now;
        if self.dirty {
            self.build();
        }
        let moving = self.show.step(dt);
        // Long press -> accent strip; held repeating keys.
        let long = Duration::from_millis(self.theme.osk.long_press_ms as u64);
        if self.strip.is_none()
            && let Some(ti) = self.touches.iter().position(|t| !t.long_done && !t.fired && t.key.is_some() && now >= t.since + long)
        {
            self.open_strip(ti);
        }
        let every = Duration::from_millis(self.theme.osk.repeat_ms.max(10) as u64);
        for ti in 0..self.touches.len() {
            if let (Some(at), Some(ki)) = (self.touches[ti].repeat_at, self.touches[ti].key)
                && now >= at
            {
                let act = self.keys[ki].act;
                self.fire(act);
                // Late frames do not burst: the next repeat counts from now.
                self.touches[ti].repeat_at = Some(now.max(at) + every);
            }
        }
        self.apply();
        moving
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        match ev.phase {
            Phase::Down => {
                if !self.wants(ev.id, ev.x, ev.y) {
                    return false;
                }
                if self.handle_rect().is_some_and(|r| r.contains(ev.x, ev.y)) {
                    self.handle_touch = Some(ev.id);
                    set_rect_color(&mut self.tree, self.handle, self.theme.osk.key_pressed.premultiplied());
                    return true;
                }
                self.now = self.now.max(ev.time);
                // Another finger was still down: its key types now (rollover).
                for t in std::mem::take(&mut self.touches) {
                    self.finish(t);
                }
                let key = self.key_at(ev.x, ev.y);
                let mut t = Touch { id: ev.id, key, since: ev.time, fired: false, repeat_at: None, long_done: false };
                if let Some(ki) = key
                    && self.keys[ki].act.repeats()
                {
                    let act = self.keys[ki].act;
                    self.fire(act);
                    t.fired = true;
                    t.long_done = true;
                    t.repeat_at = Some(ev.time + Duration::from_millis(self.theme.osk.repeat_delay_ms as u64));
                }
                if key.is_some_and(|ki| !matches!(self.keys[ki].act, Act::Letter(_) | Act::Sym(..))) {
                    t.long_done = true;
                }
                self.touches.push(t);
                self.apply();
                true
            }
            Phase::Motion if self.handle_touch == Some(ev.id) => true,
            Phase::Up if self.handle_touch == Some(ev.id) => {
                self.handle_touch = None;
                set_rect_color(&mut self.tree, self.handle, self.theme.osk.key_fn.premultiplied());
                self.show_asked = true;
                true
            }
            Phase::Motion => {
                let Some(ti) = self.touches.iter().position(|t| t.id == ev.id) else { return false };
                if self.strip.as_ref().is_some_and(|s| s.touch == ev.id) {
                    self.strip_pick(ev.x);
                } else {
                    let key = self.key_at(ev.x, ev.y);
                    let t = &mut self.touches[ti];
                    if key != t.key {
                        // Slid onto another key: that one types on release (a repeating key stops).
                        t.key = key;
                        t.since = ev.time;
                        t.repeat_at = None;
                        t.long_done = key.is_none_or(|ki| !matches!(self.keys[ki].act, Act::Letter(_) | Act::Sym(..)));
                    }
                }
                self.apply();
                true
            }
            Phase::Up => {
                let Some(ti) = self.touches.iter().position(|t| t.id == ev.id) else { return false };
                let t = self.touches.remove(ti);
                self.finish(t);
                if self.dirty {
                    self.build();
                }
                self.apply();
                true
            }
            Phase::Cancel => {
                if self.handle_touch.is_some_and(|h| ev.id == CANCEL_ALL || h == ev.id) {
                    self.handle_touch = None;
                    set_rect_color(&mut self.tree, self.handle, self.theme.osk.key_fn.premultiplied());
                }
                let had = self.touches.iter().any(|t| ev.id == CANCEL_ALL || t.id == ev.id);
                self.touches.retain(|t| ev.id != CANCEL_ALL && t.id != ev.id);
                if self.strip.as_ref().is_some_and(|s| ev.id == CANCEL_ALL || s.touch == ev.id) {
                    self.close_strip();
                }
                self.apply();
                had
            }
        }
    }

    fn next_wakeup(&self) -> Option<Duration> {
        let long = Duration::from_millis(self.theme.osk.long_press_ms as u64);
        self.touches
            .iter()
            .filter_map(|t| match (t.repeat_at, self.strip.is_none() && !t.long_done && !t.fired && t.key.is_some()) {
                (Some(at), _) => Some(at),
                (None, true) => Some(t.since + long),
                _ => None,
            })
            .min()
    }

    fn set_size(&mut self, w: f32, h: f32) {
        if (w, h) != (self.w, self.h) {
            self.w = w;
            self.h = h;
            self.build();
        }
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        self.show.params = self.springs;
        self.build();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb() -> Keyboard {
        let mut kb = Keyboard::new(&Theme::builtin_dark());
        kb.set_size(1366.0, 768.0);
        kb.set_wanted(true, TextPurpose::Text);
        kb.show.set(1.0, 0.0);
        kb
    }

    fn tap(kb: &mut Keyboard, name: &str, at_ms: u64) {
        let (x, y) = kb.point(name).unwrap_or_else(|| panic!("no key {name}"));
        let t = Duration::from_millis(at_ms);
        kb.update(t, 0.0);
        assert!(kb.input(PointerEvent { id: 0, phase: Phase::Down, x, y, time: t }));
        assert!(kb.input(PointerEvent { id: 0, phase: Phase::Up, x: 0.0, y: 0.0, time: t + Duration::from_millis(50) }));
    }

    fn typed(kb: &mut Keyboard) -> String {
        kb.take_keys().iter().map(|k| k.text.clone().unwrap_or_else(|| format!("<{:x}>", k.sym))).collect()
    }

    #[test]
    fn rows_fill_the_width() {
        let kb = kb();
        let t = &kb.theme.osk;
        for row in 0..kb.rows.len() {
            let keys: Vec<&KeyView> = kb.keys.iter().filter(|k| k.row == row).collect();
            assert!((keys[0].rect.x - t.padding).abs() < 1.0);
            let right = keys.last().unwrap().rect.right();
            assert!((right - (1366.0 - t.padding)).abs() < 1.0, "row {row} ends at {right}");
        }
        assert!(kb.height() > 200.0 && kb.height() < 400.0, "{}", kb.height());
    }

    #[test]
    fn swedish_layout() {
        let mut kb = Keyboard::new(&Theme::builtin_dark()).with_layout("se");
        kb.set_size(1366.0, 768.0);
        kb.set_wanted(true, TextPurpose::Text);
        kb.show.set(1.0, 0.0);
        tap(&mut kb, "å", 0);
        tap(&mut kb, "ö", 200);
        tap(&mut kb, "shift", 400);
        tap(&mut kb, "ä", 1000);
        tap(&mut kb, "-", 1200);
        assert_eq!(typed(&mut kb), "åöÄ-");
        // Rows still fill the width, and other layouts fall back to US.
        let right = kb.keys.iter().filter(|k| k.row == 1).last().unwrap().rect.right();
        assert!((right - (1366.0 - kb.theme.osk.padding)).abs() < 1.0);
        let us = Keyboard::new(&Theme::builtin_dark()).with_layout("de");
        assert!(us.keys.iter().all(|k| k.act != Act::Letter('å')));
    }

    #[test]
    fn typing_shift_and_caps() {
        let mut kb = kb();
        tap(&mut kb, "h", 0);
        tap(&mut kb, "i", 200);
        tap(&mut kb, "shift", 400);
        tap(&mut kb, "a", 1000);
        tap(&mut kb, "a", 1200);
        assert_eq!(typed(&mut kb), "hiAa");
        // Double tap: Caps lock until Shift again.
        tap(&mut kb, "shift", 2000);
        tap(&mut kb, "shift", 2150);
        tap(&mut kb, "o", 2500);
        tap(&mut kb, "k", 2700);
        tap(&mut kb, "shift", 3500);
        tap(&mut kb, "k", 3700);
        assert_eq!(typed(&mut kb), "OKk");
    }

    #[test]
    fn pages_ctrl_and_special_keys() {
        let mut kb = kb();
        tap(&mut kb, "123", 0);
        tap(&mut kb, "4", 200);
        tap(&mut kb, "abc", 400);
        tap(&mut kb, "ctrl", 600);
        tap(&mut kb, "c", 1200);
        let keys = kb.take_keys();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].text.as_deref(), Some("4"));
        assert!(keys[1].ctrl && keys[1].text.as_deref() == Some("c"));
        tap(&mut kb, "x", 1400);
        assert!(!kb.take_keys()[0].ctrl, "Ctrl once ends after a key");
        tap(&mut kb, "enter", 1600);
        tap(&mut kb, "esc", 1800);
        let keys = kb.take_keys();
        assert_eq!((keys[0].sym, keys[1].sym), (k::KEY_Return, k::KEY_Escape));
    }

    #[test]
    fn backspace_repeats_while_held() {
        let mut kb = kb();
        let (x, y) = kb.point("backspace").unwrap();
        kb.input(PointerEvent { id: 0, phase: Phase::Down, x, y, time: Duration::ZERO });
        assert_eq!(kb.take_keys().len(), 1, "types on press");
        let delay = kb.theme.osk.repeat_delay_ms as u64;
        let every = kb.theme.osk.repeat_ms as u64;
        assert_eq!(kb.next_wakeup(), Some(Duration::from_millis(delay)));
        for i in 0..4 {
            kb.update(Duration::from_millis(delay + i * every), 0.0);
        }
        assert_eq!(kb.take_keys().len(), 4);
        kb.input(PointerEvent { id: 0, phase: Phase::Up, x: 0.0, y: 0.0, time: Duration::from_millis(900) });
        assert!(kb.take_keys().is_empty(), "nothing more on release");
    }

    #[test]
    fn long_press_accents() {
        let mut kb = kb();
        let (x, y) = kb.point("e").unwrap();
        kb.input(PointerEvent { id: 0, phase: Phase::Down, x, y, time: Duration::ZERO });
        kb.update(Duration::from_millis(kb.theme.osk.long_press_ms as u64 + 1), 0.0);
        let (ex, _) = kb.point("é").expect("strip open");
        kb.input(PointerEvent { id: 0, phase: Phase::Motion, x: ex, y: y - 80.0, time: Duration::from_millis(600) });
        kb.input(PointerEvent { id: 0, phase: Phase::Up, x: 0.0, y: 0.0, time: Duration::from_millis(700) });
        assert_eq!(typed(&mut kb), "é");
        assert!(kb.strip.is_none());
    }

    #[test]
    fn rollover_and_slide() {
        let mut kb = kb();
        let (ax, ay) = kb.point("a").unwrap();
        let (sx, sy) = kb.point("s").unwrap();
        kb.input(PointerEvent { id: 1, phase: Phase::Down, x: ax, y: ay, time: Duration::ZERO });
        kb.input(PointerEvent { id: 2, phase: Phase::Down, x: sx, y: sy, time: Duration::from_millis(30) });
        assert_eq!(typed(&mut kb), "a", "the first finger types when the second lands");
        let (dx, _) = kb.point("d").unwrap();
        kb.input(PointerEvent { id: 2, phase: Phase::Motion, x: dx, y: sy, time: Duration::from_millis(60) });
        kb.input(PointerEvent { id: 2, phase: Phase::Up, x: 0.0, y: 0.0, time: Duration::from_millis(90) });
        kb.input(PointerEvent { id: 1, phase: Phase::Up, x: 0.0, y: 0.0, time: Duration::from_millis(95) });
        assert_eq!(typed(&mut kb), "d");
    }

    #[test]
    fn gaps_go_to_the_nearest_key_and_outside_is_nothing() {
        let kb = kb();
        let q = kb.keys.iter().position(|k| k.act == Act::Letter('q')).unwrap();
        let r = kb.keys[q].rect;
        let top = kb.rect().y;
        assert_eq!(kb.key_at(r.right() + 1.0, top + r.y + 5.0), Some(q));
        assert_eq!(kb.key_at(r.x + 5.0, top - 50.0), None);
    }

    #[test]
    fn portrait_is_shorter_rows() {
        let mut kb = kb();
        let land = kb.height();
        kb.set_size(768.0, 1366.0);
        assert!(kb.height() < land);
        assert!(kb.point("q").is_some_and(|(x, _)| x < 768.0 / 10.0));
    }
}
