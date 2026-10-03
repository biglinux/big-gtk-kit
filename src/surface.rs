// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Windows whose surface is released after a popover or tooltip showed on them.

use std::cell::Cell;

use gtk::prelude::*;
use gtk::{gdk, glib};

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// `bool` on a window or popover whose map this module watches.
const WATCHED_KEY: &str = "big-gtk-kit-surface-watched";
/// `ParentRef` on a popup surface whose parent this module releases.
const PARENT_KEY: &str = "big-gtk-kit-surface-parent";

/// Release the parent surface every Wayland popup keeps. Called by
/// [`crate::install`].
///
/// GDK's Wayland popup takes its parent with `g_value_dup_object`
/// (`gdkpopup-wayland.c`) and nothing ever drops that reference
/// (`gdk_surface_finalize` only unlinks the popup from the parent's children),
/// so a window that showed one popover, menu or tooltip kept its
/// `GdkWaylandToplevel` after it closed: 5 of 5 measured, against 0 for windows
/// that showed none. Measured on GTK 4.22.4 and still so on `main`; the
/// release is limited to those versions, so a GTK that fixes it is not
/// unreferenced twice.
pub(crate) fn install() {
    if INSTALLED.replace(true) || gtk::check_version(4, 22, 5).is_none() {
        return;
    }
    crate::on_every_emission("realize", |widget: gtk::Widget| {
        if !widget.is::<gtk::Native>() {
            return;
        }
        // SAFETY: WATCHED_KEY holds a `bool` (see its definition).
        if unsafe { widget.data::<bool>(WATCHED_KEY).is_some() } {
            return;
        }
        // SAFETY: as above.
        unsafe { widget.set_data(WATCHED_KEY, true) };
        // Each realize makes a new popup surface; the map after it has it.
        widget.connect_map(watch);
    });
}

fn watch(native: &gtk::Widget) {
    let Some(surface) = native
        .downcast_ref::<gtk::Native>()
        .and_then(NativeExt::surface)
    else {
        return;
    };
    // SAFETY: PARENT_KEY holds a `ParentRef` (see its definition).
    if surface.type_().name() != "GdkWaylandPopup"
        || unsafe { surface.data::<ParentRef>(PARENT_KEY).is_some() }
    {
        return;
    }
    let Some(parent) = surface
        .downcast_ref::<gdk::Popup>()
        .and_then(gdk::prelude::PopupExt::parent)
    else {
        return;
    };
    // SAFETY: as above. Dropped when the popup surface finalizes.
    unsafe { surface.set_data(PARENT_KEY, ParentRef(parent.downgrade())) };
}

/// Drops the popup's lost reference on its parent once the popup is gone.
struct ParentRef(glib::WeakRef<gdk::Surface>);

impl Drop for ParentRef {
    fn drop(&mut self) {
        // Runs as the popup finalizes, after GDK has unlinked it from the parent;
        // the lost reference keeps the parent alive until here.
        if let Some(parent) = self.0.upgrade() {
            // SAFETY: drops only the reference the popup took and never released;
            // `parent` keeps its own until the end of this scope.
            unsafe { glib::gobject_ffi::g_object_unref(parent.as_ptr().cast()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{pump, watch};

    #[gtk::test]
    #[ignore = "needs a display"]
    fn windows_that_showed_a_popover_release_their_surface() {
        gtk::init().expect("a display");
        install();
        let finalized = Rc::new(Cell::new(0));
        for _ in 0..25 {
            let popover = gtk::Popover::builder()
                .child(&gtk::Label::new(Some("x")))
                .build();
            let button = gtk::MenuButton::builder().popover(&popover).build();
            let window = gtk::Window::builder().child(&button).build();
            window.present();
            pump();
            watch(&window.surface().expect("realized"), &finalized);
            button.popup();
            pump();
            button.popdown();
            pump();
            drop((popover, button));
            window.destroy();
            drop(window);
            pump();
        }
        // GDK holds the last closed window's surface until another window
        // comes, popover or not: one more moves that reference along.
        let last = gtk::Window::new();
        last.present();
        pump();
        last.destroy();
        pump();
        assert_eq!(finalized.get(), 25);
    }
}
