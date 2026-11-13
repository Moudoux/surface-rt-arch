//! App switch animations (DESKTOP-SHELL.md Phase 3 "switch animations", base for Phase 4's Home gesture).
//!
//! Core layer: knows windows and rectangles, not the policy or the UI. A `Switch` holds copies of the app's
//! windows (the app and its dialogs, at their space positions, taken from the `Space` around the policy
//! call) and one progress spring `p`: 0 = the app fills its area, 1 = the app sits at its target (its tile
//! on Home, or nothing). Going Home runs p 0 -> 1, going into an app 1 -> 0; retargeting keeps the velocity,
//! so either direction can be reversed at any time. Phase 4's gesture will drive `p` with `Spring::set`
//! while a finger is down and release it with its velocity.
//!
//! The windows are drawn by `render::output_elements` as Smithay `Rescale` + `Relocate` elements above the
//! backdrop (Home's UI layer, or the previous app drawn full size). While a switch runs, direct scanout is
//! off by itself (the UI layer is below the app); it comes back on the first frame after.

use std::time::Duration;

use smithay::{
    desktop::Window,
    utils::{Logical, Point, Rectangle},
};

use crate::{
    anim::{Spring, SpringParams},
    render::batch::Rect,
};

/// What is drawn behind the moving app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// The shell UI (Home): it is drawn, and stays visible until the app has fully grown.
    Home,
    /// The previous app, full size and still (app -> app switch).
    App,
}

/// How the moving app travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Into its tile on Home (or to the middle), or growing over the previous app.
    Tile,
    /// Sliding in from the left edge over the previous app (left-edge swipe), p = 1 is off screen.
    Slide,
    /// Pulled down from the top to close it: p = 1 is below the screen (top-edge pull).
    Close,
    /// Growing from / shrinking into a fixed card rectangle (`tile`, set by the caller, e.g. an Overview card),
    /// fully opaque all the way.
    Card,
}

/// A finger holding the app: the point of the app under the finger stays under it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anchor {
    /// The grabbed point relative to the app's centre, in unscaled app pixels.
    u: Point<f64, Logical>,
    /// Extra offset kept from the moment of the grab (non-zero only if the finger was off the app).
    e: Point<f64, Logical>,
    finger: Point<f64, Logical>,
}

/// Where the moving app is drawn this frame: scaled by `scale` about the centre of its area, which is
/// moved to `center`; `alpha` multiplies its opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub scale: f64,
    pub center: Point<f64, Logical>,
    pub alpha: f32,
}

pub struct Switch {
    /// The moving app family, bottom to top, at their space locations.
    pub windows: Vec<(Window, Point<i32, Logical>)>,
    /// Drawn full size under the moving app (`Backdrop::App`).
    pub under: Vec<(Window, Point<i32, Logical>)>,
    pub backdrop: Backdrop,
    pub style: Style,
    /// A finger drives `p` (the spring does not step); see `grab`, `drag`, `release`.
    pub held: bool,
    anchor: Option<Anchor>,
    /// Where the app was drawn relative to its base pose when the finger let go; fades out as `p` reaches
    /// its target (`offset_p` = p at that moment), so a release never jumps.
    offset: Point<f64, Logical>,
    offset_p: f32,
    /// Fades the offset when `p` is already (almost) at its target at the release (1 -> 0).
    fade: Spring,
    /// The moving app's xdg app_id (lowercased), to find its tile.
    pub app_id: Option<String>,
    pub p: Spring,
    /// The tile rectangle the app shrinks into / grows from (screen pixels), refreshed every frame from the
    /// overlay so it follows Home's layout; None = no tile on screen.
    pub tile: Option<Rect>,
    /// The rectangle that was tapped to start the switch (used while the overlay has no tile on screen for
    /// the app, e.g. an entry in All apps).
    pub tile_hint: Option<Rect>,
    /// The area the app fills when shown (space coordinates).
    pub area: Rectangle<i32, Logical>,
    last_t: Option<Duration>,
    /// Frames drawn, first presentation time and the largest gap between two frames (for the log line).
    pub frames: u32,
    first_t: Option<Duration>,
    pub max_gap: Duration,
    /// Slowest backend render (CPU time in `render`) while the switch ran.
    pub max_render: Duration,
    /// Frames drawn while a finger held the app (before `release` restarted the counters).
    pub held_frames: u32,
    /// Frames that came more than 1.5 refreshes after the previous one: (frame index, gap ms).
    pub late: Vec<(u32, u32)>,
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Scale of an app shrinking to the middle when there is no tile, and of an app growing over another.
const NO_TILE_SCALE: f64 = 0.4;
const OVER_APP_SCALE: f64 = 0.92;
/// Scale of an app pulled all the way down to close it, reached at p = `CLOSE_HELD` (the most a finger
/// drives it to); from there to 1 it drops off the bottom and fades.
const CLOSE_SCALE: f64 = 0.4;
pub const CLOSE_HELD: f32 = 0.6;

impl Switch {
    /// A switch starting at progress `from` (0 = app shown, 1 = app gone) heading to `to`.
    pub fn new(
        windows: Vec<(Window, Point<i32, Logical>)>,
        backdrop: Backdrop,
        app_id: Option<String>,
        area: Rectangle<i32, Logical>,
        params: SpringParams,
        from: f32,
        to: f32,
    ) -> Switch {
        let mut p = Spring::new(from, params).with_eps(0.002);
        p.set_target(to);
        Switch {
            windows,
            under: Vec::new(),
            backdrop,
            style: Style::Tile,
            held: false,
            anchor: None,
            offset: Point::default(),
            offset_p: 0.0,
            fade: Spring::new(1.0, params).with_eps(0.002),
            app_id, p, tile: None, tile_hint: None, area, last_t: None, frames: 0, held_frames: 0, first_t: None, max_gap: Duration::ZERO, max_render: Duration::ZERO, late: Vec::new() }
    }

