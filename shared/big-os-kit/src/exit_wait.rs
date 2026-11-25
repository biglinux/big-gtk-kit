// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The process's exit, held while other threads draw with cairo or Pango.
//!
//! Pixman frees its compositors in a library destructor, which runs after the
//! exit handlers, under any thread still drawing: that thread then crashes the
//! exit. A thread that draws holds [`drawing`] while it does.

use std::sync::{OnceLock, PoisonError, RwLock, RwLockReadGuard};

/// Held for reading while a thread draws. At exit the process takes it for
/// writing and keeps it, so exit waits for the drawings under way and none
/// starts after.
static DRAWING: RwLock<()> = RwLock::new(());

extern "C" fn stop_drawing() {
    std::mem::forget(DRAWING.write().unwrap_or_else(PoisonError::into_inner));
}

/// Hold the process's exit until the guard drops, for drawing with cairo or
/// Pango off the main thread. Once exit has begun, this blocks until the
/// process ends. `None` when the exit handler could not be registered: draw
/// nothing then.
///
/// A thread that holds it does not take it again, nor wait for a thread that
/// exits the process: either would keep exit waiting forever.
#[must_use]
pub fn drawing() -> Option<RwLockReadGuard<'static, ()>> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    // SAFETY: `stop_drawing` stays loaded until exit, as the host never unloads
    // a module, and touches only a static.
    let registered = *REGISTERED.get_or_init(|| unsafe { libc::atexit(stop_drawing) } == 0);
    registered.then(|| DRAWING.read().unwrap_or_else(PoisonError::into_inner))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    const CHILD: &str = "BIG_EXIT_WAIT_CHILD";

    /// In the child, the main thread returns while another thread draws; the
    /// drawing still finishes before the process ends.
    #[test]
    fn exit_waits_for_drawing() {
        if std::env::var_os(CHILD).is_some() {
            let (held, holding) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _drawing = super::drawing().expect("exit handler registered");
                held.send(()).expect("main thread waits");
                std::thread::sleep(Duration::from_millis(200));
                println!("drawn");
            });
            holding.recv().expect("drawing held");
            return;
        }
        let child = crate::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "--nocapture",
                "exit_wait::tests::exit_waits_for_drawing",
            ])
            .env(CHILD, "1")
            .build()
            .run()
            .expect("child run");
        assert!(child.status.success(), "{}", child.stdout_lossy());
        assert!(
            child.stdout_lossy().contains("drawn"),
            "{}",
            child.stdout_lossy()
        );
    }
}
