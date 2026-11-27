// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Changing how many pixels a decoded frame has: with gdk-pixbuf's bilinear
//! filter, and 16-bit RGB, which gdk-pixbuf cannot hold, by means of blocks.

use super::{BigImageError, BigRgbaImage};

/// The size `natural` becomes when it is made to fit inside `bound` without
/// changing its proportions.
///
/// Never upscales: asking a 400-pixel-wide photograph for 1920 gives back 400,
/// because the pixels to fill the difference do not exist and inventing them
/// makes a bigger, blurrier file.
#[must_use]
pub fn fit_within(natural: (i32, i32), bound: (i32, i32)) -> (i32, i32) {
    let (width, height) = natural;
    let (max_width, max_height) = bound;
    if width <= 0 || height <= 0 || max_width <= 0 || max_height <= 0 {
        return natural;
    }
    let scale = f64::from(max_width) / f64::from(width);
    let scale = scale.min(f64::from(max_height) / f64::from(height));
    if scale >= 1.0 {
        return natural;
    }
    (
        ((f64::from(width) * scale).round() as i32).max(1),
        ((f64::from(height) * scale).round() as i32).max(1),
    )
}

/// Return a packed RGBA copy of `image` that is `width` by `height` pixels.
///
/// The source is never modified. Proportions are the caller's business: this
/// writes exactly the size it is given, which is what a dialog offering
/// separate width and height fields promises.
///
/// # Errors
///
/// Returns [`BigImageError::InvalidBytePixels`] for a malformed source buffer,
/// a non-positive target, or a target past the pixel budget.
pub fn resize_rgba(
    image: &BigRgbaImage,
    width: i32,
    height: i32,
) -> Result<BigRgbaImage, BigImageError> {
    if width <= 0
        || height <= 0
        || u64::try_from(i64::from(width) * i64::from(height))
            .map_or(true, |pixels| pixels > crate::secure::MAX_SOURCE_PIXELS)
        || crate::texture_stride(image).is_none()
    {
        return Err(BigImageError::InvalidBytePixels);
    }
    if (width, height) == (image.width, image.height) {
        return Ok(image.clone());
    }
    let source = super::save::rgba_to_pixbuf(image).ok_or(BigImageError::InvalidBytePixels)?;
    let scaled = source
        .scale_simple(width, height, gtk::gdk_pixbuf::InterpType::Bilinear)
        .ok_or(BigImageError::InvalidBytePixels)?;
    let mut resized = super::pixbuf_to_rgba(&scaled).ok_or(BigImageError::InvalidBytePixels)?;
    // The pixels are the same color space they arrived in; only their count
    // changed. Dropping this is how a Display-P3 photograph gets written back
    // as if it were sRGB.
    resized.cicp = image.cicp;
    // The pixbuf always carries alpha, which reads back as not opaque; an
    // opaque source scales to all-255 alpha.
    resized.opaque = image.opaque;
    Ok(resized)
}

