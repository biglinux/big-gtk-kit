// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Private shared row contracts.

#[must_use]
pub(crate) fn row_text(text: &str, allow_markup: bool) -> String {
    if allow_markup {
        text.to_string()
    } else {
        gtk::glib::markup_escape_text(text).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_text_escapes_markup_by_default() {
        assert_eq!(row_text("<b>Audio</b>", false), "&lt;b&gt;Audio&lt;/b&gt;");
    }

    #[test]
    fn row_text_allows_markup_explicitly() {
        assert_eq!(row_text("<b>Audio</b>", true), "<b>Audio</b>");
    }
}
