//! The Overview (DESKTOP-SHELL.md 3.3 / Phase 4): every running app as a card in a row, most recent in the
//! middle and older ones to the left (the same direction the left-edge swipe brings them from).
//!
//! Core layer: knows windows, rectangles and touches, not the policy or the UI. Cards are the apps' own last
//! buffers drawn scaled (the same Rescale + Relocate elements as `switch.rs`), so there are no extra
//! offscreen passes: background apps are suspended and show their last frame. Each card has its own springs
//! (offset, scale, alpha), so opening from a held app, closing a card and the gap closing all move smoothly
//! and can be interrupted.
//!
//! Touch: drag sideways to scroll (snaps to a card), tap a card to go to that app, flick a card up to close
//! that app (both apps of a split pair), tap outside the cards to go Home. Drag a card down to lift it and
//! drop it on the first or last `drop_zone` of the screen (left/right; top/bottom in portrait): it stays
//! pinned in that half and the other cards move to the other half; tap the one to pair it with (with only one
//! other app it pairs at once). A tap on the pinned card or on empty space puts it back.

use std::time::Duration;

use smithay::{
    backend::renderer::element::Id,
    desktop::Window,
    utils::{Logical, Point, Rectangle},
};

use crate::{
    anim::{Spring, SpringParams, VelocityTracker},
    policy::SnapSide,
    render::batch::Rect,
    switch::Pose,
};

/// Card scale, gap between cards (px) and the row's centre (fraction of the height).
pub const SCALE: f64 = 0.5;
const GAP: f64 = 48.0;
const ROW_Y: f64 = 0.47;
/// Touch slop before a drag direction is chosen (px).
const SLOP: f32 = 10.0;
/// A card flicked up faster than this (px/s), or dragged up more than `CLOSE_DRAG` of the height, closes.
const CLOSE_FLICK: f32 = 700.0;
const CLOSE_DRAG: f64 = 0.3;
/// A lifted card (being dragged towards a split side) shrinks to this fraction of its size.
const LIFT_SCALE: f32 = 0.8;
/// Row card scale while a partner is picked, and how much of its half the pinned card fills.
const PICK_SCALE: f64 = 0.38;
const PIN_FILL: f64 = 0.9;

pub struct Card {
    /// The app and its dialogs, bottom to top, at the space positions they have when shown.
    pub windows: Vec<(Window, Point<i32, Logical>)>,
    dx: Spring,
    dy: Spring,
    sc: Spring,
    alpha: Spring,
    /// Flicked away: flying up and fading, removed when gone.
    closing: bool,
}

impl Card {
    pub fn root(&self) -> Option<&Window> {
        self.windows.first().map(|(w, _)| w)
    }
}

/// What the caller must do after a touch.
#[derive(Debug, PartialEq)]
pub enum Action {
    None,
    /// Show this app, growing from its card (already removed from the Overview) at `Pose`.
    Activate(Box<Card>, Pose),
    /// Ask this app to close (its card is flying away).
    Close(Window),
    /// Split view: `first` on `side`, paired with `second` (None: nothing to pair with, just show `first`);
    /// cards already removed; the pair grows from `pose`.
    Pair { first: Box<Card>, second: Option<Box<Card>>, side: SnapSide, pose: Pose },
    /// Leave to Home.
    Home,
}

impl PartialEq for Card {
    fn eq(&self, other: &Self) -> bool {
        self.root() == other.root()
    }
}

impl std::fmt::Debug for Card {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Card").field("windows", &self.windows.len()).field("closing", &self.closing).finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    /// Down, no direction yet; the card under the finger.
    Undecided(Option<usize>),
    Scroll { scroll0: f32 },
    Card { index: usize },
    /// A card lifted by dragging it down: it follows the finger and can be dropped on a screen side.
    Lift { index: usize },
    /// Not ours to use (e.g. dragging down on nothing).
    Ignore,
}

#[derive(Debug, Clone, Copy)]
struct TouchState {
    slot: i32,
    start: (f32, f32),
    drag: Drag,
}

