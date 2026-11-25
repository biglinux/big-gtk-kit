// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The question answered with one line of text: a name, a pattern, a command.

use adw::prelude::*;
use big_gtk_kit::alert::Alert;

const ACCEPT: &str = "accept";

/// The words of a question answered with one line of text, each already
/// translated. `body` and `placeholder` may be empty.
pub struct TextQuestion<'a> {
    /// What is asked.
    pub heading: &'a str,
    /// What the answer is for; empty shows none.
    pub body: &'a str,
    /// The field's accessible name.
    pub field: &'a str,
    /// The field's text when the question opens.
    pub text: &'a str,
    /// What the empty field shows; empty shows nothing.
    pub placeholder: &'a str,
    /// The most characters the field takes; 0 takes any number.
    pub max_length: u16,
    /// The button that answers.
    pub accept: &'a str,
    /// The button that leaves things as they are.
    pub cancel: &'a str,
}

/// Ask `question` over `parent`. The answer is offered while `accepts` takes
/// the trimmed text; `answered` is told once: that text, or `None` for Cancel
/// and Escape.
pub fn ask_text(
    parent: Option<&impl IsA<gtk::Widget>>,
    question: TextQuestion<'_>,
    accepts: impl Fn(&str) -> bool + 'static,
    answered: impl FnOnce(Option<String>) + 'static,
) -> Alert {
    let entry = gtk::Entry::builder()
        .text(question.text)
        .placeholder_text(question.placeholder)
        .max_length(question.max_length.into())
        .activates_default(true)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label(question.field)]);
    let alert = Alert::new(parent, Some(question.heading), Some(question.body));
    alert.add_responses(&[("cancel", question.cancel), (ACCEPT, question.accept)]);
    #[expect(clippy::disallowed_methods, reason = "the owner")]
    alert.set_response_appearance(ACCEPT, adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some(ACCEPT));
    alert.set_close_response("cancel");
    alert.set_extra_child(Some(&entry));
    alert.set_focus(Some(&entry));
    alert.set_response_enabled(ACCEPT, accepts(question.text.trim()));
    entry.connect_changed({
        let alert = alert.downgrade();
        move |entry| {
            if let Some(alert) = alert.upgrade() {
                alert.set_response_enabled(ACCEPT, accepts(entry.text().trim()));
            }
        }
    });
    super::confirm::focus_default_on_map(&alert);
    alert.choose(move |response| {
        answered((response == ACCEPT).then(|| entry.text().trim().to_owned()));
    });
    alert
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// A question about "notes", its alert and its field.
    fn asked(answered: impl FnOnce(Option<String>) + 'static) -> (adw::AlertDialog, gtk::Entry) {
        let question = TextQuestion {
            heading: "Rename",
            body: "",
            field: "New name",
            text: "notes",
            placeholder: "",
            max_length: 0,
            accept: "Rename",
            cancel: "Cancel",
        };
        let alert = ask_text(
            None::<&gtk::Widget>,
            question,
            |text| text != "notes",
            answered,
        );
        let dialog = alert.dialog().clone();
        let entry = dialog
            .extra_child()
            .and_downcast::<gtk::Entry>()
            .expect("the field");
        (dialog, entry)
    }

    #[test]
    fn refused_text_holds_the_answer() {
        big_testkit::harness::require_gtk_test(|| {
            let (dialog, entry) = asked(|_| {});
            assert!(
                !dialog.is_response_enabled(ACCEPT),
                "the name it has is no answer"
            );
            entry.set_text("  draft  ");
            assert!(dialog.is_response_enabled(ACCEPT));
            entry.set_text(" notes ");
            assert!(!dialog.is_response_enabled(ACCEPT));
        });
    }

    #[test]
    fn field_has_the_keyboard_each_time() {
        big_testkit::harness::require_gtk_test(|| {
            let window = adw::Window::new();
            window.present();
            for round in 0..2 {
                let question = TextQuestion {
                    heading: "Rename",
                    body: "",
                    field: "New name",
                    text: "notes",
                    placeholder: "",
                    max_length: 0,
                    accept: "Rename",
                    cancel: "Cancel",
                };
                let alert = ask_text(Some(&window), question, |_| true, |_| {});
                let dialog = alert.dialog().clone();
                let entry = dialog.extra_child().expect("the field");
                assert!(
                    big_testkit::harness::pump_until(|| GtkWindowExt::focus(&window)
                        .is_some_and(|focus| focus == entry || focus.is_ancestor(&entry))),
                    "round {round}: the field lacks the keyboard"
                );
                dialog.close();
                assert!(big_testkit::harness::pump_until(|| dialog
                    .parent()
                    .is_none()));
            }
        });
    }

    #[test]
    fn answer_is_trimmed_text() {
        big_testkit::harness::require_gtk_test(|| {
            for (response, expected) in [(ACCEPT, Some("draft")), ("cancel", None)] {
                let got = Rc::new(RefCell::new(None));
                let (dialog, entry) = asked({
                    let got = Rc::clone(&got);
                    move |answer| *got.borrow_mut() = Some(answer)
                });
                entry.set_text("  draft ");
                dialog.emit_by_name::<()>("response", &[&response]);
                assert_eq!(
                    *got.borrow(),
                    Some(expected.map(str::to_owned)),
                    "{response}"
                );
            }
        });
    }
}
