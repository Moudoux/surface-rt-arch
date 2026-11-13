//! Edge gesture recognizer (DESKTOP-SHELL.md 3.3, Phase 4). Core layer: knows touch points and screen edges,
//! not windows or the UI. `State` (input/mod.rs) offers every touch to it before the shell UI and apps.
//!
//! - A touch is *claimed* only if it starts inside an edge zone (`zone`, default `ZONE` px) that is armed right now (the
//!   caller says which edges have an action) and no other finger is down.
//! - A claimed touch is *pending* until it has moved `lock` (default `LOCK`) px inward (it then *locks* to that edge's
//!   gesture). If it first moves along the edge, is lifted, rests for `HOLD` without locking, or a second
//!   finger comes down, it is *released*: the caller replays its down (and current position) to the normal
//!   path, so apps still get edge taps, long presses and edge scrolls (a little late).
//! - A locked gesture reports finger positions and velocity until the finger lifts (`End`). A second finger
//!   cancels it (`Cancel`); that finger and the rest of the first are swallowed.

use std::time::Duration;

use crate::anim::VelocityTracker;

/// Default edge zone width (px). Touches that start further in are never claimed.
pub const ZONE: f32 = 16.0;
/// Default inward travel that locks a pending touch to its edge gesture (px).
pub const LOCK: f32 = 10.0;
/// A pending touch that has not locked after this long is handed to the app (a long press at the edge).
pub const HOLD: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Bottom,
    Left,
    Top,
    Right,
}

impl Edge {
    /// Unit vector pointing from this edge into the screen.
    fn inward(self) -> (f32, f32) {
        match self {
            Edge::Bottom => (0.0, -1.0),
            Edge::Top => (0.0, 1.0),
            Edge::Left => (1.0, 0.0),
            Edge::Right => (-1.0, 0.0),
        }
    }
}

/// Which edges have an action now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    pub bottom: bool,
    pub left: bool,
    pub top: bool,
    pub right: bool,
}

impl Edges {
    fn has(&self, e: Edge) -> bool {
        match e {
            Edge::Bottom => self.bottom,
            Edge::Left => self.left,
            Edge::Top => self.top,
            Edge::Right => self.right,
        }
    }
}

/// A touch-down the recognizer held back, to be replayed to the normal path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Held {
    pub slot: i32,
    pub x: f32,
    pub y: f32,
    pub time: Duration,
}

/// A locked gesture's progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub edge: Edge,
    /// Where the finger came down and where it is now.
    pub start: (f32, f32),
    pub pos: (f32, f32),
    /// Finger velocity (px/s) over the last ~80 ms.
    pub velocity: (f32, f32),
}

impl Track {
    /// Distance travelled away from the edge (px; negative = back towards it).
    pub fn inward(&self) -> f32 {
        let (ix, iy) = self.edge.inward();
        (self.pos.0 - self.start.0) * ix + (self.pos.1 - self.start.1) * iy
    }