/// A card dropped on one half, waiting for its partner to be picked.
struct Pinned {
    card: Card,
    side: SnapSide,
    /// Where it was in the row (it goes back there if the pick is cancelled).
    index: usize,
    cx: Spring,
    cy: Spring,
    sc: Spring,
}

pub struct Overview {
    pub cards: Vec<Card>,
    /// The app area (space coordinates); cards are scaled copies of it.
    pub area: Rectangle<i32, Logical>,
    /// Row scroll (px): 0 = the most recent app in the middle; card i is in the middle at i * pitch.
    scroll: Spring,
    /// 0 = closed, 1 = open (scrim, Home sitting back).
    pub open: Spring,
    /// Leaving (to Home or to an app): touches go to Home again, and the Overview ends when faded.
    pub leaving: bool,
    touch: Option<TouchState>,
    vel: VelocityTracker,
    last_t: Option<Duration>,
    params: SpringParams,
    /// Frames drawn and the largest gap between two (for the log line).
    pub frames: u32,
    pub max_gap: Duration,
    /// Frames that came more than 1.5 refreshes after a frame of motion.
    pub late: u32,
    /// Slowest backend render while it was open.
    pub max_render: Duration,
    /// Something moved in the last step (the caller keeps drawing).
    pub moving: bool,
    /// Home as it was when the Overview opened, rendered once into a texture. While it exists the live Home
    /// is not drawn: this fades out over the background colour as the Overview opens (one textured quad
    /// per frame instead of Home's whole draw list), and back in when it leaves. None until the first
    /// frame, or if it could not be made (then the scrim below is used).
    pub home: Option<smithay::backend::renderer::gles::GlesTexture>,
    pub home_failed: bool,
    pub home_id: Id,
    /// Frosted wallpaper + scrim (from the overlay), faded in over `home`; None = fade to the clear colour.
    pub backdrop: Option<smithay::backend::renderer::gles::GlesTexture>,
    pub backdrop_id: Id,
    /// Scrim colour at full open (premultiplied, opaque: the theme background), so an open Overview hides
    /// Home completely and the compositor skips drawing it.
    pub scrim: [f32; 4],
    /// Render element id of the scrim (kept, so damage tracking sees one element changing).
    pub scrim_id: Id,
    /// Split drop zones: highlight colour (premultiplied) and the fraction of the screen at each end.
    pub drop_color: [f32; 4],
    pub drop_zone: f32,
    pub drop_id: Id,
    pinned: Option<Pinned>,
    /// 0 = the row in the middle, 1 = the row moved to the half the pinned card leaves free (smaller cards).
    pick: Spring,
}

fn spring(v: f32, target: f32, params: SpringParams) -> Spring {
    let mut s = Spring::new(v, params).with_eps(0.002);
    s.set_target(target);
    s
}

impl Overview {
    /// Open with these app families (most recent first). `from`: where the first card is drawn now (the app
    /// a finger was holding, or the full-screen app), so it moves from there into its place.
    pub fn new(families: Vec<Vec<(Window, Point<i32, Logical>)>>, area: Rectangle<i32, Logical>, params: SpringParams, from: Option<Pose>) -> Overview {
        let mut ov = Overview {
            cards: Vec::new(),
            area,
            scroll: Spring::new(0.0, params).with_eps(0.5),
            open: spring(0.0, 1.0, params),
            leaving: false,
            touch: None,
            vel: VelocityTracker::default(),
            last_t: None,
            params,
            frames: 0,
            max_gap: Duration::ZERO,
            late: 0,
            max_render: Duration::ZERO,
            moving: true,
            home: None,
            home_failed: false,
            home_id: Id::new(),
            backdrop: None,
            backdrop_id: Id::new(),
            scrim: [0.0, 0.0, 0.0, 1.0],
            scrim_id: Id::new(),
            drop_color: [0.0; 4],
            drop_zone: 0.0,
            drop_id: Id::new(),
            pinned: None,
            pick: Spring::new(0.0, params).with_eps(0.002),
        };
        for (i, windows) in families.into_iter().enumerate() {
            let mut card = Card {
                windows,
                dx: spring(0.0, 0.0, params),
                dy: spring(0.0, 0.0, params),
                sc: spring(0.9, 1.0, params),
                alpha: spring(0.0, 1.0, params),
                closing: false,
            };
            if i == 0 && let Some(p) = from {
                let slot = ov.slot(0);
                card.dx = Spring::new((p.center.x - slot.x) as f32, params).with_eps(0.5);
                card.dx.set_target(0.0);
                card.dy = Spring::new((p.center.y - slot.y) as f32, params).with_eps(0.5);
                card.dy.set_target(0.0);
                card.sc = spring((p.scale / SCALE) as f32, 1.0, params);
                card.alpha = spring(p.alpha, 1.0, params);
            } else {
                card.dx = Spring::new(0.0, params).with_eps(0.5);
                card.dy = Spring::new(0.0, params).with_eps(0.5);
            }
            ov.cards.push(card);
        }
        ov
    }

