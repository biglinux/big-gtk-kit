// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Where the keyboard focus is.

use gtk::prelude::*;

/// Whether the keyboard focus of `widget`'s window is on `widget` or inside it.
///
/// Reads the window's focus widget, which stays put while the window is
/// inactive. GTK clears the `FOCUS_WITHIN` state flag for that time, so the
/// flag says no for a window the person will come back to, focus and all.
pub fn holds_focus(widget: &impl IsA<gtk::Widget>) -> bool {
    let widget = widget.as_ref();
    widget
        .root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| focus == *widget || focus.is_ancestor(widget))
}

#[cfg(test)]
mod tests {
    use big_testkit::harness::{pump_until, require_gtk_test};
    use gtk::prelude::*;

    use super::holds_focus;

    #[test]
    fn focus_held_in_inactive_window() {
        require_gtk_test(|| {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let first = gtk::Button::with_label("First");
            let second = gtk::Button::with_label("Second");
            column.append(&first);
            column.append(&second);
            let window = gtk::Window::builder().child(&column).build();
            window.present();
            assert!(pump_until(|| window.is_active()));
            assert!(second.grab_focus());
            let other = gtk::Window::new();
            other.present();
            assert!(pump_until(|| other.is_active() && !window.is_active()));

            assert!(holds_focus(&second), "the focus widget itself");
            assert!(holds_focus(&column), "an ancestor of it");
            assert!(!holds_focus(&first), "a sibling");
            assert!(!second.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN));
            other.destroy();
            window.destroy();
        });
    }
}
