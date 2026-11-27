// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A new name for a file or folder, typed into an entry.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::feedback::field_error::FieldError;

/// Say under `entry`, as it is typed, why its name cannot be taken: a name no
/// entry can have (in words read through `translate`), or one `folder` already
/// holds (`taken` words it), asked off the main loop since the folder can be a
/// slow mount. `acceptable` hears whether the typed name may be used; empty
/// text and `current` may not.
pub fn refuse_name_as_typed(
    entry: &gtk::Entry,
    refusal: &FieldError,
    current: &str,
    folder: Option<gio::File>,
    translate: impl Fn(&str) -> String + 'static,
    taken: impl Fn(&str) -> String + 'static,
    acceptable: impl Fn(bool) + 'static,
) {
    let current = current.to_owned();
    let (refusal, taken, acceptable) = (refusal.clone(), Rc::new(taken), Rc::new(acceptable));
    entry.connect_changed(move |entry| {
        let wanted = entry.text().trim().to_owned();
        let refused = big_os_kit::file_name::file_name_refusal(&wanted)
            .map(|refused| refused.words(&translate));
        let usable = refused.is_none() && !wanted.is_empty() && wanted != current;
        acceptable(usable);
        match refused {
            Some(words) => refusal.show(&words),
            None => refusal.clear(),
        }
        let Some(folder) = folder.as_ref().filter(|_| usable) else {
            return;
        };
        let child = folder.child(&wanted);
        let (entry, refusal, taken, acceptable) = (
            entry.downgrade(),
            refusal.clone(),
            Rc::clone(&taken),
            Rc::clone(&acceptable),
        );
        glib::spawn_future_local(async move {
            let exists = child
                .query_info_future(
                    gio::FILE_ATTRIBUTE_STANDARD_NAME,
                    gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                    glib::Priority::DEFAULT,
                )
                .await
                .is_ok();
            // Only for the name still in the field.
            if exists
                && entry
                    .upgrade()
                    .is_some_and(|entry| entry.text().trim() == wanted)
            {
                acceptable(false);
                refusal.show(&taken(&wanted));
            }
        });
    });
}