    /// Row card scale now (smaller while a partner is picked).
    fn scale(&self) -> f64 {
        SCALE + (PICK_SCALE - SCALE) * self.pick.value as f64
    }

    fn pitch(&self) -> f64 {
        self.area.size.w as f64 * self.scale() + GAP
    }

    fn landscape(&self) -> bool {
        self.area.size.w >= self.area.size.h
    }

    /// The half of the area on `side` (left/right, or top/bottom in portrait).
    fn half(&self, side: SnapSide) -> Rectangle<i32, Logical> {
        let a = self.area;
        if self.landscape() {
            let w = a.size.w / 2;
            let x = if side == SnapSide::Start { a.loc.x } else { a.loc.x + a.size.w - w };
            Rectangle::new((x, a.loc.y).into(), (w, a.size.h).into())
        } else {
            let h = a.size.h / 2;
            let y = if side == SnapSide::Start { a.loc.y } else { a.loc.y + a.size.h - h };
            Rectangle::new((a.loc.x, y).into(), (a.size.w, h).into())
        }
    }

    /// Centre and scale of a card filling (`PIN_FILL` of) the half on `side`.
    fn pin_target(&self, side: SnapSide) -> (Point<f64, Logical>, f64) {
        let h = self.half(side).to_f64();
        let a = self.area.to_f64();
        let fit = (h.size.w / a.size.w).min(h.size.h / a.size.h) * PIN_FILL;
        (Point::from((h.loc.x + h.size.w / 2.0, h.loc.y + h.size.h / 2.0)), fit)
    }

    /// The pinned card (waiting for a partner) and where it is drawn.
    pub fn pinned(&self) -> Option<(&Card, Pose)> {
        let p = self.pinned.as_ref()?;
        let pose = Pose { scale: p.sc.value as f64, center: Point::from((p.cx.value as f64, p.cy.value as f64)), alpha: p.card.alpha.value.clamp(0.0, 1.0) };
        Some((&p.card, pose))
    }

    fn pinned_hit(&self, x: f32, y: f32) -> bool {
        self.pinned().is_some_and(|(_, p)| {
            let (w, h) = (self.area.size.w as f64 * p.scale, self.area.size.h as f64 * p.scale);
            (x as f64 - p.center.x).abs() < w / 2.0 && (y as f64 - p.center.y).abs() < h / 2.0
        })
    }

    /// Put the pinned card back into the row.
    fn unpin(&mut self) {
        let Some(p) = self.pinned.take() else { return };
        let mut card = p.card;
        card.dx = Spring::new(0.0, self.params).with_eps(0.5);
        card.dy = Spring::new(0.0, self.params).with_eps(0.5);
        card.sc = spring(0.9, 1.0, self.params);
        let i = p.index.min(self.cards.len());
        self.cards.insert(i, card);
        self.pick.set_target(0.0);
        self.clamp_scroll();
    }

