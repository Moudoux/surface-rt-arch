//! Split view geometry (DESKTOP-SHELL.md Phase 7): where the two halves and the divider go, which snap point
//! a divider release picks, and when a release dismisses one side. Pure functions, used by
//! `FullscreenPolicy`; no windows or protocol state here.
//!
//! The split axis follows the longer side of the work area: side by side in landscape, top/bottom in
//! portrait (and when the keyboard's strip makes the area taller than wide). "Start" is left or top.

use std::time::Duration;

use smithay::utils::{Logical, Rectangle};

use crate::policy::SnapSide;

/// Layout values from the theme's `[split]` table (handed over by `main.rs`).
#[derive(Debug, Clone, PartialEq)]
pub struct SplitMetrics {
    /// Divider thickness between the halves (px).
    pub divider: i32,
    /// Extra touch width on each side of the divider (px).
    pub grab_margin: i32,
    /// Snap points: the start app's share of the space next to the divider, ascending (e.g. 1/3, 1/2, 2/3).
    pub snaps: Vec<f64>,
    /// A release closer to an end than this fraction of the outermost snap dismisses that side.
    pub dismiss: f64,
    /// Two taps on the divider within this time swap the sides.
    pub double_tap: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side (left | right).
    Horizontal,
    /// Stacked (top / bottom).
    Vertical,
}

pub fn axis(area: Rectangle<i32, Logical>) -> Axis {
    if area.size.w >= area.size.h { Axis::Horizontal } else { Axis::Vertical }
}

/// Length of `area` along the split axis, and the start coordinate.
fn along(area: Rectangle<i32, Logical>, axis: Axis) -> (i32, i32) {
    match axis {
        Axis::Horizontal => (area.loc.x, area.size.w),
        Axis::Vertical => (area.loc.y, area.size.h),
    }
}