    /// True if the app ends up shown (heading to p = 0).
    pub fn entering(&self) -> bool {
        self.p.target < 0.5
    }

    /// Reverse or confirm the direction; the current velocity carries over.
    pub fn head(&mut self, enter: bool) {
        // Re-base the release offset at the current progress so it fades out towards the new end.
        self.offset = self.offset_now();
        self.offset_p = self.p.value;
        self.fade = Spring::new(1.0, self.fade.params).with_eps(0.002);
        self.fade.set_target(0.0);
        self.p.set_target(if enter { 0.0 } else { 1.0 });
    }

    /// A finger takes hold of the app at `finger` (screen position). From now on `drag` moves it.
    pub fn grab(&mut self, finger: Point<f64, Logical>, anchored: bool) {
        let pose = self.pose();
        self.held = true;
        self.p.set(self.p.value, 0.0);
        self.anchor = anchored.then(|| {
            let a = self.area.to_f64();
            let s = pose.scale.max(0.05);
            let raw = (finger - pose.center).downscale(s);
            let u = Point::from((raw.x.clamp(-a.size.w / 2.0, a.size.w / 2.0), raw.y.clamp(-a.size.h / 2.0, a.size.h / 2.0)));
            let e = pose.center - (finger - u.upscale(s));
            Anchor { u, e, finger }
        });
        self.offset = Point::default();
    }

    /// The finger moved: progress `p` with its rate `velocity` (per second), finger at `finger`.
    pub fn drag(&mut self, finger: Point<f64, Logical>, p: f32, velocity: f32) {
        self.p.set(p, velocity);
        if let Some(a) = self.anchor.as_mut() {
            a.finger = finger;
        }
    }

    /// The finger let go: keep where the app is (as an offset that fades out) and head for an end with the
    /// finger's velocity.
    pub fn release(&mut self, enter: bool) {
        if self.anchor.is_some() {
            let base = self.base_pose();
            self.offset = self.pose().center - base.center;
            self.anchor = None;
        }
        self.offset_p = self.p.value;
        self.fade = Spring::new(1.0, self.fade.params).with_eps(0.002);
        self.held = false;
        // Timing restarts: the finger may have rested before lifting (no frames then), and the log line
        // should show the animation after the release.
        self.held_frames = self.frames;
        self.frames = 0;
        self.first_t = None;
        self.last_t = None;
        self.max_gap = Duration::ZERO;
        self.late.clear();
        self.head(enter);
    }

    /// From the first to the last frame drawn.
    pub fn duration(&self) -> Duration {
        match (self.first_t, self.last_t) {
            (Some(a), Some(b)) => b.saturating_sub(a),
            _ => Duration::ZERO,
        }
    }

    pub fn is_done(&self) -> bool {
        !self.held && self.p.is_resting() && (self.fade.is_resting() || self.offset == Point::default())
    }

