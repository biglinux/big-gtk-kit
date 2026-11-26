// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Writing an animation to a file through ImageMagick, the one encoder here that
//! takes a per-frame delay. It runs confined to the private directory that
//! holds the frames and the animation it writes, and is handed only raw RGBA
//! this process decoded; frames are written to disk as they come, since a
//! minute of 1080p is gigabytes of RGBA.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use big_os_kit::confinement::BWRAP;
use big_os_kit::subprocess::{BigSubprocessOutputMode, BigSubprocessSpec};

use super::{BigImageError, BigRgbaImage};

const MAGICK: &str = "/usr/bin/magick";
/// Wall-clock ceiling for the encoder.
const ENCODE_TIMEOUT: Duration = Duration::from_secs(120);
/// Frames beyond this are refused rather than written slowly forever.
const MAX_FRAMES: usize = 2_000;

/// A format that can hold more than one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BigAnimationFormat {
    /// Universally readable, at most 256 colors per frame.
    Gif,
    /// Full color and much smaller, readable by every current browser.
    WebP,
}

impl BigAnimationFormat {
    /// The name ImageMagick knows this format by.
    ///
    /// Passed as a prefix on the output argument, because the encoder picks
    /// the format from the file name and the file it writes is a partial one.
    const fn magick_prefix(self) -> &'static str {
        match self {
            Self::Gif => "gif:",
            Self::WebP => "webp:",
        }
    }

    /// The format `path`'s extension asks for, when it can hold an animation.
    #[must_use]
    pub fn for_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "gif" => Some(Self::Gif),
            "webp" => Some(Self::WebP),
            _ => None,
        }
    }
}

/// Collects transformed frames on disk and encodes them in one pass.
pub struct BigAnimationWriter {
    directory: PathBuf,
    arguments: Vec<OsString>,
    frames: usize,
}

impl Drop for BigAnimationWriter {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl BigAnimationWriter {
    /// Open a private directory for the frames.
    ///
    /// # Errors
    ///
    /// Fails when no private directory can be created.
    pub fn new() -> Result<Self, BigImageError> {
        let directory =
            super::secure::create_private_directory().map_err(|_| BigImageError::SaveFailed {
                reason: "a private frame buffer could not be created".to_owned(),
            })?;
        Ok(Self {
            directory,
            arguments: Vec::new(),
            frames: 0,
        })
    }

    /// Add one frame, shown for `delay`.
    ///
    /// # Errors
    ///
    /// Fails when the frame is malformed, cannot be written, or the animation
    /// is longer than this build accepts.
    pub fn push(&mut self, image: &BigRgbaImage, delay: Duration) -> Result<(), BigImageError> {
        if self.frames >= MAX_FRAMES {
            return Err(BigImageError::SaveFailed {
                reason: format!("an animation of more than {MAX_FRAMES} frames is refused"),
            });
        }
        let expected = usize::try_from(image.width)
            .ok()
            .and_then(|width| width.checked_mul(usize::try_from(image.height).ok()?))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(BigImageError::InvalidBytePixels)?;
        if image.width <= 0 || image.height <= 0 || image.rgba.len() != expected {
            return Err(BigImageError::InvalidBytePixels);
        }
        let image = &crate::save::in_srgb(image)?;
        let frame_path = self.directory.join(format!("{:05}.rgba", self.frames));
        big_os_kit::storage::atomic_write_private(&frame_path, &image.rgba).map_err(|error| {
            BigImageError::SaveFailed {
                reason: error.to_string(),
            }
        })?;

        // Both the size and the delay apply to the file that follows them, so
        // every frame carries its own even when they all happen to match.
        self.arguments.push(OsString::from("-size"));
        self.arguments
            .push(OsString::from(format!("{}x{}", image.width, image.height)));
        self.arguments.push(OsString::from("-depth"));
        self.arguments.push(OsString::from("8"));
        self.arguments.push(OsString::from("-delay"));
        self.arguments.push(OsString::from(centiseconds(delay)));
        let mut source = OsString::from("rgba:");
        source.push(&frame_path);
        self.arguments.push(source);
        self.frames += 1;
        Ok(())
    }

    /// Encode everything pushed so far into `destination`.
    ///
    /// # Errors
    ///
    /// Fails when no frame was added, the name asks for a format that cannot
    /// animate, or the encoder does not finish cleanly.
    pub fn finish(self, destination: &Path) -> Result<(), BigImageError> {
        if self.frames == 0 {
            return Err(BigImageError::SaveFailed {
                reason: "an animation needs at least one frame".to_owned(),
            });
        }
        let format = BigAnimationFormat::for_path(destination)
            .ok_or(BigImageError::UnsupportedSaveFormat)?;
        // Encoded beside the frames, then written to the destination as any
        // saved picture is, so an interrupted encode never leaves a
        // half-written file where the person's file should be, and the
        // encoder never sees the destination's folder.
        let staged = self.directory.join("animation");
        self.run_encoder(format, &staged)?;
        std::fs::read(&staged)
            .and_then(|bytes| big_os_kit::storage::atomic_save_document(destination, &bytes))
            .map_err(|error| BigImageError::SaveFailed {
                reason: error.to_string(),
            })
    }