    /// Velocity away from the edge (px/s).
    pub fn inward_velocity(&self) -> f32 {
        let (ix, iy) = self.edge.inward();
        self.velocity.0 * ix + self.velocity.1 * iy
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// Not ours: deliver normally.
    Pass,
    /// Ours, nothing to do (pending, or the rest of a cancelled gesture).
    Swallow,
    /// Give up a pending touch: replay `Held` (the down) to the normal path, then deliver this event normally.
    Release(Held),
    /// A pending touch locked: the gesture starts (the caller may refuse it with `Recognizer::refuse`).
    Begin(Track),
    /// The finger moved.
    Move(Track),
    /// The finger lifted: finish by the track's distance and velocity.
    End(Track),
    /// A second finger came down: abandon the gesture (this event is swallowed).
    Cancel(Track),
}

#[derive(Debug, Clone, PartialEq)]
enum Claim {
    Pending { held: Held, edge: Edge },
    Active { slot: i32, edge: Edge, start: (f32, f32) },
    /// Gesture cancelled or refused; swallow these slots until they lift.
    Dead,
}

#[derive(Debug)]
pub struct Recognizer {
    /// Edge zone width and lock distance (px; config `input.edge_zone`, `input.edge_lock`).
    pub zone: f32,
    pub lock: f32,
    claim: Option<Claim>,
    /// Slots of a dead claim still down (swallowed until up).
    dead: Vec<i32>,
    /// Slots down that the recognizer did not take (apps, Home).
    others: Vec<i32>,
    vel: VelocityTracker,
}

impl Default for Recognizer {
    fn default() -> Self {
        Recognizer { zone: ZONE, lock: LOCK, claim: None, dead: Vec::new(), others: Vec::new(), vel: VelocityTracker::default() }
    }
}

/// The edge zone `(x, y)` is in, if any (corners: bottom/top win over the sides).
pub fn edge_at(x: f32, y: f32, w: f32, h: f32, zone: f32) -> Option<Edge> {
    if y >= h - zone {
        Some(Edge::Bottom)
    } else if y < zone {
        Some(Edge::Top)
    } else if x < zone {
        Some(Edge::Left)
    } else if x >= w - zone {
        Some(Edge::Right)
    } else {
        None
    }
}

impl Recognizer {
    /// True while a locked gesture owns a finger.
    #[cfg(test)]
    pub fn active(&self) -> bool {
        matches!(self.claim, Some(Claim::Active { .. }))
    }

    /// The pending touch, if it is still pending and came down at `time` (for the `HOLD` timer).
    pub fn pending_since(&self) -> Option<Duration> {
        match &self.claim {
            Some(Claim::Pending { held, .. }) => Some(held.time),
            _ => None,
        }
    }

    pub fn down(&mut self, slot: i32, x: f32, y: f32, t: Duration, size: (f32, f32), armed: Edges) -> Verdict {
        match self.claim.clone() {
            Some(Claim::Pending { held, .. }) => {
                // Two fingers: not an edge gesture. Hand both to the normal path.
                self.claim = None;
                self.others.push(held.slot);
                self.others.push(slot);
                Verdict::Release(held)
            }
            Some(Claim::Active { slot: s, edge, start }) => {
                let track = self.track(edge, start);
                self.claim = Some(Claim::Dead);
                self.dead.extend([s, slot]);
                Verdict::Cancel(track)
            }
            Some(Claim::Dead) => {
                self.dead.push(slot);
                Verdict::Swallow
            }
            None => {
                let edge = edge_at(x, y, size.0, size.1, self.zone).filter(|e| armed.has(*e));
                match edge {
                    Some(edge) if self.others.is_empty() => {
                        self.vel.reset();
                        self.vel.add(t, x, y);
                        self.claim = Some(Claim::Pending { held: Held { slot, x, y, time: t }, edge });
                        Verdict::Swallow
                    }
                    _ => {
                        self.others.push(slot);
                        Verdict::Pass
                    }
                }
            }
        }
    }

    pub fn motion(&mut self, slot: i32, x: f32, y: f32, t: Duration) -> Verdict {
        if self.dead.contains(&slot) {
            return Verdict::Swallow;
        }
        match self.claim.clone() {
            Some(Claim::Pending { held, edge }) if held.slot == slot => {
                self.vel.add(t, x, y);
                let (ix, iy) = edge.inward();
                let (dx, dy) = (x - held.x, y - held.y);
                let inward = dx * ix + dy * iy;
                let along = (dx * iy - dy * ix).abs();
                if inward >= self.lock && inward >= along {
                    self.claim = Some(Claim::Active { slot, edge, start: (held.x, held.y) });
                    Verdict::Begin(self.track(edge, (held.x, held.y)))
                } else if along >= self.lock || inward <= -self.lock || t.saturating_sub(held.time) >= HOLD {
                    self.claim = None;
                    self.others.push(slot);
                    Verdict::Release(held)
                } else {
                    Verdict::Swallow
                }
            }
            Some(Claim::Active { slot: s, edge, start }) if s == slot => {
                self.vel.add(t, x, y);
                Verdict::Move(self.track(edge, start))
            }
            _ => Verdict::Pass,
        }
    }

