// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Teardown discipline for manually parented popovers.
//!
//! `gtk::Popover::set_parent` never gets an automatic counterpart: GTK4
//! expects the owner to `unparent()` during the parent's dispose, otherwise
//! the parent finalizes with the popover still attached ("Finalizing
//! GtkWidget, but it still has children left") and the popover leaks.
//!
//! Two hooks, both needed (signal-level equivalent of the dispose-vfunc
//! pattern for code that does not subclass):
//! - `unrealize`: fires early (widget leaves the tree / toplevel closes),
//!   but only for widgets that were realized; a control on a never-shown
//!   stack page never realizes, so unrealize alone misses it.
//! - `destroy`: emitted at the start of the widget's dispose, in time to
//!   unparent before the finalize check. An external strong ref (module
//!   registries, controllers) can delay dispose indefinitely, so unrealize
//!   stays as the early path.

use relm4::gtk;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

/// Unparent (and pop down) `popover` when `parent` unrealizes or is
/// destroyed, whichever comes first; idempotent. Accepts any popover type
/// (`Popover`, `PopoverMenu`, …). Holds only a weak ref to the popover, so
/// callers keep full ownership.
///
/// An unrealize is not always the end: a widget taken out of its window and
/// put back (a tab moved or brought back, a window hidden and shown again)
/// unrealizes and realizes again, and the popover it lost is attached again
/// then. Without that the next `popup()` realizes a popover with no window
/// under it, and GTK aborts the whole desktop.
pub fn unparent_popover_on_parent_teardown(parent: &gtk::Widget, popover: &impl IsA<gtk::Popover>) {
    let popover: &gtk::Popover = popover.upcast_ref();
    let popover_weak = popover.downgrade();
    parent.connect_realize(move |parent| {
        if let Some(popover) = popover_weak.upgrade()
            && popover.parent().is_none()
        {
            popover.set_parent(parent);
        }
    });
    let popover_weak = popover.downgrade();
    parent.connect_unrealize(move |_| release_popover(&popover_weak));
    let popover_weak = popover.downgrade();
    parent.connect_destroy(move |_| release_popover(&popover_weak));
}

fn release_popover(popover_weak: &glib::WeakRef<gtk::Popover>) {
    if let Some(popover) = popover_weak.upgrade()
        && popover.parent().is_some()
    {
        popover.popdown();
        popover.unparent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A control moved between boxes keeps a popover that still opens, while a
    /// parent that only leaves the tree lets its popover go.
    #[test]
    fn popover_survives_parent_reparent() {
        big_testkit::harness::require_gtk_test(|| {
            let first = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let second = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.append(&first);
            row.append(&second);
            let window = gtk::Window::new();
            window.set_child(Some(&row));
            let button = gtk::Button::new();
            first.append(&button);
            let popover = gtk::Popover::new();
            popover.set_parent(&button);
            unparent_popover_on_parent_teardown(button.upcast_ref(), &popover);
            window.present();
            assert!(button.is_realized());

            button.unparent();
            second.append(&button);
            assert_eq!(popover.parent().as_ref(), Some(button.upcast_ref()));

            let detached_parent = gtk::Button::new();
            let detached = gtk::Popover::new();
            detached.set_parent(&detached_parent);
            unparent_popover_on_parent_teardown(detached_parent.upcast_ref(), &detached);
            second.append(&detached_parent);
            second.remove(&detached_parent);
            assert!(detached.parent().is_none());
            window.destroy();
        });
    }
}