    /// Step to the presentation time of the frame being built. Returns true while moving.
    pub fn step(&mut self, at: Duration) -> bool {
        self.frames += 1;
        self.first_t.get_or_insert(at);
        if let Some(last) = self.last_t {
            let gap = at.saturating_sub(last);
            self.max_gap = self.max_gap.max(gap);
            if gap > Duration::from_millis(25) && self.late.len() < 8 {
                self.late.push((self.frames - 1, gap.as_millis() as u32));
            }
        }
        let dt = match self.last_t {
            Some(last) if at > last => (at - last).as_secs_f32().min(0.1),
            Some(_) => 0.0,
            // The first frame already moves one refresh: the key press was a frame ago.
            None => 1.0 / 60.0,
        };
        self.last_t = Some(at);
        if self.held {
            return false;
        }
        let fading = self.fade.step(dt);
        self.p.step(dt) || (fading && self.offset != Point::default())
    }

    /// Forget a destroyed window. Returns true if nothing is left to draw moving.
    pub fn forget(&mut self, w: &Window) -> bool {
        self.windows.retain(|(x, _)| x != w);
        self.under.retain(|(x, _)| x != w);
        self.windows.is_empty()
    }

    /// The release offset scaled down by how far `p` has gone towards its target since the release.
    fn offset_now(&self) -> Point<f64, Logical> {
        let (p, p0) = (self.p.value.clamp(0.0, 1.0), self.offset_p.clamp(0.0, 1.0));
        // Proportional to the way left, so the app lands exactly at its end; if there is (almost) no way
        // left, by time.
        let left = if self.p.target >= 0.5 { 1.0 - p0 } else { p0 };
        let k = if left < 0.05 {
            self.fade.value.clamp(0.0, 1.0)
        } else if self.p.target >= 0.5 {
            ((1.0 - p) / (1.0 - p0)).clamp(0.0, 1.0)
        } else {
            (p / p0).clamp(0.0, 1.0)
        };
        self.offset.upscale(k as f64)
    }

    /// The pose at progress `p` without a finger or release offset.
    fn base_pose(&self) -> Pose {
        let p = self.p.value.clamp(0.0, 1.0);
        let a = self.area.to_f64();
        let ac = Point::<f64, Logical>::from((a.loc.x + a.size.w / 2.0, a.loc.y + a.size.h / 2.0));
        let lerp = |x: f64, y: f64| x + (y - x) * p as f64;
        let (target_scale, target_center, alpha) = match (self.style, self.backdrop, self.tile) {
            (Style::Slide, _, _) => (1.0, Point::from((ac.x - a.size.w, ac.y)), 1.0),
            (Style::Close, _, _) => {
                let q = (p / CLOSE_HELD).min(1.0) as f64;
                let pose = Pose {
                    scale: 1.0 + (CLOSE_SCALE - 1.0) * q,
                    center: Point::from((ac.x, lerp(ac.y, ac.y + a.size.h))),
                    alpha: 1.0 - smoothstep(0.75, 1.0, p),
                };
                return pose;
            }
            (Style::Card, _, Some(t)) => {
                let s = (t.w as f64 / a.size.w).min(t.h as f64 / a.size.h);
                (s, Point::from(((t.x + t.w / 2.0) as f64, (t.y + t.h / 2.0) as f64)), 1.0)
            }
            (Style::Card, _, None) => (NO_TILE_SCALE, ac, 1.0 - smoothstep(0.5, 1.0, p)),
            (Style::Tile, Backdrop::App, _) => (OVER_APP_SCALE, ac, 1.0 - smoothstep(0.0, 1.0, p)),
            (Style::Tile, Backdrop::Home, Some(t)) => {
                // Fit inside the tile, then cross-fade into it over the last part.
                let s = (t.w as f64 / a.size.w).min(t.h as f64 / a.size.h);
                let c = Point::from(((t.x + t.w / 2.0) as f64, (t.y + t.h / 2.0) as f64));
                (s, c, 1.0 - smoothstep(0.55, 1.0, p))
            }
            (Style::Tile, Backdrop::Home, None) => (NO_TILE_SCALE, ac, 1.0 - smoothstep(0.5, 1.0, p)),
        };
        Pose {
            scale: lerp(1.0, target_scale),
            center: Point::from((lerp(ac.x, target_center.x), lerp(ac.y, target_center.y))),
            alpha,
        }
    }