    /// The half to highlight: where a lifted card would go, or the pinned card's half.
    pub fn highlight(&self) -> Option<Rectangle<i32, Logical>> {
        if let Some((_, half)) = self.drop_target() {
            return Some(half);
        }
        self.pinned.as_ref().map(|p| self.half(p.side))
    }

    fn max_scroll(&self) -> f32 {
        (self.cards.len().saturating_sub(1) as f64 * self.pitch()) as f32
    }

    /// Centre of card slot `i` at the current scroll.
    fn slot(&self, i: usize) -> Point<f64, Logical> {
        let a = self.area.to_f64();
        let (mut cx, mut cy) = (a.loc.x + a.size.w / 2.0, a.loc.y + a.size.h * ROW_Y);
        // While picking a partner the row moves to the half the pinned card leaves free.
        if let Some(p) = self.pinned.as_ref() {
            let other = if p.side == SnapSide::Start { SnapSide::End } else { SnapSide::Start };
            let h = self.half(other).to_f64();
            let k = self.pick.value as f64;
            cx += (h.loc.x + h.size.w / 2.0 - cx) * k;
            cy += (h.loc.y + h.size.h / 2.0 - cy) * k;
        }
        Point::from((cx - i as f64 * self.pitch() + self.scroll.value as f64, cy))
    }

    pub fn card_pose(&self, i: usize) -> Pose {
        let c = &self.cards[i];
        let slot = self.slot(i);
        Pose {
            scale: self.scale() * c.sc.value as f64,
            center: Point::from((slot.x + c.dx.value as f64, slot.y + c.dy.value as f64)),
            alpha: c.alpha.value.clamp(0.0, 1.0),
        }
    }

    pub fn card_rect(&self, i: usize) -> Rect {
        let p = self.card_pose(i);
        let (w, h) = (self.area.size.w as f64 * p.scale, self.area.size.h as f64 * p.scale);
        Rect::new((p.center.x - w / 2.0) as f32, (p.center.y - h / 2.0) as f32, w as f32, h as f32)
    }

    /// The card being dragged (drawn on top), if any.
    pub fn dragged(&self) -> Option<usize> {
        match self.touch.map(|t| t.drag) {
            Some(Drag::Card { index } | Drag::Lift { index }) => Some(index),
            _ => None,
        }
    }

    fn lifted(&self) -> Option<usize> {
        match self.touch.map(|t| t.drag) {
            Some(Drag::Lift { index }) => Some(index),
            _ => None,
        }
    }

    /// The screen side a lifted card would be dropped on now, with the half it would take (for the
    /// highlight). Side by side when the area is wider than tall, else top/bottom.
    pub fn drop_target(&self) -> Option<(SnapSide, Rectangle<i32, Logical>)> {
        let index = self.lifted()?;
        let r = self.card_rect(index);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let finger = self.vel.last().unwrap_or((cx, cy));
        let a = self.area;
        let z = self.drop_zone.clamp(0.0, 0.5);
        let (pos, a0, len) = if a.size.w >= a.size.h {
            (finger.0, a.loc.x as f32, a.size.w as f32)
        } else {
            (finger.1, a.loc.y as f32, a.size.h as f32)
        };
        let side = if pos < a0 + len * z {
            SnapSide::Start
        } else if pos > a0 + len * (1.0 - z) {
            SnapSide::End
        } else {
            return None;
        };
        Some((side, self.half(side)))
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        (0..self.cards.len()).find(|&i| {
            let r = self.card_rect(i);
            !self.cards[i].closing && x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
        })
    }

    /// Fade out (to Home, or because an app is being shown).
    pub fn leave(&mut self) {
        self.leaving = true;
        self.touch = None;
        self.open.set_target(0.0);
        for c in &mut self.cards {
            c.alpha.set_target(0.0);
            c.sc.set_target(0.85);
        }
        if let Some(p) = self.pinned.as_mut() {
            p.card.alpha.set_target(0.0);
        }
    }

