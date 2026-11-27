// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A new name for a file or folder, typed into an entry.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::feedback::field_error::FieldError;

/// Say under `entry`, as it is typed, why its name cannot be taken: a name no
/// entry can have (in words read through `translate`), or one `folder` already
/// holds (`taken` words it), asked off the main loop since the folder can be a
/// slow mount. `acceptable` hears whether the typed name may be used; empty
/// text and `current` may not, nor a name `folder` has not yet answered for.
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
    // Counts the edits, so a lookup answers only for the last one.
    let edits = Rc::new(Cell::new(0_u64));
    entry.connect_changed(move |entry| {
        let edit = edits.get() + 1;
        edits.set(edit);
        let wanted = entry.text().trim().to_owned();
        let refused = big_os_kit::file_name::file_name_refusal(&wanted)
            .map(|refused| refused.words(&translate));
        let usable = refused.is_none() && !wanted.is_empty() && wanted != current;
        let lookup = folder.as_ref().filter(|_| usable);
        acceptable(usable && lookup.is_none());
        match refused {
            Some(words) => refusal.show(&words),
            None => refusal.clear(),
        }
        let Some(folder) = lookup else {
            return;
        };
        let child = folder.child(&wanted);
        let (edits, refusal, taken, acceptable) = (
            Rc::clone(&edits),
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
            if edits.get() != edit {
                return;
            }
            // Only a file found refuses the name: a lookup failing otherwise
            // leaves it to the write, which reports its own error.
            acceptable(!exists);
            if exists {
                refusal.show(&taken(&wanted));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn scratch_folder(name: &str) -> PathBuf {
        glib::mkdtemp(std::env::temp_dir().join(format!("{name}-XXXXXX")))
            .expect("temporary directory")
    }

    /// A field naming a file in `folder`, its refusal, and the last word its
    /// `acceptable` heard.
    fn field_in(folder: &Path) -> (gtk::Entry, FieldError, Rc<Cell<Option<bool>>>) {
        let entry = gtk::Entry::new();
        let refusal = FieldError::under(&entry);
        let heard = Rc::new(Cell::new(None));
        let sink = Rc::clone(&heard);
        refuse_name_as_typed(
            &entry,
            &refusal,
            "current.txt",
            Some(gio::File::for_path(folder)),
            str::to_owned,
            |name| format!("{name} is taken"),
            move |usable| sink.set(Some(usable)),
        );
        (entry, refusal, heard)
    }

    /// A name is usable only once the folder said it holds no such file.
    #[test]
    fn pending_lookup_keeps_name_unusable() {
        big_testkit::harness::require_gtk_test(|| {
            let folder = scratch_folder("big-name-pending");
            std::fs::write(folder.join("taken.txt"), "").unwrap();
            let (entry, refusal, heard) = field_in(&folder);
            entry.set_text("free.txt");
            assert_eq!(
                heard.get(),
                Some(false),
                "usable before the folder answered"
            );
            let usable = || heard.get() == Some(true);
            assert!(big_testkit::harness::pump_until(usable));
            entry.set_text("taken.txt");
            let refused = || refusal.line().is_visible();
            assert!(big_testkit::harness::pump_until(refused));
            assert_eq!(refusal.line().label(), "taken.txt is taken");
            assert_eq!(heard.get(), Some(false));
            std::fs::remove_dir_all(folder).unwrap();
        });
    }

    /// An answer for an earlier edit of the same name, asked before the file
    /// went away, does not refuse the name typed again. Under load the first
    /// lookup may end only after the file is gone; the test then passes without
    /// a stale answer, and cannot fail wrongly.
    #[test]
    fn superseded_lookup_is_ignored() {
        big_testkit::harness::require_gtk_test(|| {
            let folder = scratch_folder("big-name-superseded");
            std::fs::write(folder.join("a.txt"), "").unwrap();
            let (entry, refusal, heard) = field_in(&folder);
            entry.set_text("a.txt");
            // The first lookup starts and finds the file, its answer not yet read.
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(200));
            entry.set_text("b.txt");
            std::fs::remove_file(folder.join("a.txt")).unwrap();
            entry.set_text("a.txt");
            assert!(big_testkit::harness::pump_until(
                || heard.get() == Some(true)
            ));
            big_testkit::harness::pump();
            assert!(!refusal.line().is_visible());
            assert_eq!(heard.get(), Some(true));
            std::fs::remove_dir_all(folder).unwrap();
        });
    }
}
