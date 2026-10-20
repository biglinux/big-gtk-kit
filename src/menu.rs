// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Menu sections made once per process.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::gio;
use gtk::gio::prelude::*;

thread_local! {
    static SECTIONS: RefCell<HashMap<&'static str, gio::Menu>> = RefCell::default();
}

/// Append to `menu` the section `build` makes the first time `key` is asked
/// for, and the same section every time after.
///
/// GTK 4.22's menu tracker takes a reference to a section it shows unmerged
/// and never releases it (`gtk_menu_tracker_add_items`), so a section built
/// per window stayed alive per window: three `GMenu` for every video player
/// opened and closed in a long-lived host. Shared, the lost references pile up
/// on one object and cost nothing more. The section must read the same in
/// every menu that shows it; its actions still resolve per widget.
pub fn append_shared_section(
    menu: &gio::Menu,
    label: Option<&str>,
    key: &'static str,
    build: impl FnOnce() -> gio::Menu,
) {
    // `build` runs outside the borrow: it may append shared sections itself.
    let section = SECTIONS
        .with_borrow(|sections| sections.get(key).cloned())
        .unwrap_or_else(|| {
            let section = build();
            SECTIONS.with_borrow_mut(|sections| sections.entry(key).or_insert(section).clone())
        });
    #[allow(
        clippy::disallowed_methods,
        reason = "the one place allowed to add sections"
    )]
    menu.append_section(label, &section);
}

/// Append to `menu` the section kept under `key`, emptied and refilled by
/// `fill`; an empty section is left out.
///
/// For sections whose items change between builds, such as a context menu
/// made per click: the lost references then pile up on the one section kept
/// per key instead of on a new one each build. Refilling also changes the
/// section in any other live menu showing `key`, so use it only where each
/// menu is rebuilt before it is shown.
pub fn append_refilled_section(
    menu: &gio::Menu,
    label: Option<&str>,
    key: &'static str,
    fill: impl FnOnce(&gio::Menu),
) {
    let section = SECTIONS.with_borrow_mut(|sections| sections.entry(key).or_default().clone());
    section.remove_all();
    fill(&section);
    if section.n_items() > 0 {
        #[allow(
            clippy::disallowed_methods,
            reason = "the one place allowed to add sections"
        )]
        menu.append_section(label, &section);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gtk::prelude::*;

    use super::*;
    use crate::testing::{pump, watch};

    /// One section for every window's menu; each window's own menu and
    /// popover still finalize.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn a_shared_section_is_built_once() {
        gtk::init().expect("a display");
        let (built, finalized) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        for _ in 0..25 {
            let menu = gio::Menu::new();
            let count = built.clone();
            append_shared_section(&menu, None, "big-gtk-kit-test", || {
                count.set(count.get() + 1);
                let section = gio::Menu::new();
                section.append(Some("Open"), Some("app.open"));
                section
            });
            watch(&menu, &finalized);
            let button = gtk::MenuButton::builder().menu_model(&menu).build();
            let window = gtk::Window::builder().child(&button).build();
            window.present();
            pump();
            button.popup();
            pump();
            button.popdown();
            drop((menu, button));
            window.destroy();
            drop(window);
            pump();
        }
        assert_eq!((built.get(), finalized.get()), (1, 25));
    }

    /// A section built from shared sections of its own: building must not
    /// hold the table the inner sections are kept in.
    #[test]
    fn shared_sections_nest() {
        let menu = gio::Menu::new();
        append_shared_section(&menu, None, "big-gtk-kit-outer", || {
            let outer = gio::Menu::new();
            append_shared_section(&outer, None, "big-gtk-kit-inner", gio::Menu::new);
            outer
        });
        assert_eq!(menu.n_items(), 1);
    }

    /// A context menu rebuilt with 0, 5 and 25 items keeps one section, and
    /// every menu that showed it finalizes.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn a_refilled_section_is_kept_once() {
        gtk::init().expect("a display");
        let finalized = Rc::new(Cell::new(0));
        let mut sections = Vec::new();
        for size in [0, 5, 25].repeat(25) {
            let menu = gio::Menu::new();
            append_refilled_section(&menu, None, "big-gtk-kit-refill-test", |section| {
                for item in 0..size {
                    section.append(Some(&item.to_string()), Some("app.open"));
                }
                sections.push(section.clone());
            });
            assert_eq!(menu.n_items(), i32::from(size > 0));
            watch(&menu, &finalized);
            let button = gtk::MenuButton::builder().menu_model(&menu).build();
            let window = gtk::Window::builder().child(&button).build();
            window.present();
            pump();
            button.popup();
            pump();
            button.popdown();
            drop((menu, button));
            window.destroy();
            drop(window);
            pump();
        }
        assert!(sections.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(finalized.get(), 75);
    }
}
