// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Breakpoints that go when their window or dialog goes.

use adw::prelude::*;
use gtk::glib;

/// Add `breakpoint` to `owner` (an `AdwWindow`, `AdwApplicationWindow`,
/// `AdwDialog` or `AdwBreakpointBin`) and let go of it when `owner` is
/// destroyed.
///
/// libadwaita up to 1.10 keeps a window's breakpoints in a `GPtrArray` made
/// without a free function (`adw_breakpoint_bin_init`), so the reference
/// `add_breakpoint` takes is never dropped, and leaves each breakpoint's
/// `notify::condition` handler aimed at the freed bin. Under such a libadwaita,
/// once `owner` has gone the breakpoint loses that handler, and the orphaned
/// reference too if it is the only one left: one the program still holds is
/// left alone.
///
/// Breakpoints in a `.ui` file, and the three an adaptive `AdwDialog` makes
/// for itself, cannot be reached from here.
pub fn add(owner: &impl IsA<gtk::Widget>, breakpoint: adw::Breakpoint) {
    let owner = owner.as_ref();
    let weak = breakpoint.downgrade();
    #[expect(
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
    if !BINS_LEAK_BREAKPOINTS.with(|leak| *leak) {
        return;
    }
    // The bin the breakpoint went to is one of these. Only their addresses are
    // kept: the handler's data is matched, never read, after the bins are freed.
    let bins: Vec<glib::ffi::gpointer> = crate::widget_tree::subtree(owner)
        .filter(|widget| widget.is::<adw::BreakpointBin>())
        .map(|bin| bin.as_ptr().cast())
        .collect();
    owner.connect_destroy(move |_| {
        if let Some(breakpoint) = weak.upgrade() {
            for &bin in &bins {
                // SAFETY: `breakpoint` is alive, and GLib compares `bin` with each
                // handler's data without following it.
                unsafe {
                    glib::gobject_ffi::g_signal_handlers_disconnect_matched(
                        breakpoint.as_ptr().cast(),
                        glib::gobject_ffi::G_SIGNAL_MATCH_DATA,
                        0,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        bin,
                    );
                }
            }
        }
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

thread_local! {
    /// Whether this libadwaita keeps the breakpoints of a disposed bin, measured
    /// once on a throwaway bin: a release that frees them would have the orphan
    /// unreference above drop the program's own reference instead.
    static BINS_LEAK_BREAKPOINTS: bool = {
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            0.0,
            adw::LengthUnit::Px,
        ));
        let bin = adw::BreakpointBin::new();
        #[expect(
            clippy::disallowed_methods,
            reason = "the probe of what add works around"
        )]
        bin.add_breakpoint(breakpoint.clone());
        drop(bin);
        let leak = breakpoint.ref_count() == 2;
        if leak {
            // SAFETY: drops the reference the disposed bin took and lost;
            // `breakpoint` keeps its own.
            unsafe { glib::gobject_ffi::g_object_unref(breakpoint.as_ptr().cast()) };
        }
        leak
    };
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gtk::glib::translate::IntoGlib;

    use super::*;
    use crate::testing::{init, pump, watch};

    #[gtk::test]
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

    #[gtk::test]
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

    #[gtk::test]
    #[ignore = "needs a display"]
    fn kept_breakpoint_leaves_no_handler() {
        init();
        let window = adw::Window::new();
        let condition = adw::BreakpointCondition::parse("max-width: 500sp").expect("valid");
        let kept = adw::Breakpoint::new(condition);
        add(&window, kept.clone());
        window.destroy();
        drop(window);
        pump();
        let notify =
            glib::subclass::signal::SignalId::lookup("notify", glib::Object::static_type())
                .expect("GObject's notify");
        // SAFETY: `kept` is alive; the lookup only reads its handler list.
        let handler = unsafe {
            glib::gobject_ffi::g_signal_handler_find(
                kept.as_ptr().cast(),
                glib::gobject_ffi::G_SIGNAL_MATCH_ID | glib::gobject_ffi::G_SIGNAL_MATCH_DETAIL,
                notify.into_glib(),
                glib::Quark::from_str("condition").into_glib(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(
            handler, 0,
            "a notify::condition handler aimed at the freed bin"
        );
    }
}
