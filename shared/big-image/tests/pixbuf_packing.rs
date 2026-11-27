// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! An RGB (no-alpha) pixbuf is widened to RGBA as it is packed, keeping
//! channel order and padding the alpha, and is reported opaque so the texture
//! can skip the premultiply pass. A swapped channel here would tint every album
//! art and video poster, and a wrong `opaque` flag would misdeclare the memory
//! format.

#![expect(clippy::expect_used, reason = "a failed fixture fails the test")]

use gtk::gdk::prelude::TextureExtManual;
use gtk::gdk_pixbuf::{Colorspace, Pixbuf};

fn filled(has_alpha: bool, channels: [u8; 4]) -> Pixbuf {
    let pixbuf = Pixbuf::new(Colorspace::Rgb, has_alpha, 8, 2, 2).expect("pixbuf");
    // fill() takes 0xRRGGBBAA and ignores the alpha byte on an RGB pixbuf.
    pixbuf.fill(u32::from_be_bytes(channels));
    pixbuf
}

#[test]
fn rgb_source_is_widened_with_opaque_alpha() {
    let image = big_image::pixbuf_to_rgba(&filled(false, [10, 20, 30, 0])).expect("packed image");

    assert_eq!((image.width, image.height), (2, 2));
    assert!(image.opaque, "an RGB source carries no transparency");
    assert_eq!(image.rgba, [10, 20, 30, 255].repeat(4));
}

#[test]
fn rgba_source_keeps_alpha() {
    let image = big_image::pixbuf_to_rgba(&filled(true, [10, 20, 30, 128])).expect("packed image");

    assert!(!image.opaque, "an RGBA source may carry transparency");
    assert_eq!(image.rgba, [10, 20, 30, 128].repeat(4));
}

/// The opaque tag is a memory-format declaration, not a color change: a fully
/// opaque color downloads the same whether it arrived as RGB or RGBA.
#[test]
fn opaque_tag_keeps_downloaded_color() {
    big_testkit::harness::require_gtk_test(|| {
        let download = |pixbuf: &Pixbuf| {
            let image = big_image::pixbuf_to_rgba(pixbuf).expect("packed image");
            let mut pixels = vec![0_u8; 2 * 2 * 4];
            big_image::rgba_to_texture(&image)
                .expect("texture")
                .download(&mut pixels, 2 * 4);
            pixels
        };
        assert_eq!(
            download(&filled(false, [10, 20, 30, 0])),
            download(&filled(true, [10, 20, 30, 255]))
        );
    });
}
