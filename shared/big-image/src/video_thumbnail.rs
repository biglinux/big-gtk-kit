// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Frames, covers and tags of videos and songs, read by `ffmpeg` and
//! `ffprobe` in the thumbnailer's confinement.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use big_os_kit::file_io::with_owned_private_directory;

use crate::{BigImageError, BigImageSpec, BigRgbaImage, decode_rgba_sandboxed};

const MAX_VIDEO_FRAME_PIXELS: i64 = 64_000_000;
const FFMPEG_PATH: &str = "/usr/bin/ffmpeg";

/// Why a video or a song gave no picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFailure {
    /// The file gave none: it is broken, or it holds no picture.
    NoPicture,
    /// The file could not be read now, `ffmpeg` could not run or ran out of
    /// time, or the work was cancelled: another try may give one.
    Transient,
}

impl From<std::io::Error> for FrameFailure {
    fn from(_: std::io::Error) -> Self {
        Self::Transient
    }
}

/// Extract one bounded video frame into memory without creating durable cache state.
///
/// The frame is written only inside an owned private staging directory, decoded
/// through the required image sandbox, and removed before this function returns.
/// The FFmpeg process runs in its own process group and follows `cancellation`.
///
/// # Errors
///
/// [`FrameFailure`] says whether the file gave no frame or the extraction
/// could not run.
pub fn extract_video_frame_rgba_cancellable(
    video_path: &Path,
    bound: i32,
    cancellation: &AtomicBool,
) -> Result<BigRgbaImage, FrameFailure> {
    extract_frame(video_path, bound, true, cancellation)
}

/// The picture a song carries (its cover), at no more than `bound` pixels.
///
/// The same extraction as a video's frame, without the seek: a cover is a
/// one-frame stream, and a second into it there is nothing. A song without
/// one fails, and the caller keeps the type's icon.
///
/// # Errors
///
/// As [`extract_video_frame_rgba_cancellable`].
pub fn extract_cover_art_rgba_cancellable(
    audio_path: &Path,
    bound: i32,
    cancellation: &AtomicBool,
) -> Result<BigRgbaImage, FrameFailure> {
    extract_frame(audio_path, bound, false, cancellation)
}

/// What a song or a video says about itself: the names it was tagged with,
/// and how long it plays.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaTags {
    /// The title it was tagged with.
    pub title: Option<String>,
    /// Who made it.
    pub artist: Option<String>,
    /// The album it belongs to.
    pub album: Option<String>,
    /// How long it plays.
    pub duration: Option<std::time::Duration>,
}

const FFPROBE_PATH: &str = "/usr/bin/ffprobe";
/// The most of `ffprobe`'s answer that is read: lyrics and pictures can ride in
/// tags, and only three short names and a number are wanted.
const MAX_TAGS_BYTES: u64 = 64 * 1024;

/// Read the tags and the duration of `media` with `ffprobe`, confined the way
/// the frame extractor is: the open file mounted read-only, nothing else of
/// the person's, no network. `None` when nothing could be read, and where
/// bubblewrap cannot run.
#[must_use]
pub fn read_media_tags_cancellable(media: &Path, cancellation: &AtomicBool) -> Option<MediaTags> {
    if cancellation.load(Ordering::Acquire) || !crate::thumbnailer::sandbox_works() {
        return None;
    }
    let source = crate::thumbnailer::open_source(media)?;
    let parent = big_os_kit::microdaemon::user_runtime_directory().ok()?;
    with_owned_private_directory(
        &parent,
        ".big-media-tags",
        |staging| -> std::io::Result<Option<MediaTags>> {
            let command = crate::thumbnailer::sandboxed_command(
                &tags_argv(
                    Path::new(crate::thumbnailer::SANDBOX_SOURCE),
                    Path::new("/tmp/tags.txt"),
                ),
                crate::thumbnailer::SandboxSource::Descriptor(source),
                staging,
            );
            let output = command
                .run_cancellable(cancellation)
                .map_err(std::io::Error::other)?;
            if !output.status.success() || cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            use std::io::Read as _;
            let mut text = Vec::new();
            big_os_kit::file_io::open_regular_no_follow(&staging.join("tags.txt"))?
                .take(MAX_TAGS_BYTES)
                .read_to_end(&mut text)?;
            Ok(Some(parse_media_tags(&String::from_utf8_lossy(&text))))
        },
    )
    .ok()
    .flatten()
}

