// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Cutting a rectangle out of a decoded frame. The rectangle is in fractions of
//! the image: it is drawn on a scaled view and applied to the full-size pixels.

use super::{BigImageError, BigRgbaImage};

/// A rectangle in image fractions, with the origin at the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BigCropRect {
    /// Left edge, 0.0 to 1.0.
    pub x: f64,
    /// Top edge, 0.0 to 1.0.
    pub y: f64,
    /// Width, 0.0 to 1.0.
    pub width: f64,
    /// Height, 0.0 to 1.0.
    pub height: f64,
}

impl BigCropRect {
    /// Build a rectangle from two dragged corners, in any order.
    ///
    /// Returns `None` for a rectangle too small to be a deliberate selection:
    /// a stray click must not silently reduce an image to a few pixels.
    #[must_use]
    pub fn from_corners(first: (f64, f64), second: (f64, f64)) -> Option<Self> {
        const MIN_SIDE: f64 = 0.01;

        let x = first.0.min(second.0).clamp(0.0, 1.0);
        let y = first.1.min(second.1).clamp(0.0, 1.0);
        let width = first.0.max(second.0).clamp(0.0, 1.0) - x;
        let height = first.1.max(second.1).clamp(0.0, 1.0) - y;
        (width >= MIN_SIDE && height >= MIN_SIDE).then_some(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// How many pixels cutting this rectangle out of `natural` leaves.
    ///
    /// The same rounding [`crop_rgba`] performs, so anything that has to name
    /// the size of the result before the pixels exist (a dialog offering to
    /// resize what is about to be saved) says the number the save produces
    /// and not one a pixel beside it.
    #[must_use]
    pub fn size_in(self, natural: (i32, i32)) -> (i32, i32) {
        let (width, height) = natural;
        if width <= 0 || height <= 0 {
            return natural;
        }
        let (left, right) = self.span(self.x, self.width, width as usize);
        let (top, bottom) = self.span(self.y, self.height, height as usize);
        ((right - left) as i32, (bottom - top) as i32)
    }

    /// One axis of the rounded rectangle, as whole pixels inside `extent`.
    fn span(self, start: f64, length: f64, extent: usize) -> (usize, usize) {
        let first = scaled_index(start, extent, f64::floor);
        let last = scaled_index(start + length, extent, f64::ceil).max(first + 1);
        (first, last.min(extent))
    }
}

/// Return the part of `image` inside `rect`.
///
/// The rectangle is rounded outward to whole pixels and clamped to the image,
/// so a selection drawn to the edge keeps the edge.
///
/// # Errors
///
/// [`BigImageError::InvalidBytePixels`] for an empty image, a buffer that does
/// not match its size, a rectangle that leaves no pixels inside the image, or
/// no memory for the copy.
pub fn crop_rgba(image: &BigRgbaImage, rect: BigCropRect) -> Result<BigRgbaImage, BigImageError> {
    let width = usize::try_from(image.width).map_err(|_| BigImageError::InvalidBytePixels)?;
    let height = usize::try_from(image.height).map_err(|_| BigImageError::InvalidBytePixels)?;
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(BigImageError::InvalidBytePixels)?;
    if width == 0 || height == 0 || image.rgba.len() != expected {
        return Err(BigImageError::InvalidBytePixels);
    }

    let (left, right) = rect.span(rect.x, rect.width, width);
    let (top, bottom) = rect.span(rect.y, rect.height, height);
    let cropped_width = right - left;
    let cropped_height = bottom - top;
    if cropped_width == 0 || cropped_height == 0 {
        return Err(BigImageError::InvalidBytePixels);
    }

    let row = cropped_width * 4;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(row * cropped_height)
        .map_err(|_| BigImageError::InvalidBytePixels)?;
    for y in top..top + cropped_height {
        let start = (y * width + left) * 4;
        rgba.extend_from_slice(
            image
                .rgba
                .get(start..start + row)
                .ok_or(BigImageError::InvalidBytePixels)?,
        );
    }
    Ok(BigRgbaImage {
        rgba,
        width: i32::try_from(cropped_width).map_err(|_| BigImageError::InvalidBytePixels)?,
        height: i32::try_from(cropped_height).map_err(|_| BigImageError::InvalidBytePixels)?,
        cicp: image.cicp,
        opaque: image.opaque,
    })
}

fn scaled_index(fraction: f64, extent: usize, round: fn(f64) -> f64) -> usize {
    let scaled = round(fraction.clamp(0.0, 1.0) * extent as f64);
    scaled as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(width: i32, height: i32) -> BigRgbaImage {
        let pixels = (0..width * height)
            .flat_map(|index| [index as u8, 0, 0, 255])
            .collect();
        BigRgbaImage {
            rgba: pixels,
            width,
            height,
            cicp: None,
            opaque: false,
        }
    }

    #[test]
    fn stray_click_selects_nothing() {
        assert_eq!(BigCropRect::from_corners((0.5, 0.5), (0.5005, 0.9)), None);
        let rect = BigCropRect::from_corners((0.8, 0.9), (0.2, 0.1)).expect("dragged backwards");
        assert!((rect.x - 0.2).abs() < f64::EPSILON);
        assert!((rect.width - 0.6).abs() < 1e-9);
        let clamped =
            BigCropRect::from_corners((-3.0, -3.0), (2.0, 2.0)).expect("outside the image");
        assert_eq!(
            (clamped.x, clamped.y, clamped.width, clamped.height),
            (0.0, 0.0, 1.0, 1.0)
        );
    }

    #[test]
    fn cut_matches_selected_pixels() {
        let image = ramp(4, 2);
        let cropped = crop_rgba(
            &image,
            BigCropRect {
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5,
            },
        )
        .expect("half of each side");
        assert_eq!((cropped.width, cropped.height), (2, 1));
        assert_eq!(
            cropped
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            vec![6, 7],
            "the bottom-right quarter of a 4x2 ramp"
        );
    }

    /// The size named before the cut is the size the cut produces.
    #[test]
    fn announced_size_matches_output() {
        let image = ramp(40, 25);
        for rect in [
            BigCropRect {
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5,
            },
            BigCropRect {
                x: 0.13,
                y: 0.27,
                width: 0.61,
                height: 0.42,
            },
            BigCropRect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        ] {
            let cropped = crop_rgba(&image, rect).expect("a real rectangle");
            assert_eq!(
                rect.size_in((image.width, image.height)),
                (cropped.width, cropped.height)
            );
        }
    }

    #[test]
    fn subpixel_selection_keeps_one_pixel() {
        let cropped = crop_rgba(
            &ramp(10, 10),
            BigCropRect {
                x: 0.0,
                y: 0.0,
                width: 0.011,
                height: 0.011,
            },
        )
        .expect("rounded outward");
        assert_eq!((cropped.width, cropped.height), (1, 1));
    }

    #[test]
    fn a_malformed_buffer_is_refused() {
        let broken = BigRgbaImage {
            rgba: vec![0; 3],
            width: 2,
            height: 2,
            cicp: None,
            opaque: false,
        };
        assert!(
            crop_rgba(
                &broken,
                BigCropRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0
                }
            )
            .is_err()
        );
    }
}
