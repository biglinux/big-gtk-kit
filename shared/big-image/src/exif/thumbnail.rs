// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The JPEG thumbnail a camera embeds in its EXIF block (the second directory,
//! tags 0x0201 and 0x0202), found in the head of the file alone. The bytes come
//! from another device: every offset is checked against the head, and the walk
//! stops after a fixed number of segments.

use super::{Reader, tiff_reader};

/// Marker segments looked at before giving up: EXIF sits right after SOI and
/// the JFIF header.
const MAX_SEGMENTS: usize = 16;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_THUMBNAIL_OFFSET: u16 = 0x0201;
const TAG_THUMBNAIL_LENGTH: u16 = 0x0202;

/// A JPEG found inside the EXIF block of a JPEG or a HEIC file.
#[derive(Debug, PartialEq, Eq)]
pub struct EmbeddedThumbnail<'a> {
    /// The thumbnail file, from its SOI marker.
    pub jpeg: &'a [u8],
    /// The picture's EXIF orientation (1 to 8), which the thumbnail does not
    /// carry itself; 1 when the file says nothing sensible.
    pub orientation: u16,
}

/// The embedded thumbnail of the JPEG or HEIC file whose first bytes are
/// `head`, or `None` when the head holds no whole thumbnail.
#[must_use]
pub fn embedded_thumbnail(head: &[u8]) -> Option<EmbeddedThumbnail<'_>> {
    if head.get(4..8) == Some(b"ftyp") {
        return super::heif::thumbnail(head);
    }
    thumbnail_in(jpeg_exif(head)?)
}

/// The TIFF block of the EXIF segment of the JPEG file whose first bytes are
/// `head`, cut where the head ends.
pub(super) fn jpeg_exif(head: &[u8]) -> Option<&[u8]> {
    if head.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2usize;
    for _ in 0..MAX_SEGMENTS {
        // Fill bytes may precede a marker.
        while head.get(at) == Some(&0xFF) && head.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if *head.get(at)? != 0xFF {
            return None;
        }
        let marker = *head.get(at + 1)?;
        match marker {
            // Entropy-coded data or the end: no EXIF past here.
            0xD9 | 0xDA => return None,
            // Standalone markers carry no length.
            0x01 | 0xD0..=0xD7 => {
                at += 2;
                continue;
            }
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*head.get(at + 2)?, *head.get(at + 3)?]));
        let end = (at + 2).checked_add(length)?;
        // A segment may run past the head (a full-size APP1 ends at 64 KiB
        // plus the markers before it); what it holds is checked against what
        // was read.
        let segment = head.get(at + 4..end.min(head.len()))?;
        if marker == 0xE1
            && let Some(tiff) = segment.strip_prefix(b"Exif\0\0")
        {
            return Some(tiff);
        }
        at = end;
    }
    None
}

/// The picture's orientation and the offset of the second directory, from the
/// TIFF block `reader` reads.
fn first_directory(reader: &Reader<'_>) -> Option<(u16, u32)> {
    let first = usize::try_from(reader.u32_at(4)?).ok()?;
    let mut orientation = 1;
    let next = scan(reader, first, |tag, value| {
        if tag == TAG_ORIENTATION {
            // Some apps write 0 (undefined): upright.
            orientation = u16::try_from(value)
                .ok()
                .filter(|o| (1..=8).contains(o))
                .unwrap_or(1);
        }
        Some(())
    })?;
    Some((orientation, next))
}

/// The orientation a TIFF block states, 1 when it states none.
pub(super) fn tiff_orientation(tiff: &[u8]) -> u16 {
    tiff_reader(tiff)
        .and_then(|reader| first_directory(&reader))
        .map_or(1, |(orientation, _)| orientation)
}

/// The thumbnail the second directory of the TIFF block `tiff` holds.
pub(super) fn thumbnail_in(tiff: &[u8]) -> Option<EmbeddedThumbnail<'_>> {
    let reader = tiff_reader(tiff)?;
    let (orientation, next) = first_directory(&reader)?;
    let second = usize::try_from(next).ok().filter(|at| *at != 0)?;
    let (mut offset, mut length) = (None, None);
    scan(&reader, second, |tag, value| {
        match tag {
            TAG_THUMBNAIL_OFFSET => offset = Some(value),
            TAG_THUMBNAIL_LENGTH => length = Some(value),
            _ => {}
        }
        Some(())
    })?;
    let start = usize::try_from(offset?).ok()?;
    let end = start.checked_add(usize::try_from(length?).ok()?)?;
    let jpeg = tiff.get(start..end)?;
    (jpeg.get(..2)? == [0xFF, 0xD8]).then_some(EmbeddedThumbnail { jpeg, orientation })
}

