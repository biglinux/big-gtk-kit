// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Playing an animated image. Every frame comes from the same sandboxed process,
//! so the decoder is held exactly while the animation plays; glycin's pool
//! reclaims the process 30 seconds after it is let go.

use std::path::Path;
use std::time::Duration;

use super::{BigImageError, BigImageSpec, BigRgbaImage};

/// One decoded frame and how long it stays on screen.
pub struct BigAnimationFrame {
    /// Decoded pixels, already bounded by the requested display size.
    pub image: BigRgbaImage,
    /// How long to show it. `None` means the source is a still image.
    pub delay: Option<Duration>,
}

/// Frames faster than this are treated as a broken or hostile file.
const SUSPICIOUS_DELAY: Duration = Duration::from_millis(10);
/// What such a frame is shown for instead.
const DEFAULT_DELAY: Duration = Duration::from_millis(100);

/// The delay to actually honor for a frame.
///
/// Firefox and Chromium both replace any delay of 10 ms or less with 100 ms:
/// files write 0 either because an encoder meant "default" or because someone
/// wanted the image to flash as fast as the machine allows. Following them
/// costs nothing on well-made files and stops a 315-byte GIF from pinning a
/// core.
#[must_use]
pub fn displayed_delay(delay: Option<Duration>) -> Option<Duration> {
    delay.map(|delay| {
        if delay <= SUSPICIOUS_DELAY {
            DEFAULT_DELAY
        } else {
            delay
        }
    })
}

/// A live sandboxed decoder for one animated image.
pub struct BigAnimation {
    inner: super::secure::SandboxedAnimation,
}

// The decoder is a handle to another process, and glycin declares it `Send` and
// `Sync`; holding it in an `Arc` shared with the frame task depends on that.
const _: () = {
    const fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<glycin::Image>();
};

impl BigAnimation {
    /// Decode the next frame of the loop.
    ///
    /// Glycin returns to the first frame after the last one, so this never
    /// ends on its own.
    ///
    /// # Errors
    ///
    /// Fails when the sandboxed decoder cannot deliver a usable frame.
    pub async fn next_frame(&self) -> Result<BigAnimationFrame, BigImageError> {
        self.inner.next_frame().await
    }
}

