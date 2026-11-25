// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A once-timer for an action that waits: a pointer resting on a row, a press
//! held long enough to drag, a burst of changes settling.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;

/// One pending action on the main loop, replaced when armed again.
///
/// GLib destroys a once-timer when it runs, and may give its id to another
/// source after that. The timer forgets its id before it calls the action, so
/// [`OnceTimer::cancel`] never removes a source that already ran or one that
/// took its id. Clones share the pending action. Dropping the last clone leaves
/// a pending action to run.
#[derive(Clone, Default)]
pub struct OnceTimer(Rc<Cell<Option<glib::SourceId>>>);

impl OnceTimer {
    /// A timer with nothing pending.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `fire` once `delay` has passed, in place of the action pending.
    ///
    /// # Panics
    ///
    /// Panics off the thread that owns GLib's default main context.
    pub fn arm(&self, delay: Duration, fire: impl FnOnce() + 'static) {
        let id = glib::timeout_add_local_once(delay, self.replace_with(fire));
        self.0.set(Some(id));
    }

    /// Run `fire` once `seconds` have passed, in place of the action pending.
    ///
    /// GLib rounds every seconds timer of the process to one shared tick, so
    /// they wake it together and the action runs within a second of `seconds`.
    ///
    /// # Panics
    ///
    /// Panics off the thread that owns GLib's default main context.
    pub fn arm_seconds(&self, seconds: u32, fire: impl FnOnce() + 'static) {
        let id = glib::timeout_add_seconds_local_once(seconds, self.replace_with(fire));
        self.0.set(Some(id));
    }

    /// Whether an action waits to run.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        let id = self.0.take();
        let pending = id.is_some();
        self.0.set(id);
        pending
    }

    /// Drop the pending action, if any.
    pub fn cancel(&self) {
        if let Some(id) = self.0.take() {
            big_gtk_kit::source::remove(id);
        }
    }

    /// Cancel the pending action and wrap `fire` to forget its own id first.
    fn replace_with<F: FnOnce() + 'static>(&self, fire: F) -> impl FnOnce() + 'static + use<F> {
        self.cancel();
        let pending = Rc::clone(&self.0);
        move || {
            pending.set(None);
            fire();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn rearm_replaces_pending_action() {
        big_testkit::harness::require_gtk_test(|| {
            let context = glib::MainContext::default();
            let ran = Rc::new(RefCell::new(Vec::new()));
            let timer = OnceTimer::new();
            for name in ["first", "second"] {
                let ran = Rc::clone(&ran);
                timer.arm(Duration::ZERO, move || ran.borrow_mut().push(name));
            }
            assert!(timer.is_pending());
            while context.iteration(false) {}
            assert_eq!(*ran.borrow(), ["second"]);
            assert!(!timer.is_pending());

            // Cancelling after it ran, then with nothing armed, does nothing.
            timer.cancel();
            timer.cancel();

            let late = Rc::clone(&ran);
            timer.arm(Duration::ZERO, move || late.borrow_mut().push("cancelled"));
            timer.cancel();
            assert!(!timer.is_pending());
            while context.iteration(false) {}
            assert_eq!(*ran.borrow(), ["second"]);
        });
    }

    #[test]
    fn arm_seconds_replaces_pending_action() {
        big_testkit::harness::require_gtk_test(|| {
            let context = glib::MainContext::default();
            let ran = Rc::new(RefCell::new(Vec::new()));
            let push = |name| {
                let ran = Rc::clone(&ran);
                move || ran.borrow_mut().push(name)
            };
            let timer = OnceTimer::new();
            timer.arm(Duration::ZERO, push("milliseconds"));
            timer.arm_seconds(0, push("cancelled"));
            timer.cancel();
            timer.arm_seconds(1, push("seconds"));
            assert!(timer.is_pending());
            // Seconds timers share one tick grid, so a zero-second source left
            // behind would ring a tick before the one-second action.
            while ran.borrow().is_empty() {
                context.iteration(true);
            }
            assert_eq!(*ran.borrow(), ["seconds"]);
            assert!(!timer.is_pending());
        });
    }
}
