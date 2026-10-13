// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `AdwPasswordEntryRow` without the buffer it leaks.

use adw::prelude::*;
use gtk::glib;

/// Build a password row and release the extra reference libadwaita keeps on its
/// buffer.
///
/// libadwaita 1.9 (`adw_password_entry_row_init`) passes a fresh
/// `gtk_password_entry_buffer_new ()` straight to `gtk_text_set_buffer`, which
/// takes a reference of its own, so the creation reference is never dropped:
/// every row leaves a `GtkPasswordEntryBuffer`, and the password typed into it,
/// behind after it closes. The release is conditional, so a libadwaita that
/// fixes this is left alone instead of being unreferenced twice.
pub fn password_row(builder: adw::builders::PasswordEntryRowBuilder) -> adw::PasswordEntryRow {
    #[allow(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let row = builder.build();
    let buffer = row
        .delegate()
        .and_then(|delegate| delegate.downcast::<gtk::Text>().ok())
        .map(|text| text.buffer());
    // Held by the GtkText, by the leaked creation reference, and by `buffer` here.
    if let Some(buffer) = buffer.filter(|buffer| buffer.ref_count() == 3) {
        // SAFETY: drops the reference libadwaita created and never released; the
        // GtkText and `buffer` still hold theirs.
        unsafe { glib::gobject_ffi::g_object_unref(buffer.as_ptr().cast()) };
    }
    row
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{init, watch};

    #[gtk::test]
    #[ignore = "needs a display"]
    fn buffers_finalize_with_their_rows() {
        init();
        let finalized = Rc::new(Cell::new(0));
        for size in [0, 5, 25] {
            for _ in 0..25 {
                let row = password_row(adw::PasswordEntryRow::builder());
                row.set_text(&"x".repeat(size));
                let text = row.delegate().and_downcast::<gtk::Text>().expect("a text");
                watch(&text.buffer(), &finalized);
                drop((text, row));
            }
        }
        assert_eq!(finalized.get(), 75, "password buffers outlived their rows");
    }
}
