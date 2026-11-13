//! `FullscreenPolicy` (DESKTOP-SHELL.md 3.4): one app at a time filling the screen, or two side by side
//! (split view, Phase 7).
//!
//! - Toplevels without a parent get the full work area with `maximized` + all four `tiled_*` states (apps
//!   drop CSD shadows and rounded corners but keep their toolbars). A client asking for fullscreen gets the
//!   whole output with the `fullscreen` state.
//! - Toplevels with a parent (dialogs) float centred over their app at their own size, bounded by its area.
//! - Split view: a `Pair` of two apps shares the work area (geometry in `split.rs`); both get their half
//!   with the `tiled_*` states and without `maximized`. A pair is one unit: raised, cycled, hidden by Home
//!   and shown in the Overview together. Focus is on whichever app (or dialog) was raised last.
//! - Only the top unit (an app or a pair, with their dialogs) is mapped in the space. Other apps are
//!   unmapped and configured `suspended` (xdg-shell v6), so they stop drawing.
//! - Stack order is most-recently-used: the last element of `stack` is on top.

use std::time::Duration;

use smithay::{
    desktop::Window,
    utils::{Logical, Point, Rectangle, Size},
};
use tracing::{debug, info};

use super::split::{self, Axis, Release, SplitMetrics};
use crate::{
    policy::{DecoRole, Decoration, FocusChange, HandlePhase, SnapSide, WindowPolicy, WmCtx},
    winconf,
};

/// Two apps (roots, without their dialogs) sharing the screen; `ratio` = the start app's share.
#[derive(Debug, Clone, PartialEq)]
struct Pair {
    start: Window,
    end: Window,
    ratio: f64,
}

impl Pair {
    fn has(&self, w: &Window) -> bool {
        &self.start == w || &self.end == w
    }

    fn side_of(&self, w: &Window) -> Option<SnapSide> {
        if &self.start == w {
            Some(SnapSide::Start)
        } else if &self.end == w {
            Some(SnapSide::End)
        } else {
            None
        }
    }

    fn at(&self, side: SnapSide) -> &Window {
        match side {
            SnapSide::Start => &self.start,
            SnapSide::End => &self.end,
        }
    }
}

/// A finger (or the mouse) on the divider.
#[derive(Debug, Clone, Copy)]
struct DividerDrag {
    from: Point<f64, Logical>,
    pos: Point<f64, Logical>,
    /// Moved past the slop: a drag, not a tap.
    moved: bool,
}

/// Movement (px) before a touch on the divider counts as a drag.
const SLOP: f64 = 10.0;

#[derive(Debug)]
pub struct FullscreenPolicy {
    /// Mapped toplevels, bottom to top.
    stack: Vec<Window>,
    /// Home is shown: no app is mapped into the space.
    home: bool,
    pairs: Vec<Pair>,
    metrics: SplitMetrics,
    drag: Option<DividerDrag>,
    /// When the divider was last tapped (a second tap soon after swaps the sides).
    last_tap: Option<Duration>,
}

// Windows are configured through `winconf` (xdg toplevels and X11 windows alike).
fn send(w: &Window) {
    winconf::send(w);
}

fn is_fullscreen(w: &Window) -> bool {
    winconf::with_pending(w, |p| p.fullscreen)
}

/// The client's minimum size along the split axis (0 = none).
fn min_len(w: &Window, axis: Axis) -> i32 {
    let min: Size<i32, Logical> = winconf::min_size(w);
    match axis {
        Axis::Horizontal => min.w,
        Axis::Vertical => min.h,
    }
}


impl FullscreenPolicy {
    pub fn new(metrics: SplitMetrics) -> Self {
        FullscreenPolicy { stack: Vec::new(), home: false, pairs: Vec::new(), metrics, drag: None, last_tap: None }
    }

    fn parent_of(&self, w: &Window) -> Option<Window> {
        self.stack.iter().find(|c| *c != w && winconf::is_child_of(w, c)).cloned()
    }

    /// A dialog is a toplevel whose parent we know. Orphans (parent gone or never mapped) count as apps.
    fn is_dialog(&self, w: &Window) -> bool {
        self.parent_of(w).is_some()
    }

    fn root_app(&self, w: &Window) -> Window {
        let mut cur = w.clone();
        // Bounded walk: a malicious client could build a parent cycle.
        for _ in 0..16 {
            match self.parent_of(&cur) {
                Some(p) => cur = p,
                None => break,
            }
        }
        cur
    }

