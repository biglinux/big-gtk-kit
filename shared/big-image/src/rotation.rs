// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Pure, checked rotations for packed RGBA viewer frames.

use super::{BigImageError, BigRgbaImage};

/// Display-only rotation applied to a decoded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BigImageQuarterTurn {
    /// Rotate 90 degrees clockwise.
    Clockwise,
    /// Rotate 180 degrees.
    Half,
    /// Rotate 90 degrees counter-clockwise.
    CounterClockwise,
}

/// Return a packed RGBA copy rotated by `turn`.
///
/// The source is never modified.
///
/// # Errors
///
/// [`BigImageError::InvalidBytePixels`] for dimensions, a byte length or an
/// offset that does not fit.
pub fn rotate_rgba_quarter_turn(
    image: &BigRgbaImage,
    turn: BigImageQuarterTurn,
) -> Result<BigRgbaImage, BigImageError> {
    let width = usize::try_from(image.width).map_err(|_| BigImageError::InvalidBytePixels)?;
    let height = usize::try_from(image.height).map_err(|_| BigImageError::InvalidBytePixels)?;
    let pixels = width
        .checked_mul(height)
        .filter(|pixels| {
            u64::try_from(*pixels).is_ok_and(|n| n <= crate::secure::MAX_SOURCE_PIXELS)
        })
        .ok_or(BigImageError::InvalidBytePixels)?;
    let expected = pixels
        .checked_mul(4)
        .ok_or(BigImageError::InvalidBytePixels)?;
    if width == 0 || height == 0 || image.rgba.len() != expected {
        return Err(BigImageError::InvalidBytePixels);
    }

    let (rotated_width, rotated_height) = match turn {
        BigImageQuarterTurn::Half => (width, height),
        BigImageQuarterTurn::Clockwise | BigImageQuarterTurn::CounterClockwise => (height, width),
    };
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(expected)
        .map_err(|_| BigImageError::InvalidBytePixels)?;
    rgba.resize(expected, 0);
    match turn {
        BigImageQuarterTurn::Half => rotate_half(&image.rgba, &mut rgba, width, height),
        BigImageQuarterTurn::Clockwise => {
            rotate_quarter(&image.rgba, &mut rgba, width, height, true);
        }
        BigImageQuarterTurn::CounterClockwise => {
            rotate_quarter(&image.rgba, &mut rgba, width, height, false);
        }
    }
    Ok(BigRgbaImage {
        rgba,
        width: i32::try_from(rotated_width).map_err(|_| BigImageError::InvalidBytePixels)?,
        height: i32::try_from(rotated_height).map_err(|_| BigImageError::InvalidBytePixels)?,
        cicp: image.cicp,
        opaque: image.opaque,
    })
}

/// Half a turn: every row comes back reversed, in reverse order.
fn rotate_half(source: &[u8], target: &mut [u8], width: usize, height: usize) {
    let row = width * 4;
    for y in 0..height {
        let read = &source[y * row..(y + 1) * row];
        let write = &mut target[(height - 1 - y) * row..(height - y) * row];
        for (pixel, slot) in read
            .as_chunks::<4>()
            .0
            .iter()
            .rev()
            .zip(write.as_chunks_mut::<4>().0.iter_mut())
        {
            slot.copy_from_slice(pixel);
        }
    }
}

/// A quarter turn, walked in tiles.
///
/// A transpose reads along rows and writes down columns, so a straight nested
/// loop touches a new cache line on every single pixel. Tiles keep both sides
/// of the copy inside the cache and cost nothing but two more loops.
fn rotate_quarter(source: &[u8], target: &mut [u8], width: usize, height: usize, clockwise: bool) {
    /// Pixels per tile side: 32 x 32 x 4 bytes is 4 KiB on each side.
    const TILE: usize = 32;

    let source_row = width * 4;
    let target_row = height * 4;
    for tile_y in (0..height).step_by(TILE) {
        for tile_x in (0..width).step_by(TILE) {
            for y in tile_y..(tile_y + TILE).min(height) {
                for x in tile_x..(tile_x + TILE).min(width) {
                    let (target_x, target_y) = if clockwise {
                        (height - 1 - y, x)
                    } else {
                        (y, width - 1 - x)
                    };
                    let read = y * source_row + x * 4;
                    let write = target_y * target_row + target_x * 4;
                    target[write..write + 4].copy_from_slice(&source[read..read + 4]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> BigRgbaImage {
        BigRgbaImage {
            rgba: (1_u8..=6).flat_map(|value| [value, 0, 0, 255]).collect(),
            width: 2,
            height: 3,
            cicp: None,
            opaque: false,
        }
    }

    fn red_channels(image: &BigRgbaImage) -> Vec<u8> {
        image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[0])
            .collect()
    }

    #[test]
    fn rotation_preserves_the_pixel_color_space() {
        let mut image = fixture();
        image.cicp = Some(crate::BigCicp {
            color_primaries: 12,
            transfer_characteristics: 13,
            matrix_coefficients: 0,
            full_range: true,
        });
        for turn in [
            BigImageQuarterTurn::Clockwise,
            BigImageQuarterTurn::Half,
            BigImageQuarterTurn::CounterClockwise,
        ] {
            assert_eq!(
                rotate_rgba_quarter_turn(&image, turn).unwrap().cicp,
                image.cicp
            );
        }
    }

    #[test]
    fn quarter_turns_preserve_pixels_and_dimensions() {
        let clockwise =
            rotate_rgba_quarter_turn(&fixture(), BigImageQuarterTurn::Clockwise).unwrap();
        assert_eq!((clockwise.width, clockwise.height), (3, 2));
        assert_eq!(red_channels(&clockwise), [5, 3, 1, 6, 4, 2]);

        let counter =
            rotate_rgba_quarter_turn(&fixture(), BigImageQuarterTurn::CounterClockwise).unwrap();
        assert_eq!((counter.width, counter.height), (3, 2));
        assert_eq!(red_channels(&counter), [2, 4, 6, 1, 3, 5]);

        let half = rotate_rgba_quarter_turn(&fixture(), BigImageQuarterTurn::Half).unwrap();
        assert_eq!((half.width, half.height), (2, 3));
        assert_eq!(red_channels(&half), [6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn invalid_layout_is_rejected_without_allocation() {
        for image in [
            BigRgbaImage {
                rgba: vec![],
                width: 0,
                height: 1,
                cicp: None,
                opaque: false,
            },
            BigRgbaImage {
                rgba: vec![0; 3],
                width: 1,
                height: 1,
                cicp: None,
                opaque: false,
            },
            BigRgbaImage {
                rgba: vec![],
                width: i32::MAX,
                height: i32::MAX,
                cicp: None,
                opaque: false,
            },
        ] {
            assert!(matches!(
                rotate_rgba_quarter_turn(&image, BigImageQuarterTurn::Clockwise),
                Err(BigImageError::InvalidBytePixels)
            ));
        }
    }
}
