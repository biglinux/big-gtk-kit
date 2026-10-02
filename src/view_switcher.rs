// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A segmented control over a `GtkStack`, without what `AdwInlineViewSwitcher` and
//! `AdwViewStack` leak.
//!
//! libadwaita 1.9's `AdwInlineViewSwitcher` gives every page's `AdwToggle` a custom
//! child, and `AdwToggle` takes that child with `g_object_ref_sink` and never lets it
//! go: its finalize frees the strings and nothing else. Every switcher built therefore
//! kept one small widget tree per page for the rest of the session — measured on the
//! desktop as 24 widgets for every opening of the calendar card, which is rebuilt each
//! time it opens. A toggle with a label or an icon and no child keeps nothing.
//!
//! A `GtkStack` rather than an `AdwViewStack` for the same reason: `AdwViewStack`
//! creates its transition animation at init and never releases it.

use adw::prelude::*;

/// Show what `stack`'s pages are called, as text (`icons == false`) or as icons with
/// their titles as tooltips and accessible names, and switch between them.
///
/// The pages are read once: add every page to `stack` first.
#[must_use]
pub fn toggles_for(stack: &gtk::Stack, icons: bool) -> adw::ToggleGroup {
    let group = adw::ToggleGroup::new();
    let pages = stack.pages();
    for position in 0..pages.n_items() {
        let Some(page) = pages.item(position).and_downcast::<gtk::StackPage>() else {
            continue;
        };
        let Some(name) = page.name() else {
            continue;
        };
        let title = page.title().unwrap_or_default();
        let toggle = if icons {
            adw::Toggle::builder()
                .name(name.as_str())
                .icon_name(page.icon_name().unwrap_or_default().as_str())
                .tooltip(title.as_str())
                .build()
        } else {
            adw::Toggle::builder()
                .name(name.as_str())
                .label(title.as_str())
                .build()
        };
        group.add(toggle);
    }
    stack
        .bind_property("visible-child-name", &group, "active-name")
        .bidirectional()
        .sync_create()
        .build();
    group
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{init, pump, watch};

    #[test]
    #[ignore = "needs a display"]
    fn switchers_and_their_pages_finalize() {
        init();
        let finalized = Rc::new(Cell::new(0));
        for _ in 0..25 {
            let stack = gtk::Stack::new();
            for name in ["one", "two", "three"] {
                let page = gtk::Label::new(Some(name));
                watch(&page, &finalized);
                stack.add_titled(&page, Some(name), name);
            }
            let group = toggles_for(&stack, false);
            group.set_active_name(Some("two"));
            assert_eq!(stack.visible_child_name().as_deref(), Some("two"));
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&group);
            content.append(&stack);
            let window = gtk::Window::builder().child(&content).build();
            window.present();
            pump();
            drop((stack, group, content));
            window.destroy();
            drop(window);
            pump();
        }
        assert_eq!(finalized.get(), 75);
    }
}