    pub fn up(&mut self, slot: i32, t: Duration) -> Verdict {
        if let Some(i) = self.dead.iter().position(|s| *s == slot) {
            self.dead.remove(i);
            if self.dead.is_empty() && self.claim == Some(Claim::Dead) {
                self.claim = None;
            }
            return Verdict::Swallow;
        }
        match self.claim.clone() {
            Some(Claim::Pending { held, .. }) if held.slot == slot => {
                // A tap at the edge: the app gets down + up.
                self.claim = None;
                self.others.push(slot);
                self.forget(slot);
                Verdict::Release(held)
            }
            Some(Claim::Active { slot: s, edge, start }) if s == slot => {
                // A lift adds no position; age the samples to the lift time so a finger that stopped
                // before lifting reads as still.
                if let Some((x, y)) = self.vel.last() {
                    self.vel.add(t, x, y);
                }
                let track = self.track(edge, start);
                self.claim = None;
                Verdict::End(track)
            }
            _ => {
                self.forget(slot);
                Verdict::Pass
            }
        }
    }

    /// The pending touch rested for `HOLD` without moving: release it (called from a timer).
    pub fn timeout(&mut self, now: Duration) -> Option<Held> {
        match self.claim.clone() {
            Some(Claim::Pending { held, .. }) if now.saturating_sub(held.time) >= HOLD => {
                self.claim = None;
                self.others.push(held.slot);
                Some(held)
            }
            _ => None,
        }
    }

    /// The caller cannot run the gesture that just began: swallow the finger until it lifts.
    pub fn refuse(&mut self) {
        if let Some(Claim::Active { slot, .. }) = self.claim {
            self.claim = Some(Claim::Dead);
            self.dead.push(slot);
        }
    }

    /// All touches were cancelled (libinput TouchCancel, session switch).
    pub fn cancel_all(&mut self) -> Option<Track> {
        let track = match self.claim {
            Some(Claim::Active { edge, start, .. }) => Some(self.track(edge, start)),
            _ => None,
        };
        *self = Recognizer { zone: self.zone, lock: self.lock, ..Recognizer::default() };
        track
    }

    fn forget(&mut self, slot: i32) {
        self.others.retain(|s| *s != slot);
    }