    pub fn pose(&self) -> Pose {
        let mut pose = self.base_pose();
        match &self.anchor {
            Some(a) => pose.center = a.finger - a.u.upscale(pose.scale) + a.e,
            None => pose.center += self.offset_now(),
        }
        pose
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: SpringParams = SpringParams { stiffness: 380.0, damping: 1.0 };

    fn sw(tile: Option<Rect>, backdrop: Backdrop, p: f32) -> Switch {
        let mut s = Switch::new(Vec::new(), backdrop, None, Rectangle::from_size((1366, 768).into()), P, p, p);
        s.tile = tile;
        s
    }

    #[test]
    fn pose_ends() {
        let tile = Some(Rect::new(100.0, 200.0, 148.0, 148.0));
        let full = sw(tile, Backdrop::Home, 0.0).pose();
        assert_eq!(full, Pose { scale: 1.0, center: (683.0, 384.0).into(), alpha: 1.0 });
        let gone = sw(tile, Backdrop::Home, 1.0).pose();
        assert!((gone.scale - 148.0 / 1366.0).abs() < 1e-6);
        assert_eq!(gone.center, (174.0, 274.0).into());
        assert_eq!(gone.alpha, 0.0);
        let mid = sw(None, Backdrop::Home, 0.7).pose();
        assert!(mid.alpha > 0.0 && mid.alpha < 1.0 && mid.scale < 1.0);
    }

    #[test]
    fn reverse_keeps_velocity() {
        let mut s = sw(None, Backdrop::Home, 0.0);
        s.head(false);
        let t0 = Duration::from_secs(1);
        s.step(t0);
        s.step(t0 + Duration::from_millis(50));
        let v = s.p.velocity;
        assert!(v > 0.0);
        s.head(true);
        assert!(s.entering());
        assert_eq!(s.p.velocity, v);
        let mut t = t0 + Duration::from_millis(50);
        while s.step(t) {
            t += Duration::from_millis(16);
            assert!(t < t0 + Duration::from_secs(3));
        }
        assert_eq!(s.p.value, 0.0);
    }

    fn run(s: &mut Switch, t0: Duration) -> Vec<Pose> {
        let mut out = Vec::new();
        let mut t = t0;
        while s.step(t) {
            out.push(s.pose());
            t += Duration::from_millis(16);
            assert!(t < t0 + Duration::from_secs(3));
        }
        out.push(s.pose());
        out
    }

    #[test]
    fn finger_drag_and_release_never_jump() {
        let tile = Some(Rect::new(100.0, 200.0, 148.0, 148.0));
        let mut s = sw(tile, Backdrop::Home, 0.0);
        let t0 = Duration::from_secs(1);
        s.step(t0);
        // Grab at the bottom edge, off-centre; the grabbed point stays under the finger.
        s.grab((900.0, 760.0).into(), true);
        s.drag((950.0, 500.0).into(), 0.4, 1.0);
        assert!(!s.step(t0 + Duration::from_millis(16)), "held: the spring does not step");
        assert!(!s.is_done());
        let held = s.pose();
        let grabbed = Point::<f64, Logical>::from((900.0 - 683.0, 760.0 - 384.0));
        let under = held.center + grabbed.upscale(held.scale);
        assert!((under.x - 950.0).abs() < 1e-6 && (under.y - 500.0).abs() < 1e-6, "{under:?}");
        // Release towards Home: no jump, lands in the tile.
        s.release(false);
        assert_eq!(s.pose(), held);
        let poses = run(&mut s, t0 + Duration::from_millis(32));
        assert_eq!(poses.last().unwrap().center, (174.0, 274.0).into());
        // Steps stay small (no jumps).
        let mut prev = held.center;
        for p in &poses {
            assert!((p.center.x - prev.x).abs() < 200.0 && (p.center.y - prev.y).abs() < 200.0);
            prev = p.center;
        }
    }

    #[test]
    fn release_back_to_full_from_tiny_progress() {
        let mut s = sw(None, Backdrop::Home, 0.0);
        let t0 = Duration::from_secs(1);
        s.grab((600.0, 760.0).into(), true);
        s.drag((640.0, 755.0).into(), 0.001, 0.0);
        let held = s.pose();
        s.release(true);
        assert_eq!(s.pose(), held);
        let poses = run(&mut s, t0);
        assert_eq!(*poses.last().unwrap(), Pose { scale: 1.0, center: (683.0, 384.0).into(), alpha: 1.0 });
    }

    #[test]
    fn slide_follows_the_finger() {
        let mut s = sw(None, Backdrop::App, 1.0);
        s.style = Style::Slide;
        s.grab((5.0, 400.0).into(), false);
        s.drag((400.0, 400.0).into(), 1.0 - 400.0 / 1366.0, 0.0);
        let pose = s.pose();
        // Right edge of the card under the finger.
        assert!((pose.center.x + 683.0 - 400.0).abs() < 1e-3, "{pose:?}");
        s.release(true);
        let poses = run(&mut s, Duration::from_secs(1));
        assert_eq!(poses.last().unwrap().center, (683.0, 384.0).into());
    }
}
