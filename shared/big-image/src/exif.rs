// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A bounded EXIF reader for the fields a viewer shows: the directory walk has a
//! visited set and an entry cap, and nothing is allocated from a number the file
//! chose. Location is kept apart so it never leaves through "Copy All".

use std::collections::BTreeSet;

mod heif;
mod thumbnail;

pub use thumbnail::{EmbeddedThumbnail, embedded_thumbnail};

/// Hard cap on directory entries, so a hostile file cannot make the walk long.
const MAX_ENTRIES: usize = 512;
/// Hard cap on the number of directories followed (IFD0, Exif, GPS, …).
const MAX_DIRECTORIES: usize = 8;
/// Longest string this reader will return; camera fields are short.
const MAX_TEXT_BYTES: usize = 128;

/// The fields a viewer shows, all optional because most files have none.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BigExif {
    /// Camera manufacturer (tag 0x010F).
    pub camera_make: Option<String>,
    /// Camera model (tag 0x0110).
    pub camera_model: Option<String>,
    /// When the photograph was taken (tag 0x9003), as written in the file.
    pub taken: Option<String>,
    /// Lens focal length in millimeters (tag 0x920A).
    pub focal_length_mm: Option<f64>,
    /// Exposure time in seconds (tag 0x829A).
    pub exposure_seconds: Option<f64>,
    /// Aperture as an f-number (tag 0x829D).
    pub aperture_f_number: Option<f64>,
    /// ISO speed (tag 0x8827).
    pub iso: Option<u32>,
    /// Where the photograph was taken, kept out of the copyable lines.
    pub location: Option<BigExifLocation>,
}

/// Decimal degrees, positive north and east.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BigExifLocation {
    /// Latitude in decimal degrees.
    pub latitude: f64,
    /// Longitude in decimal degrees.
    pub longitude: f64,
}

impl BigExif {
    /// Whether anything at all was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy)]
struct Reader<'a> {
    bytes: &'a [u8],
    big_endian: bool,
}

