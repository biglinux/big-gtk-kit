// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Turning or mirroring a JPEG without decoding it, by rewriting its EXIF
//! orientation tag. Every read is bounded by the slice; a file this cannot
//! rewrite in place answers `None`.

/// How a JPEG's stored pixels are shown, after an EXIF orientation: quarter
/// turns clockwise after an optional mirror left to right, as `(turns, mirrored)`.
const ORIENTATIONS: [(u8, bool); 8] = [
    (0, false), // 1: as stored
    (0, true),  // 2: mirrored
    (2, false), // 3: half a turn
    (2, true),  // 4: mirrored top to bottom
    (3, true),  // 5: mirrored, then a quarter anticlockwise
    (1, false), // 6: a quarter clockwise
    (1, true),  // 7: mirrored, then a quarter clockwise
    (3, false), // 8: a quarter anticlockwise
];

const ORIENTATION_TAG: u16 = 0x0112;
const SHORT: u16 = 3;
/// Most directory entries looked at before giving up on finding the tag.
const MAX_ENTRIES: usize = 512;

/// `bytes`, a JPEG, shown `quarter_turns` clockwise further after mirroring
/// it left to right when `mirrored`, with nothing but its orientation changed.
///
/// A JPEG without EXIF gets an EXIF block holding only the orientation.
/// `None` when the bytes are not a JPEG, or carry EXIF without an
/// orientation tag to rewrite.
#[must_use]
pub fn turned_jpeg(bytes: &[u8], quarter_turns: u8, mirrored: bool) -> Option<Vec<u8>> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    // Where an EXIF block goes when there is none: after a JFIF one, which
    // readers expect first.
    let mut insert_at = 2;
    loop {
        let marker = *bytes.get(at..at + 2)?.get(1)?;
        if bytes[at] != 0xFF || marker == 0xDA || marker == 0xD9 {
            break;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(at + 2)?,
            *bytes.get(at + 3)?,
        ]));
        let segment = bytes.get(at + 4..at + 2 + length)?;
        if marker == 0xE1 && segment.starts_with(b"Exif\0\0") {
            let tiff = at + 4 + 6;
            let value = orientation_offset(bytes.get(tiff..at + 2 + length)?)? + tiff;
            let big_endian = bytes[tiff] == b'M';
            let read = [bytes[value], bytes[value + 1]];
            let current = if big_endian {
                u16::from_be_bytes(read)
            } else {
                u16::from_le_bytes(read)
            };
            let next = composed(current, quarter_turns, mirrored);
            let mut turned = bytes.to_vec();
            let written = if big_endian {
                next.to_be_bytes()
            } else {
                next.to_le_bytes()
            };
            turned[value..value + 2].copy_from_slice(&written);
            return Some(turned);
        }
        if marker == 0xE0 {
            insert_at = at + 2 + length;
        }
        at += 2 + length;
    }
    let mut turned = Vec::with_capacity(bytes.len() + 36);
    turned.extend_from_slice(&bytes[..insert_at]);
    turned.extend_from_slice(&exif_block(composed(1, quarter_turns, mirrored)));
    turned.extend_from_slice(&bytes[insert_at..]);
    Some(turned)
}

/// Where in `tiff` the orientation tag's value is.
fn orientation_offset(tiff: &[u8]) -> Option<usize> {
    let big_endian = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |offset: usize| {
        let pair = [*tiff.get(offset)?, *tiff.get(offset + 1)?];
        Some(if big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    };
    let quad = tiff.get(4..8)?;
    let directory = usize::try_from(if big_endian {
        u32::from_be_bytes([quad[0], quad[1], quad[2], quad[3]])
    } else {
        u32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]])
    })
    .ok()?;
    let count = usize::from(u16_at(directory)?).min(MAX_ENTRIES);
    (0..count).find_map(|index| {
        let entry = directory + 2 + index * 12;
        (u16_at(entry)? == ORIENTATION_TAG && u16_at(entry + 2)? == SHORT)
            .then_some(entry + 8)
            .filter(|value| value + 2 <= tiff.len())
    })
}

/// The EXIF orientation that shows what `current` shows, turned and mirrored
/// further as asked.
fn composed(current: u16, quarter_turns: u8, mirrored: bool) -> u16 {
    let (turns, flipped) = usize::from(current)
        .checked_sub(1)
        .and_then(|index| ORIENTATIONS.get(index))
        .copied()
        .unwrap_or((0, false));
    // A mirror after a turn is the turn the other way after the mirror.
    let asked = quarter_turns % 4;
    let turns = if mirrored {
        (asked + 4 - turns) % 4
    } else {
        (asked + turns) % 4
    };
    let wanted = (turns, flipped != mirrored);
    let index = ORIENTATIONS
        .iter()
        .position(|orientation| *orientation == wanted)
        .unwrap_or(0);
    (index + 1) as u16
}

