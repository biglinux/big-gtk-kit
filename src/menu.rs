// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Menu sections made once per process.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::gio;

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
    let section =
        SECTIONS.with_borrow_mut(|sections| sections.entry(key).or_insert_with(build).clone());
    #[allow(
        clippy::disallowed_methods,
        reason = "the one place allowed to add sections"
    )]
    menu.append_section(label, &section);
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
    #[test]
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
}
