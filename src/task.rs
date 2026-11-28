// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Off-main-thread task helpers for GTK apps.
//!
//! Run blocking work (directory walks, subprocess spawns, network requests)
//! off the GTK main loop and deliver the result back on it, so the UI never
//! freezes.

/// Run `task` off the GTK main thread, then deliver its result to `on_result`
/// back on the main thread.
///
/// `task` must be `Send` (it crosses to a worker thread); `on_result` need not
/// be: it runs on the GLib main loop, so it can touch `!Send` GTK objects and
/// `Rc` UI state. Use a trailing-edge debouncer
/// (for example, a debounced UI action) before this for slider /
/// spin-button storms, and a generation/stale guard in `on_result` when rapid
/// input can supersede an in-flight task.
///
/// A `task` that panics is logged and `on_result` never runs: resumed on the
/// main loop, the panic could not unwind through GLib and would abort every
/// program sharing the process.
pub fn spawn_blocking_result<T, B, U>(task: B, on_result: U)
where
    T: Send + 'static,
    B: FnOnce() -> T + Send + 'static,
    U: FnOnce(T) + 'static,
{
    gtk::glib::spawn_future_local(async move {
        match gtk::gio::spawn_blocking(task).await {
            Ok(value) => on_result(value),
            Err(payload) => {
                let reason = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("no message");
                log::error!("a blocking task panicked, its result is dropped: {reason}");
            }
        }
    });
}