/// A slice of `area` along the axis: from `from` (offset from the area start) with length `len`.
fn slice(area: Rectangle<i32, Logical>, axis: Axis, from: i32, len: i32) -> Rectangle<i32, Logical> {
    let len = len.max(0);
    match axis {
        Axis::Horizontal => Rectangle::new((area.loc.x + from, area.loc.y).into(), (len, area.size.h).into()),
        Axis::Vertical => Rectangle::new((area.loc.x, area.loc.y + from).into(), (area.size.w, len).into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub axis: Axis,
    pub start: Rectangle<i32, Logical>,
    pub divider: Rectangle<i32, Logical>,
    pub end: Rectangle<i32, Logical>,
}

impl Layout {
    pub fn rect(&self, side: SnapSide) -> Rectangle<i32, Logical> {
        match side {
            SnapSide::Start => self.start,
            SnapSide::End => self.end,
        }
    }
}

/// Space left for the two apps.
fn avail(area: Rectangle<i32, Logical>, divider: i32) -> i32 {
    (along(area, axis(area)).1 - divider).max(0)
}

fn start_len(avail: i32, ratio: f64) -> i32 {
    ((avail as f64 * ratio).round() as i32).clamp(0, avail)
}

pub fn layout(area: Rectangle<i32, Logical>, ratio: f64, divider: i32) -> Layout {
    let axis = axis(area);
    let avail = avail(area, divider);
    let s = start_len(avail, ratio);
    Layout {
        axis,
        start: slice(area, axis, 0, s),
        divider: slice(area, axis, s, divider),
        end: slice(area, axis, s + divider, avail - s),
    }
}

/// The divider rectangle if it were centred on `pos` (space coordinate along the axis), kept inside the area.
pub fn divider_at(area: Rectangle<i32, Logical>, pos: f64, divider: i32) -> Rectangle<i32, Logical> {
    let ax = axis(area);
    let (a0, len) = along(area, ax);
    let from = ((pos - a0 as f64 - divider as f64 / 2.0).round() as i32).clamp(0, (len - divider).max(0));
    slice(area, ax, from, divider)
}

/// The finger's coordinate along the split axis.
pub fn pos_along(area: Rectangle<i32, Logical>, x: f64, y: f64) -> f64 {
    match axis(area) {
        Axis::Horizontal => x,
        Axis::Vertical => y,
    }
}

/// Snap points that leave both apps at least their minimum length along the axis (0 = no minimum).
pub fn allowed_snaps(area: Rectangle<i32, Logical>, m: &SplitMetrics, min_start: i32, min_end: i32) -> Vec<f64> {
    let avail = avail(area, m.divider);
    m.snaps
        .iter()
        .copied()
        .filter(|&r| {
            let s = start_len(avail, r);
            s >= min_start && avail - s >= min_end
        })
        .collect()
}

fn nearest(snaps: &[f64], r: f64) -> Option<f64> {
    snaps.iter().copied().min_by(|a, b| (a - r).abs().total_cmp(&(b - r).abs()))
}

/// The ratio a new pair starts at: the allowed snap nearest to an even split; None if no snap fits.
pub fn initial_ratio(area: Rectangle<i32, Logical>, m: &SplitMetrics, min_start: i32, min_end: i32) -> Option<f64> {
    nearest(&allowed_snaps(area, m, min_start, min_end), 0.5)
}

/// What letting go of the divider at `pos` (space coordinate along the axis) does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Release {
    /// Settle at this ratio.
    Snap(f64),
    /// Close the split; this side's app goes to the background.
    Dismiss(SnapSide),
}

pub fn release(area: Rectangle<i32, Logical>, m: &SplitMetrics, pos: f64, current: f64, min_start: i32, min_end: i32) -> Release {
    let (a0, _) = along(area, axis(area));
    let avail = avail(area, m.divider).max(1) as f64;
    let raw = (pos - a0 as f64 - m.divider as f64 / 2.0) / avail;
    let first = m.snaps.first().copied().unwrap_or(0.5);
    let last = m.snaps.last().copied().unwrap_or(0.5);
    if raw < first * m.dismiss {
        return Release::Dismiss(SnapSide::Start);
    }
    if raw > 1.0 - (1.0 - last) * m.dismiss {
        return Release::Dismiss(SnapSide::End);
    }
    Release::Snap(nearest(&allowed_snaps(area, m, min_start, min_end), raw).unwrap_or(current))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> SplitMetrics {
        SplitMetrics { divider: 10, grab_margin: 14, snaps: vec![1.0 / 3.0, 0.5, 2.0 / 3.0], dismiss: 0.5, double_tap: Duration::from_millis(300) }
    }

    fn landscape() -> Rectangle<i32, Logical> {
        Rectangle::from_size((1366, 768).into())
    }

    #[test]
    fn halves_and_divider_tile_the_area() {
        let l = layout(landscape(), 0.5, 10);
        assert_eq!(l.axis, Axis::Horizontal);
        assert_eq!(l.start, Rectangle::new((0, 0).into(), (678, 768).into()));
        assert_eq!(l.divider, Rectangle::new((678, 0).into(), (10, 768).into()));
        assert_eq!(l.end, Rectangle::new((688, 0).into(), (678, 768).into()));
        // Portrait (or a tall area): stacked.
        let p = layout(Rectangle::new((0, 0).into(), (768, 1000).into()), 1.0 / 3.0, 10);
        assert_eq!(p.axis, Axis::Vertical);
        assert_eq!(p.start.size, (768, 330).into());
        assert_eq!(p.divider.loc.y, 330);
        assert_eq!(p.end.loc.y + p.end.size.h, 1000);
    }

    #[test]
    fn release_snaps_to_the_nearest_allowed_point() {
        let a = landscape();
        assert_eq!(release(a, &m(), 600.0, 0.5, 0, 0), Release::Snap(0.5));
        assert_eq!(release(a, &m(), 420.0, 0.5, 0, 0), Release::Snap(1.0 / 3.0));
        assert_eq!(release(a, &m(), 1000.0, 0.5, 0, 0), Release::Snap(2.0 / 3.0));
        // The end app needs 600 px: 2/3 would leave it 452, so 1/2 wins.
        assert_eq!(release(a, &m(), 1000.0, 0.5, 0, 600), Release::Snap(0.5));
        // Nothing fits: stay.
        assert_eq!(release(a, &m(), 1000.0, 0.5, 900, 900), Release::Snap(0.5));
    }

    #[test]
    fn release_near_an_end_dismisses_that_side() {
        let a = landscape();
        assert_eq!(release(a, &m(), 100.0, 0.5, 0, 0), Release::Dismiss(SnapSide::Start));
        assert_eq!(release(a, &m(), 1300.0, 0.5, 0, 0), Release::Dismiss(SnapSide::End));
        // In portrait the axis is vertical.
        let p = Rectangle::from_size((768, 1366).into());
        assert_eq!(release(p, &m(), 60.0, 0.5, 0, 0), Release::Dismiss(SnapSide::Start));
        assert_eq!(pos_along(p, 700.0, 60.0), 60.0);
    }

    #[test]
    fn initial_ratio_respects_minimum_sizes() {
        let a = landscape();
        assert_eq!(initial_ratio(a, &m(), 0, 0), Some(0.5));
        assert_eq!(initial_ratio(a, &m(), 800, 0), Some(2.0 / 3.0));
        assert_eq!(initial_ratio(a, &m(), 800, 800), None);
    }

    #[test]
    fn divider_follows_the_finger_inside_the_area() {
        let a = landscape();
        assert_eq!(divider_at(a, 500.0, 10).loc.x, 495);
        assert_eq!(divider_at(a, -50.0, 10).loc.x, 0);
        assert_eq!(divider_at(a, 5000.0, 10).loc.x, 1356);
    }
}
