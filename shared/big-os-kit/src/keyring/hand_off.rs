// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! When the keyring may be asked for a saved password in a session that
//! started locked after an automatic login.
//!
//! Nobody typed a password at such a login, so the account's keyring is still
//! locked: a program that asks it for a saved password then has the keyring
//! ask for the password in a window of its own, behind the lock screen. The
//! password that first unlocks the screen opens it instead. `bigshell-session`
//! leaves [`open_keyring_mark`] when it starts the session locked; the lock
//! screen takes it once its keyring helper has the password, and the helper
//! leaves [`handed_file`] once the keyring modules have answered, whatever
//! they made of the password. The programs that open at login wait for that
//! file too (`bigshell-autostart.path`).

use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};

use rustix::fs::inotify;

/// In the runtime directory.
const MARK: &str = "bigshell/open-keyring";
/// In the runtime directory.
const HANDED: &str = "bigshell/keyring-handed";

/// `$XDG_RUNTIME_DIR/bigshell/open-keyring`: the session started locked and
/// the lock screen has not handed the password to the keyring yet.
#[must_use]
pub fn open_keyring_mark() -> PathBuf {
    glib::user_runtime_dir().join(MARK)
}

/// `$XDG_RUNTIME_DIR/bigshell/keyring-handed`: the keyring modules have had
/// the password that unlocked the screen.
#[must_use]
pub fn handed_file() -> PathBuf {
    glib::user_runtime_dir().join(HANDED)
}

/// Run `then` once the keyring may be asked for a saved password: at once,
/// unless the session started locked and the password that unlocks the
/// screen has not reached the keyring yet. Then a thread of its own sleeps
/// in an inotify read until the file appears, and `then` runs on the default
/// main context, which this thread must own (GTK's main thread does).
///
/// A session unlocked without a password (`loginctl unlock-session`) waits on
/// until a password unlocks it; a watch that cannot be made, or that ends
/// with its folder removed, runs `then` at once, since a password window
/// beats a program that never asks. A thread
/// that cannot start drops `then` unrun, which the log says.
pub fn when_handed(then: impl FnOnce() + 'static) {
    wait_in(&glib::user_runtime_dir(), then);
}

/// [`when_handed`] with `runtime` as the runtime directory.
fn wait_in(runtime: &Path, then: impl FnOnce() + 'static) {
    let mark = runtime.join(MARK);
    if !mark.exists() {
        then();
        return;
    }
    let handed = runtime.join(HANDED);
    // The mark's directory, where the file is made: it exists, the mark is in
    // it.
    let watched = inotify::init(inotify::CreateFlags::CLOEXEC).and_then(|watch| {
        let directory = mark.parent().unwrap_or(runtime);
        let made = inotify::WatchFlags::CREATE | inotify::WatchFlags::MOVED_TO;
        inotify::add_watch(&watch, directory, made).map(|_| watch)
    });
    let watch = match watched {
        Ok(watch) => watch,
        Err(error) => {
            log::warn!("keyring: the hand-off cannot be waited for: {error}");
            then();
            return;
        }
    };
    // Looked at after the watch exists, so a file made in between is seen by
    // one of the two.
    if handed.exists() {
        then();
        return;
    }
    let then = glib::thread_guard::ThreadGuard::new(then);
    let waiting = std::thread::Builder::new()
        .name("keyring-hand-off".to_owned())
        .spawn(move || {
            let mut buffer = [MaybeUninit::uninit(); 4096];
            let mut events = inotify::Reader::new(&watch, &mut buffer);
            while !handed.exists() {
                match events.next() {
                    // The folder went, the mark with it: nothing is awaited.
                    Ok(event) if event.events().contains(inotify::ReadFlags::IGNORED) => break,
                    Ok(_) => {}
                    Err(error) => {
                        log::warn!("keyring: the hand-off is not waited for: {error}");
                        break;
                    }
                }
            }
            glib::MainContext::default().invoke(move || (then.into_inner())());
        });
    if let Err(error) = waiting {
        log::warn!("keyring: the hand-off cannot be waited for: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    /// A directory of the test's own, removed when it ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("big-os-kit-hand-off-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("bigshell")).expect("scratch directory");
            Self(dir)
        }

        fn leave(&self, name: &str) {
            std::fs::File::create(self.0.join(name)).expect("file left");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// How many times `then` ran.
    fn counted(runtime: &Path) -> Rc<Cell<u32>> {
        let runs = Rc::new(Cell::new(0));
        let counter = Rc::clone(&runs);
        wait_in(runtime, move || counter.set(counter.get() + 1));
        runs
    }

    /// Iterate the default context until `runs` is `want`, for at most two
    /// seconds, then for a moment more to catch a run that should not come.
    fn settle(context: &glib::MainContext, runs: &Cell<u32>, want: u32) {
        let until = Instant::now() + Duration::from_secs(2);
        while runs.get() < want && Instant::now() < until {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(10));
        }
        let until = Instant::now() + Duration::from_millis(200);
        while Instant::now() < until {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Without the mark nothing waits, and with the hand-off already done
    /// neither.
    #[test]
    fn unmarked_or_handed_runs_at_once() {
        let scratch = Scratch::new("at-once");
        assert_eq!(counted(&scratch.0).get(), 1, "no mark");
        scratch.leave(MARK);
        scratch.leave(HANDED);
        assert_eq!(counted(&scratch.0).get(), 1, "already handed");
    }

    /// Marked: nothing runs until the hand-off's file appears, then once, and
    /// not again when it is made again.
    #[test]
    fn marked_waits_for_hand_off() {
        let context = glib::MainContext::default();
        let _owned = context.acquire().expect("the default context");
        let scratch = Scratch::new("waits");
        scratch.leave(MARK);
        let runs = counted(&scratch.0);
        settle(&context, &runs, 0);
        assert_eq!(runs.get(), 0, "ran before the hand-off");
        scratch.leave(HANDED);
        settle(&context, &runs, 1);
        assert_eq!(runs.get(), 1, "after the hand-off");
        std::fs::remove_file(scratch.0.join(HANDED)).expect("removed");
        scratch.leave(HANDED);
        settle(&context, &runs, 1);
        assert_eq!(runs.get(), 1, "made again");
    }

    /// A folder removed under the wait takes its watch and the mark with it:
    /// no hand-off is coming to wake it, and none is awaited any more.
    #[test]
    fn removed_folder_ends_the_wait() {
        let context = glib::MainContext::default();
        let _owned = context.acquire().expect("the default context");
        let scratch = Scratch::new("removed");
        scratch.leave(MARK);
        let runs = counted(&scratch.0);
        settle(&context, &runs, 0);
        assert_eq!(runs.get(), 0, "ran before the folder went");
        std::fs::remove_dir_all(scratch.0.join("bigshell")).expect("removed");
        settle(&context, &runs, 1);
        assert_eq!(runs.get(), 1, "after the folder went");
    }
}
