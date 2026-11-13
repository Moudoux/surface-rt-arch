//! Shell UI layer (DESKTOP-SHELL.md 3.6, layer 3): a small retained widget tree on top of the render kit.
//!
//! Nodes live in an arena (`Tree`) and are addressed by `NodeId`. Each node has a size rule per axis
//! (fixed, fit content, fill the parent), lays out its children (stack with alignment, row, column, grid),
//! paints one thing (rounded rect, glass, image, icon, text) with an optional shadow, and has opacity, a
//! transform (translate + scale about its centre), an optional clip and a hit-test flag. Layout runs when
//! something structural changed (`mark_layout`); paint and hit testing run every frame from the cached
//! frames. Visual values come from theme tokens (callers pass them in); nothing here hard-codes looks.

pub mod board;
pub mod control;
pub mod demo;
pub mod home;
pub mod lock;
pub mod osk;
pub mod osk_layout;
pub mod pins;
pub mod search;
pub mod settings;
pub mod shell;
pub mod springboard;

/// Styles (DESKTOP-SHELL.md 3.6) this build has: `style` in config.toml must be one of them.
pub const STYLES: [&str; 3] = ["rt", "ipad", "none"];

/// Drawn for apps whose icon is not installed.
pub const FALLBACK_ICON: &str = "application-x-executable";

use crate::render::{
    icons::Drawn,
    Painter,
    batch::{Rect, Rgba, Xf},
    gl::Texture,
    text::{TextLayout, TextStyle},
};

pub type NodeId = usize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Fixed(f32),
    /// Content size (text, icon, or the children).
    Fit,
    /// Share of the parent's remaining space (weight).
    Fill(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    /// Children placed by their own alignment and offset.
    Stack,
    Row { gap: f32 },
    Column { gap: f32 },
    /// Fixed number of equal-width columns; rows as tall as their tallest child.
    Grid { columns: usize, gap: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy)]
pub struct Shadow {
    pub sigma: f32,
    pub dx: f32,
    pub dy: f32,
    pub color: Rgba,
}

pub enum Paint {
    None,
    Rect { color: Rgba, radius: f32 },
    /// Blurred wallpaper at screen position, tinted.
    Glass { radius: f32, tint: Rgba },
    Image { tex: Texture, radius: f32 },
    /// An opaque image at full size (the wallpaper): drawn without blending.
    Backdrop { tex: Texture },
    Icon { name: String, size: u32 },
    Text { text: String, style: TextStyle, color: Rgba, wrap: bool, layout: Option<TextLayout> },
    /// Dims everything painted before it (earlier siblings and their subtrees) by `color`'s alpha times the
    /// node's opacity. A black scrim is applied as a colour multiplier on those nodes instead of a
    /// full-screen layer (DrawList::darken); other colours are drawn as a rect over the node's frame.
    Scrim { color: Rgba },
}

pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub layout: Layout,
    pub width: Size,
    pub height: Size,
    pub padding: f32,
    /// Alignment inside the parent's box on the cross axis (row/column) or both axes (stack).
    pub halign: Align,
    pub valign: Align,
    /// Stack children: offset from the aligned position.
    pub offset: (f32, f32),
    pub paint: Paint,
    pub shadow: Option<Shadow>,
    pub opacity: f32,
    pub translate: (f32, f32),
    pub scale: f32,
    pub clip: bool,
    /// Hit testing returns this node (the deepest such node under the point wins).
    pub hit: bool,
    pub visible: bool,
    /// Layout result, in the parent's untransformed coordinates.
    pub frame: Rect,
}

