// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Leave a trace when a process dies of a panic.
//!
//! glib's signal and closure paths have no `catch_unwind`, so a panic inside a
//! GTK callback unwinds out of an `extern "C"` trampoline and aborts. GUI
//! processes record the panic as one file in the app's own state directory,
//! where a release check reads it; nothing leaves the machine.

use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a hook is already installed, so a second call cannot chain two.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Where this application records that it panicked, or `None` with no state
/// directory to put it in.
///
/// `$XDG_STATE_HOME/<app>/crash`, falling back to `~/.local/state`, because a crash
/// record is state rather than configuration or cache: it must survive a cache
/// clear and it must not be synced as a setting.
#[must_use]
pub fn crash_dir(app: &str) -> Option<PathBuf> {
    big_os_kit::settings::state_home_checked().map(|state| state.join(app).join("crash"))
}

/// The most records a directory keeps: a program crashing in a loop keeps its
/// first ones, not a file per start for good.
const MAX_RECORDS: usize = 100;

/// A new record file in `directory`, numbered after the files already there;
/// `None` once it holds [`MAX_RECORDS`].
///
/// Created exclusively, so a record a person deleted, or another process
/// panicking at the same moment, moves the number on instead of overwriting one.
fn create_record(directory: &Path) -> Option<File> {
    let mut number = std::fs::read_dir(directory).map_or(0, Iterator::count);
    if number >= MAX_RECORDS {
        return None;
    }
    loop {
        let path = directory.join(format!("{number:04}.panic"));
        match File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => return Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => number += 1,
            Err(_) => return None,
        }
    }
}

/// Record every panic in this process under [`crash_dir`], then panic as before.
///
/// The previous hook still runs, so the standard library's backtrace is still
/// printed. One file per panic, named by a counter so the name does not depend
/// on a clock.
pub fn install(app: &str) {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // A test would leave records in the real home, which nothing can tell
    // from a real crash.
    if cfg!(test) {
        return;
    }
    let app = app.to_owned();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best effort throughout: a process that is already dying must not panic
        // again inside its own panic hook, so every failure here is ignored and the
        // standard library's hook still gets to run.
        if let Some(directory) = crash_dir(&app)
            && std::fs::create_dir_all(&directory).is_ok()
            && let Some(mut file) = create_record(&directory)
        {
            let where_at = info
                .location()
                .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
                .unwrap_or_else(|| String::from("unknown location"));
            // `PanicHookInfo::payload` is the message for the two cases a panic
            // carries one; anything else is recorded as the location alone,
            // which is still enough to find it.
            let said = info
                .payload()
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_default();
            let _ = writeln!(file, "app: {app}");
            let _ = writeln!(file, "at: {where_at}");
            let _ = writeln!(file, "said: {said}");
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_record_overwrites_none() {
        let directory = std::env::temp_dir().join(format!(
            "big-crash-marker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).expect("test directory");
        // 0000 was deleted, so two records remain and the count names 0002.
        for (name, text) in [("0001.panic", "first"), ("0002.panic", "second")] {
            std::fs::write(directory.join(name), text).expect("seed record");
        }
        writeln!(create_record(&directory).expect("new record"), "third").expect("write");

        let read = |name: &str| std::fs::read_to_string(directory.join(name)).unwrap_or_default();
        assert_eq!(read("0002.panic"), "second");
        assert_eq!(read("0003.panic"), "third\n");
        std::fs::remove_dir_all(&directory).expect("remove test directory");
    }

    /// The record holds the panic message, which can carry the person's data.
    #[test]
    fn record_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = std::env::temp_dir().join(format!(
            "big-crash-marker-mode-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).expect("test directory");
        let record = create_record(&directory).expect("new record");
        let mode = record
            .metadata()
            .expect("record metadata")
            .permissions()
            .mode();
        std::fs::remove_dir_all(&directory).expect("remove test directory");
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_full_directory_takes_no_more() {
        let directory = std::env::temp_dir().join(format!(
            "big-crash-marker-full-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).expect("test directory");
        for number in 0..MAX_RECORDS {
            std::fs::write(directory.join(format!("{number:04}.panic")), "").expect("seed record");
        }
        let refused = create_record(&directory).is_none();
        std::fs::remove_dir_all(&directory).expect("remove test directory");
        assert!(refused);
    }
}