/// Formats that can carry an animation.
///
/// Opening a decoder is the only way to know whether a file is animated, and
/// for a large still photograph that is a second full decode. The extension
/// decides whether the question is worth asking; when it lies, the viewer
/// simply keeps showing the still frame it already has.
#[must_use]
pub fn may_be_animated(path: &Path) -> bool {
    const ANIMATED_EXTENSIONS: &[&str] = &["gif", "webp", "png", "apng", "avif"];

    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ANIMATED_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

/// Whether the file itself says it carries more than one frame.
///
/// The extension only says the format *can* animate, and most PNGs are
/// photographs or screenshots that would be decoded a second time for nothing.
/// Every format here announces animation in its header, so a few kilobytes
/// answer the question exactly.
fn has_animation_header(path: &Path) -> bool {
    use std::io::Read as _;

    const PROBE_BYTES: usize = 64 * 1024;

    let Ok(mut file) = big_os_kit::file_io::open_regular_no_follow(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file
        .by_ref()
        .take(PROBE_BYTES as u64)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    if head.starts_with(b"GIF8") {
        // A GIF is cheap to decode and announces nothing useful in its header.
        return true;
    }
    if let Some(chunks) = head.strip_prefix(b"\x89PNG\r\n\x1a\n") {
        return png_announces_animation(chunks, head.len() < PROBE_BYTES);
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        // The animation flag of the extended header, which comes first
        // (RFC 9649); `ANIM` may follow a color profile of any size.
        return head.get(12..16) == Some(b"VP8X")
            && head.get(20).is_some_and(|flags| flags & 0x02 != 0);
    }
    // An animated AVIF declares the image-sequence brand in its ftyp box.
    if head.get(4..8) == Some(b"ftyp") {
        let ftyp_end = head.len().min(64);
        return find(&head[..ftyp_end], b"avis").is_some();
    }
    false
}

/// Whether the PNG `chunks` that follow the signature hold `acTL`, which APNG
/// puts before the first `IDAT`. Walked by their lengths, since a chunk's data
/// may spell either name; where the probe ends before an `IDAT` of a file it
/// did not read `whole`, the sandbox decides.
fn png_announces_animation(mut chunks: &[u8], whole: bool) -> bool {
    while let Some((header, rest)) = chunks.split_first_chunk::<8>() {
        match &header[4..] {
            b"acTL" => return true,
            b"IDAT" => return false,
            _ => {}
        }
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        // The data, then its CRC.
        let Some(next) = usize::try_from(length)
            .ok()
            .and_then(|length| length.checked_add(4))
            .and_then(|skip| rest.get(skip..))
        else {
            return !whole;
        };
        chunks = next;
    }
    !whole
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Open `spec` as an animation, returning its first frame.
///
/// `Ok(None)` means the source is a still image and no decoder was kept.
///
/// # Errors
///
/// Fails when the source cannot be staged or decoded inside the sandbox.
pub async fn open_animation(
    spec: BigImageSpec,
) -> Result<Option<(BigAnimation, BigAnimationFrame)>, BigImageError> {
    if !may_be_animated(&spec.path) {
        return Ok(None);
    }
    let staged = gtk::gio::spawn_blocking(move || {
        has_animation_header(&spec.path)
            .then(|| super::secure::stage_animation(&spec))
            .transpose()
    })
    .await
    .map_err(|_| BigImageError::WorkerGone)??;
    let Some(staged) = staged else {
        return Ok(None);
    };
    let opened = super::secure::SandboxedAnimation::open(staged).await?;
    Ok(opened.map(|(inner, frame)| (BigAnimation { inner }, frame)))
}

impl BigAnimation {
    /// The next frame for export, or `None` once every frame has been given.
    ///
    /// A decoder used for export must not be the one driving playback: this
    /// walks the animation once and then stops, and the two would steal
    /// frames from each other.
    ///
    /// # Errors
    ///
    /// Fails when the sandboxed decoder cannot deliver a usable frame.
    pub async fn next_export_frame(&self) -> Result<Option<BigAnimationFrame>, BigImageError> {
        self.inner.next_export_frame().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_delays_become_browser_default() {
        assert_eq!(displayed_delay(None), None, "a still image has no delay");
        assert_eq!(
            displayed_delay(Some(Duration::ZERO)),
            Some(DEFAULT_DELAY),
            "zero is what encoders write when they mean \"default\""
        );
        assert_eq!(
            displayed_delay(Some(Duration::from_millis(10))),
            Some(DEFAULT_DELAY),
            "the browsers' threshold is inclusive"
        );
        assert_eq!(
            displayed_delay(Some(Duration::from_millis(20))),
            Some(Duration::from_millis(20)),
            "the fastest delay a well-made GIF can ask for is honored"
        );
        assert_eq!(
            displayed_delay(Some(Duration::from_secs(5))),
            Some(Duration::from_secs(5)),
            "there is no ceiling: a slideshow GIF is legitimate"
        );
    }

    #[test]
    fn header_decides_before_second_decode() {
        let directory = std::env::temp_dir().join(format!("big-image-anim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("test directory");
        let write = |name: &str, bytes: &[u8]| {
            let path = directory.join(name);
            std::fs::write(&path, bytes).expect("test file");
            path
        };

        let still_png = write(
            "still.png",
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRxxxxxxxxxxxxxcrc!\0\0\0\x04IDATdatacrc!",
        );
        assert!(
            !has_animation_header(&still_png),
            "an ordinary PNG must never be decoded twice"
        );
        let apng = write(
            "moving.png",
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRxxxxxxxxxxxxxcrc!\0\0\0\x08acTLxxxxxxxxcrc!\0\0\0\x04IDATdatacrc!",
        );
        assert!(has_animation_header(&apng));
        assert!(has_animation_header(&write("a.gif", b"GIF89a rest")));
        assert!(!has_animation_header(&write(
            "still.webp",
            b"RIFF\0\0\0\0WEBPVP8 still"
        )));
        assert!(has_animation_header(&write(
            "moving.webp",
            b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0\0\0\0\0\0\0ANIM"
        )));
        assert!(!has_animation_header(&write(
            "alpha.webp",
            b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x10\0\0\0\0\0\0\0\0\0ALPH"
        )));
        assert!(!has_animation_header(&directory.join("missing.gif")));

        let _ = std::fs::remove_dir_all(&directory);
    }

    fn probe(name: &str, bytes: &[u8]) -> bool {
        let path =
            std::env::temp_dir().join(format!("big-image-probe-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("probe file");
        let animated = has_animation_header(&path);
        let _ = std::fs::remove_file(&path);
        animated
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(data);
        chunk.extend_from_slice(&[0; 4]);
        chunk
    }

    /// `acTL` is found by walking the chunks: after text that spells `IDAT`,
    /// and after a chunk larger than the probe reads.
    #[test]
    fn apng_found_by_its_chunks() {
        let png = |before: &[Vec<u8>]| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend(png_chunk(b"IHDR", &[0; 13]));
            for chunk in before {
                bytes.extend_from_slice(chunk);
            }
            bytes.extend(png_chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
            bytes.extend(png_chunk(b"IDAT", &[0; 8]));
            bytes.extend(png_chunk(b"IEND", &[]));
            bytes
        };
        assert!(probe(
            "text.png",
            &png(&[png_chunk(b"tEXt", b"Comment\0IDAT")])
        ));
        assert!(probe(
            "large.png",
            &png(&[png_chunk(b"iTXt", &[b' '; 70_000])])
        ));

        let mut still = b"\x89PNG\r\n\x1a\n".to_vec();
        still.extend(png_chunk(b"IHDR", &[0; 13]));
        still.extend(png_chunk(b"IDAT", b"acTL"));
        still.extend(png_chunk(b"IEND", &[]));
        assert!(!probe("still.png", &still));
    }

    /// A color profile larger than the probe can put `ANIM` out of its
    /// reach; the extended header's flag still says so.
    #[test]
    fn webp_flag_survives_large_profile() {
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x22\0\0\0\0\0\0\0\0\0".to_vec();
        webp.extend_from_slice(b"ICCP");
        webp.extend_from_slice(&70_000u32.to_le_bytes());
        webp.extend_from_slice(&[0; 70_000]);
        webp.extend_from_slice(b"ANIM\x06\0\0\0\0\0\0\0\0\0");
        assert!(probe("profiled.webp", &webp));
    }

    #[test]
    fn fifo_probe_gives_up() {
        assert!(crate::thumbnailer::tests::refuses_fifo(
            has_animation_header
        ));
    }

    #[test]
    fn only_animated_formats_reopen() {
        assert!(may_be_animated(Path::new("/tmp/a.GIF")));
        assert!(may_be_animated(Path::new("/tmp/a.webp")));
        assert!(may_be_animated(Path::new("/tmp/a.png")));
        assert!(
            !may_be_animated(Path::new("/tmp/huge.jpg")),
            "a photograph must never be decoded twice to learn it is still"
        );
        assert!(!may_be_animated(Path::new("/tmp/a")));
    }
}