    fn run_encoder(&self, format: BigAnimationFormat, staged: &Path) -> Result<(), BigImageError> {
        let status = BigSubprocessSpec::builder()
            .program(BWRAP)
            .args(big_os_kit::confinement::system_only_namespace())
            // The system's limits for the encoder, then the frames and the
            // animation, and nothing else of the person's.
            .args([
                "--ro-bind-try",
                "/etc/ImageMagick-7",
                "/etc/ImageMagick-7",
                "--bind",
            ])
            .arg(&self.directory)
            .arg(&self.directory)
            .arg("--")
            .args([
                MAGICK, "-limit", "memory", "512MiB", "-limit", "map", "1GiB", "-limit", "area",
                "256MP", "-limit", "time", "110", "-loop", "0",
            ])
            .args(self.arguments.iter().map(OsString::as_os_str))
            .arg({
                let mut output = OsString::from(format.magick_prefix());
                output.push(staged);
                output
            })
            .stdout(BigSubprocessOutputMode::Null)
            .stderr(BigSubprocessOutputMode::Null)
            .timeout(ENCODE_TIMEOUT)
            .unix_process_group(true)
            .env_inherit(false)
            .allow_list([BWRAP])
            .build()
            .run()
            .map_err(|error| BigImageError::SaveFailed {
                reason: error.to_string(),
            })?
            .status;
        if status.success() {
            Ok(())
        } else {
            Err(BigImageError::SaveFailed {
                reason: "the encoder refused the frames".to_owned(),
            })
        }
    }
}

/// The delay in the hundredths of a second the formats actually store.
///
/// Rounded up, never to zero: a frame written as zero is the very thing every
/// player replaces with a tenth of a second.
fn centiseconds(delay: Duration) -> String {
    let hundredths = delay.as_millis().div_ceil(10).max(1);
    hundredths.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_animated_formats_are_offered() {
        assert_eq!(
            BigAnimationFormat::for_path(Path::new("/tmp/a.GIF")),
            Some(BigAnimationFormat::Gif)
        );
        assert_eq!(
            BigAnimationFormat::for_path(Path::new("/tmp/a.webp")),
            Some(BigAnimationFormat::WebP)
        );
        assert_eq!(
            BigAnimationFormat::for_path(Path::new("/tmp/a.png")),
            None,
            "a still format must not silently drop every frame but one"
        );
    }

    #[test]
    fn delay_uses_format_unit() {
        assert_eq!(centiseconds(Duration::from_millis(100)), "10");
        assert_eq!(centiseconds(Duration::from_millis(45)), "5", "rounded up");
        assert_eq!(
            centiseconds(Duration::ZERO),
            "1",
            "zero is what players replace with a default; never write it"
        );
        assert_eq!(centiseconds(Duration::from_secs(5)), "500");
    }

    #[test]
    fn malformed_frame_encodes_nothing() {
        let mut writer = BigAnimationWriter::new().expect("private directory");
        let broken = BigRgbaImage {
            rgba: vec![0; 3],
            width: 2,
            height: 2,
            cicp: None,
            opaque: false,
        };
        assert!(writer.push(&broken, Duration::from_millis(50)).is_err());
        let destination = std::env::temp_dir().join("big-image-empty-animation.gif");
        let _ = std::fs::remove_file(&destination);
        assert!(writer.finish(&destination).is_err(), "no frame, no file");
        assert!(!destination.exists());
    }

    /// The encoder reads the frames it was handed and nothing else of the
    /// person's: a frame named outside its directory is not found.
    #[test]
    fn encoder_reads_only_its_frames() {
        if !Path::new(MAGICK).exists() || !Path::new(BWRAP).exists() {
            return;
        }
        let directory =
            std::env::temp_dir().join(format!("big-image-animation-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("test directory");
        let pixel = BigRgbaImage {
            rgba: vec![255; 4],
            width: 1,
            height: 1,
            cicp: None,
            opaque: false,
        };
        let own = directory.join("own.gif");
        let mut writer = BigAnimationWriter::new().expect("private directory");
        writer
            .push(&pixel, Duration::from_millis(50))
            .expect("frame");
        writer.finish(&own).expect("its own frame encodes");
        assert!(std::fs::read(&own).expect("written").starts_with(b"GIF8"));

        let outside = directory.join("outside.rgba");
        std::fs::write(&outside, [255; 4]).expect("a frame elsewhere");
        let mut writer = BigAnimationWriter::new().expect("private directory");
        writer
            .arguments
            .extend(["-size", "1x1", "-depth", "8", "-delay", "5"].map(OsString::from));
        let mut source = OsString::from("rgba:");
        source.push(&outside);
        writer.arguments.push(source);
        writer.frames = 1;
        let foreign = directory.join("foreign.gif");
        let result = writer.finish(&foreign);
        std::fs::remove_dir_all(&directory).expect("test directory removed");
        assert!(result.is_err());
        assert!(!foreign.exists());
    }

    #[test]
    fn a_neighboring_file_is_left_alone() {
        let directory =
            std::env::temp_dir().join(format!("big-image-staging-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("test directory");
        let neighbor = directory.join("a.big-part");
        std::fs::write(&neighbor, b"the person's file").expect("neighbor");
        let mut writer = BigAnimationWriter::new().expect("private directory");
        let pixel = BigRgbaImage {
            rgba: vec![255; 4],
            width: 1,
            height: 1,
            cicp: None,
            opaque: false,
        };
        writer
            .push(&pixel, Duration::from_millis(50))
            .expect("frame");
        // Whether or not an encoder is installed, the file beside the
        // destination is not the staging file.
        let _ = writer.finish(&directory.join("a.gif"));
        assert_eq!(
            std::fs::read(&neighbor).expect("still there"),
            b"the person's file"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
