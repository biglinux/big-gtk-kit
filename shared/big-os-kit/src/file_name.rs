// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Whether a name typed for a new or renamed file can be given to it, and
//! whether a listed name is one a file view hides.
//!
//! Asked as the name is typed, so the reason shows under the field before the
//! file system is asked.

/// The longest name, in bytes, Linux file systems take (`NAME_MAX`).
const NAME_MAX: usize = 255;

/// Why a name cannot be given to a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileNameRefusal {
    /// `.` or `..`, the folder itself and its parent.
    Dot,
    /// A `/`, which separates folders; a NUL, which no file system takes; or a
    /// line break, which every tool reading one name per line splits on.
    Separator,
    /// Longer than 255 bytes, the `NAME_MAX` of Linux file systems.
    TooLong,
}

/// Why `name` cannot be a file's name, or `None` when it can.
///
/// An empty name is not refused: a caller keeps its answer off for it
/// without saying anything.
#[must_use]
pub fn file_name_refusal(name: &str) -> Option<FileNameRefusal> {
    if name == "." || name == ".." {
        Some(FileNameRefusal::Dot)
    } else if name.contains(['/', '\n', '\r', '\0']) {
        Some(FileNameRefusal::Separator)
    } else if name.len() > NAME_MAX {
        Some(FileNameRefusal::TooLong)
    } else {
        None
    }
}

impl FileNameRefusal {
    /// The refusal in words, read through the caller's catalog, which lists
    /// this file in its `POTFILES`.
    #[must_use]
    pub fn words(self, gettext: impl Fn(&str) -> String) -> String {
        match self {
            Self::Dot => gettext("Choose a name other than “.” or “..”."),
            Self::Separator => gettext("Use a name without “/” or line breaks."),
            Self::TooLong => gettext("The name is too long. Use a shorter name."),
        }
    }
}

/// That `name` already names something in the folder it would go to, in
/// words, read through the caller's catalog.
#[must_use]
pub fn name_taken_words(name: &str, gettext: impl Fn(&str) -> String) -> String {
    gettext("“{name}” already exists in this folder").replace("{name}", name)
}

/// Whether a file view hides `name` until hidden files are shown: a dot first,
/// as in `.cache`, except the `.` and `..` that name folders themselves.
#[must_use]
pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.') && !matches!(name, "." | "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_rules_table() {
        let longest = "a".repeat(NAME_MAX);
        let over = "é".repeat(128);
        let cases: &[(&str, Option<FileNameRefusal>)] = &[
            ("", None),
            ("notes.txt", None),
            (".hidden", None),
            ("...", None),
            (" spaced ", None),
            (&longest, None),
            (".", Some(FileNameRefusal::Dot)),
            ("..", Some(FileNameRefusal::Dot)),
            ("a/b", Some(FileNameRefusal::Separator)),
            ("line\nbreak", Some(FileNameRefusal::Separator)),
            ("carriage\rreturn", Some(FileNameRefusal::Separator)),
            ("nul\0byte", Some(FileNameRefusal::Separator)),
            // 128 two-byte characters: 256 bytes, one over.
            (&over, Some(FileNameRefusal::TooLong)),
        ];
        for (name, expected) in cases {
            assert_eq!(file_name_refusal(name), *expected, "{name:?}");
        }
    }

    #[test]
    fn dot_names_are_hidden() {
        assert!(!is_hidden("."));
        assert!(!is_hidden(".."));
        assert!(is_hidden(".cache"));
        assert!(!is_hidden("notes.txt"));
    }
}