impl Node {
    pub fn new() -> Node {
        Node {
            parent: None,
            children: Vec::new(),
            layout: Layout::Stack,
            width: Size::Fit,
            height: Size::Fit,
            padding: 0.0,
            halign: Align::Start,
            valign: Align::Start,
            offset: (0.0, 0.0),
            paint: Paint::None,
            shadow: None,
            opacity: 1.0,
            translate: (0.0, 0.0),
            scale: 1.0,
            clip: false,
            hit: false,
            visible: true,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    pub fn size(mut self, w: Size, h: Size) -> Node {
        self.width = w;
        self.height = h;
        self
    }
    pub fn layout(mut self, l: Layout) -> Node {
        self.layout = l;
        self
    }
    pub fn padding(mut self, p: f32) -> Node {
        self.padding = p;
        self
    }
    pub fn align(mut self, h: Align, v: Align) -> Node {
        self.halign = h;
        self.valign = v;
        self
    }
    pub fn offset(mut self, x: f32, y: f32) -> Node {
        self.offset = (x, y);
        self
    }
    pub fn paint(mut self, p: Paint) -> Node {
        self.paint = p;
        self
    }
    pub fn shadow(mut self, s: Shadow) -> Node {
        self.shadow = Some(s);
        self
    }
    pub fn hit(mut self) -> Node {
        self.hit = true;
        self
    }
    #[allow(dead_code)]
    pub fn clip(mut self) -> Node {
        self.clip = true;
        self
    }

    /// The node's own transform: scale about its centre, then translate.
    fn local_xf(&self) -> Xf {
        let (cx, cy) = (self.frame.x + self.frame.w * 0.5, self.frame.y + self.frame.h * 0.5);
        Xf { s: self.scale, tx: cx - self.scale * cx + self.translate.0, ty: cy - self.scale * cy + self.translate.1 }
    }
}

pub struct Tree {
    nodes: Vec<Option<Node>>,
    pub root: NodeId,
    layout_dirty: bool,
}

impl std::ops::Index<NodeId> for Tree {
    type Output = Node;
    fn index(&self, id: NodeId) -> &Node {
        self.nodes[id].as_ref().expect("live node")
    }
}

impl std::ops::IndexMut<NodeId> for Tree {
    fn index_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id].as_mut().expect("live node")
    }
}

impl Tree {
    pub fn new(root: Node) -> Tree {
        Tree { nodes: vec![Some(root)], root: 0, layout_dirty: true }
    }

    pub fn add(&mut self, parent: NodeId, mut node: Node) -> NodeId {
        node.parent = Some(parent);
        let id = match self.nodes.iter().position(Option::is_none) {
            Some(free) => {
                self.nodes[free] = Some(node);
                free
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        };
        self[parent].children.push(id);
        self.layout_dirty = true;
        id
    }

    pub fn remove(&mut self, id: NodeId) {
        if let Some(p) = self[id].parent {
            self[p].children.retain(|&c| c != id);
        }
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes[n].take() {
                stack.extend(node.children);
            }
        }
        self.layout_dirty = true;
    }

    /// Show or hide a node. Hidden subtrees are not laid out, so showing one again re-runs layout.
    pub fn set_visible(&mut self, id: NodeId, visible: bool) {
        if self[id].visible != visible {
            self[id].visible = visible;
            self.layout_dirty = true;
        }
    }

    /// Remove every child of `id` (and their subtrees).
    pub fn clear_children(&mut self, id: NodeId) {
        for c in self[id].children.clone() {
            self.remove(c);
        }
    }

    /// Something that affects sizes or positions changed (text, size rules, children).
    pub fn mark_layout(&mut self) {
        self.layout_dirty = true;
    }

    pub fn set_text(&mut self, id: NodeId, new: &str) {
        if let Paint::Text { text, .. } = &mut self[id].paint
            && text != new
        {
            *text = new.to_string();
            self.layout_dirty = true;
        }
    }

    // ---- layout

    /// Lay out the tree into `bounds` if anything changed.
    pub fn layout(&mut self, p: &mut Painter, bounds: Rect) {
        if !self.layout_dirty && self[self.root].frame == bounds {
            return;
        }
        self.layout_dirty = false;
        let root = self.root;
        self[root].frame = bounds;
        self.arrange(p, root);
    }

    /// Preferred size given the space available (for Fill and wrapping text).
    fn measure(&mut self, p: &mut Painter, id: NodeId, avail_w: f32, avail_h: f32) -> (f32, f32) {
        let (wr, hr, pad) = (self[id].width, self[id].height, self[id].padding);
        let fixed = |s: Size, avail: f32| match s {
            Size::Fixed(v) => Some(v),
            Size::Fill(_) => Some(avail),
            Size::Fit => None,
        };
        let (fw, fh) = (fixed(wr, avail_w), fixed(hr, avail_h));
        if let (Some(w), Some(h)) = (fw, fh) {
            return (w, h);
        }
        let inner_w = fw.unwrap_or(avail_w) - 2.0 * pad;
        let inner_h = fh.unwrap_or(avail_h) - 2.0 * pad;
        let content = self.content_size(p, id, inner_w, inner_h);
        (fw.unwrap_or(content.0 + 2.0 * pad), fh.unwrap_or(content.1 + 2.0 * pad))
    }

