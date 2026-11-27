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
    listed.or_else(|| app_info_in(data_dirs(), desktop_id))
}

/// The program `desktop_id` names, read from its file in the application
/// directories of `data_dirs`.
fn app_info_in(
    data_dirs: impl IntoIterator<Item = PathBuf>,
    desktop_id: &str,
) -> Option<gio::AppInfo> {
    let keys = gtk::glib::KeyFile::new();
    keys.load_from_file(
        desktop_file_in(data_dirs, desktop_id)?,
        gtk::glib::KeyFileFlags::NONE,
    )
    .ok()?;
    app_info_from_keyfile(&keys)
}

/// The file `desktop_id` names in the first application directory holding it
/// (`$XDG_DATA_HOME`, then each of `$XDG_DATA_DIRS`), where a `-` may stand for
/// a folder: `wine-Programs-App.desktop` is `wine/Programs/App.desktop`.
///
/// An id without the `.desktop` suffix, or with a `/`, names no file, and no
/// `-` stands for a `.` or `..` folder, so the file is inside the directory.
#[must_use]
pub fn desktop_file(desktop_id: &str) -> Option<PathBuf> {
    desktop_file_in(data_dirs(), desktop_id)
}

fn data_dirs() -> impl Iterator<Item = PathBuf> {
    std::iter::once(gtk::glib::user_data_dir()).chain(gtk::glib::system_data_dirs())
}

fn desktop_file_in(
    data_dirs: impl IntoIterator<Item = PathBuf>,
    desktop_id: &str,
) -> Option<PathBuf> {
    if !desktop_id.ends_with(".desktop") || desktop_id.contains(['/', '\0']) {
        return None;
    }
    data_dirs
        .into_iter()
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

    fn scratch_data_dir(name: &str) -> PathBuf {
        gtk::glib::mkdtemp(std::env::temp_dir().join(format!("{name}-XXXXXX")))
            .expect("temporary directory")
    }

    /// A window may name any id: a `..` in it never reaches past the
    /// applications directory.
    #[test]
    fn dot_folders_stay_inside_applications() {
        let data_home = scratch_data_dir("big-desktop-file");
        let applications = data_home.join("applications");
        std::fs::create_dir_all(applications.join("vendor")).unwrap();
        let inside = applications.join("vendor/inside.desktop");
        std::fs::write(data_home.join("outside.desktop"), "").unwrap();
        std::fs::write(&inside, "").unwrap();
        let find = |id| desktop_file_in([data_home.clone()], id);
        assert_eq!(find("vendor-inside.desktop"), Some(inside));
        assert_eq!(find("../outside.desktop"), None);
        assert_eq!(find("..-outside.desktop"), None);
        assert_eq!(find("vendor-..-..-outside.desktop"), None);
        std::fs::remove_dir_all(data_home).unwrap();
    }

    /// A `Hidden=true` entry deletes the program it names.
    #[test]
    fn hidden_entry_names_nothing() {
        let data_home = scratch_data_dir("big-desktop-hidden");
        let applications = data_home.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        let entry = "[Desktop Entry]\nType=Application\nName=H\nExec=sh\n";
        std::fs::write(applications.join("shown.desktop"), entry).unwrap();
        std::fs::write(
            applications.join("hidden.desktop"),
            format!("{entry}Hidden=true\n"),
        )
        .unwrap();
        let shown = app_info_in([data_home.clone()], "shown.desktop");
        let hidden = app_info_in([data_home.clone()], "hidden.desktop");
        std::fs::remove_dir_all(data_home).unwrap();
        assert!(shown.is_some() && hidden.is_none());
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
