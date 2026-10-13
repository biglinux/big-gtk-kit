// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! How wide a line of text is, mostly without laying it out.

use gtk::pango;
use std::collections::HashMap;

/// The widths of single characters in one font, summed into the width of a name.
///
/// Laying out the 6 800 names of `/usr/bin` one by one took 28 ms; summing
/// their characters from this table takes under one, and the table holds the
/// few dozen characters they are written with. Measured over 27 000 real file
/// names the sum is long by at most a few pixels, the kerning it leaves out,
/// and practically never short. Text whose characters change shape next to
/// each other (Arabic, Devanagari, combining accents, emoji sequences), or that
/// holds a line break, is laid out whole instead: summing it was off by fifty.
#[derive(Default)]
pub(crate) struct CharWidths(HashMap<char, i32>);

impl CharWidths {
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    /// The width of `text` on one line, in pixels, in the font of `layout`:
    /// a plain layout, with no width, wrapping or ellipsis, whose text this
    /// replaces.
    pub(crate) fn measure(&mut self, layout: &pango::Layout, text: &str) -> f32 {
        let units = if text.chars().all(stands_alone) {
            let mut total = 0;
            for ch in text.chars() {
                total += *self
                    .0
                    .entry(ch)
                    .or_insert_with(|| logical_width(layout, ch.encode_utf8(&mut [0; 4])));
            }
            total
        } else {
            logical_width(layout, text)
        };
        units as f32 / pango::SCALE as f32
    }
}

fn logical_width(layout: &pango::Layout, text: &str) -> i32 {
    layout.set_text(text);
    layout.extents().1.width()
}

/// Whether `ch` is drawn the same whatever stands beside it, kerning aside:
/// Latin, Greek, Cyrillic, common punctuation and symbols, kana, Hangul
/// syllables and CJK ideographs, without their combining marks. Anything
/// else is left to the shaper.
fn stands_alone(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x20..=0x7e
            | 0xa0..=0x2ff
            | 0x370..=0x482
            | 0x48a..=0x52f
            | 0x1e00..=0x1fff
            | 0x2010..=0x2027
            | 0x2030..=0x205e
            | 0x20a0..=0x20bf
            | 0x2100..=0x27ff
            | 0x3000..=0x3029
            | 0x3030..=0x3098
            | 0x309b..=0x9fff
            | 0xac00..=0xd7a3
            | 0xff00..=0xffef
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;

    #[gtk::test]
    #[ignore = "needs a display"]
    fn summed_width_matches_layout() {
        let layout = gtk::Label::new(None).create_pango_layout(None);
        let exact = |text: &str| logical_width(&layout, text) as f32 / pango::SCALE as f32;
        let mut widths = CharWidths::default();
        for text in [
            "ecryptfs-rewrap-passphrase",
            "AVATAR Tower WAVE.png",
            "Relatório Anual — Versão Final.odt",
            "Привет мир.doc",
            "日本語のファイル名.txt",
            "한국어 파일.txt",
        ] {
            let summed = widths.measure(&layout, text);
            let laid_out = exact(text);
            assert!(
                summed >= laid_out - 1.0 && summed <= laid_out + 8.0,
                "{text:?}: summed {summed}, laid out {laid_out}"
            );
        }
        // Shaped together, or on two lines: measured whole, so exactly.
        for text in [
            "مرحبا بالعالم.pdf",
            "नमस्ते दुनिया.txt",
            "e\u{301}cole.txt",
            "👨\u{200d}👩\u{200d}👧 família.jpg",
            "first line\nsecond line.pdf",
        ] {
            assert_eq!(widths.measure(&layout, text), exact(text), "{text:?}");
        }
    }
}
