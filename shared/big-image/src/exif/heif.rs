// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The JPEG thumbnail a HEIC photo carries, found in the head of the file
//! alone: a `thmb` item of type `jpeg`, or else the second EXIF directory of
//! the `Exif` item. The bytes come from another device: boxes are walked with
//! checked slices, the counts are capped, and an item that lies past the head
//! is no thumbnail.

use super::EmbeddedThumbnail;
use super::thumbnail::{thumbnail_in, tiff_orientation};

/// Boxes, items and references looked at in one list before giving up. A
/// tiled photo lists a few hundred tile items ahead of `Exif` and `thmb`.
const MAX_ENTRIES: usize = 4096;
const BRANDS: [&[u8; 4]; 6] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"mif1"];

/// Big-endian reads that stop at the end of the slice.
struct Cursor<'a> {
    bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.bytes.split_at_checked(count)?;
        self.bytes = rest;
        Some(taken)
    }

    /// An unsigned integer of `width` bytes (0 to 8).
    fn uint(&mut self, width: usize) -> Option<u64> {
        if width > 8 {
            return None;
        }
        Some(
            self.take(width)?
                .iter()
                .fold(0, |at, byte| at << 8 | u64::from(*byte)),
        )
    }
}

/// The boxes of `bytes` as `(type, payload)`. A box that runs past the slice
/// keeps what is there.
fn boxes(mut rest: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    std::iter::from_fn(move || {
        let mut header = Cursor { bytes: rest };
        let size = header.uint(4)?;
        let kind = header.take(4)?;
        let (header_len, total) = match size {
            0 => (8, rest.len()),
            1 => (16, usize::try_from(header.uint(8)?).ok()?),
            _ => (8, usize::try_from(size).ok()?),
        };
        let end = total.min(rest.len());
        let payload = rest.get(header_len..end)?;
        rest = &rest[end..];
        Some((kind, payload))
    })
    .take(MAX_ENTRIES)
}

/// The id width of a box whose id is 16 bits in version 0 and 32 after.
fn id_width(version: u64) -> usize {
    if version == 0 { 2 } else { 4 }
}

/// `(id, type)` of every item `iinf` declares.
fn item_types(iinf: &[u8]) -> Option<Vec<(u64, [u8; 4])>> {
    let mut cursor = Cursor { bytes: iinf };
    let version = cursor.uint(1)?;
    cursor.take(3)?;
    cursor.uint(if version == 0 { 2 } else { 4 })?;
    let mut types = Vec::new();
    for (kind, entry) in boxes(cursor.bytes) {
        if kind != b"infe" {
            continue;
        }
        let mut entry = Cursor { bytes: entry };
        let version = entry.uint(1)?;
        entry.take(3)?;
        // Versions 0 and 1 name no item type.
        if version < 2 {
            continue;
        }
        let id = entry.uint(if version == 2 { 2 } else { 4 })?;
        entry.take(2)?;
        types.push((id, entry.take(4)?.try_into().ok()?));
    }
    Some(types)
}

/// The item that the first `thmb` reference of `iref` says is a thumbnail.
fn thumbnail_item(iref: &[u8]) -> Option<u64> {
    let mut cursor = Cursor { bytes: iref };
    let version = cursor.uint(1)?;
    cursor.take(3)?;
    let (_, reference) = boxes(cursor.bytes).find(|(kind, _)| *kind == b"thmb")?;
    Cursor { bytes: reference }.uint(id_width(version))
}

/// `(id, offset, length)` of every item `iloc` places in the file itself, in
/// one extent.
fn locations(iloc: &[u8]) -> Option<Vec<(u64, u64, u64)>> {
    let mut cursor = Cursor { bytes: iloc };
    let version = cursor.uint(1)?;
    cursor.take(3)?;
    let sizes = cursor.uint(2)?;
    let (offset_width, length_width) = (
        usize::try_from(sizes >> 12).ok()?,
        usize::try_from(sizes >> 8 & 15).ok()?,
    );
    let (base_width, index_width) = (
        usize::try_from(sizes >> 4 & 15).ok()?,
        usize::try_from(sizes & 15).ok()?,
    );
    let count = cursor.uint(if version < 2 { 2 } else { 4 })?;
    let mut found = Vec::new();
    for _ in 0..count.min(MAX_ENTRIES as u64) {
        let id = cursor.uint(if version < 2 { 2 } else { 4 })?;
        // Construction method 0 is an offset into the file.
        let in_file = version == 0 || cursor.uint(2)? & 15 == 0;
        cursor.take(2)?;
        let base = cursor.uint(base_width)?;
        let extents = cursor.uint(2)?;
        if extents != 1 {
            // A scattered item is no thumbnail; the next item's fields are
            // not at a known place, so the list ends here.
            break;
        }
        if version >= 1 {
            cursor.take(index_width)?;
        }
        let offset = cursor.uint(offset_width)?;
        let length = cursor.uint(length_width)?;
        if in_file {
            found.push((id, base.checked_add(offset)?, length));
        }
    }
    Some(found)
}

