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
    let leaks = BINS_LEAK_BREAKPOINTS.with(|leak| *leak);
    // The handler libadwaita connects is the one `notify::condition` handler
    // left unblocked once those already there are blocked: the bin it goes to
    // may not be in `owner`'s tree yet (an `AdwDialog` not presented).
    let earlier = if leaks {
        block_condition_handlers(&breakpoint)
    } else {
        Vec::new()
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to add them"
    )]
    let added = if let Some(window) = owner.downcast_ref::<adw::ApplicationWindow>() {
        window.add_breakpoint(breakpoint.clone());
        true
    } else if let Some(window) = owner.downcast_ref::<adw::Window>() {
        window.add_breakpoint(breakpoint.clone());
        true
    } else if let Some(dialog) = owner.downcast_ref::<adw::Dialog>() {
        dialog.add_breakpoint(breakpoint.clone());
        true
    } else if let Some(bin) = owner.downcast_ref::<adw::BreakpointBin>() {
        bin.add_breakpoint(breakpoint.clone());
        true
    } else {
        false
    };
    let bins_handler = if added && leaks {
        condition_handler(&breakpoint)
    } else {
        0
    };
    for handler in earlier {
        // SAFETY: `breakpoint` is alive and `handler` one of its handlers this
        // function blocked.
        unsafe { glib::gobject_ffi::g_signal_handler_unblock(breakpoint.as_ptr().cast(), handler) };
    }
    if !(added && leaks) {
        return;
    }
    drop(breakpoint);
    owner.connect_destroy(move |_| {
        if let Some(breakpoint) = weak.upgrade()
            && bins_handler != 0
        {
            let instance = breakpoint.as_ptr().cast();
            // SAFETY: `breakpoint` is alive; the id is checked before it is used.
            unsafe {
                if glib::gobject_ffi::g_signal_handler_is_connected(instance, bins_handler) != 0 {
                    glib::gobject_ffi::g_signal_handler_disconnect(instance, bins_handler);
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

/// The first unblocked `notify::condition` handler of `breakpoint`, 0 for none.
fn condition_handler(breakpoint: &adw::Breakpoint) -> glib::ffi::gulong {
    // SAFETY: `breakpoint` is alive; GLib only compares the signal and detail.
    unsafe {
        glib::gobject_ffi::g_signal_handler_find(
            breakpoint.as_ptr().cast(),
            glib::gobject_ffi::G_SIGNAL_MATCH_ID
                | glib::gobject_ffi::G_SIGNAL_MATCH_DETAIL
                | glib::gobject_ffi::G_SIGNAL_MATCH_UNBLOCKED,
            glib::gobject_ffi::g_signal_lookup(
                c"notify".as_ptr(),
                glib::gobject_ffi::g_object_get_type(),
            ),
            glib::ffi::g_quark_from_static_string(c"condition".as_ptr()),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

/// Block every `notify::condition` handler `breakpoint` has, returning them.
fn block_condition_handlers(breakpoint: &adw::Breakpoint) -> Vec<glib::ffi::gulong> {
    let mut blocked = Vec::new();
    loop {
        let handler = condition_handler(breakpoint);
        if handler == 0 {
            return blocked;
        }
        // SAFETY: `breakpoint` is alive and `handler` one of its handlers.
        unsafe { glib::gobject_ffi::g_signal_handler_block(breakpoint.as_ptr().cast(), handler) };
        blocked.push(handler);
    }
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

    /// A kept breakpoint keeps no handler aimed at its owner's freed bin, from
    /// a window, a dialog never presented and one presented after the add (its
    /// bin is out of the tree until then).
    #[gtk::test]
    #[ignore = "needs a display"]
    #[expect(
        clippy::disallowed_methods,
        reason = "a plain AdwDialog is the owner under test"
    )]
    fn kept_breakpoint_leaves_no_handler() {
        init();
        let parent = adw::Window::new();
        parent.present();
        let condition = || adw::BreakpointCondition::parse("max-width: 500sp").expect("valid");
        for owner in ["window", "unpresented dialog", "dialog"] {
            let kept = adw::Breakpoint::new(condition());
            match owner {
                "window" => {
                    let window = adw::Window::new();
                    add(&window, kept.clone());
                    window.destroy();
                }
                "unpresented dialog" => add(&adw::Dialog::new(), kept.clone()),
                _ => {
                    let dialog = adw::Dialog::new();
                    add(&dialog, kept.clone());
                    dialog.present(Some(&parent));
                    pump();
                    dialog.force_close();
                    // The host lets go of a closed sheet on a later turn.
                    let gone = dialog.downgrade();
                    drop(dialog);
                    for _ in 0..200 {
                        pump();
                        if gone.upgrade().is_none() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    assert!(gone.upgrade().is_none(), "the closed dialog went");
                }
            }
            pump();
            assert_eq!(
                condition_handler(&kept),
                0,
                "{owner}: a notify::condition handler aimed at the freed bin"
            );
        }
        parent.destroy();
    }
}
