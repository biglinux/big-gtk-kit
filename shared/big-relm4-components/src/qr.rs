// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Drawing a QR code, for anything that wants a phone pointed at the screen.
//!
//! Shared by the shell's Wi-Fi password, the clipboard and the camera's
//! address; each keeps its own policy around the code.

use gtk::prelude::*;

/// The margin readers use to find the code's edges.
///
/// Part of the code, not padding: a code drawn to the border of its own widget
/// is one many phones simply do not see.
const QUIET_MODULES: i32 = 4;

/// The side of the PNG, in pixels.
///
/// Fixed rather than the widget's size: what lands on the clipboard is going
/// into somebody's document, and it should not depend on how wide a popover
/// happened to be.
const PNG_SIDE: i32 = 512;

/// A widget drawing `text` as a QR code, or nothing when it cannot be encoded.
///
/// `None` rather than a broken picture: the only way this fails is a payload
/// longer than the largest QR version holds, and a gray square a camera cannot
/// read is worse than a sentence saying so.
///
/// `accessible_label` is what a screen reader says. A code is not decoration,
/// and an unnamed drawing is one the reader skips, so the caller must name it in
/// its own words.
#[must_use]
pub fn drawing(text: &str, side: i32, accessible_label: &str) -> Option<gtk::DrawingArea> {
    let code = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let area = gtk::DrawingArea::builder()
        .content_width(side)
        .content_height(side)
        .build();
    area.update_property(&[gtk::accessible::Property::Label(accessible_label)]);
    area.set_draw_func(move |_, context, width, height| {
        draw(&code, context, width, height);
    });
    Some(area)
}

/// The same code as a PNG, for putting on the clipboard or in a file.
#[must_use]
pub fn png(text: &str) -> Option<Vec<u8>> {
    let code = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let surface = cairo::ImageSurface::create(cairo::Format::Rgb24, PNG_SIDE, PNG_SIDE).ok()?;
    {
        let context = cairo::Context::new(&surface).ok()?;
        draw(&code, &context, PNG_SIDE, PNG_SIDE);
    }
    let mut bytes = Vec::new();
    surface.write_to_png(&mut bytes).ok()?;
    Some(bytes)
}

/// Dark modules on a light square, with the quiet zone the readers need.
///
/// White and black rather than the theme's colors, in both light and dark:
/// many phone cameras do not read a code on a dark background.
fn draw(code: &qrcodegen::QrCode, context: &cairo::Context, width: i32, height: i32) {
    let modules = code.size();
    let across = modules + QUIET_MODULES * 2;
    let scale = f64::from(width.min(height)) / f64::from(across);

    context.set_source_rgb(1.0, 1.0, 1.0);
    let _ = context.paint();
    context.set_source_rgb(0.0, 0.0, 0.0);
    for y in 0..modules {
        for x in 0..modules {
            if code.get_module(x, y) {
                context.rectangle(
                    f64::from(x + QUIET_MODULES) * scale,
                    f64::from(y + QUIET_MODULES) * scale,
                    scale,
                    scale,
                );
            }
        }
    }
    let _ = context.fill();
}

#[cfg(test)]
mod tests {
    use super::png;

    #[test]
    fn rendered_code_is_png() {
        let bytes = png("https://example.invalid/frame").expect("a short payload encodes");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn oversized_payload_yields_no_code() {
        // The largest QR version holds a few thousand bytes; this is past it.
        assert!(png(&"x".repeat(8_000)).is_none());
    }
}
