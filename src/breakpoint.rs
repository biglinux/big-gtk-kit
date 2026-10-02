// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Breakpoints that go when their window or dialog goes.

use adw::prelude::*;
use gtk::glib;

/// Add `breakpoint` to `owner` — an `AdwWindow`, `AdwApplicationWindow`,
/// `AdwDialog` or `AdwBreakpointBin` — and let go of it when `owner` is
/// destroyed.
///
/// libadwaita 1.9 keeps a window's breakpoints in a `GPtrArray` made without a
/// free function (`adw_breakpoint_bin_init`), so the reference `add_breakpoint`
/// takes is never dropped: each window or dialog closed left its breakpoints
/// behind. Once `owner` has gone, the breakpoint is released if that orphaned
/// reference is the only one left; a libadwaita that fixes this has already
/// finalized it, and one the program still holds is left alone.
///
/// Breakpoints in a `.ui` file, and the three an adaptive `AdwDialog` makes
/// for itself, cannot be reached from here.
pub fn add(owner: &impl IsA<gtk::Widget>, breakpoint: adw::Breakpoint) {
    let owner = owner.as_ref();
    let weak = breakpoint.downgrade();
    #[allow(
        clippy::disallowed_methods,
        reason = "the one place allowed to add them"
    )]
    if let Some(window) = owner.downcast_ref::<adw::ApplicationWindow>() {
        window.add_breakpoint(breakpoint);
    } else if let Some(window) = owner.downcast_ref::<adw::Window>() {
        window.add_breakpoint(breakpoint);
    } else if let Some(dialog) = owner.downcast_ref::<adw::Dialog>() {
        dialog.add_breakpoint(breakpoint);
    } else if let Some(bin) = owner.downcast_ref::<adw::BreakpointBin>() {
        bin.add_breakpoint(breakpoint);
    } else {
        return;
    }
    owner.connect_destroy(move |_| {
        let weak = weak.clone();
        // After the owner's children, and with them its breakpoint bin, are gone.
        glib::idle_add_local_once(move || {
            // The orphaned reference and the one `upgrade` just took.
            if let Some(breakpoint) = weak.upgrade().filter(|it| it.ref_count() == 2) {
                // SAFETY: drops only the reference libadwaita took and lost;
                // `breakpoint` keeps its own until the end of this scope.
                unsafe { glib::gobject_ffi::g_object_unref(breakpoint.as_ptr().cast()) };
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{init, pump, watch};

    #[test]
    #[ignore = "needs a display"]
    fn breakpoints_finalize_with_their_windows() {
        init();
        let finalized = Rc::new(Cell::new(0));
        for _ in 0..25 {
            let window = adw::Window::new();
            let condition = adw::BreakpointCondition::parse("max-width: 500sp").expect("valid");
            let breakpoint = adw::Breakpoint::new(condition);
            watch(&breakpoint, &finalized);
            add(&window, breakpoint);
            window.present();
            pump();
            window.destroy();
            drop(window);
            pump();
        }
        assert_eq!(finalized.get(), 25);
    }

    #[test]
    #[ignore = "needs a display"]
    fn a_breakpoint_the_program_keeps_stays() {
        init();
        let finalized = Rc::new(Cell::new(0));
        let window = adw::Window::new();
        let condition = adw::BreakpointCondition::parse("max-width: 500sp").expect("valid");
        let kept = adw::Breakpoint::new(condition);
        watch(&kept, &finalized);
        add(&window, kept.clone());
        window.destroy();
        drop(window);
        pump();
        assert_eq!(finalized.get(), 0, "freed under the program's reference");
        drop(kept);
    }
}
