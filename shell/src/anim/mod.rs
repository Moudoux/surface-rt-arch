//! Motion (DESKTOP-SHELL.md 3.1): springs driven by the display's vblank clock.
//!
//! Springs are solved analytically per step (exact for any dt, so a late frame never overshoots or
//! explodes), keep their velocity when retargeted (interruptible) and accept a release velocity from a
//! gesture. The frame clock predicts when the frame being built will reach the panel: the last vblank's
//! timestamp plus whole refresh periods. Animations are evaluated at that time, so motion stays even when
//! rendering starts at different points inside a refresh period. Nothing ticks while all springs rest:
//! the shell asks for frames only while `animating()` is true (idle = no frames).

use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct SpringParams {
    /// Stiffness k (mass 1): higher is faster. 380 settles a critically damped spring in ~0.3 s.
    pub stiffness: f32,
    /// Damping ratio: 1 = critically damped (no overshoot), < 1 bounces.
    pub damping: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub params: SpringParams,
    /// Resting when |value - target| and |velocity| / 10 are below this (units of `value`).
    pub eps: f32,
}

impl Spring {
    pub fn new(value: f32, params: SpringParams) -> Spring {
        Spring { value, velocity: 0.0, target: value, params, eps: 0.01 }
    }

    pub fn with_eps(mut self, eps: f32) -> Spring {
        self.eps = eps;
        self
    }

    pub fn is_resting(&self) -> bool {
        (self.value - self.target).abs() < self.eps && self.velocity.abs() < self.eps * 10.0
    }

    /// New target; the current velocity carries over.
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump (e.g. following a finger 1:1); `velocity` is what the gesture measured.
    pub fn set(&mut self, value: f32, velocity: f32) {
        self.value = value;
        self.target = value;
        self.velocity = velocity;
    }

    /// Advance by `dt` seconds. Returns true while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.is_resting() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let w = self.params.stiffness.max(1.0).sqrt();
        let z = self.params.damping.max(0.0);
        let d0 = self.value - self.target;
        let v0 = self.velocity;
        let (d, v) = if (z - 1.0).abs() < 1e-3 {
            let b = v0 + w * d0;
            let e = (-w * dt).exp();
            ((d0 + b * dt) * e, (b - w * (d0 + b * dt)) * e)
        } else if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let a = d0;
            let b = (v0 + z * w * d0) / wd;
            let e = (-z * w * dt).exp();
            let (s, c) = (wd * dt).sin_cos();
            (e * (a * c + b * s), e * ((b * wd - z * w * a) * c - (a * wd + z * w * b) * s))
        } else {
            let sq = (z * z - 1.0).sqrt();
            let r1 = -w * (z - sq);
            let r2 = -w * (z + sq);
            let c2 = (v0 - r1 * d0) / (r2 - r1);
            let c1 = d0 - c2;
            let (e1, e2) = ((r1 * dt).exp(), (r2 * dt).exp());
            (c1 * e1 + c2 * e2, r1 * c1 * e1 + r2 * c2 * e2)
        };
        self.value = self.target + d;
        self.velocity = v;
        if self.is_resting() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}

/// Predicts presentation times from vblank timestamps (CLOCK_MONOTONIC, as Smithay's clock).
#[derive(Debug, Clone, Copy)]
pub struct FrameClock {
    last_vblank: Option<Duration>,
    pub refresh: Duration,
}

impl FrameClock {
    pub fn new(refresh: Duration) -> FrameClock {
        FrameClock { last_vblank: None, refresh }
    }

    pub fn vblank(&mut self, at: Duration) {
        self.last_vblank = Some(at);
    }

    /// When a frame started rendering at `now` will be shown: the first vblank after now.
    pub fn next_present(&self, now: Duration) -> Duration {
        match self.last_vblank {
            Some(last) if now >= last && !self.refresh.is_zero() => {
                let periods = (now - last).as_nanos() / self.refresh.as_nanos() + 1;
                last + self.refresh * periods as u32
            }
            _ => now + self.refresh,
        }
    }
}

/// Measures a finger's velocity (px/s) over the last ~80 ms of samples.
#[derive(Debug, Default, Clone)]
pub struct VelocityTracker {
    samples: Vec<(Duration, f32, f32)>,
}

impl VelocityTracker {
    #[allow(dead_code)] // gesture recognizer (Phase 4)
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    pub fn add(&mut self, t: Duration, x: f32, y: f32) {
        self.samples.push((t, x, y));
        let cutoff = t.saturating_sub(Duration::from_millis(80));
        self.samples.retain(|s| s.0 >= cutoff);
    }

    /// The latest sample's position.
    pub fn last(&self) -> Option<(f32, f32)> {
        self.samples.last().map(|s| (s.1, s.2))
    }

    pub fn velocity(&self) -> (f32, f32) {
        let (Some(a), Some(b)) = (self.samples.first(), self.samples.last()) else { return (0.0, 0.0) };
        let dt = (b.0 - a.0).as_secs_f32();
        if dt < 0.005 {
            return (0.0, 0.0);
        }
        ((b.1 - a.1) / dt, (b.2 - a.2) / dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: SpringParams = SpringParams { stiffness: 380.0, damping: 1.0 };

    #[test]
    fn critically_damped_settles_without_overshoot() {
        let mut s = Spring::new(0.0, P);
        s.set_target(100.0);
        let mut t = 0.0;
        while s.step(1.0 / 60.0) {
            assert!(s.value <= 100.0 + 1e-3);
            t += 1.0 / 60.0;
            assert!(t < 2.0);
        }
        assert_eq!(s.value, 100.0);
        assert!(t > 0.2 && t < 0.8, "settled after {t}");
    }

    #[test]
    fn step_size_does_not_change_the_path() {
        let (mut a, mut b) = (Spring::new(0.0, P), Spring::new(0.0, P));
        a.set_target(1.0);
        b.set_target(1.0);
        a.eps = 1e-6;
        b.eps = 1e-6;
        for _ in 0..6 {
            a.step(1.0 / 60.0);
        }
        b.step(0.1);
        assert!((a.value - b.value).abs() < 1e-4, "{} vs {}", a.value, b.value);
    }

    #[test]
    fn underdamped_bounces_and_overdamped_does_not() {
        let mut s = Spring::new(0.0, SpringParams { stiffness: 300.0, damping: 0.4 });
        s.set_target(1.0);
        s.eps = 1e-4;
        let mut max: f32 = 0.0;
        while s.step(1.0 / 120.0) {
            max = max.max(s.value);
        }
        assert!(max > 1.1);
        let mut s = Spring::new(0.0, SpringParams { stiffness: 300.0, damping: 2.0 });
        s.set_target(1.0);
        s.eps = 1e-4;
        while s.step(1.0 / 120.0) {
            assert!(s.value <= 1.0);
        }
    }

    #[test]
    fn release_velocity_carries() {
        let mut s = Spring::new(0.0, P);
        s.set(50.0, -2000.0);
        s.set_target(0.0);
        s.step(1.0 / 60.0);
        assert!(s.value < 50.0 - 20.0);
    }

    #[test]
    fn frame_clock_predicts_next_vblank() {
        let mut c = FrameClock::new(Duration::from_micros(16_667));
        c.vblank(Duration::from_millis(1000));
        assert_eq!(c.next_present(Duration::from_millis(1005)), Duration::from_micros(1_016_667));
        assert_eq!(c.next_present(Duration::from_millis(1020)), Duration::from_micros(1_033_334));
    }
}
