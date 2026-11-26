// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Entry rows a screen reader names once.
//!
//! `AdwEntryRow` shows its title in two labels, one over an empty field and one
//! over a filled field, and both name the row as their mnemonic widget: GTK
//! labels the row by the two, "Username Username". The row keeps one of them;
//! both follow the title. The title is drawn as typed: libadwaita reads it as
//! Pango markup by default, and titles carry names other programs chose.
//! `clippy.toml` sends every entry and password row of the suite through
//! [`build`] and [`password`].

use adw::prelude::*;

/// The entry row `builder` describes, named once.
pub fn build(builder: adw::builders::EntryRowBuilder) -> adw::EntryRow {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let row = builder.use_markup(false).build();
    named(row)
}

/// The password row `builder` describes, named once and without the buffer
/// libadwaita leaks.
pub fn password(builder: adw::builders::PasswordEntryRowBuilder) -> adw::PasswordEntryRow {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let row = big_gtk_kit::password::password_row(builder.use_markup(false));
    named(row)
}

/// `row`, labeled by one of its title labels.
fn named<R: IsA<adw::EntryRow>>(row: R) -> R {
    let entry = row.upcast_ref::<adw::EntryRow>();
    if let Some(title) = entry.list_mnemonic_labels().first() {
        big_gtk_kit::accessible::relate(
            entry,
            gtk::AccessibleRelation::LabelledBy,
            &[title.upcast_ref()],
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `row` is labeled by `labels` and nothing else.
    fn labeled_by(row: &adw::EntryRow, labels: &[gtk::Widget]) -> bool {
        use gtk::glib::translate::ToGlibPtr;
        let accessible = row.upcast_ref::<gtk::Accessible>().to_glib_none().0;
        let relation = gtk::ffi::GTK_ACCESSIBLE_RELATION_LABELLED_BY;
        let label = |index: usize| -> *mut gtk::ffi::GtkAccessible {
            labels[index]
                .upcast_ref::<gtk::Accessible>()
                .to_glib_none()
                .0
        };
        let none = std::ptr::null_mut::<gtk::ffi::GtkAccessible>();
        // SAFETY: the row and the labels outlive the call, and the relation's
        // variadic value is a NULL-terminated list of accessibles.
        let mismatch: Option<gtk::glib::GString> = unsafe {
            gtk::glib::translate::from_glib_full(match labels.len() {
                1 => gtk::ffi::gtk_test_accessible_check_relation(
                    accessible,
                    relation,
                    label(0),
                    none,
                ),
                2 => gtk::ffi::gtk_test_accessible_check_relation(
                    accessible,
                    relation,
                    label(0),
                    label(1),
                    none,
                ),
                _ => unreachable!("an entry row has two title labels"),
            })
        };
        mismatch.is_none()
    }

    #[test]
    fn rows_have_one_label() {
        big_testkit::harness::require_gtk_test(|| {
            #[expect(clippy::disallowed_methods, reason = "the row as libadwaita builds it")]
            let plain = adw::EntryRow::builder().title("Username").build();
            assert!(labeled_by(&plain, &plain.list_mnemonic_labels()));
            let row = build(adw::EntryRow::builder().title("Username"));
            assert!(labeled_by(&row, &row.list_mnemonic_labels()[..1]));
            let row = password(adw::PasswordEntryRow::builder().title("Password"));
            let row = row.upcast_ref::<adw::EntryRow>();
            assert!(labeled_by(row, &row.list_mnemonic_labels()[..1]));
        });
    }
}
