// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Text that starts with a capital letter: a heading from a calendar's month
//! ("outubro de 2026"), a name built from an identifier.

/// `text` with its first character in upper case and the rest as it is.
/// Empty text stays empty.
#[must_use]
pub fn of(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn first_letter_raised() {
        assert_eq!(super::of("outubro de 2026"), "Outubro de 2026");
        assert_eq!(super::of("état"), "État");
        // Upper case, not title case: Rust's std has no title-case mapping.
        assert_eq!(super::of("ßtraße"), "SStraße");
        assert_eq!(super::of("Já"), "Já");
        assert_eq!(super::of(""), "");
    }
}
