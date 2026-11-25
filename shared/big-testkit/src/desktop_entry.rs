// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A program made from a desktop file a test wrote.

use std::path::Path;

use gtk::gio;
use gtk::glib::translate::from_glib_full;

/// The program the desktop file at `path` describes, or `None` when GIO would
/// not run it. Its `filename` property is `path`, which an entry made from keys
/// has not.
#[must_use]
pub fn app_info_from_file(path: &Path) -> Option<gio::AppInfo> {
    // libgio exports `g_desktop_app_info_new_from_filename` with this
    // signature.
    unsafe extern "C" {
        fn g_desktop_app_info_new_from_filename(
            filename: *const std::ffi::c_char,
        ) -> *mut gio::ffi::GAppInfo;
    }
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: the path is only read; the result is NULL or a new reference
    // (transfer full) to a `GDesktopAppInfo`, which implements `GAppInfo`.
    unsafe { from_glib_full(g_desktop_app_info_new_from_filename(path.as_ptr())) }
}
