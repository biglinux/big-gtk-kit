// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The question before an act: Cancel or the act.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use big_gtk_kit::alert::Alert;
use gtk::glib;

/// Ask over `parent` before an act that cannot be taken back; `answered` is
/// told once whether `verb` was chosen. Cancel is the default and the close
/// response, so Enter and Escape keep everything; the verb is drawn destructive.
pub fn confirm_destructive(
    parent: Option<&impl IsA<gtk::Widget>>,
    heading: &str,
    body: &str,
    verb: &str,
    cancel: &str,
    answered: impl FnOnce(bool) + 'static,
) -> Alert {
    ask(
        parent,
        heading,
        body,
        verb,
        cancel,
        adw::ResponseAppearance::Destructive,
        None,
        answered,
    )
}

/// [`confirm_destructive`] for an act that can be undone: the verb is the
/// suggested answer, which Enter gives; Escape still cancels.
pub fn confirm_suggested(
    parent: Option<&impl IsA<gtk::Widget>>,
    heading: &str,
    body: &str,
    verb: &str,
    cancel: &str,
    answered: impl FnOnce(bool) + 'static,
) -> Alert {
    ask(
        parent,
        heading,
        body,
        verb,
        cancel,
        adw::ResponseAppearance::Suggested,
        None,
        answered,
    )
}

/// The verb drawn as `appearance`; only a suggested verb is the default answer.
/// `extra` goes under the body.
pub(super) fn ask(
    parent: Option<&impl IsA<gtk::Widget>>,
    heading: &str,
    body: &str,
    verb: &str,
    cancel: &str,
    appearance: adw::ResponseAppearance,
    extra: Option<&gtk::Widget>,
    answered: impl FnOnce(bool) + 'static,
) -> Alert {
    let alert = Alert::new(parent, Some(heading), Some(body));
    alert.set_extra_child(extra);
    alert.add_responses(&[("cancel", cancel), ("go", verb)]);
    #[expect(clippy::disallowed_methods, reason = "the owner")]
    alert.set_response_appearance("go", appearance);
    let suggested = appearance == adw::ResponseAppearance::Suggested;
    alert.set_default_response(Some(if suggested { "go" } else { "cancel" }));
    alert.set_close_response("cancel");
    focus_default_on_map(&alert);
    alert.choose(move |response| answered(response == "go"));
    alert
}

/// Give the field set as the dialog's focus, else the default answer, the
/// keyboard when it maps: libadwaita focuses a destructive answer over the
/// default on the first map, and gives a reused dialog's field none. The handler
/// goes with the first map: the dialog is pooled and asks other questions after
/// this one.
pub(super) fn focus_default_on_map(alert: &Alert) {
    let focus_default = Rc::new(Cell::new(None::<glib::SignalHandlerId>));
    let handler = alert.dialog().connect_map({
        let focus_default = Rc::clone(&focus_default);
        move |dialog| {
            let field = dialog.focus().filter(|focus| !focus.is::<gtk::Button>());
            if let Some(target) = field.or_else(|| dialog.default_widget()) {
                target.grab_focus();
            }
            if let Some(handler) = focus_default.take() {
                dialog.disconnect(handler);
            }
        }
    });
    focus_default.set(Some(handler));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_keeps_the_target() {
        big_testkit::harness::require_gtk_test(|| {
            let alert = confirm_destructive(
                None::<&gtk::Widget>,
                "Delete “a”?",
                "",
                "Delete",
                "Cancel",
                |_| {},
            );
            let dialog = alert.dialog();
            assert_eq!(dialog.default_response().as_deref(), Some("cancel"));
            assert_eq!(
                dialog.response_appearance("go"),
                adw::ResponseAppearance::Destructive
            );
        });
    }

    #[test]
    fn suggested_act_is_the_default() {
        big_testkit::harness::require_gtk_test(|| {
            let alert = confirm_suggested(
                None::<&gtk::Widget>,
                "Run “a”?",
                "",
                "Run",
                "Cancel",
                |_| {},
            );
            let dialog = alert.dialog();
            assert_eq!(dialog.default_response().as_deref(), Some("go"));
            assert_eq!(dialog.close_response().as_str(), "cancel");
            assert_eq!(
                dialog.response_appearance("go"),
                adw::ResponseAppearance::Suggested
            );
        });
    }

    /// Tab cycles Cancel, the verb, Cancel, and never leaves the dialog without
    /// a focused widget.
    #[test]
    fn tab_cycles_between_the_buttons() {
        big_testkit::harness::require_gtk_test(|| {
            let window = adw::Window::new();
            window.present();
            let alert = confirm_destructive(
                Some(&window),
                "Empty the Trash?",
                "Everything in it is deleted.",
                "Empty Trash",
                "Cancel",
                |_| {},
            );
            let dialog = alert.dialog();
            assert!(big_testkit::harness::pump_until(|| dialog.is_mapped()));
            let label = || {
                GtkWindowExt::focus(&window)
                    .and_downcast::<gtk::Button>()
                    .and_then(|button| button.label())
                    .map(|label| label.to_string())
            };
            // The sheet takes focus while it opens, and its buttons take none
            // until it has: a Tab (or a read) before then finds nothing, most
            // often on a loaded display. Each step waits for its focus.
            let moved_from = |previous: Option<String>| {
                assert!(big_testkit::harness::pump_until(|| {
                    let now = label();
                    now.is_some() && now != previous
                }));
                label()
            };
            let mut seen = vec![moved_from(None)];
            for _ in 0..3 {
                window.emit_by_name::<()>("move-focus", &[&gtk::DirectionType::TabForward]);
                seen.push(moved_from(seen.last().cloned().flatten()));
            }
            let names = ["Cancel", "Empty Trash", "Cancel", "Empty Trash"];
            assert_eq!(seen, names.map(|name| Some(name.to_string())));
        });
    }
}