/// The `ffprobe` command line, program first. The tags of the container and
/// of its streams both: an Ogg file keeps its title on the stream.
fn tags_argv(source: &Path, answer: &Path) -> Vec<std::ffi::OsString> {
    let mut argv: Vec<std::ffi::OsString> = [
        FFPROBE_PATH,
        "-v",
        "error",
        "-protocol_whitelist",
        "file",
        "-show_entries",
        "format=duration:format_tags:stream_tags",
        "-of",
        "default=noprint_wrappers=1",
        "-o",
    ]
    .map(Into::into)
    .into();
    argv.push(answer.into());
    argv.push(source.into());
    argv
}

/// Read `key=value` lines: the first title, artist and album, whatever case
/// the tagger used, and the duration in seconds.
fn parse_media_tags(text: &str) -> MediaTags {
    let mut tags = MediaTags::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let named = |slot: &mut Option<String>| {
            if slot.is_none() {
                *slot = Some(value.chars().take(200).collect());
            }
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "tag:title" => named(&mut tags.title),
            "tag:artist" => named(&mut tags.artist),
            "tag:album" => named(&mut tags.album),
            "duration" if tags.duration.is_none() => {
                tags.duration = value
                    .parse::<f64>()
                    .ok()
                    .and_then(|seconds| std::time::Duration::try_from_secs_f64(seconds).ok());
            }
            _ => {}
        }
    }
    tags
}

fn extract_frame(
    video_path: &Path,
    bound: i32,
    seek: bool,
    cancellation: &AtomicBool,
) -> Result<BigRgbaImage, FrameFailure> {
    if cancellation.load(Ordering::Acquire) {
        return Err(FrameFailure::Transient);
    }
    if bound <= 0 || i64::from(bound) * i64::from(bound) > MAX_VIDEO_FRAME_PIXELS {
        return Err(FrameFailure::NoPicture);
    }
    // `ffmpeg` parses whatever the file holds, and a folder opened in the file
    // manager has every video in it thumbnailed without a click, so it runs
    // only in the thumbnailers' confinement: the system, the one file
    // read-only, an empty /tmp and no network. Where bubblewrap cannot run,
    // the file shows its type's icon.
    if !crate::thumbnailer::sandbox_works() {
        return Err(FrameFailure::Transient);
    }
    // Through any links, as a document's page is: anything but a plain file,
    // or a loop of links, has no picture; anything else that stops the open
    // may pass.
    let source = big_os_kit::file_io::open_regular(video_path).map_err(|error| {
        if error.raw_os_error() == Some(libc::ELOOP)
            || error.kind() == std::io::ErrorKind::InvalidInput
        {
            FrameFailure::NoPicture
        } else {
            FrameFailure::Transient
        }
    })?;
    let parent = big_os_kit::microdaemon::user_runtime_directory()?;
    with_owned_private_directory(
        &parent,
        ".big-video-frame",
        |staging| -> Result<BigRgbaImage, FrameFailure> {
            let frame = staging.join("frame.png");
            // A video shorter than the seek ends before it: `ffmpeg` then
            // succeeds and writes nothing, so its first frame is the picture.
            let attempts: &[bool] = if seek { &[true, false] } else { &[false] };
            for &seek in attempts {
                let command = crate::thumbnailer::sandboxed_command(
                    &frame_argv(
                        Path::new(crate::thumbnailer::SANDBOX_SOURCE),
                        Path::new("/tmp/frame.png"),
                        bound,
                        seek,
                    ),
                    crate::thumbnailer::SandboxSource::Descriptor(source.try_clone()?),
                    staging,
                );
                // Not running, out of time or cancelled says nothing of the file.
                let output = command
                    .run_cancellable(cancellation)
                    .map_err(|_| FrameFailure::Transient)?;
                if cancellation.load(Ordering::Acquire) {
                    return Err(FrameFailure::Transient);
                }
                if !output.status.success() {
                    return Err(FrameFailure::NoPicture);
                }
                if frame.is_file() {
                    return decode_rgba_sandboxed(&BigImageSpec::thumbnail(&frame, bound, bound))
                        .map_err(|error| match error {
                            BigImageError::SandboxUnavailable
                            | BigImageError::ResourceLimit
                            | BigImageError::Cancelled
                            | BigImageError::WorkerGone => FrameFailure::Transient,
                            _ => FrameFailure::NoPicture,
                        });
                }
            }
            Err(FrameFailure::NoPicture)
        },
    )
}

