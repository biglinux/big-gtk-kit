// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Removing a GLib main-loop source that may already be gone.

use gtk::glib;

/// Remove a GLib source if it is still attached to the default main context.
///
/// `glib::SourceId::remove` unwraps GLib's "not found". A once-timer, or a
/// repeating source that returned `Break`, is destroyed by GLib as soon as it
/// finishes, so removing it afterwards panics; inside a GLib callback that
/// panic cannot unwind and aborts the process. This looks the source up in the
/// same context `g_source_remove` does and does nothing when it already ran.
pub fn remove(id: glib::SourceId) {
    if let Some(source) = glib::MainContext::default().find_source_by_id(&id) {
        source.destroy();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use super::*;

    #[gtk::test]
    #[ignore = "needs a display"]
    fn removes_only_pending_sources() {
        let context = glib::MainContext::default();
        let ran = glib::timeout_add_local_once(Duration::ZERO, || {});
        while context.iteration(false) {}
        remove(ran);

        let fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&fired);
        let pending = glib::timeout_add_local_once(Duration::ZERO, move || flag.set(true));
        remove(pending);
        while context.iteration(false) {}
        assert!(!fired.get());
    }
}