    fn top_app(&self) -> Option<Window> {
        self.stack.iter().rev().find(|w| !self.is_dialog(w)).cloned()
    }

    fn pair_of(&self, root: &Window) -> Option<&Pair> {
        self.pairs.iter().find(|p| p.has(root))
    }

    /// The window that names `root`'s unit (the pair's start app, or the app itself).
    fn unit_key(&self, root: &Window) -> Window {
        self.pair_of(root).map(|p| p.start.clone()).unwrap_or_else(|| root.clone())
    }

    /// The pair shown now, if the top unit is one.
    fn shown_pair(&self) -> Option<&Pair> {
        if self.home {
            return None;
        }
        self.pair_of(&self.top_app()?)
    }

    /// The apps shown now: none (Home), the top app, or both apps of its pair.
    fn shown_roots(&self) -> Vec<Window> {
        if self.home {
            return Vec::new();
        }
        let Some(top) = self.top_app() else { return Vec::new() };
        match self.pair_of(&top) {
            Some(p) => vec![p.start.clone(), p.end.clone()],
            None => vec![top],
        }
    }

    /// Units, most recently used first, each named by its key and its topmost app.
    fn units(&self) -> Vec<(Window, Window)> {
        let mut out: Vec<(Window, Window)> = Vec::new();
        for w in self.stack.iter().rev().filter(|w| !self.is_dialog(w)) {
            let key = self.unit_key(w);
            if !out.iter().any(|(k, _)| *k == key) {
                out.push((key, w.clone()));
            }
        }
        out
    }

    /// Drop pairs whose apps are gone or became dialogs.
    fn prune_pairs(&mut self) {
        let ok = |w: &Window, s: &Self| s.stack.contains(w) && !s.is_dialog(w);
        let keep: Vec<bool> = self.pairs.iter().map(|p| ok(&p.start, self) && ok(&p.end, self) && p.start != p.end).collect();
        let mut it = keep.into_iter();
        self.pairs.retain(|_| it.next().unwrap_or(false));
    }

    fn unpair(&mut self, root: &Window) {
        if self.pairs.iter().any(|p| p.has(root)) {
            self.drag = None;
        }
        self.pairs.retain(|p| !p.has(root));
    }

    /// Where an app (root) goes: its half if paired, else the whole work area (or the output when fullscreen).
    fn app_rect(&self, ctx: &WmCtx, root: &Window) -> Rectangle<i32, Logical> {
        if is_fullscreen(root) {
            return ctx.output_geometry;
        }
        match self.pair_of(root) {
            Some(p) => {
                let l = split::layout(ctx.area, p.ratio, self.metrics.divider);
                l.rect(p.side_of(root).unwrap_or(SnapSide::Start))
            }
            None => ctx.area,
        }
    }

    /// Set the pending state for an app (no parent) or a dialog.
    fn configure(&self, ctx: &WmCtx, w: &Window, is_dialog: bool) {
        let output = ctx.output_geometry;
        let root = self.root_app(w);
        let rect = if is_fullscreen(&root) && root != *w { ctx.area } else { self.app_rect(ctx, &root) };
        let paired = !is_dialog && self.pair_of(w).is_some();
        winconf::with_pending(w, |s| {
            s.bounds = Some(rect.size);
            if is_dialog {
                s.maximized = false;
                s.fullscreen = false;
                s.tiled = false;
                // Let the dialog pick its size, but never larger than its app's area.
                if let Some(size) = s.size.as_mut() {
                    size.w = size.w.min(rect.size.w);
                    size.h = size.h.min(rect.size.h);
                }
            } else if s.fullscreen {
                s.maximized = false;
                s.tiled = false;
                s.size = Some(output.size);
            } else {
                // Split halves are tiled on all sides but not maximized (the app is not the whole screen).
                s.maximized = !paired;
                s.tiled = true;
                s.size = Some(rect.size);
            }
        });
    }

    fn location(&self, ctx: &WmCtx, w: &Window, is_dialog: bool) -> Point<i32, Logical> {
        if is_dialog {
            let size = w.geometry().size;
            let area = self.app_rect(ctx, &self.root_app(w));
            let area = if area == ctx.output_geometry { ctx.area } else { area };
            let free: Size<i32, Logical> = (area.size.w - size.w, area.size.h - size.h).into();
            Point::from((area.loc.x + (free.w / 2).max(0), area.loc.y + (free.h / 2).max(0)))
        } else {
            self.app_rect(ctx, w).loc
        }
    }

