// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Transient popovers that let go of themselves.

use gtk::prelude::*;

/// Unparent `popover` once it closes, so a menu made per click does not pin
/// itself and its children to the widget it was attached to.
///
/// GTK 4 does not unparent a popover attached with `set_parent` when that
/// widget is disposed: the parent finalizes "with children left" and the live
/// popover keeps its whole subtree, one per use, forever. Call this right
/// after `set_parent` on a transient popover; one owned by a struct should be
/// unparented in that owner's `Drop` instead.
pub fn auto_unparent_on_close(popover: &impl IsA<gtk::Popover>) {
    // Actions finish after `closed`; keep their ancestry until an idle, and the
    // parent alive with it, in case the action closes the parent's owner.
    popover.as_ref().connect_closed(move |popover| {
        let parent = popover.parent();
        let popover = popover.clone();
        gtk::glib::idle_add_local_once(move || {
            if popover.parent().is_some() {
                popover.unparent();
            }
            drop(parent);
        });
    });
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{pump, watch};

    #[test]
    #[ignore = "needs a display"]
    fn closing_keeps_the_parent_until_idle_then_finalizes() {
        gtk::init().expect("a display");
        let finalized = Rc::new(Cell::new(0));
        for cycle in 1..=25 {
            let parent = gtk::Button::new();
            let popover = gtk::Popover::new();
            popover.set_parent(&parent);
            watch(&popover, &finalized);
            auto_unparent_on_close(&popover);
            popover.emit_by_name::<()>("closed", &[]);
            assert!(
                popover.parent().is_some(),
                "the action still needs its parent"
            );
            drop((parent, popover));
            pump();
            assert_eq!(finalized.get(), cycle);
        }
    }
}