    /// Forget a destroyed window (its card goes away at once).
    pub fn forget(&mut self, w: &Window) {
        for c in &mut self.cards {
            c.windows.retain(|(x, _)| x != w);
        }
        if let Some(p) = self.pinned.as_mut() {
            p.card.windows.retain(|(x, _)| x != w);
            if p.card.windows.is_empty() {
                self.pinned = None;
                self.pick.set_target(0.0);
            }
        }
        let before = self.cards.len();
        self.cards.retain(|c| !c.windows.is_empty());
        if self.cards.len() != before {
            self.touch = None;
            self.clamp_scroll();
        }
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        let t = self.scroll.target.clamp(0.0, max);
        self.scroll.set_target(t);
    }

    pub fn is_done(&self) -> bool {
        self.leaving
            && self.open.is_resting()
            && self.cards.iter().all(|c| c.alpha.is_resting())
            && self.pinned.as_ref().is_none_or(|p| p.card.alpha.is_resting())
    }

    /// Step to the presentation time of the frame being built. Returns true while anything moves.
    pub fn step(&mut self, at: Duration) -> bool {
        self.frames += 1;
        let dt = match self.last_t {
            Some(last) if at > last => {
                // Only between frames of motion (nothing is drawn while the Overview rests).
                if self.moving {
                    self.max_gap = self.max_gap.max(at - last);
                    if at - last > Duration::from_millis(25) {
                        self.late += 1;
                    }
                }
                (at - last).as_secs_f32().min(0.1)
            }
            Some(_) => 0.0,
            None => 1.0 / 60.0,
        };
        self.last_t = Some(at);
        let dragged = self.dragged();
        let scrolling = matches!(self.touch.map(|t| t.drag), Some(Drag::Scroll { .. }));
        let mut moving = self.open.step(dt) | self.pick.step(dt);
        if let Some(p) = self.pinned.as_mut() {
            moving |= p.cx.step(dt) | p.cy.step(dt) | p.sc.step(dt) | p.card.alpha.step(dt);
        }
        if !scrolling {
            moving |= self.scroll.step(dt);
        }
        let lifted = self.lifted();
        for (i, c) in self.cards.iter_mut().enumerate() {
            moving |= c.sc.step(dt) | c.alpha.step(dt);
            if lifted != Some(i) {
                moving |= c.dx.step(dt);
            }
            if dragged != Some(i) {
                moving |= c.dy.step(dt);
            }
        }
        // Cards flicked away are removed once gone; the ones to their left move over by one slot.
        if let Some(k) = self.cards.iter().position(|c| c.closing && c.alpha.is_resting()) {
            self.cards.remove(k);
            let pitch = self.pitch() as f32;
            for c in self.cards.iter_mut().skip(k) {
                c.dx = Spring::new(c.dx.value - pitch, self.params).with_eps(0.5);
                c.dx.set_target(0.0);
            }
            self.clamp_scroll();
            moving = true;
        }
        self.moving = moving;
        moving
    }

    pub fn touch_down(&mut self, slot: i32, x: f32, y: f32, t: Duration) {
        if self.touch.is_some() || self.leaving {
            return;
        }
        self.vel.reset();
        self.vel.add(t, x, y);
        self.touch = Some(TouchState { slot, start: (x, y), drag: Drag::Undecided(if self.pinned_hit(x, y) { None } else { self.hit(x, y) }) });
    }

