// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Copyable key/value rows for diagnostics and media-details dialogs.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::gtk;

/// Shared collection backing a dialog-level "Copy All" button.
#[derive(Debug, Clone, Default)]
pub struct BigDetailLines {
    lines: Rc<RefCell<Vec<String>>>,
}

impl BigDetailLines {
    /// Create an empty detail-line collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one row in insertion order.
    pub fn push(&self, title: impl Into<String>, value: impl Into<String>) {
        let line = format!("{}: {}", title.into(), value.into());
        self.lines.borrow_mut().push(line);
    }

    /// Register a line that is no row, such as a section's heading or the
    /// blank line before it.
    pub fn push_line(&self, line: impl Into<String>) {
        self.lines.borrow_mut().push(line.into());
    }

    /// Render all registered rows as copyable plain text.
    #[must_use]
    pub fn copy_text(&self) -> String {
        self.lines.borrow().join("\n")
    }
}

/// A "Copy All" button: it puts every line `lines` holds on the clipboard,
/// named `label` for its tooltip and a screen reader.
#[must_use]
pub fn copy_all_button(label: &str, lines: BigDetailLines) -> gtk::Button {
    let button = crate::feedback::tooltip::icon_button("edit-copy-symbolic", label, &[]);
    button.connect_clicked(move |button| {
        button.clipboard().set_text(&lines.copy_text());
    });
    button
}

/// Display-free specification for one copyable detail row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigCopyableDetailRowSpec {
    /// Row title.
    pub title: String,
    /// Row value.
    pub value: String,
    /// Copy button tooltip and accessible label.
    pub copy_label: String,
}

impl BigCopyableDetailRowSpec {
    /// Build a row specification.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        value: impl Into<String>,
        copy_label: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            value: value.into(),
            copy_label: copy_label.into(),
        }
    }
}

/// Copyable Adwaita action row.
#[derive(Debug, Clone)]
pub struct BigCopyableDetailRow {
    row: adw::ActionRow,
}

impl BigCopyableDetailRow {
    /// Build a detail row and optionally register it in a copy-all collection.
    #[must_use]
    pub fn new(spec: BigCopyableDetailRowSpec, lines: Option<&BigDetailLines>) -> Self {
        if let Some(lines) = lines {
            lines.push(spec.title.clone(), spec.value.clone());
        }

        // `use_markup(false)` first, because `AdwPreferencesRow:use-markup`
        // defaults to TRUE and this row shows text nobody here wrote: media tags,
        // codec strings, EXIF fields, arbitrary metadata keys. A title tag of
        // `<a href="…">support</a>` would render a clickable link, and
        // `<span size="100000">` would break the layout.
        //
        // One property covers both lines: libadwaita documents that subclasses
        // use the same flag for their other labels, the subtitle included.
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .title(&spec.title)
            .subtitle(&spec.value)
            .selectable(true)
            .build();
        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .build();
        copy_button.set_tooltip_text(Some(&spec.copy_label));
        let value = spec.value;
        copy_button.connect_clicked(move |button| {
            button.clipboard().set_text(&value);
        });
        row.add_suffix(&copy_button);

        Self { row }
    }

    /// Consume this wrapper and return the underlying row.
    #[must_use]
    pub fn into_row(self) -> adw::ActionRow {
        self.row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_all_keeps_insertion_order() {
        let lines = BigDetailLines::new();

        lines.push("File", "/video/movie.mkv");
        lines.push_line("");
        lines.push_line("[Video]");
        lines.push("Duration", "1:02:03");

        assert_eq!(
            lines.copy_text(),
            "File: /video/movie.mkv\n\n[Video]\nDuration: 1:02:03"
        );
    }

    /// A detail row shows text the person did not write (media tags, codec
    /// strings, EXIF fields), and `AdwPreferencesRow:use-markup` defaults to
    /// TRUE. With markup on, a tag of `<a href="…">…</a>` renders a clickable
    /// link inside the details list.
    #[test]
    fn detail_row_shows_markup_verbatim() {
        big_testkit::harness::require_gtk_test(|| {
            let row = BigCopyableDetailRow::new(
                BigCopyableDetailRowSpec::new(
                    "Title",
                    "<a href=\"https://evil.example\">support</a>",
                    "Copy",
                ),
                None,
            );
            let row = row.into_row();
            assert!(
                !row.uses_markup(),
                "the row interprets metadata as Pango markup"
            );
            // The text is kept whole, not swallowed by a parser.
            assert!(
                row.subtitle()
                    .is_some_and(|shown| shown.contains("<a href"))
            );
        });
    }
}