    /// Take `root` and its dialogs out of the stack (keeping order). Returns (family, rest).
    fn split_family(&mut self, root: &Window) -> (Vec<Window>, Vec<Window>) {
        let member: Vec<bool> = self.stack.iter().map(|x| &self.root_app(x) == root).collect();
        let mut family = Vec::new();
        let mut rest = Vec::new();
        for (w, m) in std::mem::take(&mut self.stack).into_iter().zip(member) {
            if m { family.push(w) } else { rest.push(w) }
        }
        (family, rest)
    }

    fn raise_family(&mut self, root: &Window) {
        let (family, rest) = self.split_family(root);
        self.stack = rest;
        self.stack.extend(family);
    }

    /// Move `w`'s unit to the top: its partner's family (if paired), then `w`'s app with all its dialogs,
    /// keeping their order, then `w` itself on top of them.
    fn raise(&mut self, w: &Window) {
        let root = self.root_app(w);
        if let Some(partner) = self.pair_of(&root).map(|p| if p.start == root { p.end.clone() } else { p.start.clone() }) {
            self.raise_family(&partner);
        }
        self.raise_family(&root);
        if let Some(pos) = self.stack.iter().position(|x| x == w) {
            let w = self.stack.remove(pos);
            self.stack.push(w);
        }
    }

    /// Configure everything, map the top unit and its dialogs in stack order, unmap and suspend the rest.
    /// Returns the window that should have focus (the topmost shown one).
    fn relayout(&mut self, ctx: &mut WmCtx) -> FocusChange {
        self.prune_pairs();
        let shown = self.shown_roots();
        let mut focus = None;
        let stack = self.stack.clone();
        for w in &stack {
            let is_shown = shown.contains(&self.root_app(w));
            let is_dialog = self.is_dialog(w);
            self.configure(ctx, w, is_dialog);
            winconf::with_pending(w, |s| s.suspended = !is_shown);
            if is_shown {
                let loc = self.location(ctx, w, is_dialog);
                // map_element also raises, so walking bottom to top rebuilds the stacking order.
                ctx.space.map_element(w.clone(), loc, false);
                winconf::place(w, loc);
                focus = Some(w.clone());
            } else {
                ctx.space.unmap_elem(w);
            }
            send(w);
        }
        match focus {
            Some(w) => FocusChange::Focus(w),
            None => FocusChange::Clear,
        }
    }

    fn mins(&self, area: Rectangle<i32, Logical>, start: &Window, end: &Window) -> (i32, i32) {
        let ax = split::axis(area);
        (min_len(start, ax), min_len(end, ax))
    }

    /// The divider's release outcome for a finger at `pos`, for the shown pair.
    fn release_at(&self, ctx: &WmCtx, p: &Pair, pos: Point<f64, Logical>) -> Release {
        let (ms, me) = self.mins(ctx.area, &p.start, &p.end);
        split::release(ctx.area, &self.metrics, split::pos_along(ctx.area, pos.x, pos.y), p.ratio, ms, me)
    }

    fn swap_shown(&mut self, ctx: &mut WmCtx) -> FocusChange {
        let Some(top) = self.top_app() else { return FocusChange::Keep };
        let Some(p) = self.pairs.iter_mut().find(|p| p.has(&top)) else { return FocusChange::Keep };
        std::mem::swap(&mut p.start, &mut p.end);
        p.ratio = 1.0 - p.ratio;
        info!("split: swap sides");
        self.relayout(ctx)
    }
}

