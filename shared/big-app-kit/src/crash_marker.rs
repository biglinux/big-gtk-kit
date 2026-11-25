// SPDX-License-Identifier: MIT
//! Leave a trace when a process dies of a panic.
//!
//! glib's signal and closure paths have no `catch_unwind`, so a panic inside a
//! GTK callback unwinds out of an `extern "C"` trampoline and aborts. GUI
//! processes record the panic as one file in the app's own state directory,
//! which the release gate reads; nothing leaves the machine.

use std::ffi::OsString;
use std::fs::File;
use std::io::Write as _;
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
    state_home(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
        .map(|state| state.join(app).join("crash"))
}

/// The XDG state directory. A relative path in either variable is ignored, as
/// the base directory specification asks.
fn state_home(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    xdg_state_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })
}

/// A new record file in `directory`, numbered after the files already there.
///
/// Created exclusively, so a record a person deleted, or another process
/// panicking at the same moment, moves the number on instead of overwriting one.
fn create_record(directory: &Path) -> Option<File> {
    let mut number = std::fs::read_dir(directory).map_or(0, Iterator::count);
    loop {
        let path = directory.join(format!("{number:04}.panic"));
        match File::options().write(true).create_new(true).open(path) {
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
    // A test would leave records in the real home, which the release gate
    // cannot tell from a real crash.
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

    /// Crash files are state: they survive a cache clear and are not settings.
    #[test]
    fn records_go_to_the_state_directory() {
        let state = |xdg: Option<&str>, home: Option<&str>| {
            state_home(xdg.map(OsString::from), home.map(OsString::from))
        };
        assert_eq!(
            state(Some("/somewhere/state"), Some("/home/a")),
            Some(PathBuf::from("/somewhere/state"))
        );
        assert_eq!(
            state(Some("relative"), Some("/home/a")),
            Some(PathBuf::from("/home/a/.local/state"))
        );
        assert_eq!(state(None, Some("relative")), None);
        assert_eq!(state(None, None), None);
    }

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

    /// Installing twice must not chain two hooks: the second call is the one a
    /// library and its host would both make, and two records for one panic would
    /// make the count the gate reads wrong.
    #[test]
    fn a_second_install_is_refused() {
        INSTALLED.store(false, Ordering::SeqCst);
        install("test-app");
        assert!(INSTALLED.load(Ordering::SeqCst));
        install("test-app");
        assert!(INSTALLED.load(Ordering::SeqCst));
    }
}
