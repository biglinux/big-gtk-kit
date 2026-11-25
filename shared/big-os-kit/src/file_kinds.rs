// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Which files count as video, audio or a picture.
//!
//! One question, asked by the converter, the two players, the file manager, the
//! terminal's preview, the search index and the cast server. One list keeps
//! them agreeing: a `.wmv` the converter accepts and the player plays also gets
//! a preview in the file manager.

use std::path::Path;

/// Everything the media stack will open as a video.
pub(crate) const VIDEO_EXTENSIONS: &[&str] = &[
    "3gp", "asf", "avi", "divx", "f4v", "flv", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg",
    "mts", "ogv", "ts", "vob", "webm", "wmv",
];

/// Everything the media stack will open as sound.
pub(crate) const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "ac3", "aif", "aiff", "ape", "dts", "flac", "m4a", "mka", "mp3", "oga", "ogg", "opus",
    "wav", "wma", "wv",
];

/// Everything the desktop presents as a picture.
///
/// Wider than what every decoder reads: the file manager and the search index
/// only have to recognise a picture, and the platform draws `svg` and `heic`
/// for them. Code that must actually decode the bytes asks `big-image`, and
/// takes its answer.
pub(crate) const IMAGE_EXTENSIONS: &[&str] = &[
    "avif", "bmp", "dib", "exr", "gif", "heic", "heif", "ico", "jpe", "jpeg", "jpg", "pam", "pbm",
    "pgm", "png", "pnm", "ppm", "qoi", "svg", "tga", "tif", "tiff", "webp",
];

/// True when `path` ends in one of `extensions`, whatever its capitalisation.
#[must_use]
pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            extensions.contains(&extension.as_str())
        })
}

/// True when the path names a video file.
#[must_use]
pub fn is_video(path: &Path) -> bool {
    has_extension(path, VIDEO_EXTENSIONS)
}

/// True when the path names a sound file.
#[must_use]
pub fn is_audio(path: &Path) -> bool {
    has_extension(path, AUDIO_EXTENSIONS)
}

/// True when the path names a picture.
#[must_use]
pub fn is_image(path: &Path) -> bool {
    has_extension(path, IMAGE_EXTENSIONS)
}

/// The length in bytes of `name` without its extension: what a rename selects,
/// so typing over it keeps the kind of file.
///
/// A compressed tarball's extension is both parts: renaming `site.tar.gz` to
/// `backup.gz` makes a file no program recognises as a tarball. A name whose
/// only dot starts it (`.profile`) is all name.
#[must_use]
pub fn stem_len(name: &str) -> usize {
    const COMPOUND: &[&str] = &[
        ".tar.gz",
        ".tar.bz2",
        ".tar.xz",
        ".tar.zst",
        ".tar.lz",
        ".tar.lzma",
        ".tar.lz4",
        ".tar.z",
        ".pkg.tar.zst",
        ".pkg.tar.xz",
    ];
    let lower = name.to_ascii_lowercase();
    let compound = COMPOUND
        .iter()
        .filter(|suffix| lower.ends_with(*suffix) && lower.len() > suffix.len())
        .map(|suffix| suffix.len())
        .max();
    let position = compound
        .map(|length| name.len() - length)
        .or_else(|| name.rfind('.'));
    match position {
        Some(position) if position > 0 && name.is_char_boundary(position) => position,
        _ => name.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_keeps_whole_extension() {
        assert_eq!(stem_len("report.txt"), 6);
        assert_eq!(stem_len(".profile"), 8);
        assert_eq!(stem_len("README"), 6);
        assert_eq!(stem_len(""), 0);
        assert_eq!(stem_len("site.tar.gz"), 4);
        assert_eq!(stem_len("jemalloc-5.4.0-1-x86_64.pkg.tar.zst"), 23);
        assert_eq!(stem_len("Notas.TAR.XZ"), 5);
        assert_eq!(stem_len("photo.gz"), 5);
        assert_eq!(stem_len(".tar.gz"), 4);
        assert_eq!(stem_len("relatório.pdf"), "relatório".len());
    }

    #[test]
    fn extensions_are_matched_whatever_the_capitalisation() {
        assert!(is_video(Path::new("/tmp/Holiday.WMV")));
        assert!(is_audio(Path::new("/tmp/take.FLAC")));
        assert!(is_image(Path::new("/tmp/scan.JPEG")));
        assert!(!is_video(Path::new("/tmp/notes.txt")));
        assert!(!is_video(Path::new("/tmp/no-extension")));
    }

    /// The three sets name different files, or a caller asking one question
    /// would get two answers.
    #[test]
    fn file_kind_sets_are_disjoint() {
        for video in VIDEO_EXTENSIONS {
            assert!(!AUDIO_EXTENSIONS.contains(video), "{video} is in both");
            assert!(!IMAGE_EXTENSIONS.contains(video), "{video} is in both");
        }
        for audio in AUDIO_EXTENSIONS {
            assert!(!IMAGE_EXTENSIONS.contains(audio), "{audio} is in both");
        }
    }
}
