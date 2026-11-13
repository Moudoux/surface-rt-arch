//! Screen rotation (DESKTOP-SHELL.md Phase 5, auto-rotate): which output transform an accelerometer
//! orientation needs, the size the UI lays out in, and mapping touch points back into the rotated space.
//!
//! The orientation comes from iio-sensor-proxy through the services thread and the Control Center (which owns
//! the rotation lock); the core applies it (`State::apply_rotation`) unless a hardware keyboard (the Type
//! Cover) is attached, which keeps the panel in its normal landscape orientation.
//!
//! Convention, as seen on the device (GLES renderer, 2026-11-12): with output transform T a logical point L
//! is drawn at framebuffer point `T.invert().transform_point_in(L, logical_size)` (the opposite of what
//! Smithay's pixman renderer code suggests; the first version assumed that and had both the picture and,
//! after swapping the transforms, the touches upside down in portrait). The touch panel reports framebuffer
//! positions, so a touch maps back with `T.transform_point_in(fb, mode_size)`.

use smithay::{
    output::Output,
    utils::{Logical, Physical, Point, Size, Transform},
};

/// iio-sensor-proxy's `AccelerometerOrientation` ("undefined" = lying flat: keep what we have).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Normal,
    /// The device's left edge is up (portrait).
    LeftUp,
    RightUp,
    BottomUp,
}

impl Orientation {
    pub fn parse(s: &str) -> Option<Orientation> {
        match s {
            "normal" => Some(Orientation::Normal),
            "left-up" => Some(Orientation::LeftUp),
            "right-up" => Some(Orientation::RightUp),
            "bottom-up" => Some(Orientation::BottomUp),
            _ => None,
        }
    }

    /// The output transform that shows the UI upright. Checked by turning the device (2026-11-12): left-up
    /// needs `_90` (as Mutter maps LEFT_UP -> 90); `_270` was upside down.
    pub fn transform(self) -> Transform {
        match self {
            Orientation::Normal => Transform::Normal,
            Orientation::LeftUp => Transform::_90,
            Orientation::RightUp => Transform::_270,
            Orientation::BottomUp => Transform::_180,
        }
    }
}

/// The size the UI and the window policy see: the mode size with the output's transform applied.
pub fn ui_size(output: &Output) -> Size<i32, Physical> {
    let mode = output.current_mode().map(|m| m.size).unwrap_or_default();
    output.current_transform().transform_size(mode)
}

/// A framebuffer position (as the touch panel reports it, in mode pixels) in the rotated logical space.
pub fn touch_point(transform: Transform, mode: Size<i32, Physical>, fb: Point<f64, Physical>) -> Point<f64, Logical> {
    let area: Size<f64, Physical> = (mode.w as f64, mode.h as f64).into();
    let p = transform.transform_point_in(fb, &area);
    (p.x, p.y).into()
}

/// A keyboard that means "used as a laptop": the Type Cover (USB 045E:07A9) or any other external keyboard.
/// Built-in key sources (gpio-keys for the buttons and lid, the codec's beep device) and touch screens do
/// not count.
pub fn is_hw_keyboard(name: &str, usb_id: Option<(u32, u32)>, keyboard: bool, touch: bool) -> bool {
    if !keyboard || touch {
        return false;
    }
    let internal = ["gpio-keys", "Beep Generator", "Hall Effect", "Headphones Jack", "Mic Jack"];
    if internal.iter().any(|n| name.contains(n)) {
        return false;
    }
    // Platform devices report vendor 0 or 1 (gpio-keys: 0001:0001).
    usb_id.is_some_and(|(vendor, _)| vendor > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_maps_back_to_what_is_drawn_there() {
        let mode: Size<i32, Physical> = (1366, 768).into();
        for o in [Orientation::Normal, Orientation::LeftUp, Orientation::RightUp, Orientation::BottomUp] {
            let t = o.transform();
            let logical = t.transform_size(mode);
            let area: Size<f64, Physical> = (logical.w as f64, logical.h as f64).into();
            for l in [(10.0, 20.0), (0.0, 0.0), (300.5, 700.25)] {
                // How the GLES renderer places it (see the module comment).
                let fb = t.invert().transform_point_in(Point::<f64, Physical>::from(l), &area);
                let back = touch_point(t, mode, fb);
                assert!((back.x - l.0).abs() < 1e-9 && (back.y - l.1).abs() < 1e-9, "{o:?} {l:?} -> {fb:?} -> {back:?}");
            }
        }
        // Left-up (verified on the device: left-up = `_90` draws upright): a touch at the panel's left edge
        // (framebuffer x = 0) is at the logical top (y = 0).
        let p = touch_point(Orientation::LeftUp.transform(), (1366, 768).into(), (0.0, 300.0).into());
        assert_eq!(p.y, 0.0);
    }

    #[test]
    fn keyboards() {
        assert!(is_hw_keyboard("Microsoft Surface Type Cover Keyboard", Some((0x045e, 0x07a9)), true, false));
        assert!(!is_hw_keyboard("gpio-keys", Some((1, 1)), true, false));
        assert!(!is_hw_keyboard("WM8962 Beep Generator", Some((0, 0)), true, false));
        assert!(!is_hw_keyboard("hid-over-i2c 03EB:8207", Some((0x03eb, 0x8207)), false, true));
        assert!(!is_hw_keyboard("Type Cover touchpad", Some((0x045e, 0x07a9)), false, false));
    }
}