    fn content_size(&mut self, p: &mut Painter, id: NodeId, inner_w: f32, inner_h: f32) -> (f32, f32) {
        if let Paint::Text { text, style, wrap, layout, .. } = &mut self[id].paint {
            let max_w = wrap.then_some(inner_w.max(1.0));
            if !layout.as_ref().is_some_and(|l| l.is_for(text, style, max_w)) {
                *layout = Some(p.text.layout(text, style, max_w));
            }
            let l = layout.as_ref().unwrap();
            return (l.w, l.h);
        }
        if let Paint::Icon { size, .. } = &self[id].paint {
            return (*size as f32, *size as f32);
        }
        let children = self[id].children.clone();
        let layout = self[id].layout;
        let visible: Vec<NodeId> = children.into_iter().filter(|&c| self[c].visible).collect();
        let sizes: Vec<(f32, f32)> = if let Layout::Row { gap } = layout {
            // As `arrange` places them: fixed / fit children first, fill children share the width that is left.
            // (Measured at the full width, wrapping text in a fill child came out too short.)
            let mut sizes = vec![(0.0f32, 0.0f32); visible.len()];
            let mut used = gap * (visible.len() as f32 - 1.0).max(0.0);
            let mut weights = 0.0;
            for (i, &c) in visible.iter().enumerate() {
                if let Size::Fill(w) = self[c].width {
                    weights += w;
                } else {
                    sizes[i] = self.measure(p, c, inner_w, inner_h);
                    used += sizes[i].0;
                }
            }
            let left = (inner_w - used).max(0.0);
            for (i, &c) in visible.iter().enumerate() {
                if let Size::Fill(w) = self[c].width {
                    let m = left * w / weights.max(1e-6);
                    sizes[i] = (m, self.measure(p, c, m, inner_h).1);
                }
            }
            sizes
        } else {
            visible.iter().map(|&c| self.measure(p, c, inner_w, inner_h)).collect()
        };
        let n = sizes.len() as f32;
        match layout {
            Layout::Stack => sizes.iter().fold((0.0, 0.0), |a, s| (a.0.max(s.0), a.1.max(s.1))),
            Layout::Row { gap } => {
                (sizes.iter().map(|s| s.0).sum::<f32>() + gap * (n - 1.0).max(0.0), sizes.iter().map(|s| s.1).fold(0.0, f32::max))
            }
            Layout::Column { gap } => {
                (sizes.iter().map(|s| s.0).fold(0.0, f32::max), sizes.iter().map(|s| s.1).sum::<f32>() + gap * (n - 1.0).max(0.0))
            }
            Layout::Grid { columns, gap } => {
                let cols = columns.max(1);
                let rows = sizes.chunks(cols).map(|r| r.iter().map(|s| s.1).fold(0.0, f32::max));
                let h: f32 = rows.clone().sum::<f32>() + gap * (rows.count() as f32 - 1.0).max(0.0);
                (inner_w, h)
            }
        }
    }