/// The `ffmpeg` command line, program first, that writes one frame of
/// `source` to `frame` at no more than `bound` pixels. Only local files may be
/// opened: a playlist or a concat list inside the file cannot point it at the
/// network or anywhere else.
fn frame_argv(source: &Path, frame: &Path, bound: i32, seek: bool) -> Vec<std::ffi::OsString> {
    let seek: &[&str] = if seek { &["-ss", "1"] } else { &[] };
    let mut argv: Vec<std::ffi::OsString> = [FFMPEG_PATH, "-nostdin", "-v", "error"]
        .iter()
        .chain(seek)
        .chain(&["-protocol_whitelist", "file", "-i"])
        .map(Into::into)
        .collect();
    argv.push(source.into());
    argv.extend(["-frames:v", "1", "-vf"].map(Into::into));
    argv.push(format!("scale={bound}:{bound}:force_original_aspect_ratio=decrease").into());
    argv.extend(["-f", "image2", "-vcodec", "png", "-y", "--"].map(Into::into));
    argv.push(frame.into());
    argv
}

#[cfg(test)]
mod tests {
    use big_os_kit::subprocess::BigSubprocessSpec;

    use super::*;

    /// Set in the child that [`media_unread_without_sandbox`] starts.
    const WITHOUT_SANDBOX: &str = "BIG_IMAGE_TEST_WITHOUT_SANDBOX";

    /// A FIFO named like a video in a browsed folder has no picture, at once.
    #[test]
    fn fifo_video_is_refused() {
        assert!(crate::thumbnailer::tests::refuses_fifo(|path| {
            let cancellation = AtomicBool::new(false);
            extract_frame(path, 96, false, &cancellation).is_ok()
                || read_media_tags_cancellable(path, &cancellation).is_some()
        }));
    }

    #[test]
    fn frame_argv_opens_only_local_files() {
        let argv = frame_argv(
            Path::new("/tmp/source with spaces.mp4"),
            Path::new("/tmp/frame.png"),
            96,
            true,
        );
        assert_eq!(
            argv,
            [
                FFMPEG_PATH,
                "-nostdin",
                "-v",
                "error",
                "-ss",
                "1",
                "-protocol_whitelist",
                "file",
                "-i",
                "/tmp/source with spaces.mp4",
                "-frames:v",
                "1",
                "-vf",
                "scale=96:96:force_original_aspect_ratio=decrease",
                "-f",
                "image2",
                "-vcodec",
                "png",
                "-y",
                "--",
                "/tmp/frame.png",
            ]
        );
    }