impl<'a> Reader<'a> {
    fn u16_at(&self, offset: usize) -> Option<u16> {
        let slice = self.bytes.get(offset..offset.checked_add(2)?)?;
        let pair = [slice[0], slice[1]];
        Some(if self.big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        let slice = self.bytes.get(offset..offset.checked_add(4)?)?;
        let quad = [slice[0], slice[1], slice[2], slice[3]];
        Some(if self.big_endian {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        })
    }

    fn rational_at(&self, offset: usize) -> Option<f64> {
        let numerator = f64::from(self.u32_at(offset)?);
        let denominator = f64::from(self.u32_at(offset.checked_add(4)?)?);
        (denominator != 0.0).then(|| numerator / denominator)
    }

    fn text_at(&self, offset: usize, count: usize) -> Option<String> {
        let count = count.min(MAX_TEXT_BYTES);
        let slice = self.bytes.get(offset..offset.checked_add(count)?)?;
        let end = slice.iter().position(|byte| *byte == 0).unwrap_or(count);
        let text = String::from_utf8_lossy(&slice[..end]).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

/// When the photograph whose file starts with `head` was taken, as its EXIF
/// writes it (`2024:05:17 14:03:22`, the camera's local time): for a JPEG or
/// a HEIC file whose EXIF lies within the head.
#[must_use]
pub fn taken_in_head(head: &[u8]) -> Option<String> {
    let tiff = if head.get(4..8) == Some(b"ftyp") {
        heif::exif(head)
    } else {
        thumbnail::jpeg_exif(head)
    };
    read_exif(tiff?).taken
}

/// Read the displayable fields out of one EXIF blob.
///
/// Accepts both the bare TIFF structure and the `Exif\0\0`-prefixed form that
/// JPEG carries. Anything malformed yields whatever was readable before the
/// damage, never an error: a corrupt tag is not a reason to hide the camera
/// model that parsed fine.
#[must_use]
pub(crate) fn read_exif(bytes: &[u8]) -> BigExif {
    let tiff = bytes
        .strip_prefix(b"Exif\0\0")
        .or_else(|| bytes.strip_prefix(b"Exif\0\xff"))
        .unwrap_or(bytes);
    let mut exif = BigExif::default();
    let Some(reader) = tiff_reader(tiff) else {
        return exif;
    };
    let Some(first) = reader
        .u32_at(4)
        .and_then(|offset| usize::try_from(offset).ok())
    else {
        return exif;
    };

    let mut visited = BTreeSet::new();
    let mut pending = vec![first];
    let mut directories = 0usize;
    let mut gps = GpsScratch::default();
    while let Some(offset) = pending.pop() {
        if directories >= MAX_DIRECTORIES || !visited.insert(offset) {
            continue;
        }
        directories += 1;
        read_directory(&reader, offset, &mut exif, &mut gps, &mut pending);
    }
    exif.location = gps.into_location();
    exif
}

/// The TIFF block of `raw` for an upright copy: Orientation 1, and no thumbnail,
/// which shows the picture before its turn, crop and resize. `None` when no
/// thumbnail can be cut out safely, or the first directory cannot be read.
#[must_use]
pub(crate) fn upright(raw: &[u8]) -> Option<Vec<u8>> {
    let tiff = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    let reader = tiff_reader(tiff)?;
    let first = usize::try_from(reader.u32_at(4)?).ok()?;
    let count = usize::from(reader.u16_at(first)?);
    let next = first.checked_add(2)?.checked_add(count.checked_mul(12)?)?;
    let first_end = next.checked_add(4)?;
    let second = usize::try_from(reader.u32_at(next)?).ok()?;
    let mut dropped = Vec::new();
    if second != 0 {
        if thumbnail::in_strips(&reader, second) {
            return None;
        }
        let entries = usize::from(reader.u16_at(second).unwrap_or(0));
        dropped.push(second..second.saturating_add(2 + entries * 12 + 4));
        dropped.extend(thumbnail::thumbnail_span(&reader, second));
    }
    // What the first directory points to lies before the second one, as
    // writers lay the block out; a thumbnail there could be any of it.
    let kept = [0..8, first..first_end];
    if dropped.iter().any(|span| {
        span.start < second
            || kept
                .iter()
                .any(|keep| span.start < keep.end && keep.start < span.end)
    }) {
        return None;
    }

    let mut out = tiff.to_vec();
    let short = |value: u16| {
        if reader.big_endian {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        }
    };
    for index in 0..count {
        let entry = first + 2 + index * 12;
        // Orientation is one SHORT, held in the entry itself.
        if reader.u16_at(entry)? == 0x0112 && reader.u16_at(entry + 2)? == 3 {
            out[entry + 8..entry + 10].copy_from_slice(&short(1));
        }
    }
    out[next..first_end].fill(0);
    // Spans that reach the end of the block are cut off; the rest are blanked.
    dropped.sort_by_key(|span| std::cmp::Reverse(span.end));
    let mut end = out.len();
    for span in dropped {
        let blank = span.start.min(out.len())..span.end.min(out.len());
        out[blank].fill(0);
        if span.end >= end {
            end = end.min(span.start);
        }
    }
    out.truncate(end);
    Some(out)
}

fn tiff_reader(tiff: &[u8]) -> Option<Reader<'_>> {
    let big_endian = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let reader = Reader {
        bytes: tiff,
        big_endian,
    };
    // The magic 42 is what tells a TIFF header from two coincidental letters.
    (reader.u16_at(2)? == 42).then_some(reader)
}

/// GPS arrives as four independent tags in any order, so the hemisphere is
/// remembered beside the magnitude instead of being folded into it as it
/// arrives: a reference tag read before its value has nothing to negate.
#[derive(Default)]
struct GpsScratch {
    latitude: Option<f64>,
    longitude: Option<f64>,
    south: bool,
    west: bool,
}

impl GpsScratch {
    fn into_location(self) -> Option<BigExifLocation> {
        let latitude = self.latitude?;
        let longitude = self.longitude?;
        (latitude.is_finite() && longitude.is_finite()).then_some(BigExifLocation {
            latitude: if self.south { -latitude } else { latitude },
            longitude: if self.west { -longitude } else { longitude },
        })
    }
}

fn read_directory(
    reader: &Reader<'_>,
    offset: usize,
    exif: &mut BigExif,
    gps: &mut GpsScratch,
    pending: &mut Vec<usize>,
) {
    let Some(count) = reader.u16_at(offset) else {
        return;
    };
    let count = usize::from(count).min(MAX_ENTRIES);
    for index in 0..count {
        let Some(entry) = index
            .checked_mul(12)
            .and_then(|shift| offset.checked_add(2)?.checked_add(shift))
        else {
            return;
        };
        let (Some(tag), Some(format), Some(components)) = (
            reader.u16_at(entry),
            reader.u16_at(entry + 2),
            reader.u32_at(entry + 4),
        ) else {
            return;
        };
        let value_offset = entry + 8;
        read_entry(
            reader,
            tag,
            format,
            components,
            value_offset,
            exif,
            gps,
            pending,
        );
    }
}

fn read_entry(
    reader: &Reader<'_>,
    tag: u16,
    format: u16,
    components: u32,
    value_offset: usize,
    exif: &mut BigExif,
    gps: &mut GpsScratch,
    pending: &mut Vec<usize>,
) {
    let size = format_size(format).saturating_mul(components as usize);
    // Values longer than four bytes live elsewhere and the field holds a
    // pointer; shorter ones are inline.
    let data_offset = if size > 4 {
        match reader
            .u32_at(value_offset)
            .and_then(|at| usize::try_from(at).ok())
        {
            Some(at) => at,
            None => return,
        }
    } else {
        value_offset
    };
    match tag {
        0x010F => exif.camera_make = reader.text_at(data_offset, components as usize),
        0x0110 => exif.camera_model = reader.text_at(data_offset, components as usize),
        0x9003 => exif.taken = reader.text_at(data_offset, components as usize),
        0x920A => exif.focal_length_mm = reader.rational_at(data_offset),
        0x829A => exif.exposure_seconds = reader.rational_at(data_offset),
        0x829D => exif.aperture_f_number = reader.rational_at(data_offset),
        0x8827 => exif.iso = reader.u16_at(data_offset).map(u32::from),
        // Sub-directories: the Exif and GPS blocks the fields above live in.
        0x8769 | 0x8825 => {
            if let Some(at) = reader
                .u32_at(value_offset)
                .and_then(|at| usize::try_from(at).ok())
            {
                pending.push(at);
            }
        }
        0x0001 => gps.south = reader.bytes.get(data_offset) == Some(&b'S'),
        0x0003 => gps.west = reader.bytes.get(data_offset) == Some(&b'W'),
        0x0002 => gps.latitude = degrees_from_rationals(reader, data_offset),
        0x0004 => gps.longitude = degrees_from_rationals(reader, data_offset),
        _ => {}
    }
}

/// Degrees, minutes and seconds as three rationals, folded into degrees.
fn degrees_from_rationals(reader: &Reader<'_>, offset: usize) -> Option<f64> {
    let degrees = reader.rational_at(offset)?;
    let minutes = reader.rational_at(offset.checked_add(8)?).unwrap_or(0.0);
    let seconds = reader.rational_at(offset.checked_add(16)?).unwrap_or(0.0);
    let total = degrees + minutes / 60.0 + seconds / 3600.0;
    total.is_finite().then_some(total)
}

const fn format_size(format: u16) -> usize {
    match format {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Little-endian TIFF with one IFD holding `entries`.
    fn tiff(entries: &[(u16, u16, u32, [u8; 4])], trailer: &[u8]) -> Vec<u8> {
        let mut bytes = b"II".to_vec();
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, format, components, value) in entries {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&format.to_le_bytes());
            bytes.extend_from_slice(&components.to_le_bytes());
            bytes.extend_from_slice(value);
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(trailer);
        bytes
    }

    #[test]
    fn camera_model_reads_its_directory() {
        let text_at = (bytes_len_of_header(1) as u32).to_le_bytes();
        let blob = tiff(&[(0x0110, 2, 6, text_at)], b"NIKON\0");
        let exif = read_exif(&blob);
        assert_eq!(exif.camera_model.as_deref(), Some("NIKON"));
        assert!(exif.location.is_none());
    }

    fn bytes_len_of_header(entries: usize) -> usize {
        2 + 2 + 4 + 2 + entries * 12 + 4
    }

    /// A JPEG's capture time is read from the head of the file; a head cut
    /// before its EXIF segment, or another kind of file, gives none.
    #[test]
    fn taken_found_in_jpeg_head() {
        let text_at = (bytes_len_of_header(1) as u32).to_le_bytes();
        let blob = tiff(&[(0x9003, 2, 20, text_at)], b"2021:03:04 05:06:07\0");
        let mut head = vec![0xFF, 0xD8, 0xFF, 0xE1];
        head.extend_from_slice(&((blob.len() + 8) as u16).to_be_bytes());
        head.extend_from_slice(b"Exif\0\0");
        head.extend(&blob);
        head.extend_from_slice(&[0xFF, 0xDA, 0, 2]);
        assert_eq!(taken_in_head(&head).as_deref(), Some("2021:03:04 05:06:07"));
        assert_eq!(taken_in_head(&head[..12]), None);
        assert_eq!(taken_in_head(b"\x89PNG\r\n\x1a\n"), None);
    }

    #[test]
    fn garbage_reads_as_empty() {
        assert!(read_exif(b"").is_empty());
        assert!(read_exif(b"not a tiff at all").is_empty());
        // Right magic, truncated body.
        assert!(read_exif(b"II\x2a\x00\x08\x00").is_empty());
    }

    #[test]
    fn self_referencing_directory_ends() {
        // Tag 0x8769 (Exif sub-directory) pointing back at the first IFD.
        let blob = tiff(&[(0x8769, 4, 1, 8u32.to_le_bytes())], b"");
        let exif = read_exif(&blob);
        assert!(
            exif.is_empty(),
            "a self-referencing directory yields nothing"
        );
    }

    #[test]
    fn an_absurd_entry_count_is_capped() {
        let mut blob = b"II".to_vec();
        blob.extend_from_slice(&42u16.to_le_bytes());
        blob.extend_from_slice(&8u32.to_le_bytes());
        blob.extend_from_slice(&u16::MAX.to_le_bytes());
        // No entries follow at all: every read must fail closed.
        assert!(read_exif(&blob).is_empty());
    }

    #[test]
    fn a_southern_western_location_is_negative() {
        let mut blob = read_exif(&gps_blob());
        let location = blob.location.take().expect("gps parsed");
        assert!((location.latitude + 23.5).abs() < 0.01, "{location:?}");
        assert!((location.longitude + 46.6).abs() < 0.01, "{location:?}");
    }

    /// IFD0 with a GPS pointer, and a GPS IFD with S/W references.
    fn gps_blob() -> Vec<u8> {
        let mut bytes = b"II".to_vec();
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        // IFD0: one entry, GPS pointer.
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&0x8825u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let gps_ifd_offset = 8 + 2 + 12 + 4;
        bytes.extend_from_slice(&(gps_ifd_offset as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        // GPS IFD: four entries.
        let entries_start = gps_ifd_offset + 2;
        let values_start = entries_start + 4 * 12 + 4;
        bytes.extend_from_slice(&4u16.to_le_bytes());
        let mut push = |tag: u16, format: u16, count: u32, value: [u8; 4]| {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&format.to_le_bytes());
            bytes.extend_from_slice(&count.to_le_bytes());
            bytes.extend_from_slice(&value);
        };
        push(0x0001, 2, 2, [b'S', 0, 0, 0]);
        push(0x0002, 5, 3, (values_start as u32).to_le_bytes());
        push(0x0003, 2, 2, [b'W', 0, 0, 0]);
        push(0x0004, 5, 3, ((values_start + 24) as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        // 23° 30' 0"  and  46° 36' 0"
        for value in [23u32, 1, 30, 1, 0, 1, 46, 1, 36, 1, 0, 1] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// A copy keeps no camera thumbnail: the second directory and the JPEG it
    /// names are cut off at the end of the block, or blanked before other data.
    #[test]
    fn upright_drops_thumbnail() {
        let thumbnail = [0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9];
        let at_end = thumbnail::tests::camera_tiff(6, Some(&thumbnail));
        let mut before_tail = at_end.clone();
        before_tail.extend_from_slice(b"tail");
        let before_tail_len = before_tail.len();
        // The header and first directory end at 26 (see `camera_tiff`).
        for (camera, copy_len) in [(at_end, 26), (before_tail, before_tail_len)] {
            let copy = upright(&camera).expect("first directory reads");
            assert!(
                !copy.windows(2).any(|pair| pair == [0xFF, 0xD8]),
                "{copy:?}"
            );
            assert_eq!(thumbnail::tiff_orientation(&copy), 1);
            assert_eq!(copy.len(), copy_len);
        }
    }

    /// A thumbnail named before the second directory may be the first one's
    /// data: the copy goes without EXIF rather than with it blanked.
    #[test]
    fn upright_keeps_first_directory_data() {
        let mut tiff = thumbnail::tests::camera_tiff(6, Some(&[0xFF, 0xD8, 0xFF, 0xD9]));
        // Eight bytes of the first directory's data at 26, the second
        // directory moved to 34, its thumbnail named at 26.
        tiff[22..26].copy_from_slice(&34u32.to_le_bytes());
        tiff.splice(26..26, *b"IFD0DATA");
        tiff[44..48].copy_from_slice(&26u32.to_le_bytes());
        tiff[56..60].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(upright(&tiff), None);
    }

    /// An uncompressed thumbnail in strips holds the picture before its crop:
    /// the copy goes without EXIF rather than with those pixels.
    #[test]
    fn upright_refuses_strip_thumbnail() {
        let mut tiff = thumbnail::tests::camera_tiff(6, Some(&[0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9]));
        // The second directory at 26 names StripOffsets and StripByteCounts.
        tiff[28..30].copy_from_slice(&0x0111u16.to_le_bytes());
        tiff[40..42].copy_from_slice(&0x0117u16.to_le_bytes());
        assert_eq!(upright(&tiff), None);
    }

    /// Deterministic xorshift: enough randomness for a mutation sweep.
    fn next_random(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn reader_survives_any_bytes() {
        // A valid blob, mutated every way this reader could be walked off the
        // end of.
        let valid = gps_blob();
        let mut state = 0x5eed_1234_9876_abcdu64;
        for round in 0..2_000 {
            let mut bytes = valid.clone();
            let mutations = (next_random(&mut state) % 8) + 1;
            for _ in 0..mutations {
                if bytes.is_empty() {
                    break;
                }
                let at = (next_random(&mut state) as usize) % bytes.len();
                match next_random(&mut state) % 3 {
                    0 => bytes[at] = (next_random(&mut state) & 0xff) as u8,
                    1 => bytes.truncate(at),
                    _ => bytes.insert(at, (next_random(&mut state) & 0xff) as u8),
                }
            }
            let exif = read_exif(&bytes);
            // Whatever survives must still be bounded.
            if let Some(text) = exif.camera_model.as_deref() {
                assert!(text.len() <= MAX_TEXT_BYTES, "round {round}");
            }
            if let Some(location) = exif.location {
                assert!(location.latitude.is_finite(), "round {round}");
                assert!(location.longitude.is_finite(), "round {round}");
            }
        }
        // Pure noise, including lengths that stop mid-header.
        for length in 0..64 {
            let noise: Vec<u8> = (0..length)
                .map(|_| (next_random(&mut state) & 0xff) as u8)
                .collect();
            let _ = read_exif(&noise);
        }
    }
}
