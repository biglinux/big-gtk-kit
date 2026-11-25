// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Saying something out loud to a screen reader, without killing the process.

use gtk::prelude::*;

/// The accessibility context GTK hands out when there is nowhere to speak.
const NO_BRIDGE_CONTEXT: &str = "GtkTestATContext";

/// Say `text` to a screen reader, unless the toolkit has nowhere to say it.
///
/// A label appearing on screen is not spoken; `announce` is what makes a
/// screen reader read a notification as it arrives, a volume step as the slider
/// moves, an error as it happens.
///
/// Never call `gtk::prelude::AccessibleExt::announce` directly:
/// `gtk_accessible_announce` is a NULL function pointer on `GtkTestATContext`,
/// the context GTK 4.22 falls back to when the AT-SPI bus is missing (no
/// `at-spi2-core` running, or a headless test), and calling it there is a
/// segmentation fault.
///
/// The bad context name is matched, so any context this function does not
/// recognise still gets the announcement, and a GTK rename cannot silence it.
///
/// # Examples
///
/// ```no_run
/// use big_relm4_components::feedback::announce::announce;
///
/// # let window = gtk::Window::new();
/// announce(
///     &window,
///     "Wallpaper changed",
///     gtk::AccessibleAnnouncementPriority::Medium,
/// );
/// ```
pub fn announce(
    widget: &impl IsA<gtk::Accessible>,
    text: &str,
    priority: gtk::AccessibleAnnouncementPriority,
) {
    let accessible = widget.as_ref();
    // GTK answers NULL here when accessibility is off (`GTK_A11Y=none`), and the safe
    // binding asserts it never does, which panics inside a signal handler. Asked
    // through the C call instead.
    // SAFETY: `accessible` is a live GtkAccessible for the whole call, and a non-null
    // result is a new reference, which `from_glib_full` takes over and releases.
    let context: Option<gtk::ATContext> = unsafe {
        gtk::glib::translate::from_glib_full(gtk::ffi::gtk_accessible_get_at_context(
            gtk::glib::translate::ToGlibPtr::to_glib_none(accessible).0,
        ))
    };
    match context {
        Some(context) if context.type_().name() != NO_BRIDGE_CONTEXT => {
            accessible.announce(text, priority);
        }
        _ => {}
    }
}

/// [`announce`] from `widget`'s window, or from `widget` outside one. GTK
/// drops an announcement from a widget no screen reader has looked at: a
/// notice strip hidden between messages, a row in a list nobody opened.
pub fn announce_from_window(
    widget: &impl IsA<gtk::Widget>,
    text: &str,
    priority: gtk::AccessibleAnnouncementPriority,
) {
    match widget.as_ref().root() {
        Some(window) => announce(&window, text, priority),
        None => announce(widget.as_ref(), text, priority),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test is the guard: without it the call below is a `SIGSEGV`, and a
    /// test process that dies takes its whole binary with it. It passes by
    /// returning at all.
    #[test]
    fn announce_survives_missing_bridge() {
        big_testkit::harness::require_gtk_test(|| {
            // Presented, because an unrealized window has no accessibility
            // context yet and `announce` on one is a no-op that proves nothing.
            let window = gtk::Window::new();
            window.present();
            announce(
                &window,
                "there is nobody listening",
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        });
    }
}