    /// Where bubblewrap cannot run, neither `ffmpeg` nor `ffprobe` reads the
    /// file: a song with a title and a cover gives neither.
    #[test]
    fn media_unread_without_sandbox() {
        if !Path::new(FFMPEG_PATH).exists() || !Path::new(FFPROBE_PATH).exists() {
            return;
        }
        // Where bubblewrap cannot run, this process already is the place.
        if std::env::var_os(WITHOUT_SANDBOX).is_none() && crate::thumbnailer::sandbox_works() {
            // Run this test again in a namespace that may not make another,
            // where bubblewrap fails the way it does with user namespaces off.
            let out = BigSubprocessSpec::builder()
                .program(big_os_kit::confinement::BWRAP)
                .args(["--dev-bind", "/", "/", "--unshare-user", "--disable-userns"])
                .arg("--")
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "video_thumbnail::tests::media_unread_without_sandbox",
                    "--quiet",
                ])
                .env(WITHOUT_SANDBOX, "1")
                .timeout(std::time::Duration::from_secs(60))
                .allow_list([big_os_kit::confinement::BWRAP])
                .build()
                .run()
                .unwrap();
            assert!(
                out.status.success() && out.stdout_lossy().contains("1 passed"),
                "{}",
                out.stdout_lossy()
            );
            return;
        }
        assert!(!crate::thumbnailer::sandbox_works());
        let directory =
            std::env::temp_dir().join(format!("big-image-media-unconfined-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let song = directory.join("song.mkv");
        let made = BigSubprocessSpec::builder()
            .program(FFMPEG_PATH)
            .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=1")
            .args(["-f", "lavfi", "-i", "testsrc=size=32x32:rate=1"])
            .args(["-frames:v", "1", "-disposition:v", "attached_pic"])
            .args(["-metadata", "title=Song", "-y"])
            .arg(song.as_os_str())
            .allow_list([FFMPEG_PATH])
            .build()
            .run()
            .unwrap();
        assert!(made.status.success());
        let tags = read_media_tags_cancellable(&song, &AtomicBool::new(false));
        let cover = extract_cover_art_rgba_cancellable(&song, 32, &AtomicBool::new(false));
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(tags, None);
        assert_eq!(cover.err(), Some(FrameFailure::Transient));
    }

    /// A real frame, drawn through the confined path: bubblewrap binds the
    /// pinned `/proc/self/fd` source and the staging folder, and `ffmpeg`
    /// writes the frame where the decoder finds it.
    #[test]
    fn video_frame_draws_in_sandbox() {
        if !Path::new(FFMPEG_PATH).exists() || !crate::thumbnailer::sandbox_works() {
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "big-image-video-frame-sandbox-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        // Two seconds, and a clip that ends before the second a frame is
        // taken at: that one gets its first frame.
        for length in ["2", "0.4"] {
            let video = directory.join(format!("clip-{length}.mp4"));
            let made = BigSubprocessSpec::builder()
                .program(FFMPEG_PATH)
                .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
                .arg("testsrc=size=64x48:rate=5")
                .args(["-t", length, "-y"])
                .arg(video.as_os_str())
                .allow_list([FFMPEG_PATH])
                .build()
                .run()
                .unwrap();
            assert!(made.status.success());
            let frame = extract_video_frame_rgba_cancellable(&video, 32, &AtomicBool::new(false))
                .unwrap_or_else(|failure| panic!("a frame of the {length} s clip: {failure:?}"));
            assert_eq!((frame.width, frame.height), (32, 24));
        }
        // A link draws what it names, as a document's link does.
        let link = directory.join("link.mp4");
        std::os::unix::fs::symlink(directory.join("clip-2.mp4"), &link).unwrap();
        let frame = extract_video_frame_rgba_cancellable(&link, 32, &AtomicBool::new(false))
            .unwrap_or_else(|failure| panic!("a frame through the link: {failure:?}"));
        assert_eq!((frame.width, frame.height), (32, 24));
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// The confined form mounts the open file, not a path, and the command
    /// inside never sees where the video lives.
    #[test]
    fn confined_command_mounts_only_open_file() {
        let video = std::env::temp_dir().join(format!(
            "big-image-video-frame-argv-{}.mp4",
            std::process::id()
        ));
        std::fs::write(&video, b"not decoded here").unwrap();
        let resolved = crate::thumbnailer::sandboxed_command(
            &frame_argv(
                Path::new(crate::thumbnailer::SANDBOX_SOURCE),
                Path::new("/tmp/frame.png"),
                96,
                true,
            ),
            crate::thumbnailer::SandboxSource::Descriptor(std::fs::File::open(&video).unwrap()),
            Path::new("/run/user/1000/.big-video-frame-x"),
        )
        .resolved();
        std::fs::remove_file(&video).unwrap();
        assert_eq!(resolved.program, "/usr/bin/bwrap");
        let argv = resolved.argv;
        for flag in ["--unshare-all", "--die-with-parent", "--ro-bind-fd"] {
            assert!(argv.iter().any(|token| token == flag), "{flag} in {argv:?}");
        }
        assert!(
            !argv
                .iter()
                .any(|token| token.contains("big-image-video-frame-argv"))
        );
        let inside = argv.iter().position(|token| token == "--").unwrap();
        assert_eq!(argv[inside + 1], FFMPEG_PATH);
        assert!(
            argv[inside..]
                .windows(2)
                .any(|pair| pair == ["-protocol_whitelist", "file"])
        );
    }

    #[test]
    fn first_tag_wins_in_any_case() {
        let tags = parse_media_tags(
            "TAG:encoder=x\nTAG:TITLE=Canção\nTAG:title=Other\nTAG:Artist=Fulano\nduration=183.5\nduration=N/A\n",
        );
        assert_eq!(tags.title.as_deref(), Some("Canção"));
        assert_eq!(tags.artist.as_deref(), Some("Fulano"));
        assert_eq!(tags.album, None);
        assert_eq!(
            tags.duration,
            Some(std::time::Duration::from_millis(183_500))
        );
        assert_eq!(parse_media_tags("duration=N/A\n").duration, None);
    }

    /// ffprobe does not escape a newline in a tag, so a title can carry a
    /// duration line of its own.
    #[test]
    fn huge_duration_is_dropped() {
        assert_eq!(parse_media_tags("duration=1e300\n").duration, None);
    }

    /// A real song's tags, read through the confined `ffprobe`.
    #[test]
    fn song_shows_title_artist_length() {
        if !Path::new(FFMPEG_PATH).exists() || !Path::new(FFPROBE_PATH).exists() {
            return;
        }
        let directory =
            std::env::temp_dir().join(format!("big-image-media-tags-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let song = directory.join("song.ogg");
        let made = BigSubprocessSpec::builder()
            .program(FFMPEG_PATH)
            .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=3")
            .args([
                "-metadata",
                "title=Canção",
                "-metadata",
                "artist=Fulano",
                "-y",
            ])
            .arg(song.as_os_str())
            .allow_list([FFMPEG_PATH])
            .build()
            .run()
            .unwrap();
        assert!(made.status.success());
        let link = directory.join("link.ogg");
        std::os::unix::fs::symlink(&song, &link).unwrap();
        let tags = read_media_tags_cancellable(&link, &AtomicBool::new(false)).unwrap();
        assert_eq!(tags.title.as_deref(), Some("Canção"));
        assert_eq!(tags.artist.as_deref(), Some("Fulano"));
        assert_eq!(tags.duration.map(|length| length.as_secs()), Some(3));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cancellable_frame_extractor_rejects_oversized_bounds() {
        assert_eq!(
            extract_video_frame_rgba_cancellable(
                Path::new("/tmp/missing.mp4"),
                8_001,
                &AtomicBool::new(false),
            )
            .err(),
            Some(FrameFailure::NoPicture)
        );
    }

    /// A frame that could not be made now says so, so it is not taken for a
    /// file without a picture.
    #[test]
    fn unfinished_extraction_is_transient() {
        let directory = std::env::temp_dir().join(format!(
            "big-image-video-frame-transient-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let video = directory.join("clip.mp4");
        std::fs::write(&video, b"not decoded here").unwrap();
        let cancelled = extract_video_frame_rgba_cancellable(&video, 96, &AtomicBool::new(true));
        let missing = extract_video_frame_rgba_cancellable(
            &directory.join("gone.mp4"),
            96,
            &AtomicBool::new(false),
        );
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(cancelled.err(), Some(FrameFailure::Transient));
        assert_eq!(missing.err(), Some(FrameFailure::Transient));
    }
}