/// The items the `meta` box of a HEIC head declares.
struct Items<'a> {
    head: &'a [u8],
    /// `(id, type)` of each item.
    types: Vec<(u64, [u8; 4])>,
    /// The item a `thmb` reference names.
    thumb: Option<u64>,
    /// `(id, offset, length)` of each item stored in the file.
    places: Vec<(u64, u64, u64)>,
}

impl<'a> Items<'a> {
    /// The items of the HEIC file whose first bytes are `head`.
    fn of(head: &'a [u8]) -> Option<Self> {
        let (kind, ftyp) = boxes(head).next()?;
        if kind != b"ftyp" || !BRANDS.iter().any(|brand| ftyp.starts_with(*brand)) {
            return None;
        }
        let (_, meta) = boxes(head).find(|(kind, _)| *kind == b"meta")?;
        let (mut types, mut thumb, mut places) = (None, None, None);
        for (kind, payload) in boxes(meta.get(4..)?) {
            match kind {
                b"iinf" => types = item_types(payload),
                b"iref" => thumb = thumbnail_item(payload),
                b"iloc" => places = locations(payload),
                _ => {}
            }
        }
        Some(Self {
            head,
            types: types?,
            thumb,
            places: places?,
        })
    }

    /// The bytes of item `id`, when they lie within the head.
    fn bytes(&self, id: u64) -> Option<&'a [u8]> {
        let &(_, offset, length) = self.places.iter().find(|place| place.0 == id)?;
        let start = usize::try_from(offset).ok()?;
        self.head
            .get(start..start.checked_add(usize::try_from(length).ok()?)?)
    }

    /// The TIFF block of the `Exif` item. Its payload starts with the
    /// distance from its own end of that field to the TIFF header.
    fn exif(&self) -> Option<&'a [u8]> {
        let exif = self.types.iter().find(|item| item.1 == *b"Exif")?;
        let payload = self.bytes(exif.0)?;
        let distance =
            usize::try_from(u32::from_be_bytes(payload.get(..4)?.try_into().ok()?)).ok()?;
        payload.get(4usize.checked_add(distance)?..)
    }
}

/// The embedded thumbnail of the HEIC file whose first bytes are `head`, or
/// `None` when the head holds no whole thumbnail.
pub fn thumbnail(head: &[u8]) -> Option<EmbeddedThumbnail<'_>> {
    let items = Items::of(head)?;
    let tiff = items.exif();
    let jpeg = items
        .thumb
        .filter(|id| {
            items
                .types
                .iter()
                .any(|item| item.0 == *id && item.1 == *b"jpeg")
        })
        .and_then(|id| items.bytes(id))
        .filter(|jpeg| jpeg.starts_with(&[0xFF, 0xD8]));
    match jpeg {
        Some(jpeg) => Some(EmbeddedThumbnail {
            jpeg,
            orientation: tiff.map_or(1, tiff_orientation),
        }),
        None => thumbnail_in(tiff?),
    }
}

/// The TIFF block of the `Exif` item of the HEIC file whose first bytes are
/// `head`, when it lies within the head.
pub(super) fn exif(head: &[u8]) -> Option<&[u8]> {
    Items::of(head)?.exif()
}

#[cfg(test)]
mod tests {
    use super::super::thumbnail::tests::camera_tiff;
    use super::*;

    const THUMBNAIL: &[u8] = &[0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9];

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn infe(id: u16, kind: &[u8; 4]) -> Vec<u8> {
        let mut payload = vec![2, 0, 0, 0];
        payload.extend_from_slice(&id.to_be_bytes());
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(kind);
        payload.push(0);
        boxed(b"infe", &payload)
    }

    /// A HEIC head: a primary item, a `jpeg` thumbnail item when `thumbnail`
    /// is set and an `Exif` item holding `tiff`, with `mdat` after `meta`.
    fn camera_heic(thumbnail: Option<&[u8]>, tiff: &[u8]) -> Vec<u8> {
        tiled_heic(0, thumbnail, tiff)
    }

