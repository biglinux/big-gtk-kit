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
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        // APNG puts its animation control chunk before the first image data.
        let first_data = find(&head, b"IDAT").unwrap_or(head.len());
        return find(&head[..first_data], b"acTL").is_some();
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return find(&head, b"ANIM").is_some();
    }
    // An animated AVIF declares the image-sequence brand in its ftyp box.
    if head.get(4..8) == Some(b"ftyp") {
        let ftyp_end = head.len().min(64);
        return find(&head[..ftyp_end], b"avis").is_some();
    }
    false
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
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRxxxxxxxxxxxxx\0\0\0\x08IDATdata",
        );
        assert!(
            !has_animation_header(&still_png),
            "an ordinary PNG must never be decoded twice"
        );
        let apng = write(
            "moving.png",
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRxxxxxxxxxxxxx\0\0\0\x08acTLxxxx\0\0\0\x08IDATdata",
        );
        assert!(has_animation_header(&apng));
        assert!(has_animation_header(&write("a.gif", b"GIF89a rest")));
        assert!(!has_animation_header(&write(
            "still.webp",
            b"RIFF\0\0\0\0WEBPVP8 still"
        )));
        assert!(has_animation_header(&write(
            "moving.webp",
            b"RIFF\0\0\0\0WEBPVP8XANIM"
        )));
        assert!(!has_animation_header(&directory.join("missing.gif")));

        let _ = std::fs::remove_dir_all(&directory);
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