/// Hand every single SHORT or LONG entry of the directory at `at` to `each`
/// as `(tag, value)`, and return the offset of the next directory. `each`
/// returns `None` to reject a value.
fn scan(
    reader: &Reader<'_>,
    at: usize,
    mut each: impl FnMut(u16, u32) -> Option<()>,
) -> Option<u32> {
    let count = usize::from(reader.u16_at(at)?);
    for index in 0..count {
        let entry = at.checked_add(2)?.checked_add(index * 12)?;
        let (tag, format) = (reader.u16_at(entry)?, reader.u16_at(entry + 2)?);
        // The value of a single SHORT or LONG sits in the entry itself.
        let value = match format {
            3 => u32::from(reader.u16_at(entry + 8)?),
            4 => reader.u32_at(entry + 8)?,
            _ => continue,
        };
        each(tag, value)?;
    }
    reader.u32_at(at.checked_add(2)?.checked_add(count * 12)?)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    const THUMBNAIL: &[u8] = &[0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9];

    /// The TIFF block of a camera file: an orientation and, when `thumbnail`
    /// is set, a second directory holding it.
    pub(in crate::exif) fn camera_tiff(orientation: u16, thumbnail: Option<&[u8]>) -> Vec<u8> {
        let entry = |tag: u16, format: u16, value: u32| {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&format.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
            bytes
        };
        let mut tiff = b"II".to_vec();
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        // First directory at 8: one entry, then the offset of the second at 26.
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend(entry(TAG_ORIENTATION, 3, orientation.into()));
        tiff.extend_from_slice(&(if thumbnail.is_some() { 26u32 } else { 0 }).to_le_bytes());
        if let Some(jpeg) = thumbnail {
            tiff.extend_from_slice(&2u16.to_le_bytes());
            tiff.extend(entry(TAG_THUMBNAIL_OFFSET, 4, 56));
            tiff.extend(entry(TAG_THUMBNAIL_LENGTH, 4, jpeg.len() as u32));
            tiff.extend_from_slice(&0u32.to_le_bytes());
            tiff.extend_from_slice(jpeg);
        }
        tiff
    }

    /// A JPEG head: SOI, an EXIF segment, then a scan marker.
    fn camera_head(orientation: u16, thumbnail: Option<&[u8]>) -> Vec<u8> {
        let tiff = camera_tiff(orientation, thumbnail);
        let mut head = vec![0xFF, 0xD8, 0xFF, 0xE1];
        head.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        head.extend_from_slice(b"Exif\0\0");
        head.extend(tiff);
        head.extend_from_slice(&[0xFF, 0xDA, 0, 2]);
        head
    }

    #[test]
    fn thumbnail_comes_with_orientation() {
        let head = camera_head(6, Some(THUMBNAIL));
        assert_eq!(
            embedded_thumbnail(&head),
            Some(EmbeddedThumbnail {
                jpeg: THUMBNAIL,
                orientation: 6
            })
        );
    }

    #[test]
    fn segment_past_the_head_still_yields() {
        let mut head = camera_head(1, Some(THUMBNAIL));
        let length = u16::from_be_bytes([head[4], head[5]]) + 100;
        head[4..6].copy_from_slice(&length.to_be_bytes());
        assert_eq!(embedded_thumbnail(&head).map(|t| t.jpeg), Some(THUMBNAIL));
    }

    #[test]
    fn undefined_orientation_is_upright() {
        let head = camera_head(0, Some(THUMBNAIL));
        assert_eq!(embedded_thumbnail(&head).map(|t| t.orientation), Some(1));
    }

    #[test]
    fn camera_without_thumbnail_has_none() {
        assert_eq!(embedded_thumbnail(&camera_head(1, None)), None);
    }

    #[test]
    fn truncated_head_has_none() {
        let head = camera_head(1, Some(THUMBNAIL));
        for cut in 0..head.len() - 4 {
            assert_eq!(embedded_thumbnail(&head[..cut]), None, "cut at {cut}");
        }
    }

    #[test]
    fn other_formats_have_none() {
        assert_eq!(embedded_thumbnail(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(embedded_thumbnail(&[]), None);
    }
}