    /// `camera_heic` with `tiles` image items listed ahead of the others.
    fn tiled_heic(tiles: u16, thumbnail: Option<&[u8]>, tiff: &[u8]) -> Vec<u8> {
        let mut exif = 0u32.to_be_bytes().to_vec();
        exif.extend_from_slice(tiff);
        let build = |data_at: u32| {
            let mut iinf = vec![0, 0, 0, 0];
            iinf.extend_from_slice(&(3 + tiles).to_be_bytes());
            for id in 0..tiles {
                iinf.extend(infe(100 + id, b"hvc1"));
            }
            iinf.extend(infe(1, b"hvc1"));
            iinf.extend(infe(2, b"jpeg"));
            iinf.extend(infe(3, b"Exif"));
            let mut iref = vec![0, 0, 0, 0];
            iref.extend(boxed(b"thmb", &[0, 2, 0, 1, 0, 1]));
            // Offsets and lengths of 4 bytes, no base offset.
            let mut iloc = vec![0, 0, 0, 0, 0x44, 0];
            iloc.extend_from_slice(&(2 + tiles).to_be_bytes());
            let thumb_at = data_at + exif.len() as u32;
            let tile_items = (0..tiles).map(|id| (100 + id, data_at, 0));
            let items = tile_items.chain([
                (2u16, thumb_at, thumbnail.map_or(0, <[u8]>::len) as u32),
                (3, data_at, exif.len() as u32),
            ]);
            for (id, at, length) in items {
                iloc.extend_from_slice(&id.to_be_bytes());
                iloc.extend_from_slice(&[0, 0, 0, 1]);
                iloc.extend_from_slice(&at.to_be_bytes());
                iloc.extend_from_slice(&length.to_be_bytes());
            }
            let mut meta = vec![0, 0, 0, 0];
            meta.extend(boxed(b"iinf", &iinf));
            if thumbnail.is_some() {
                meta.extend(boxed(b"iref", &iref));
            }
            meta.extend(boxed(b"iloc", &iloc));
            boxed(b"meta", &meta)
        };
        let ftyp = boxed(b"ftyp", b"heic\0\0\0\0mif1heic");
        let data_at = (ftyp.len() + build(0).len() + 8) as u32;
        let mut file = ftyp;
        file.extend(build(data_at));
        let mut data = exif;
        data.extend_from_slice(thumbnail.unwrap_or_default());
        file.extend(boxed(b"mdat", &data));
        file
    }

    /// The `Exif` item's TIFF block is found past the distance field.
    #[test]
    fn exif_item_found() {
        let tiff = camera_tiff(6, None);
        assert_eq!(exif(&camera_heic(None, &tiff)), Some(&tiff[..]));
        assert_eq!(exif(b"\0\0\0\x10ftypheic\0\0\0\0"), None);
    }

    #[test]
    fn thumbnail_item_comes_with_orientation() {
        let head = camera_heic(Some(THUMBNAIL), &camera_tiff(6, None));
        assert_eq!(
            thumbnail(&head),
            Some(EmbeddedThumbnail {
                jpeg: THUMBNAIL,
                orientation: 6
            })
        );
    }

    #[test]
    fn thumbnail_behind_many_tiles_is_found() {
        let head = tiled_heic(300, Some(THUMBNAIL), &camera_tiff(6, None));
        assert_eq!(
            thumbnail(&head),
            Some(EmbeddedThumbnail {
                jpeg: THUMBNAIL,
                orientation: 6
            })
        );
    }

    #[test]
    fn exif_directory_thumbnail_is_found() {
        let head = camera_heic(None, &camera_tiff(3, Some(THUMBNAIL)));
        assert_eq!(
            thumbnail(&head),
            Some(EmbeddedThumbnail {
                jpeg: THUMBNAIL,
                orientation: 3
            })
        );
    }

    #[test]
    fn photo_without_thumbnail_has_none() {
        assert_eq!(thumbnail(&camera_heic(None, &camera_tiff(1, None))), None);
    }

    #[test]
    fn thumbnail_past_the_head_has_none() {
        let head = camera_heic(Some(THUMBNAIL), &camera_tiff(1, None));
        for cut in 0..head.len() - 1 {
            assert_eq!(thumbnail(&head[..cut]), None, "cut at {cut}");
        }
    }

    #[test]
    fn other_brands_have_none() {
        let mut head = camera_heic(Some(THUMBNAIL), &camera_tiff(1, None));
        head[8..12].copy_from_slice(b"avif");
        assert_eq!(thumbnail(&head), None);
    }
}
