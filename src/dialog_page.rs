// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The page a dialog shows: a header bar, and content that may scroll.

use adw::prelude::*;

/// The dialog margins around a page's content: start and end, top and bottom.
const MARGINS: (i32, i32) = (16, 12);

/// A header bar over a content box, in an `AdwToolbarView`: the page of a
/// dialog, or of a full-window view. Put the page's controls in
/// [`BigDialogPage::content`].
#[derive(Debug, Clone)]
pub struct BigDialogPage {
    root: adw::ToolbarView,
    header: adw::HeaderBar,
    content: gtk::Box,
}

impl BigDialogPage {
    /// A page whose content box fills the space under the header.
    #[must_use]
    pub fn plain() -> Self {
        Self::new(ScrollMode::Plain, MARGINS)
    }

    /// A page whose content box fills the space under the header edge to
    /// edge, without the dialog margins, as a full-window page such as a media
    /// viewer needs.
    #[must_use]
    pub fn edge_to_edge() -> Self {
        Self::new(ScrollMode::Plain, (0, 0))
    }

    /// A page whose content box scrolls vertically under the header.
    #[must_use]
    pub fn scrolled() -> Self {
        Self::new(ScrollMode::Scrolled, MARGINS)
    }

    /// A scrolled page whose content is at most `maximum_size` pixels wide,
    /// as [`adw::Clamp`] measures it with `tightening_threshold`.
    #[must_use]
    pub fn scrolled_clamped(maximum_size: i32, tightening_threshold: i32) -> Self {
        Self::new(
            ScrollMode::ScrolledClamped {
                maximum_size,
                tightening_threshold,
            },
            MARGINS,
        )
    }

    fn new(scroll_mode: ScrollMode, (horizontal, vertical): (i32, i32)) -> Self {
        let root = adw::ToolbarView::new();
        let header = crate::window_handle::header_bar(adw::HeaderBar::builder());
        root.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(horizontal)
            .margin_end(horizontal)
            .margin_top(vertical)
            .margin_bottom(vertical)
            .build();

        match scroll_mode {
            ScrollMode::Plain => root.set_content(Some(&content)),
            ScrollMode::Scrolled => {
                let scroll = scrolled_window();
                scroll.set_child(Some(&content));
                root.set_content(Some(&scroll));
            }
            ScrollMode::ScrolledClamped {
                maximum_size,
                tightening_threshold,
            } => {
                let scroll = scrolled_window();
                let clamp = adw::Clamp::new();
                clamp.set_maximum_size(maximum_size);
                clamp.set_tightening_threshold(tightening_threshold);
                clamp.set_child(Some(&content));
                scroll.set_child(Some(&clamp));
                root.set_content(Some(&scroll));
            }
        }

        Self {
            root,
            header,
            content,
        }
    }

    /// The toolbar view to place in a dialog.
    #[must_use]
    pub fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    /// The header bar, for the page's title and buttons.
    #[must_use]
    pub fn header(&self) -> &adw::HeaderBar {
        &self.header
    }

    /// The vertical box that holds the page's controls.
    #[must_use]
    pub fn content(&self) -> &gtk::Box {
        &self.content
    }

    /// The toolbar view to place in a dialog, giving up the other parts.
    #[must_use]
    pub fn into_root(self) -> adw::ToolbarView {
        self.root
    }

    /// Append a wrapped, dimmed paragraph of `text` to the content, and return it.
    pub fn append_description(&self, text: &str) -> gtk::Label {
        let label = gtk::Label::builder()
            .label(text)
            .halign(gtk::Align::Start)
            .wrap(true)
            .margin_bottom(16)
            .build();
        label.add_css_class("dimmed");
        self.content.append(&label);
        label
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollMode {
    Plain,
    Scrolled,
    ScrolledClamped {
        maximum_size: i32,
        tightening_threshold: i32,
    },
}

/// Focus `field` and select its text from `start` to `end` (`-1` for the end)
/// each time it is shown. The handler lives on the field, not on its dialog,
/// so a dialog reused for another opening does not keep it.
pub fn focus_when_shown(
    field: &(impl IsA<gtk::Editable> + IsA<gtk::Widget>),
    start: i32,
    end: i32,
) {
    field.connect_map(move |field| {
        let field = field.downgrade();
        gtk::glib::idle_add_local_once(move || {
            if let Some(field) = field.upgrade() {
                field.grab_focus();
                field.select_region(start, end);
            }
        });
    });
}

fn scrolled_window() -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full-window page has no dialog margins; a plain one keeps them.
    #[test]
    fn edge_to_edge_has_no_margins() {
        big_testkit::harness::require_gtk_test(|| {
            let margins = |page: &BigDialogPage| {
                let content = page.content();
                (
                    content.margin_start(),
                    content.margin_end(),
                    content.margin_top(),
                    content.margin_bottom(),
                )
            };
            assert_eq!(margins(&BigDialogPage::edge_to_edge()), (0, 0, 0, 0));
            assert_ne!(margins(&BigDialogPage::plain()), (0, 0, 0, 0));
        });
    }
}