    pub fn touch_motion(&mut self, slot: i32, x: f32, y: f32, t: Duration) {
        let Some(mut ts) = self.touch.filter(|ts| ts.slot == slot) else { return };
        self.vel.add(t, x, y);
        let (dx, dy) = (x - ts.start.0, y - ts.start.1);
        if let Drag::Undecided(hit) = ts.drag {
            if dx.abs().max(dy.abs()) < SLOP {
                return;
            }
            ts.drag = if dx.abs() >= dy.abs() {
                Drag::Scroll { scroll0: self.scroll.value }
            } else if dy < 0.0 && let Some(index) = hit {
                Drag::Card { index }
            } else if let Some(index) = hit.filter(|_| self.pinned.is_none()) {
                // Down: lift the card (it shrinks a little and follows the finger).
                if let Some(c) = self.cards.get_mut(index) {
                    c.sc.set_target(LIFT_SCALE);
                }
                Drag::Lift { index }
            } else {
                Drag::Ignore
            };
        }
        let (vx, vy) = self.vel.velocity();
        match ts.drag {
            Drag::Scroll { scroll0 } => {
                // Rubber band past the ends.
                let raw = scroll0 + dx;
                let max = self.max_scroll();
                let v = if raw < 0.0 { raw * 0.35 } else if raw > max { max + (raw - max) * 0.35 } else { raw };
                self.scroll.set(v, vx);
            }
            Drag::Card { index } => {
                let v = if dy > 0.0 { dy * 0.2 } else { dy };
                if let Some(c) = self.cards.get_mut(index) {
                    c.dy.set(v, vy);
                }
            }
            Drag::Lift { index } => {
                if let Some(c) = self.cards.get_mut(index) {
                    c.dx.set(dx, vx);
                    c.dy.set(dy, vy);
                }
            }
            _ => {}
        }
        self.touch = Some(ts);
    }

    pub fn touch_up(&mut self, slot: i32, t: Duration) -> Action {
        let Some(ts) = self.touch.filter(|ts| ts.slot == slot) else { return Action::None };
        let drop = self.drop_target();
        self.touch = None;
        if let Some((x, y)) = self.vel.last() {
            self.vel.add(t, x, y);
        }
        let (vx, vy) = self.vel.velocity();
        match ts.drag {
            Drag::Undecided(Some(i)) => {
                let pose = self.card_pose(i);
                let card = self.cards.remove(i);
                if let Some(p) = self.pinned.take() {
                    // The partner was picked.
                    self.leave();
                    return Action::Pair { first: Box::new(p.card), second: Some(Box::new(card)), side: p.side, pose };
                }
                self.leave();
                Action::Activate(Box::new(card), pose)
            }
            Drag::Undecided(None) if self.pinned.is_some() => {
                // On the pinned card or empty space: cancel the pick.
                self.unpin();
                Action::None
            }
            Drag::Undecided(None) => {
                self.leave();
                Action::Home
            }
            Drag::Scroll { .. } => {
                // Snap to the card nearest to where the flick would carry the row.
                let pitch = self.pitch() as f32;
                let projected = self.scroll.value + vx * 0.2;
                let target = ((projected / pitch).round() * pitch).clamp(0.0, self.max_scroll());
                self.scroll.velocity = vx;
                self.scroll.set_target(target);
                Action::None
            }
            Drag::Card { index } => {
                let h = self.area.size.h as f64;
                let Some(c) = self.cards.get_mut(index) else { return Action::None };
                if vy < -CLOSE_FLICK || (c.dy.value as f64) < -CLOSE_DRAG * h {
                    c.closing = true;
                    c.dy.set_target((-h * 0.9) as f32);
                    c.alpha.set_target(0.0);
                    match c.root().cloned() {
                        Some(w) => Action::Close(w),
                        None => Action::None,
                    }
                } else {
                    c.dy.set_target(0.0);
                    Action::None
                }
            }
            Drag::Lift { index } => {
                if let Some((side, _)) = drop
                    && index < self.cards.len()
                {
                    let pose = self.card_pose(index);
                    let card = self.cards.remove(index);
                    let others: Vec<usize> = (0..self.cards.len()).filter(|&i| !self.cards[i].closing).collect();
                    if others.len() <= 1 {
                        // Nothing to choose between: pair at once (or just show it).
                        let second = others.first().map(|&i| Box::new(self.cards.remove(i)));
                        self.leave();
                        return Action::Pair { first: Box::new(card), second, side, pose };
                    }
                    // Pin it in that half; the row moves to the other half for the pick.
                    let (center, scale) = self.pin_target(side);
                    let mk = |from: f64, to: f64| {
                        let mut sp = Spring::new(from as f32, self.params).with_eps(0.5);
                        sp.set_target(to as f32);
                        sp
                    };
                    let mut sc = Spring::new(pose.scale as f32, self.params).with_eps(0.002);
                    sc.set_target(scale as f32);
                    self.pinned = Some(Pinned { card, side, index, cx: mk(pose.center.x, center.x), cy: mk(pose.center.y, center.y), sc });
                    self.pick.set_target(1.0);
                    self.scroll.set_target(0.0);
                    return Action::None;
                }
                if let Some(c) = self.cards.get_mut(index) {
                    c.dx.set_target(0.0);
                    c.dy.set_target(0.0);
                    c.sc.set_target(1.0);
                }
                Action::None
            }
            Drag::Ignore => Action::None,
        }
    }