/// Packed 16-bit RGB rows, native-endian, of `bytes` (`stride` apart and
/// `from` big) shrunk to `to`: each pixel the mean of the pixels that fall in
/// it. `None` when `to` is larger than `from` or empty, or `bytes` too short.
#[must_use]
pub fn shrink_rgb16(
    bytes: &[u8],
    stride: usize,
    from: (usize, usize),
    to: (usize, usize),
) -> Option<Vec<u8>> {
    let ((from_width, from_height), (width, height)) = (from, to);
    if width == 0 || height == 0 || width > from_width || height > from_height {
        return None;
    }
    let row = from_width.checked_mul(6)?;
    let span = stride.checked_mul(from_height - 1)?.checked_add(row)?;
    if stride < row || bytes.len() < span {
        return None;
    }
    let column: Vec<usize> = (0..from_width).map(|x| x * width / from_width).collect();
    let mut across = vec![0_u64; width];
    for &at in &column {
        across[at] += 1;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(width * height * 6).ok()?;
    let mut sums = vec![0_u64; width * 3];
    let mut rows = 0_u64;
    for y in 0..from_height {
        let line = &bytes[y * stride..y * stride + from_width * 6];
        for (pixel, &at) in line.as_chunks::<6>().0.iter().zip(&column) {
            for channel in 0..3 {
                sums[at * 3 + channel] += u64::from(u16::from_ne_bytes([
                    pixel[channel * 2],
                    pixel[channel * 2 + 1],
                ]));
            }
        }
        rows += 1;
        if (y + 1) * height / from_height != y * height / from_height {
            for (sum, &count) in sums.as_chunks::<3>().0.iter().zip(&across) {
                let count = count * rows;
                for &total in sum {
                    let mean = u16::try_from((total + count / 2) / count).unwrap_or(u16::MAX);
                    out.extend_from_slice(&mean.to_ne_bytes());
                }
            }
            sums.fill(0);
            rows = 0;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: i32, height: i32) -> BigRgbaImage {
        BigRgbaImage {
            rgba: vec![200; (width * height * 4) as usize],
            width,
            height,
            cicp: None,
            opaque: false,
        }
    }

    #[test]
    fn fitting_shrinks_proportionally_and_never_grows() {
        assert_eq!(fit_within((4000, 3000), (1920, 1920)), (1920, 1440));
        assert_eq!(fit_within((3000, 4000), (1920, 1920)), (1440, 1920));
        assert_eq!(
            fit_within((400, 300), (1920, 1920)),
            (400, 300),
            "a small photograph stays small"
        );
    }

    #[test]
    fn resizing_writes_exactly_the_requested_size() {
        let resized = resize_rgba(&flat(40, 20), 10, 5).expect("bounded target");
        assert_eq!((resized.width, resized.height), (10, 5));
        assert_eq!(resized.rgba.len(), 10 * 5 * 4);
    }

    #[test]
    fn resizing_keeps_an_opaque_picture_opaque() {
        let opaque = BigRgbaImage {
            rgba: [10, 200, 90, 255].repeat(40 * 20),
            opaque: true,
            ..flat(40, 20)
        };
        let resized = resize_rgba(&opaque, 10, 5).expect("bounded target");
        assert!(resized.opaque);
        assert!(
            resized
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
        assert!(
            !resize_rgba(&flat(40, 20), 10, 5)
                .expect("bounded target")
                .opaque
        );
    }

    /// A 3×2 picture to 2×1: the first pixel the mean of two columns of
    /// two rows, the second of one.
    #[test]
    fn rgb16_shrinks_to_block_means() {
        let pixels: [[u16; 3]; 6] = [
            [0, 100, 65535],
            [2, 200, 65535],
            [7, 7, 7],
            [4, 300, 0],
            [6, 401, 0],
            [9, 9, 9],
        ];
        let bytes: Vec<u8> = pixels
            .iter()
            .flatten()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        let shrunk = shrink_rgb16(&bytes, 18, (3, 2), (2, 1)).expect("shrunk");
        let means: Vec<u16> = shrunk
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_ne_bytes(pair))
            .collect();
        assert_eq!(means, [3, 250, 32768, 8, 8, 8]);
        assert!(shrink_rgb16(&bytes, 18, (3, 2), (4, 1)).is_none());
        assert!(shrink_rgb16(&bytes[..30], 18, (3, 2), (2, 1)).is_none());
        // A stride whose second row lies past any buffer.
        assert!(shrink_rgb16(&bytes[..6], usize::MAX, (1, 2), (1, 1)).is_none());
    }

    /// A buffer that does not match its size is refused even when nothing
    /// is scaled.
    #[test]
    fn short_buffer_refused_at_same_size() {
        let mut short = flat(1, 1);
        short.rgba.truncate(3);
        assert!(matches!(
            resize_rgba(&short, 1, 1),
            Err(BigImageError::InvalidBytePixels)
        ));
    }

    #[test]
    fn impossible_target_is_refused() {
        for (width, height) in [(0, 10), (10, -1), (20_000, 20_000)] {
            assert!(resize_rgba(&flat(8, 8), width, height).is_err());
        }
    }
}
