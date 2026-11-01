// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Listing a folder with `GtkDirectoryList`.

/// A list of what `folder` holds, with `attributes`, that does not watch the
/// folder for changes.
///
/// A watched list starts a file lookup for each change in the folder and
/// frees the queued change when the list goes; the lookup's answer then reads
/// freed memory in its file-info callbacks (GTK 4.22 to 4.24, still so on
/// `main`), a crash when lists are rebuilt while files arrive. A list that
/// must follow its folder is made again when the folder changes.
pub fn unwatched(attributes: &str, folder: &gtk::gio::File) -> gtk::DirectoryList {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one caller that turns watching off before the folder is set"
    )]
    let list = gtk::DirectoryList::new(Some(attributes), None::<&gtk::gio::File>);
    list.set_monitored(false);
    list.set_file(Some(folder));
    list
}
