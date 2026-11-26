// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Writing an animation to a file through ImageMagick, the one encoder here that
//! takes a per-frame delay. It runs confined to the private directory that
//! holds the frames and the animation it writes, and is handed only raw RGBA
//! this process decoded; frames are written to disk as they come, since a
//! minute of 1080p is gigabytes of RGBA. They are staged in the cache folder,
//! never the runtime directory or `/tmp`, which are memory.

use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::time::Duration;

use big_os_kit::confinement::BWRAP;
use big_os_kit::file_io::OwnedPrivateDirectory;
use big_os_kit::subprocess::{BigSubprocessOutputMode, BigSubprocessSpec};

use super::{BigImageError, BigRgbaImage};

const MAGICK: &str = "/usr/bin/magick";
/// Wall-clock ceiling for the encoder.
const ENCODE_TIMEOUT: Duration = Duration::from_secs(120);
/// Frames beyond this are refused rather than written slowly forever.
const MAX_FRAMES: usize = 2_000;
/// RGBA bytes of every frame together, which keeps the frames and the
/// encoder's cache of them within a few gigabytes of disk.
const MAX_FRAME_BYTES: u64 = 1 << 30;
/// ImageMagick's Q16-HDRI pixel cache holds a float per channel, four times
/// the RGBA, and spills what its memory and map limits do not take into the
/// frames' folder.
const ENCODER_CACHE_FACTOR: u64 = 4;

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

/// Why [`BigAnimationWriter::push`] refuses a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BigAnimationRefusal {
    /// More frames than this build writes.
    TooManyFrames,
    /// More RGBA bytes of frames than this build stages.
    TooLarge,
    /// The cache folder cannot hold the frames and the encoder's work on them.
    NoRoom,
}

impl BigAnimationRefusal {
    /// The refusal in words, read through the caller's catalog, which lists
    /// this file in its `POTFILES`.
    #[must_use]
    pub fn words(self, gettext: impl Fn(&str) -> String) -> String {
        match self {
            Self::TooManyFrames => gettext("The animation has too many frames to save."),
            Self::TooLarge => gettext("The animation is too large to save. Choose a smaller size."),
            Self::NoRoom => gettext(
                "There is not enough free space to prepare the animation. Free some space and try again.",
            ),
        }
    }
}

/// Collects transformed frames on disk and encodes them in one pass.
pub struct BigAnimationWriter {
    directory: OwnedPrivateDirectory,
    arguments: Vec<OsString>,
    frames: usize,
    frame_bytes: u64,
}

impl BigAnimationWriter {
    /// Open a private directory for the frames in this user's cache folder.
    ///
    /// # Errors
    ///
    /// Fails when no private directory can be created.
    pub fn new() -> Result<Self, BigImageError> {
        let parent = big_os_kit::settings::cache_home().join("big-image");
        let directory = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&parent)
            .and_then(|()| OwnedPrivateDirectory::create(&parent, "animation"))
            .map_err(crate::save::unwritten)?;
        Ok(Self {
            directory,
            arguments: Vec::new(),
            frames: 0,
            frame_bytes: 0,
        })
    }

    /// Add one frame, shown for `delay`.
    ///
    /// # Errors
    ///
    /// Fails when the frame is malformed or cannot be written, and with
    /// [`BigImageError::AnimationRefused`] when the animation is larger than
    /// this build accepts, or the frames and the encoder's work would not fit
    /// the free space of the cache folder.
    pub fn push(&mut self, image: &BigRgbaImage, delay: Duration) -> Result<(), BigImageError> {
        if self.frames >= MAX_FRAMES {
            return Err(BigImageError::AnimationRefused(
                BigAnimationRefusal::TooManyFrames,
            ));
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
        let frame_bytes = image.rgba.len() as u64;
        let staged = self.frame_bytes + frame_bytes;
        if staged > MAX_FRAME_BYTES {
            return Err(BigImageError::AnimationRefused(
                BigAnimationRefusal::TooLarge,
            ));
        }
        let available = big_os_kit::filesystem_capacity::filesystem_capacity(self.directory.path())
            .map_err(crate::save::unwritten)?
            .available_bytes;
        if available < frame_bytes + ENCODER_CACHE_FACTOR * staged {
            return Err(BigImageError::AnimationRefused(BigAnimationRefusal::NoRoom));
        }
        let frame_path = self
            .directory
            .path()
            .join(format!("{:05}.rgba", self.frames));
        big_os_kit::storage::atomic_write_private(&frame_path, &image.rgba)
            .map_err(crate::save::unwritten)?;

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
        self.frame_bytes = staged;
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
        let staged = self.directory.path().join("animation");
        self.run_encoder(format, &staged)?;
        std::fs::read(&staged)
            .and_then(|bytes| big_os_kit::storage::atomic_save_document(destination, &bytes))
            .map_err(crate::save::unwritten)
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
            .arg(self.directory.path())
            .arg(self.directory.path())
            .arg("--")
            .args([
                MAGICK, "-limit", "memory", "512MiB", "-limit", "map", "1GiB", "-limit", "area",
                "256MP", "-limit", "time", "110", "-loop", "0", "-define",
            ])
            // Its pixel cache spills beside the frames, on disk, and not into
            // the sandbox's `/tmp`, which is memory.
            .arg({
                let mut temporary = OsString::from("registry:temporary-path=");
                temporary.push(self.directory.path());
                temporary
            })
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

    /// The runtime directory and `/tmp` are memory: a long animation staged
    /// there filled them for every program of the session.
    #[test]
    fn frames_stage_in_the_cache() {
        let writer = BigAnimationWriter::new().expect("private directory");
        assert!(
            writer
                .directory
                .path()
                .starts_with(big_os_kit::settings::cache_home())
        );
    }

    #[test]
    fn frames_past_ceiling_refused() {
        let mut writer = BigAnimationWriter::new().expect("private directory");
        writer.frame_bytes = MAX_FRAME_BYTES;
        let pixel = BigRgbaImage {
            rgba: vec![255; 4],
            width: 1,
            height: 1,
            cicp: None,
            opaque: false,
        };
        assert!(matches!(
            writer.push(&pixel, Duration::from_millis(50)),
            Err(BigImageError::AnimationRefused(
                BigAnimationRefusal::TooLarge
            ))
        ));
        assert_eq!(writer.frames, 0, "nothing was written");
        writer.frame_bytes = 0;
        writer.frames = MAX_FRAMES;
        assert!(matches!(
            writer.push(&pixel, Duration::from_millis(50)),
            Err(BigImageError::AnimationRefused(
                BigAnimationRefusal::TooManyFrames
            ))
        ));
    }

    /// A refusal reaches the person through the caller's catalog.
    #[test]
    fn refusal_words_are_translated() {
        let catalog = |text: &str| format!("[{text}]");
        assert_eq!(
            BigAnimationRefusal::NoRoom.words(catalog),
            "[There is not enough free space to prepare the animation. Free some space and try again.]"
        );
        assert_eq!(
            BigImageError::AnimationRefused(BigAnimationRefusal::TooLarge).to_string(),
            "The animation is too large to save. Choose a smaller size."
        );
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