    fn arrange(&mut self, p: &mut Painter, id: NodeId) {
        let frame = self[id].frame;
        let pad = self[id].padding;
        let inner = frame.inset(pad);
        // Text wraps to the final width.
        if matches!(self[id].paint, Paint::Text { wrap: true, .. }) {
            self.content_size(p, id, inner.w, inner.h);
        }
        let children: Vec<NodeId> = self[id].children.iter().copied().filter(|&c| self[c].visible).collect();
        let place = |a: Align, start: f32, avail: f32, size: f32| match a {
            Align::Start => start,
            Align::Center => start + (avail - size) * 0.5,
            Align::End => start + avail - size,
        };
        match self[id].layout {
            Layout::Stack => {
                for c in children {
                    let (w, h) = self.measure(p, c, inner.w, inner.h);
                    let n = &self[c];
                    let x = place(n.halign, inner.x, inner.w, w) + n.offset.0;
                    let y = place(n.valign, inner.y, inner.h, h) + n.offset.1;
                    self[c].frame = Rect::new(x, y, w, h);
                    self.arrange(p, c);
                }
            }
            Layout::Row { gap } | Layout::Column { gap } => {
                let row = matches!(self[id].layout, Layout::Row { .. });
                let (main, cross) = if row { (inner.w, inner.h) } else { (inner.h, inner.w) };
                let gaps = gap * (children.len() as f32 - 1.0).max(0.0);
                // Non-fill children first; fill children share what is left by weight.
                let mut sizes = vec![(0.0f32, 0.0f32); children.len()];
                let mut used = gaps;
                let mut weights = 0.0;
                for (i, &c) in children.iter().enumerate() {
                    let rule = if row { self[c].width } else { self[c].height };
                    if let Size::Fill(wgt) = rule {
                        weights += wgt;
                    } else {
                        let (w, h) = self.measure(p, c, inner.w, inner.h);
                        sizes[i] = if row { (w, h) } else { (h, w) };
                        used += sizes[i].0;
                    }
                }
                let left = (main - used).max(0.0);
                for (i, &c) in children.iter().enumerate() {
                    let rule = if row { self[c].width } else { self[c].height };
                    if let Size::Fill(wgt) = rule {
                        let m = left * wgt / weights.max(1e-6);
                        let (w, h) = if row { self.measure(p, c, m, inner.h) } else { self.measure(p, c, inner.w, m) };
                        sizes[i] = if row { (m, h) } else { (m, w) };
                    }
                }
                let mut at = if row { inner.x } else { inner.y };
                for (i, &c) in children.iter().enumerate() {
                    let (m, x) = sizes[i];
                    let cross_rule = if row { self[c].height } else { self[c].width };
                    let x = if matches!(cross_rule, Size::Fill(_)) { cross } else { x };
                    let a = if row { self[c].valign } else { self[c].halign };
                    let cpos = place(a, if row { inner.y } else { inner.x }, cross, x);
                    self[c].frame = if row { Rect::new(at, cpos, m, x) } else { Rect::new(cpos, at, x, m) };
                    at += m + gap;
                    self.arrange(p, c);
                }
            }
            Layout::Grid { columns, gap } => {
                let cols = columns.max(1);
                let cw = (inner.w - gap * (cols as f32 - 1.0)) / cols as f32;
                let mut y = inner.y;
                for row in children.chunks(cols) {
                    let hs: Vec<f32> = row.iter().map(|&c| self.measure(p, c, cw, inner.h).1).collect();
                    let rh = hs.iter().copied().fold(0.0, f32::max);
                    for (i, &c) in row.iter().enumerate() {
                        let x = inner.x + i as f32 * (cw + gap);
                        let (w, _) = self.measure(p, c, cw, rh);
                        let w = if matches!(self[c].width, Size::Fill(_)) { cw } else { w };
                        let h = if matches!(self[c].height, Size::Fill(_)) { rh } else { hs[i] };
                        self[c].frame = Rect::new(place(self[c].halign, x, cw, w), place(self[c].valign, y, rh, h), w, h);
                        self.arrange(p, c);
                    }
                    y += rh + gap;
                }
            }
        }
    }

    // ---- paint

    pub fn paint(&mut self, p: &mut Painter) {
        self.paint_node(p, self.root);
    }

    /// The node's shadow, under its transform and opacity.
    fn paint_shadow(&self, p: &mut Painter, id: NodeId) {
        let n = &self[id];
        let Some(s) = n.shadow else { return };
        if !n.visible || n.opacity <= 0.0 {
            return;
        }
        let radius = match &n.paint {
            Paint::Rect { radius, .. } | Paint::Glass { radius, .. } | Paint::Image { radius, .. } => *radius,
            _ => 0.0,
        };
        p.list.save();
        p.list.transform(n.local_xf());
        p.list.fade(n.opacity);
        p.atlas.draw_shadow(p.gl, p.list, n.frame, radius, s.sigma, s.dx, s.dy, s.color);
        p.list.restore();
    }

    /// Paint a node and its subtree. Row/column/grid containers paint all their children's shadows first
    /// (siblings sit on one plane: a shadow never darkens the neighbouring card), which also lets the
    /// batcher draw every shadow of the container in one call.
    fn paint_node(&mut self, p: &mut Painter, id: NodeId) {
        self.paint_node_inner(p, id, true);
    }

