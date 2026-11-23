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
/// The GTK 4 releases whose GDK keeps a popup's parent, as each minor series
/// and its last such micro release: 4.22.0 to 4.22.5, 4.23.0 to 4.23.4,
/// 4.24.0 to 4.24.1 and 4.25.0. The release runs on these only: on a release
/// not listed, a later micro one included, the surface leaks rather than risk
/// unreferencing it twice should that release drop the reference itself.
/// `gdk_keeps_the_parent_of_a_finalized_popup` expects the parent kept on a
/// listed release and released on any other; on a listed one that releases it,
/// it fails asking to take that release out, and on another that keeps it,
/// asking to add it.
const FAULTY_GTK: [(u32, u32); 4] = [(22, 5), (23, 4), (24, 1), (25, 0)];

/// Release the parent surface every Wayland popup keeps. Called by
/// [`crate::install`].
///
/// GDK's Wayland popup takes its parent with `g_value_dup_object`
/// (`gdkpopup-wayland.c`) and nothing ever drops that reference
/// (`gdk_surface_finalize` only unlinks the popup from the parent's children),
/// so a window that showed one popover, menu or tooltip kept its
/// `GdkWaylandToplevel` after it closed (GTK 4.22 through 4.25.0, still so on
/// `main`). The release runs only on the versions in [`FAULTY_GTK`].
pub(crate) fn install() {
    if INSTALLED.replace(true) || !faulty_gtk() {
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

/// Whether the running GTK is one of [`FAULTY_GTK`].
fn faulty_gtk() -> bool {
    let (minor, micro) = (gtk::minor_version(), gtk::micro_version());
    gtk::major_version() == 4
        && FAULTY_GTK
            .iter()
            .any(|&(series, last)| minor == series && micro <= last)
}

fn watch(native: &gtk::Widget) {
    let Some(surface) = native
        .downcast_ref::<gtk::Native>()
        .and_then(NativeExt::surface)
    else {
        return;
    };
    // SAFETY: PARENT_KEY holds a `ParentRef` (see its definition).
    let watched = unsafe { surface.data::<ParentRef>(PARENT_KEY).is_some() };
    if surface.type_().name() != "GdkWaylandPopup" || watched {
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
    fn popover_windows_release_surface() {
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
        assert_eq!(
            finalized.get(),
            25,
            "add this GTK to FAULTY_GTK if gdk_keeps_the_parent_of_a_finalized_popup asks for it"
        );
    }

    /// The defect itself, on the GTK under test: once its popup is finalized, a
    /// window that showed a popover still has its surface on a release in
    /// [`FAULTY_GTK`] and has lost it on any other, as a window that showed none
    /// has. A GTK on the wrong side of the list fails, naming the change to the
    /// list. The release of an earlier test's `install()` is taken off the
    /// popup first. On a listed GTK that drops the reference itself,
    /// `popover_windows_release_surface` unreferences twice and may abort the
    /// binary first: run this test alone (`cargo test gdk_keeps -- --ignored`).
    #[gtk::test]
    #[ignore = "needs a display"]
    fn gdk_keeps_the_parent_of_a_finalized_popup() {
        gtk::init().expect("a display");
        let without_popup = Rc::new(Cell::new(0));
        let with_popup = Rc::new(Cell::new(0));
        let popups = Rc::new(Cell::new(0));
        for (finalized, show) in [(&without_popup, false), (&with_popup, true)] {
            let popover = gtk::Popover::builder()
                .child(&gtk::Label::new(Some("x")))
                .build();
            let button = gtk::MenuButton::builder().popover(&popover).build();
            let window = gtk::Window::builder().child(&button).build();
            window.present();
            pump();
            watch(&window.surface().expect("realized"), finalized);
            if show {
                button.popup();
                pump();
                let popup = popover.surface().expect("a shown popover's surface");
                watch(&popup, &popups);
                // SAFETY: PARENT_KEY holds a `ParentRef`. Forgotten, it never
                // drops GDK's reference, as if the release were not installed.
                if let Some(release) = unsafe { popup.steal_data::<ParentRef>(PARENT_KEY) } {
                    std::mem::forget(release);
                }
                button.popdown();
                pump();
            }
            drop((popover, button));
            window.destroy();
            drop(window);
            pump();
            // GDK holds the last closed window's surface until another window
            // comes: one more moves that reference along.
            let last = gtk::Window::new();
            last.present();
            pump();
            last.destroy();
            pump();
        }
        assert_eq!(without_popup.get(), 1, "a window without a popup finalizes");
        assert_eq!(popups.get(), 1, "the popup surface finalizes");
        let version = format!(
            "{}.{}.{}",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version()
        );
        if faulty_gtk() {
            assert_eq!(
                with_popup.get(),
                0,
                "GTK {version} drops a popup's parent itself: take it out of FAULTY_GTK"
            );
        } else {
            assert_eq!(
                with_popup.get(),
                1,
                "GTK {version} keeps a popup's parent: add it to FAULTY_GTK"
            );
        }
    }
}