impl WindowPolicy for FullscreenPolicy {
    fn name(&self) -> &'static str {
        "fullscreen"
    }

    fn configure_new(&mut self, ctx: &mut WmCtx, window: &Window) {
        let is_dialog = self.is_dialog(window);
        self.configure(ctx, window, is_dialog);
    }

    fn map(&mut self, ctx: &mut WmCtx, window: &Window) -> FocusChange {
        if !self.stack.contains(window) {
            self.stack.push(window.clone());
        }
        // Re-check: the parent may have been set or mapped after the initial configure.
        let is_dialog = self.is_dialog(window);
        self.configure(ctx, window, is_dialog);
        self.raise(window);
        self.home = false;
        debug!(dialog = is_dialog, app_id = ?app_id(window), "map");
        self.relayout(ctx)
    }

    fn unmap(&mut self, ctx: &mut WmCtx, window: &Window) -> FocusChange {
        self.stack.retain(|w| w != window);
        self.unpair(window);
        ctx.space.unmap_elem(window);
        self.relayout(ctx)
    }

    fn committed(&mut self, ctx: &mut WmCtx, window: &Window) {
        // Keep dialogs centred when they resize.
        if self.is_dialog(window) && ctx.space.elements().any(|w| w == window) {
            let want = self.location(ctx, window, true);
            if ctx.space.element_location(window) != Some(want) {
                self.relayout(ctx);
            }
        }
    }

    fn activate(&mut self, ctx: &mut WmCtx, window: &Window) -> FocusChange {
        if !self.stack.contains(window) {
            return FocusChange::Keep;
        }
        self.raise(window);
        self.home = false;
        self.relayout(ctx);
        FocusChange::Focus(window.clone())
    }

    fn cycle(&mut self, ctx: &mut WmCtx, forward: bool) -> FocusChange {
        let units = self.units();
        if self.home && !units.is_empty() {
            // From Home, the first press shows the most recent app.
            self.home = false;
            return self.relayout(ctx);
        }
        if units.len() < 2 {
            return FocusChange::Keep;
        }
        let next = if forward { units[1].1.clone() } else { units[units.len() - 1].1.clone() };
        if forward {
            // Send the current top unit to the bottom so repeated presses walk through all units.
            let key = units[0].0.clone();
            let member: Vec<bool> = self.stack.iter().map(|w| self.unit_key(&self.root_app(w)) == key).collect();
            let (mut top, mut rest) = (Vec::new(), Vec::new());
            for (w, m) in std::mem::take(&mut self.stack).into_iter().zip(member) {
                if m { top.push(w) } else { rest.push(w) }
            }
            top.extend(rest);
            self.stack = top;
        }
        self.raise(&next);
        self.relayout(ctx)
    }

    fn fullscreen_request(&mut self, ctx: &mut WmCtx, window: &Window, fullscreen: bool) {
        winconf::with_pending(window, |s| s.fullscreen = fullscreen);
        if fullscreen {
            // A fullscreen app leaves its pair (the partner stays just below it).
            self.unpair(window);
        }
        let is_dialog = self.is_dialog(window);
        self.configure(ctx, window, is_dialog);
        send(window);
        if self.stack.contains(window) {
            self.relayout(ctx);
        }
    }

    fn maximize_request(&mut self, ctx: &mut WmCtx, window: &Window, maximize: bool) {
        // The app's own maximize button in split view: back to the whole screen.
        if maximize && !self.is_dialog(window) && self.pair_of(window).is_some() && self.shown_roots().contains(window) {
            let f = self.snap(ctx, window, None, None);
            debug!(?f, "maximize request in split view");
            return;
        }
        // Apps are always maximized (or tiled) here; the protocol still wants a configure in reply.
        winconf::send_always(window);
    }

    fn set_home(&mut self, ctx: &mut WmCtx, home: bool) -> FocusChange {
        let home = home || self.top_app().is_none();
        if home == self.home {
            return FocusChange::Keep;
        }
        self.home = home;
        self.drag = None;
        self.relayout(ctx)
    }

    fn families(&self, ctx: &WmCtx) -> Vec<Vec<(Window, Point<i32, Logical>)>> {
        self.units()
            .iter()
            .map(|(key, _)| {
                self.stack
                    .iter()
                    .filter(|w| &self.unit_key(&self.root_app(w)) == key)
                    .map(|w| (w.clone(), self.location(ctx, w, self.is_dialog(w))))
                    .collect()
            })
            .collect()
    }

    fn snap(&mut self, ctx: &mut WmCtx, window: &Window, side: Option<SnapSide>, with: Option<&Window>) -> FocusChange {
        if !self.stack.contains(window) {
            return FocusChange::Keep;
        }
        let root = self.root_app(window);
        let Some(side) = side else {
            if self.pair_of(&root).is_none() {
                return FocusChange::Keep;
            }
            info!(app = ?app_id(&root), "split: back to full screen");
            self.unpair(&root);
            self.raise(window);
            self.home = false;
            self.relayout(ctx);
            return FocusChange::Focus(window.clone());
        };
        let chosen = with.filter(|w| self.stack.contains(w)).map(|w| self.root_app(w)).filter(|w| *w != root);
        // Already paired with the chosen app (or no choice given): only the sides may change.
        let keep_pair = self.pair_of(&root).filter(|p| chosen.as_ref().is_none_or(|c| p.has(c))).is_some();
        if keep_pair && let Some(p) = self.pair_of(&root) {
            if p.side_of(&root) != Some(side) {
                self.raise(window);
                self.home = false;
                self.swap_shown(ctx);
            }
            return FocusChange::Focus(window.clone());
        }
        // Partner: the most recent other unit; a pair gives up the app on the side `root` takes.
        let other_side = match side {
            SnapSide::Start => SnapSide::End,
            SnapSide::End => SnapSide::Start,
        };
        let partner = match chosen {
            Some(c) => c,
            None => {
                let key = self.unit_key(&root);
                let Some((_, top)) = self.units().into_iter().find(|(k, _)| *k != key) else {
                    info!(app = ?app_id(&root), "split: no other app to pair with");
                    return FocusChange::Keep;
                };
                match self.pair_of(&top) {
                    Some(p) => p.at(other_side).clone(),
                    None => top,
                }
            }
        };
        let (start, end) = match side {
            SnapSide::Start => (root.clone(), partner.clone()),
            SnapSide::End => (partner.clone(), root.clone()),
        };
        let (ms, me) = self.mins(ctx.area, &start, &end);
        let Some(ratio) = split::initial_ratio(ctx.area, &self.metrics, ms, me) else {
            info!(min_start = ms, min_end = me, "split: the apps do not fit side by side");
            return FocusChange::Keep;
        };
        self.unpair(&root);
        self.unpair(&partner);
        for w in [&start, &end] {
            if is_fullscreen(w) {
                winconf::with_pending(w, |s| s.fullscreen = false);
            }
        }
        info!(start = ?app_id(&start), end = ?app_id(&end), ratio, min_start = ms, min_end = me, "split: pair");
        self.pairs.push(Pair { start, end, ratio });
        self.raise(window);
        self.home = false;
        self.relayout(ctx);
        FocusChange::Focus(window.clone())
    }

    fn unit_apps(&self, window: &Window) -> Vec<Window> {
        let root = self.root_app(window);
        match self.pair_of(&root) {
            Some(p) => vec![p.start.clone(), p.end.clone()],
            None => vec![root],
        }
    }

    fn handle_at(&self, ctx: &WmCtx, pos: Point<f64, Logical>) -> bool {
        let Some(p) = self.shown_pair() else { return false };
        if is_fullscreen(&p.start) || is_fullscreen(&p.end) {
            return false;
        }
        let l = split::layout(ctx.area, p.ratio, self.metrics.divider);
        let g = self.metrics.grab_margin;
        let zone = match l.axis {
            Axis::Horizontal => Rectangle::new((l.divider.loc.x - g, l.divider.loc.y).into(), (l.divider.size.w + 2 * g, l.divider.size.h).into()),
            Axis::Vertical => Rectangle::new((l.divider.loc.x, l.divider.loc.y - g).into(), (l.divider.size.w, l.divider.size.h + 2 * g).into()),
        };
        zone.to_f64().contains(pos)
    }

    fn handle_drag(&mut self, ctx: &mut WmCtx, phase: HandlePhase, pos: Point<f64, Logical>, time: Duration) -> FocusChange {
        match phase {
            HandlePhase::Down => {
                self.drag = self.shown_pair().map(|_| DividerDrag { from: pos, pos, moved: false });
                FocusChange::Keep
            }
            HandlePhase::Motion => {
                if let Some(d) = self.drag.as_mut() {
                    d.pos = pos;
                    d.moved |= (pos.x - d.from.x).hypot(pos.y - d.from.y) > SLOP;
                }
                FocusChange::Keep
            }
            HandlePhase::Cancel => {
                self.drag = None;
                FocusChange::Keep
            }
            HandlePhase::Up => {
                let Some(d) = self.drag.take() else { return FocusChange::Keep };
                let Some(p) = self.shown_pair().cloned() else { return FocusChange::Keep };
                if !d.moved {
                    let double = self.last_tap.is_some_and(|t| time.saturating_sub(t) <= self.metrics.double_tap);
                    if double {
                        self.last_tap = None;
                        return self.swap_shown(ctx);
                    }
                    self.last_tap = Some(time);
                    return FocusChange::Keep;
                }
                self.last_tap = None;
                match self.release_at(ctx, &p, d.pos) {
                    Release::Snap(r) => {
                        info!(ratio = format!("{r:.3}"), "split: divider snapped");
                        if let Some(q) = self.pairs.iter_mut().find(|q| **q == p) {
                            q.ratio = r;
                        }
                        self.relayout(ctx)
                    }
                    Release::Dismiss(side) => {
                        let (gone, stays) = match side {
                            SnapSide::Start => (p.start.clone(), p.end.clone()),
                            SnapSide::End => (p.end.clone(), p.start.clone()),
                        };
                        info!(dismissed = ?app_id(&gone), stays = ?app_id(&stays), "split: dismiss");
                        self.unpair(&gone);
                        // The dismissed app goes just below the one that stays (it is the previous app now).
                        self.raise(&gone);
                        self.raise(&stays);
                        self.relayout(ctx)
                    }
                }
            }
        }
    }

    fn decorations(&self, ctx: &WmCtx) -> Vec<Decoration> {
        let Some(p) = self.shown_pair() else { return Vec::new() };
        if is_fullscreen(&p.start) || is_fullscreen(&p.end) {
            return Vec::new();
        }
        let m = &self.metrics;
        let l = split::layout(ctx.area, p.ratio, m.divider);
        let Some(d) = self.drag.filter(|d| d.moved) else {
            return vec![Decoration { rect: l.divider, role: DecoRole::Divider }];
        };
        let live = split::divider_at(ctx.area, split::pos_along(ctx.area, d.pos.x, d.pos.y), m.divider);
        let mut out = Vec::new();
        // What the divider uncovered since it was grabbed (the apps keep their size until the release).
        let reveal = match l.axis {
            Axis::Horizontal => {
                let (a, b) = (live.loc.x.min(l.divider.loc.x), (live.loc.x + live.size.w).max(l.divider.loc.x + l.divider.size.w));
                Rectangle::new((a, l.divider.loc.y).into(), (b - a, l.divider.size.h).into())
            }
            Axis::Vertical => {
                let (a, b) = (live.loc.y.min(l.divider.loc.y), (live.loc.y + live.size.h).max(l.divider.loc.y + l.divider.size.h));
                Rectangle::new((l.divider.loc.x, a).into(), (l.divider.size.w, b - a).into())
            }
        };
        out.push(Decoration { rect: reveal, role: DecoRole::Reveal });
        match self.release_at(ctx, p, d.pos) {
            Release::Snap(r) => {
                let target = split::layout(ctx.area, r, m.divider).divider;
                if target != live {
                    out.push(Decoration { rect: target, role: DecoRole::SnapMark });
                }
            }
            Release::Dismiss(side) => {
                // Everything on the dismissed side of the finger.
                let rect = match (l.axis, side) {
                    (Axis::Horizontal, SnapSide::Start) => Rectangle::new(ctx.area.loc, (live.loc.x - ctx.area.loc.x, ctx.area.size.h).into()),
                    (Axis::Horizontal, SnapSide::End) => {
                        let x = live.loc.x + live.size.w;
                        Rectangle::new((x, ctx.area.loc.y).into(), (ctx.area.loc.x + ctx.area.size.w - x, ctx.area.size.h).into())
                    }
                    (Axis::Vertical, SnapSide::Start) => Rectangle::new(ctx.area.loc, (ctx.area.size.w, live.loc.y - ctx.area.loc.y).into()),
                    (Axis::Vertical, SnapSide::End) => {
                        let y = live.loc.y + live.size.h;
                        Rectangle::new((ctx.area.loc.x, y).into(), (ctx.area.size.w, ctx.area.loc.y + ctx.area.size.h - y).into())
                    }
                };
                out.push(Decoration { rect, role: DecoRole::Dismiss });
            }
        }
        out.push(Decoration { rect: live, role: DecoRole::DividerActive });
        out
    }

    fn area_changed(&mut self, ctx: &mut WmCtx) {
        self.drag = None;
        self.relayout(ctx);
    }
}

fn app_id(w: &Window) -> Option<String> {
    winconf::app_id(w)
}