    fn track(&self, edge: Edge, start: (f32, f32)) -> Track {
        Track { edge, start, pos: self.vel.last().unwrap_or(start), velocity: self.vel.velocity() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f32, f32) = (1366.0, 768.0);
    const ALL: Edges = Edges { bottom: true, left: true, top: true, right: true };

    fn ms(n: u64) -> Duration {
        Duration::from_millis(1000 + n)
    }

    #[test]
    fn zones() {
        assert_eq!(edge_at(600.0, 760.0, SIZE.0, SIZE.1, ZONE), Some(Edge::Bottom));
        assert_eq!(edge_at(5.0, 400.0, SIZE.0, SIZE.1, ZONE), Some(Edge::Left));
        assert_eq!(edge_at(600.0, 3.0, SIZE.0, SIZE.1, ZONE), Some(Edge::Top));
        assert_eq!(edge_at(1360.0, 400.0, SIZE.0, SIZE.1, ZONE), Some(Edge::Right));
        assert_eq!(edge_at(600.0, 700.0, SIZE.0, SIZE.1, ZONE), None);
    }

    #[test]
    fn swipe_up_locks_and_ends() {
        let mut r = Recognizer::default();
        assert_eq!(r.down(0, 600.0, 765.0, ms(0), SIZE, ALL), Verdict::Swallow);
        assert_eq!(r.motion(0, 601.0, 760.0, ms(8)), Verdict::Swallow);
        let Verdict::Begin(t) = r.motion(0, 602.0, 750.0, ms(16)) else { panic!() };
        assert_eq!(t.edge, Edge::Bottom);
        assert_eq!(t.inward(), 15.0);
        let Verdict::Move(t) = r.motion(0, 604.0, 600.0, ms(32)) else { panic!() };
        assert_eq!(t.inward(), 165.0);
        let Verdict::End(t) = r.up(0, ms(40)) else { panic!() };
        assert!(t.inward_velocity() > 1000.0, "{}", t.inward_velocity());
        assert!(!r.active());
        // Next touch in the middle passes.
        assert_eq!(r.down(1, 600.0, 400.0, ms(100), SIZE, ALL), Verdict::Pass);
    }

    #[test]
    fn tap_and_sideways_are_released() {
        let mut r = Recognizer::default();
        r.down(0, 600.0, 765.0, ms(0), SIZE, ALL);
        let held = Held { slot: 0, x: 600.0, y: 765.0, time: ms(0) };
        assert_eq!(r.up(0, ms(50)), Verdict::Release(held));
        r.down(0, 600.0, 765.0, ms(0), SIZE, ALL);
        assert_eq!(r.motion(0, 615.0, 763.0, ms(16)), Verdict::Release(held));
        assert_eq!(r.motion(0, 640.0, 700.0, ms(32)), Verdict::Pass);
        assert_eq!(r.up(0, ms(48)), Verdict::Pass);
    }

    #[test]
    fn unarmed_edge_and_second_finger() {
        let mut r = Recognizer::default();
        let none = Edges::default();
        assert_eq!(r.down(0, 600.0, 765.0, ms(0), SIZE, none), Verdict::Pass);
        // A finger already down (in an app): an edge touch is a multi-touch gesture of the app.
        assert_eq!(r.down(1, 600.0, 765.0, ms(5), SIZE, ALL), Verdict::Pass);
        r.up(0, ms(10));
        r.up(1, ms(10));
        // A second finger on a pending touch releases both.
        r.down(0, 600.0, 765.0, ms(20), SIZE, ALL);
        assert!(matches!(r.down(1, 300.0, 300.0, ms(25), SIZE, ALL), Verdict::Release(_)));
        assert_eq!(r.motion(1, 310.0, 300.0, ms(30)), Verdict::Pass);
        r.up(0, ms(40));
        r.up(1, ms(40));
        // A second finger on an active gesture cancels it; both are swallowed until they lift.
        r.down(0, 600.0, 765.0, ms(50), SIZE, ALL);
        assert!(matches!(r.motion(0, 600.0, 700.0, ms(60)), Verdict::Begin(_)));
        assert!(matches!(r.down(1, 300.0, 300.0, ms(70), SIZE, ALL), Verdict::Cancel(_)));
        assert_eq!(r.motion(0, 600.0, 600.0, ms(80)), Verdict::Swallow);
        assert_eq!(r.up(0, ms(90)), Verdict::Swallow);
        assert_eq!(r.down(2, 600.0, 765.0, ms(95), SIZE, ALL), Verdict::Swallow);
        assert_eq!(r.up(1, ms(100)), Verdict::Swallow);
        assert_eq!(r.up(2, ms(100)), Verdict::Swallow);
        assert_eq!(r.down(3, 600.0, 765.0, ms(200), SIZE, ALL), Verdict::Swallow);
    }

    #[test]
    fn hold_releases() {
        let mut r = Recognizer::default();
        r.down(0, 5.0, 400.0, ms(0), SIZE, ALL);
        assert_eq!(r.timeout(ms(100)), None);
        assert!(r.timeout(ms(260)).is_some());
        assert_eq!(r.motion(0, 50.0, 400.0, ms(300)), Verdict::Pass);
    }

    #[test]
    fn stopped_finger_has_no_velocity() {
        let mut r = Recognizer::default();
        r.down(0, 600.0, 765.0, ms(0), SIZE, ALL);
        r.motion(0, 600.0, 700.0, ms(16));
        r.motion(0, 600.0, 500.0, ms(32));
        let Verdict::End(t) = r.up(0, ms(300)) else { panic!() };
        assert_eq!(t.inward_velocity(), 0.0);
    }
}
