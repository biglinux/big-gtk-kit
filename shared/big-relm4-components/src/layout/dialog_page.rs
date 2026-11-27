// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Shared dialog page shell for BigLinux Relm4/libadwaita apps.

use adw::prelude::*;
use relm4::gtk;

/// The margins of dialog content, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BigDialogMargins {
    pub start: i32,
    pub end: i32,
    pub top: i32,
    pub bottom: i32,
}

impl Default for BigDialogMargins {
    fn default() -> Self {
        Self {
            start: 16,
            end: 16,
            top: 12,
            bottom: 12,
        }
    }
}

/// Reusable dialog shell: `AdwToolbarView`, top `AdwHeaderBar`, content box.
///
/// Use this when app dialogs share the same visual structure. Put app-specific
/// controls in [`BigDialogPage::content`].
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
        Self::new(ScrollMode::Plain)
    }

    /// A page whose content box scrolls vertically under the header.
    #[must_use]
    pub fn scrolled() -> Self {
        Self::new(ScrollMode::Scrolled)
    }

    /// A scrolled page whose content is at most `maximum_size` pixels wide,
    /// as [`adw::Clamp`] measures it with `tightening_threshold`.
    #[must_use]
    pub fn scrolled_clamped(maximum_size: i32, tightening_threshold: i32) -> Self {
        Self::new(ScrollMode::ScrolledClamped {
            maximum_size,
            tightening_threshold,
        })
    }

    fn new(scroll_mode: ScrollMode) -> Self {
        let root = adw::ToolbarView::new();
        let header = crate::layout::window_handle::header_bar(adw::HeaderBar::builder());
        root.add_top_bar(&header);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        apply_margins(&content, BigDialogMargins::default());

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

    /// The vertical box, with the dialog margins, that holds the page's controls.
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
        let label = dialog_description(text);
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

/// Build a wrapped, left-aligned dim label suitable for placing above
/// the form on a dialog page.
#[must_use]
pub(crate) fn dialog_description(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .wrap(true)
        .margin_bottom(16)
        .build();
    label.add_css_class("dimmed");
    label
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

fn apply_margins(content: &gtk::Box, margins: BigDialogMargins) {
    content.set_margin_start(margins.start);
    content.set_margin_end(margins.end);
    content.set_margin_top(margins.top);
    content.set_margin_bottom(margins.bottom);
}