/// An APP1 EXIF block holding one entry: the orientation.
fn exif_block(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::with_capacity(26);
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42_u16.to_le_bytes());
    tiff.extend_from_slice(&8_u32.to_le_bytes());
    tiff.extend_from_slice(&1_u16.to_le_bytes());
    tiff.extend_from_slice(&ORIENTATION_TAG.to_le_bytes());
    tiff.extend_from_slice(&SHORT.to_le_bytes());
    tiff.extend_from_slice(&1_u32.to_le_bytes());
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0_u32.to_le_bytes());
    let length = (2 + 6 + tiff.len()) as u16;
    let mut block = vec![0xFF, 0xE1];
    block.extend_from_slice(&length.to_be_bytes());
    block.extend_from_slice(b"Exif\0\0");
    block.extend_from_slice(&tiff);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The orientation a JPEG shows after `turned_jpeg`, read back.
    fn orientation_of(bytes: &[u8]) -> u16 {
        let start = bytes
            .windows(6)
            .position(|window| window == b"Exif\0\0")
            .expect("an EXIF block")
            + 6;
        let tiff = &bytes[start..];
        let value = orientation_offset(tiff).expect("an orientation");
        if &tiff[..2] == b"MM" {
            u16::from_be_bytes([tiff[value], tiff[value + 1]])
        } else {
            u16::from_le_bytes([tiff[value], tiff[value + 1]])
        }
    }

    /// A JPEG reduced to the parts this reads: start, a JFIF block, and the
    /// scan it stops at.
    fn plain_jpeg() -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x07];
        bytes.extend_from_slice(b"JFIF\0");
        bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0x11, 0x22, 0xFF, 0xD9]);
        bytes
    }

    /// Every orientation, turned four quarters or mirrored twice, is itself
    /// again; a quarter turn of an unturned picture is orientation 6.
    #[test]
    fn turns_compose_as_shown() {
        for current in 1..=8 {
            let mut orientation = current;
            for _ in 0..4 {
                orientation = composed(orientation, 1, false);
            }
            assert_eq!(orientation, current);
            assert_eq!(composed(composed(current, 0, true), 0, true), current);
        }
        assert_eq!(composed(1, 1, false), 6);
        assert_eq!(composed(1, 3, false), 8);
        assert_eq!(composed(6, 1, false), 3);
        assert_eq!(composed(1, 0, true), 2);
        // Any count of quarter turns, the largest one a byte holds included.
        assert_eq!(composed(1, 255, false), 8);
        assert_eq!(composed(1, 255, true), composed(1, 3, true));
    }

    /// A JPEG without EXIF gets one after its JFIF block, and the pixels'
    /// bytes are untouched.
    #[test]
    fn a_plain_jpeg_gets_an_orientation() {
        let plain = plain_jpeg();
        let turned = turned_jpeg(&plain, 1, false).expect("rewritten");
        assert_eq!(orientation_of(&turned), 6);
        assert_eq!(&turned[..11], &plain[..11], "start and JFIF first");
        assert!(turned.ends_with(&plain[11..]), "the scan as it was");
    }

    /// An orientation already there is changed in place, in either byte
    /// order, and nothing else moves.
    #[test]
    fn orientation_rewritten_in_place() {
        let once = turned_jpeg(&plain_jpeg(), 1, false).expect("rewritten");
        let twice = turned_jpeg(&once, 1, false).expect("rewritten again");
        assert_eq!(twice.len(), once.len());
        assert_eq!(orientation_of(&twice), 3);

        let mut big_endian = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x22];
        big_endian.extend_from_slice(
            b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0",
        );
        big_endian.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        let turned = turned_jpeg(&big_endian, 1, false).expect("rewritten");
        assert_eq!(orientation_of(&turned), 3);
    }

    /// What is not a JPEG, or is cut short, is refused; it never panics.
    #[test]
    fn broken_bytes_are_refused() {
        assert_eq!(turned_jpeg(b"\x89PNG\r\n", 1, false), None);
        assert_eq!(turned_jpeg(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF], 1, false), None);
        let plain = plain_jpeg();
        for cut in 0..plain.len() {
            let _ = turned_jpeg(&plain[..cut], 1, false);
        }
    }
}
