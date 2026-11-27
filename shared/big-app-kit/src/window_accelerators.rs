// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The window in use, and the accelerators of products that share one process.

use gtk::prelude::*;

/// Call `activated` each time `window` becomes the window in use.
///
/// GTK's `is-active` follows the keyboard, which a window's own popover takes
/// as it opens: the window goes inactive and active again twice for one F10,
/// the keyboard still inside the menu. Those changes are left out. The
/// toplevel's `FOCUSED` state cannot stand in: GDK applies it on the window's
/// next frame, so a window covered by another one does not see it go and come
/// back.
pub fn connect_activated(
    window: &impl IsA<gtk::Window>,
    activated: impl Fn(&gtk::Window) + 'static,
) {
    window.connect_is_active_notify(move |window| {
        let window = window.upcast_ref::<gtk::Window>();
        let in_popover = GtkWindowExt::focus(window)
            .is_some_and(|focus| focus.ancestor(gtk::Popover::static_type()).is_some());
        if window.is_active() && !in_popover {
            activated(window);
        }
    });
}

/// Publish a product's accelerators now and again each time `window` becomes
/// the window in use ([`connect_activated`]).
///
/// An accelerator belongs to an action name for the whole application, and the
/// products in a shared host spell several `win.` names alike (`win.preferences`,
/// `win.quit`) with keys of their own. The last product to publish would take the
/// keys of every other one; republishing when its window is focused gives the
/// focused window its own product's keys.
pub fn publish_while_active(
    window: &impl IsA<gtk::Window>,
    app: &gtk::Application,
    publish: impl Fn(&gtk::Application) + 'static,
) {
    publish(app);
    let app = app.clone();
    connect_activated(window, move |_| publish(&app));
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn focused_window_takes_its_keys() {
        big_testkit::harness::require_gtk_test(|| {
            let app = gtk::Application::new(
                Some("br.com.biglinux.AcceleratorTest"),
                gtk::gio::ApplicationFlags::NON_UNIQUE,
            );
            app.register(gtk::gio::Cancellable::NONE).expect("register");
            let make = |publish: fn(&gtk::Application)| {
                let window = gtk::Window::new();
                window.set_application(Some(&app));
                publish_while_active(&window, &app, publish);
                window
            };
            let files =
                make(|app| app.set_accels_for_action("win.preferences", &["<Primary>comma"]));
            let terminal = make(|app| {
                app.set_accels_for_action("win.preferences", &["<Primary><Shift>comma"]);
            });
            let keys = || app.accels_for_action("win.preferences");

            files.present();
            assert!(big_testkit::harness::pump_until(
                || files.is_active() && keys() == ["<Control>comma"]
            ));
            terminal.present();
            assert!(big_testkit::harness::pump_until(
                || terminal.is_active() && keys() == ["<Shift><Control>comma"]
            ));
            files.present();
            assert!(big_testkit::harness::pump_until(
                || files.is_active() && keys() == ["<Control>comma"]
            ));
        });
    }

    #[test]
    fn own_popover_is_no_activation() {
        big_testkit::harness::require_gtk_test(|| {
            let window = gtk::Window::new();
            let button = gtk::MenuButton::new();
            let popover = gtk::Popover::new();
            popover.set_child(Some(&gtk::Button::with_label("Row")));
            button.set_popover(Some(&popover));
            window.set_child(Some(&button));
            let activations = Rc::new(Cell::new(0));
            let counted = Rc::clone(&activations);
            connect_activated(&window, move |_| counted.set(counted.get() + 1));
            let changes = Rc::new(Cell::new(0));
            let changed = Rc::clone(&changes);
            window.connect_is_active_notify(move |_| changed.set(changed.get() + 1));

            window.present();
            assert!(big_testkit::harness::pump_until(
                || window.is_active() && activations.get() == 1
            ));
            let before = changes.get();
            button.popup();
            // The keyboard goes to the popover and the window turns active again.
            assert!(big_testkit::harness::pump_until(|| changes.get()
                >= before + 2
                && window.is_active()));
            assert_eq!(activations.get(), 1);
            button.popdown();
        });
    }
}