    fn paint_node_inner(&mut self, p: &mut Painter, id: NodeId, own_shadow: bool) {
        let n = &self[id];
        if !n.visible || n.opacity <= 0.0 {
            return;
        }
        if own_shadow {
            self.paint_shadow(p, id);
        }
        // Black scrims among the children: each earlier child (and this node's own paint, which comes before
        // all of them) is darkened by the scrims after it.
        let kids = &self[id].children;
        let mut shade = vec![1.0f32; kids.len()];
        let mut acc = 1.0;
        for (i, &c) in kids.iter().enumerate().rev() {
            shade[i] = acc;
            let n = &self[c];
            if let Paint::Scrim { color } = n.paint
                && n.visible
                && color[0] + color[1] + color[2] <= 0.0
            {
                acc *= 1.0 - color[3] * n.opacity.clamp(0.0, 1.0);
            }
        }
        let n = &self[id];
        p.list.save();
        p.list.transform(n.local_xf());
        p.list.fade(n.opacity);
        let f = n.frame;
        // A clipping node clips its own paint (e.g. a long label) and its children to its frame.
        let clip = n.clip;
        if clip {
            p.list.push_clip(f);
        }
        p.list.save();
        p.list.darken(acc);
        match &mut self[id].paint {
            Paint::None => {}
            Paint::Rect { color, radius } => p.list.rect(f, *radius, *color),
            Paint::Glass { radius, tint } => {
                if let Some(g) = p.glass {
                    p.list.glass(g, p.screen, f, *radius, *tint, 1.0);
                } else {
                    // Before the blur is ready: the tint alone, made opaque enough to read on.
                    let mut t = *tint;
                    let a = t[3].max(0.85);
                    t = t.map(|c| c * a / t[3].max(1e-3));
                    p.list.rect(f, *radius, t);
                }
            }
            Paint::Image { tex, radius } => p.list.texture(*tex, f, [0.0, 0.0, 1.0, 1.0], *radius, [1.0; 4], [0.0; 4]),
            Paint::Backdrop { tex } => p.list.opaque_texture(*tex, f, [0.0, 0.0, 1.0, 1.0]),
            Paint::Icon { name, size } => {
                let (x, y) = (f.x + (f.w - *size as f32) * 0.5, f.y + (f.h - *size as f32) * 0.5);
                if p.icons.draw(p.gl, p.list, name, x, y, *size, 1.0) == Drawn::Missing {
                    p.icons.draw(p.gl, p.list, FALLBACK_ICON, x, y, *size, 1.0);
                }
            }
            Paint::Scrim { color } => {
                // Black scrims are applied by the parent (darkening earlier siblings).
                if color[0] + color[1] + color[2] > 0.0 {
                    p.list.rect(f, 0.0, *color);
                }
            }
            Paint::Text { layout, color, .. } => {
                if let Some(l) = layout {
                    p.atlas.draw_text(p.gl, p.text, p.list, l, f.x, f.y, *color, 1.0);
                }
            }
        }
        p.list.restore();
        let children = self[id].children.clone();
        let plane = !matches!(self[id].layout, Layout::Stack);
        if plane {
            for (i, &c) in children.iter().enumerate() {
                p.list.save();
                p.list.darken(shade[i]);
                self.paint_shadow(p, c);
                p.list.restore();
            }
        }
        for (i, c) in children.into_iter().enumerate() {
            p.list.save();
            p.list.darken(shade[i]);
            self.paint_node_inner(p, c, !plane);
            p.list.restore();
        }
        if clip {
            p.list.pop_clip();
        }
        p.list.restore();
    }

    // ---- hit testing

    /// The deepest `hit` node under screen point (x, y), honouring transforms and clips.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        self.hit_node(self.root, Xf::IDENTITY, x, y)
    }

    fn hit_node(&self, id: NodeId, parent: Xf, x: f32, y: f32) -> Option<NodeId> {
        let n = &self[id];
        if !n.visible || n.opacity <= 0.0 {
            return None;
        }
        let xf = parent.then(n.local_xf());
        let (lx, ly) = xf.invert(x, y);
        let inside = n.frame.contains(lx, ly);
        if n.clip && !inside {
            return None;
        }
        for &c in n.children.iter().rev() {
            if let Some(h) = self.hit_node(c, xf, x, y) {
                return Some(h);
            }
        }
        (n.hit && inside).then_some(id)
    }

    /// A node's frame in screen coordinates (its transform and its ancestors' applied).
    pub fn screen_rect(&self, id: NodeId) -> Rect {
        let mut chain = vec![id];
        let mut cur = id;
        while let Some(p) = self[cur].parent {
            chain.push(p);
            cur = p;
        }
        let xf = chain.iter().rev().fold(Xf::IDENTITY, |acc, &n| acc.then(self[n].local_xf()));
        xf.rect(self[id].frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_respects_translate_and_order() {
        let mut t = Tree::new(Node::new().size(Size::Fixed(100.0), Size::Fixed(100.0)));
        let a = t.add(0, Node::new().hit());
        let b = t.add(0, Node::new().hit());
        t[0].frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        t[a].frame = Rect::new(0.0, 0.0, 50.0, 50.0);
        t[b].frame = Rect::new(25.0, 25.0, 50.0, 50.0);
        assert_eq!(t.hit_test(30.0, 30.0), Some(b)); // b is on top
        assert_eq!(t.hit_test(10.0, 10.0), Some(a));
        t[b].translate = (100.0, 0.0);
        assert_eq!(t.hit_test(30.0, 30.0), Some(a));
        assert_eq!(t.hit_test(130.0, 30.0), Some(b));
        t[a].scale = 0.5; // a now covers 12.5..37.5
        assert_eq!(t.hit_test(5.0, 5.0), None);
        assert_eq!(t.screen_rect(a), Rect::new(12.5, 12.5, 25.0, 25.0));
    }
}