    /// Cards still showing (not flicked away).
    pub fn live_cards(&self) -> usize {
        self.cards.iter().filter(|c| !c.closing).count() + usize::from(self.pinned.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: SpringParams = SpringParams { stiffness: 380.0, damping: 1.0 };

    fn area() -> Rectangle<i32, Logical> {
        Rectangle::from_size((1366, 768).into())
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(1000 + n)
    }

    fn settle(ov: &mut Overview, mut t: u64) -> u64 {
        while ov.step(ms(t)) {
            t += 16;
            assert!(t < 5000);
        }
        t
    }

    #[test]
    fn opens_from_the_held_pose_and_settles_in_the_middle() {
        let from = Pose { scale: 0.7, center: (700.0, 300.0).into(), alpha: 1.0 };
        let mut ov = Overview::new(vec![Vec::new(), Vec::new()], area(), P, Some(from));
        let p0 = ov.card_pose(0);
        assert!((p0.scale - 0.7).abs() < 1e-5 && (p0.center.x - 700.0).abs() < 0.01 && (p0.center.y - 300.0).abs() < 0.01, "{p0:?}");
        settle(&mut ov, 0);
        let p = ov.card_pose(0);
        assert_eq!(p.scale, SCALE);
        assert!((p.center.x - 683.0).abs() < 1.0 && (p.center.y - 768.0 * ROW_Y).abs() < 1.0, "{p:?}");
        // The previous app is to the left.
        assert!(ov.card_pose(1).center.x < 0.0 + 683.0 - 600.0);
    }

    #[test]
    fn scroll_snaps_and_tap_picks_the_card() {
        let mut ov = Overview::new(vec![Vec::new(), Vec::new(), Vec::new()], area(), P, None);
        let t = settle(&mut ov, 0);
        // Drag right by most of a card: the previous app comes to the middle.
        ov.touch_down(0, 300.0, 360.0, ms(t));
        for k in 1..=10 {
            ov.touch_motion(0, 300.0 + 50.0 * k as f32, 360.0, ms(t + 16 * k));
        }
        // Lift after a short rest: no flick, the nearest card wins.
        assert_eq!(ov.touch_up(0, ms(t + 300)), Action::None);
        let t = settle(&mut ov, t + 300);
        assert!((ov.card_pose(1).center.x - 683.0).abs() < 1.0, "{:?}", ov.card_pose(1));
        // Tap it.
        ov.touch_down(0, 683.0, 360.0, ms(t));
        let Action::Activate(_, pose) = ov.touch_up(0, ms(t + 60)) else { panic!() };
        assert!((pose.center.x - 683.0).abs() < 1.0);
        assert!(ov.leaving);
    }

    #[test]
    fn card_dragged_down_drops_on_a_side() {
        let mut ov = Overview::new(vec![Vec::new(), Vec::new()], area(), P, None);
        ov.drop_zone = 0.25;
        let t = settle(&mut ov, 0);
        // Down onto the right quarter.
        ov.touch_down(0, 683.0, 360.0, ms(t));
        // Down first (that lifts it), then over to the right.
        ov.touch_motion(0, 683.0, 400.0, ms(t + 16));
        for k in 2..=10 {
            ov.touch_motion(0, 683.0 + 50.0 * k as f32, 400.0 + 10.0 * k as f32, ms(t + 16 * k));
        }
        assert_eq!(ov.dragged(), Some(0));
        assert_eq!(ov.drop_target().map(|d| d.0), Some(SnapSide::End));
        // Only one other app: paired at once.
        let Action::Pair { side, second: Some(_), .. } = ov.touch_up(0, ms(t + 200)) else { panic!() };
        assert_eq!(side, SnapSide::End);
        assert!(ov.leaving);
        // Dropped in the middle: back into the row.
        let mut ov = Overview::new(vec![Vec::new()], area(), P, None);
        ov.drop_zone = 0.25;
        let t = settle(&mut ov, 0);
        ov.touch_down(0, 683.0, 360.0, ms(t));
        ov.touch_motion(0, 690.0, 460.0, ms(t + 16));
        assert_eq!(ov.drop_target(), None);
        assert_eq!(ov.touch_up(0, ms(t + 50)), Action::None);
        settle(&mut ov, t + 50);
        assert!((ov.card_pose(0).center.x - 683.0).abs() < 1.0);
    }

    #[test]
    fn dropped_card_waits_for_its_partner() {
        let mut ov = Overview::new(vec![Vec::new(), Vec::new(), Vec::new()], area(), P, None);
        ov.drop_zone = 0.25;
        let t = settle(&mut ov, 0);
        // Lift the middle card and drop it on the left quarter.
        ov.touch_down(0, 683.0, 360.0, ms(t));
        ov.touch_motion(0, 683.0, 400.0, ms(t + 16));
        for k in 2..=10 {
            ov.touch_motion(0, 683.0 - 60.0 * k as f32, 400.0, ms(t + 16 * k));
        }
        assert_eq!(ov.touch_up(0, ms(t + 200)), Action::None);
        assert!(!ov.leaving);
        let t = settle(&mut ov, t + 200);
        let (_, pin) = ov.pinned().unwrap();
        assert!(pin.center.x < 683.0 / 2.0 + 1.0, "{pin:?}");
        // The row now sits in the right half; its first card is the partner choice.
        let c = ov.card_pose(0);
        assert!((c.center.x - 1366.0 * 0.75).abs() < 1.0, "{c:?}");
        ov.touch_down(0, c.center.x as f32, c.center.y as f32, ms(t));
        let Action::Pair { side, second: Some(_), .. } = ov.touch_up(0, ms(t + 60)) else { panic!() };
        assert_eq!(side, SnapSide::Start);
        assert!(ov.leaving);
    }

    #[test]
    fn tap_outside_goes_home() {
        let mut ov = Overview::new(vec![Vec::new()], area(), P, None);
        let t = settle(&mut ov, 0);
        ov.touch_down(0, 683.0, 740.0, ms(t));
        assert_eq!(ov.touch_up(0, ms(t + 50)), Action::Home);
        settle(&mut ov, t + 50);
        assert!(ov.is_done());
    }

    #[test]
    fn flick_up_closes_and_the_row_closes_the_gap() {
        let mut ov = Overview::new(vec![Vec::new(), Vec::new()], area(), P, None);
        let t = settle(&mut ov, 0);
        // Flick the middle card (index 0) up. Cards have no windows here, so no Close action, but it flies.
        ov.touch_down(0, 683.0, 400.0, ms(t));
        for k in 1..=4 {
            ov.touch_motion(0, 683.0, 400.0 - 40.0 * k as f32, ms(t + 16 * k));
        }
        assert_eq!(ov.dragged(), Some(0));
        ov.touch_up(0, ms(t + 70));
        assert_eq!(ov.live_cards(), 1);
        settle(&mut ov, t + 70);
        assert_eq!(ov.cards.len(), 1);
        // The remaining card is now in the middle.
        assert!((ov.card_pose(0).center.x - 683.0).abs() < 1.0, "{:?}", ov.card_pose(0));
    }
}
