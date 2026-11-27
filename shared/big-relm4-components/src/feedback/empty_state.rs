// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! What a view says when it has nothing to show, so a filter that matches
//! nothing does not read as broken.
//!
//! - [`page`] owns the whole area: an icon, a heading and an explanation.
//! - [`loading`] is the same page while the answer is on its way.
//! - [`line()`] shares the area with other content, where a squeezed status
//!   page would drop its title and leave a large icon.
//!
//! The text is the caller's, already translated.

use adw::prelude::*;

/// The whole view's empty state: an icon, a heading, and a sentence.
///
/// `description` may be empty when the heading says everything.
#[must_use]
pub fn page(icon_name: &str, title: &str, description: &str) -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name(icon_name)
        .title(title)
        .vexpand(true)
        .build();
    if !description.trim().is_empty() {
        page.set_description(Some(description));
    }
    page
}

/// How long a view is loading before its spinner shows: an answer that comes
/// sooner leaves no flash of one.
const SPINNER_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// The whole view while its content loads: a heading, a sentence, and a spinner
/// that appears once loading has taken 300 ms. The spinner keeps its place from
/// the start, so showing it moves nothing.
#[must_use]
pub fn loading(title: &str, description: &str) -> adw::StatusPage {
    let spinner = adw::Spinner::new();
    spinner.set_opacity(0.0);
    let page = page("", title, description);
    page.set_child(Some(&spinner));
    let spinner = spinner.downgrade();
    gtk::glib::timeout_add_local_once(SPINNER_DELAY, move || {
        if let Some(spinner) = spinner.upgrade() {
            spinner.set_opacity(1.0);
        }
    });
    page
}

/// A quiet line for a view that shares its area with something else.
///
/// Carries the `status` accessible role, which makes it a live region: the
/// text changes as a person types, and a screen reader announces it instead of
/// leaving them typing into a silence they cannot see.
#[must_use]
pub fn line(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .accessible_role(gtk::AccessibleRole::Status)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .valign(gtk::Align::Center)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["dimmed"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spinner holds its place at once and shows once the wait is long.
    #[test]
    fn spinner_waits_before_showing() {
        big_testkit::harness::require_gtk_test(|| {
            let waiting = loading("Loading", "Reading the file.");
            let spinner = waiting.child().expect("a spinner");
            assert!(spinner.opacity() < 0.5);
            assert!(big_testkit::harness::pump_until(|| spinner.opacity() > 0.5));
        });
    }

    #[test]
    fn page_without_description_shows_none() {
        big_testkit::harness::require_gtk_test(|| {
            let spoken = page("edit-find-symbolic", "Nothing here", "Try another word.");
            assert_eq!(spoken.description().as_deref(), Some("Try another word."));

            // A heading that says everything must not be followed by a blank
            // second line pretending to add something.
            let silent = page("edit-find-symbolic", "Nothing here", "   ");
            assert!(silent.description().is_none_or(|text| text.is_empty()));
        });
    }

    /// The line replaces content while other content stays on screen, so it is
    /// the only thing that says the filter ran. A screen reader has to hear it.
    #[test]
    fn a_line_announces_itself() {
        big_testkit::harness::require_gtk_test(|| {
            let label = line("No commands match “zzz”.");
            assert_eq!(label.accessible_role(), gtk::AccessibleRole::Status);
        });
    }
}
