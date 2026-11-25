// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! The installed program a desktop id names.

use std::path::{Path, PathBuf};

use gtk::gio;
use gtk::glib::translate::from_glib_full;

/// The program `desktop_id` (`org.gnome.Nautilus.desktop`) names, or `None`
/// when nothing installed has that id.
///
/// gio 0.22 does not bind `GDesktopAppInfo`, and the way around it
/// (`gio::AppInfo::all()` and a search) parses every desktop file on the
/// machine, on the main loop, to find one. This reads one.
///
/// GIO lists an application directory, folders included, once, and again only
/// after the directory itself changes: an entry added to a folder it already
/// listed (`wine/Programs/<program>/`) is read from its file ([`desktop_file`]).
#[must_use]
pub fn app_info(desktop_id: &str) -> Option<gio::AppInfo> {
    // libgio exports `g_desktop_app_info_new` with this signature.
    unsafe extern "C" {
        fn g_desktop_app_info_new(desktop_id: *const std::ffi::c_char) -> *mut gio::ffi::GAppInfo;
    }
    let id = std::ffi::CString::new(desktop_id).ok()?;
    // SAFETY: the call returns NULL or a new reference (transfer full) to a
    // `GDesktopAppInfo`, which implements `GAppInfo`; the id is only read.
    let listed: Option<gio::AppInfo> =
        unsafe { from_glib_full(g_desktop_app_info_new(id.as_ptr())) };
    listed.or_else(|| {
        let keys = gtk::glib::KeyFile::new();
        keys.load_from_file(desktop_file(desktop_id)?, gtk::glib::KeyFileFlags::NONE)
            .ok()?;
        app_info_from_keyfile(&keys)
    })
}

/// The file `desktop_id` names in the first application directory holding it
/// (`$XDG_DATA_HOME`, then each of `$XDG_DATA_DIRS`), where a `-` may stand for
/// a folder: `wine-Programs-App.desktop` is `wine/Programs/App.desktop`.
///
/// An id without the `.desktop` suffix, or with a `/`, names no file, and no
/// `-` stands for a `.` or `..` folder, so the file is inside the directory.
#[must_use]
pub fn desktop_file(desktop_id: &str) -> Option<PathBuf> {
    if !desktop_id.ends_with(".desktop") || desktop_id.contains(['/', '\0']) {
        return None;
    }
    std::iter::once(gtk::glib::user_data_dir())
        .chain(gtk::glib::system_data_dirs())
        .find_map(|data| file_in(&data.join("applications"), desktop_id))
}

fn file_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let file = dir.join(name);
    if file.is_file() {
        return Some(file);
    }
    name.match_indices('-').find_map(|(at, _)| {
        let folder = &name[..at];
        if matches!(folder, "" | "." | "..") || !dir.join(folder).is_dir() {
            return None;
        }
        file_in(&dir.join(folder), &name[at + 1..])
    })
}

/// The program a desktop entry already read describes, with its `Terminal=`,
/// `Path=`, `TryExec=` and quoting, or `None` when GIO would not run it (not
/// an application, hidden, or its program missing).
///
/// From keys in memory, so what runs is what was read and shown, whatever the
/// file says by now.
#[must_use]
pub fn app_info_from_keyfile(keys: &gtk::glib::KeyFile) -> Option<gio::AppInfo> {
    use gtk::glib::translate::ToGlibPtr;
    // GIO builds a `Hidden=true` entry from a key file; only its directory
    // lookup treats one as deleted, the way the specification means it.
    if keys.boolean("Desktop Entry", "Hidden").unwrap_or(false) {
        return None;
    }
    // libgio exports `g_desktop_app_info_new_from_keyfile` with this
    // signature.
    unsafe extern "C" {
        fn g_desktop_app_info_new_from_keyfile(
            key_file: *mut gtk::glib::ffi::GKeyFile,
        ) -> *mut gio::ffi::GAppInfo;
    }
    // SAFETY: the key file is borrowed for the call and only read; the result
    // is NULL or a new reference (transfer full) to a `GDesktopAppInfo`, which
    // implements `GAppInfo`.
    unsafe { from_glib_full(g_desktop_app_info_new_from_keyfile(keys.to_glib_none().0)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;

    #[test]
    fn desktop_id_resolves_known_program() {
        assert!(app_info("no-such-program-anywhere.desktop").is_none());
        assert!(app_info("with\0nul.desktop").is_none());
        // Whatever the machine has installed, each id `all()` lists loads back
        // as the program with that id.
        if let Some(id) = gio::AppInfo::all().iter().find_map(|app| app.id()) {
            assert_eq!(app_info(&id).and_then(|app| app.id()), Some(id));
        }
    }

    /// A window may name any id: a `..` in it never reaches past the
    /// applications directory.
    #[test]
    fn dot_folders_stay_inside_applications() {
        let data = gtk::glib::user_data_dir();
        assert!(
            std::env::var_os("XDG_DATA_HOME").is_some_and(|home| data == Path::new(&home)),
            "run through tools/headless-gate.sh: this writes into the data home"
        );
        let applications = data.join("applications");
        std::fs::create_dir_all(applications.join("vendor")).unwrap();
        let outside = data.join("outside-probe.desktop");
        let inside = applications.join("vendor/inside-probe.desktop");
        std::fs::write(&outside, "").unwrap();
        std::fs::write(&inside, "").unwrap();
        assert_eq!(
            desktop_file("vendor-inside-probe.desktop"),
            Some(inside.clone()),
        );
        assert_eq!(desktop_file("..-outside-probe.desktop"), None);
        assert_eq!(desktop_file("vendor-..-..-outside-probe.desktop"), None);
        std::fs::remove_file(outside).unwrap();
        std::fs::remove_file(inside).unwrap();
    }

    /// A `Hidden=true` entry in the data home deletes the program it names.
    #[test]
    fn hidden_entry_names_nothing() {
        let data = gtk::glib::user_data_dir();
        assert!(
            std::env::var_os("XDG_DATA_HOME").is_some_and(|home| data == Path::new(&home)),
            "run through tools/headless-gate.sh: this writes into the data home"
        );
        let applications = data.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        let entry = applications.join("hidden-probe.desktop");
        std::fs::write(
            &entry,
            "[Desktop Entry]\nType=Application\nName=H\nExec=sh\nHidden=true\n",
        )
        .unwrap();
        let found = app_info("hidden-probe.desktop");
        std::fs::remove_file(entry).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn keyfile_keeps_quoted_arguments() {
        let keys = gtk::glib::KeyFile::new();
        keys.load_from_data(
            "[Desktop Entry]\nType=Application\nName=Q\nExec=sh -c 'echo \"$1\"' _ \"a b\"\n",
            gtk::glib::KeyFileFlags::NONE,
        )
        .unwrap();
        let app = app_info_from_keyfile(&keys).expect("sh is installed");
        let line = app.commandline().expect("a command line");
        assert!(line.to_string_lossy().ends_with("_ \"a b\""), "{line:?}");
        keys.set_string("Desktop Entry", "TryExec", "no-such-program-anywhere");
        assert!(app_info_from_keyfile(&keys).is_none());
    }
}
