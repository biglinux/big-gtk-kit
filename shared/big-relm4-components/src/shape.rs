// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Paths cairo has no call for.

use std::f64::consts::{FRAC_PI_2, PI};

use gtk::cairo;

/// Add a rectangle with corners of `radius` to the current path, as a closed
/// sub-path the caller fills or strokes.
///
/// The radius is held to half the shorter side, so a small rectangle becomes a
/// capsule instead of a path that crosses itself.
pub fn rounded_rectangle(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    radius: f64,
) {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    cr.new_sub_path();
    cr.arc(x + width - radius, y + radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.0,
        FRAC_PI_2,
    );
    cr.arc(x + radius, y + height - radius, radius, FRAC_PI_2, PI);
    cr.arc(x + radius, y + radius, radius, PI, PI + FRAC_PI_2);
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extents(width: f64, height: f64, radius: f64) -> (f64, f64, f64, f64) {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 64, 64).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        rounded_rectangle(&cr, 4.0, 6.0, width, height, radius);
        cr.fill_extents().unwrap()
    }

    #[test]
    fn path_fills_its_rectangle() {
        assert_eq!(extents(20.0, 10.0, 3.0), (4.0, 6.0, 24.0, 16.0));
        // A radius past half the height still fills the rectangle and no more.
        assert_eq!(extents(20.0, 10.0, 30.0), (4.0, 6.0, 24.0, 16.0));
        assert_eq!(extents(20.0, 10.0, 0.0), (4.0, 6.0, 24.0, 16.0));
    }

    #[test]
    fn corners_are_cut() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 64, 64).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        rounded_rectangle(&cr, 0.0, 0.0, 20.0, 10.0, 5.0);
        assert!(!cr.in_fill(0.5, 0.5).unwrap());
        assert!(cr.in_fill(10.0, 0.5).unwrap());
        assert!(cr.in_fill(19.5, 5.0).unwrap());
    }
}
