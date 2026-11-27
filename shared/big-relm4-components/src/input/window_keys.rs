// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The keys a window answers on its own behalf.

use gtk::glib;
use gtk::prelude::*;

/// Close `window` when Escape is pressed anywhere in it, except in a label
/// being renamed, where Escape keeps the old name.
///
/// Capture phase, so it works while a search entry has focus: an entry
/// otherwise eats Escape to clear itself and the window never hears it.
///
/// The controller holds the window weakly. Capturing it strongly in a closure
/// held by a controller added to that same window makes
/// `window → controller → closure → window`: a cycle GTK cannot break, so the
/// window's whole widget tree would stay for the session, once per open.
pub fn close_on_escape(window: &impl IsA<gtk::Window>) {
    back_or_close_on_escape(window, || false);
}

/// Close `window` on `trigger`, a [`gtk::ShortcutTrigger`] string such as
/// `"<Control>w|<Control>q"`, from anywhere in it once the focused control has
/// let the key pass.
pub fn close_on_shortcut(window: &impl IsA<gtk::Window>, trigger: &str) {
    let keys = gtk::ShortcutController::new();
    keys.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string(trigger),
        Some(gtk::NamedAction::new("window.close")),
    ));
    window.as_ref().add_controller(keys);
}

/// [`close_on_escape`] for a window with pages: Escape first asks `back` to go back
/// one page, and closes the window only when `back` answers there is nowhere to go.
pub fn back_or_close_on_escape(window: &impl IsA<gtk::Window>, back: impl Fn() -> bool + 'static) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let closing = window.as_ref().downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key != gtk::gdk::Key::Escape {
            return glib::Propagation::Proceed;
        }
        let Some(window) = closing.upgrade() else {
            return glib::Propagation::Proceed;
        };
        // A label being renamed takes its Escape back first: it means "keep the
        // old name", and closing the window under it would lose the question. An
        // open dialog takes it too, to close itself rather than the page under it.
        let focus = gtk::prelude::GtkWindowExt::focus(&window);
        let renaming = focus
            .as_ref()
            .and_then(|focus| focus.ancestor(gtk::EditableLabel::static_type()))
            .and_downcast::<gtk::EditableLabel>()
            .is_some_and(|label| label.is_editing());
        let in_dialog = focus
            .as_ref()
            .is_some_and(|focus| focus.ancestor(adw::Dialog::static_type()).is_some());
        if renaming || in_dialog {
            return glib::Propagation::Proceed;
        }
        if !back() {
            window.close();
        }
        glib::Propagation::Stop
    });
    window.as_ref().add_controller(keys);
}

/// Space activates the row that has the focus in `window` (a tick, a switch, a choice,
/// a sidebar entry), as GTK's own binding does. A search entry capturing the window's
/// keys (GTK 4.24) types the space into the search before that binding runs, which
/// filters the list and leaves the row as it was.
pub fn space_activates_rows(window: &impl IsA<gtk::Window>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let held = window.as_ref().downgrade();
    keys.connect_key_pressed(move |_, key, _, state| {
        let modified = state.intersects(
            gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::SHIFT_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK,
        );
        if modified || !matches!(key, gtk::gdk::Key::space | gtk::gdk::Key::KP_Space) {
            return glib::Propagation::Proceed;
        }
        match held
            .upgrade()
            .and_then(|window| gtk::prelude::GtkWindowExt::focus(&window))
            .and_downcast::<gtk::ListBoxRow>()
        {
            Some(row) if row.is_activatable() && row.activate() => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        }
    });
    window.as_ref().add_controller(keys);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::glib::translate::IntoGlib;

    /// Escape inside an open dialog is the dialog's: the page under it stays.
    #[test]
    fn dialog_keeps_its_escape() {
        big_testkit::harness::require_gtk_test(|| {
            let window = adw::Window::new();
            let went_back = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                let went_back = std::rc::Rc::clone(&went_back);
                back_or_close_on_escape(&window, move || {
                    went_back.set(true);
                    true
                });
            }
            window.present();
            let entry = gtk::Entry::new();
            let dialog = big_gtk_kit::dialog::Dialog::builder(Some(&window))
                .child(&entry)
                .build();
            dialog.present();
            assert!(entry.grab_focus(), "the dialog's field takes the focus");
            let keys = window
                .observe_controllers()
                .iter::<glib::Object>()
                .filter_map(Result::ok)
                .filter_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
                .find(|keys| keys.propagation_phase() == gtk::PropagationPhase::Capture)
                .expect("the window reads Escape first");
            let taken: bool = keys.emit_by_name(
                "key-pressed",
                &[
                    &gtk::gdk::Key::Escape.into_glib(),
                    &0u32,
                    &gtk::gdk::ModifierType::empty(),
                ],
            );
            assert!(
                !taken && !went_back.get(),
                "Escape went to the page under the dialog"
            );
            window.destroy();
        });
    }
}
