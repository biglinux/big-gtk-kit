// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Which desktop a program runs in, as `XDG_CURRENT_DESKTOP` names it: a list
//! of names separated by `:`, the session's own first. big-compositor's
//! session file sets `big-compositor:big`.

/// Whether `desktops`, a value of `XDG_CURRENT_DESKTOP`, names
/// big-compositor's session.
#[must_use]
pub fn names_big_compositor(desktops: &str) -> bool {
    desktops.split(':').any(|name| name == "big-compositor")
}

/// Whether this process runs in big-compositor's session.
#[must_use]
pub fn on_big_compositor() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| names_big_compositor(&desktops))
}

#[cfg(test)]
mod tests {
    use super::names_big_compositor;

    /// big-compositor's session is told by its whole name anywhere in the
    /// list; KWin's, a longer name that starts like it and an empty list
    /// are not it.
    #[test]
    fn big_compositor_named_whole() {
        for (desktops, named) in [
            ("big-compositor:big", true),
            ("big:big-compositor", true),
            ("big-compositor", true),
            ("KDE", false),
            ("big-compositor-test", false),
            ("", false),
        ] {
            assert_eq!(names_big_compositor(desktops), named, "{desktops:?}");
        }
    }
}
