// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The question before leaving unsaved work.

use adw::prelude::*;
use big_gtk_kit::alert::Alert;

use super::confirm::focus_default_on_map;

/// What the person chose when asked whether to save before leaving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAnswer {
    /// Stay with the work as it is: Cancel, Escape or closing the question.
    Cancel,
    /// Stay, and show the edits not saved yet; only when
    /// [`SaveLabels::review`] offers it.
    Review,
    /// Leave and lose what was not saved.
    Discard,
    /// Save, then leave.
    Save,
}

/// The buttons of a save question, each already translated.
pub struct SaveLabels<'a> {
    /// Stay with the work as it is.
    pub cancel: &'a str,
    /// Show the changes before deciding; `None` offers no such button.
    pub review: Option<&'a str>,
    /// Leave and lose the changes; drawn destructive.
    pub discard: &'a str,
    /// Save, then leave; drawn as the suggested answer.
    pub save: &'a str,
}

/// Ask over `parent` whether to save before leaving. The buttons read Cancel,
/// the review when there is one, Discard and Save, in GNOME's order; Enter
/// saves and Escape cancels. `answered` is told once.
pub fn ask_to_save(
    parent: Option<&impl IsA<gtk::Widget>>,
    heading: &str,
    body: &str,
    labels: SaveLabels<'_>,
    answered: impl FnOnce(SaveAnswer) + 'static,
) -> Alert {
    let alert = Alert::new(parent, Some(heading), Some(body));
    alert.add_responses(&[("cancel", labels.cancel)]);
    if let Some(review) = labels.review {
        alert.add_responses(&[("review", review)]);
    }
    alert.add_responses(&[("discard", labels.discard), ("save", labels.save)]);
    #[expect(clippy::disallowed_methods, reason = "the owner")]
    alert.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    #[expect(clippy::disallowed_methods, reason = "the owner")]
    alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("save"));
    alert.set_close_response("cancel");
    focus_default_on_map(&alert);
    alert.choose(move |response| {
        answered(match response.as_str() {
            "review" => SaveAnswer::Review,
            "discard" => SaveAnswer::Discard,
            "save" => SaveAnswer::Save,
            _ => SaveAnswer::Cancel,
        });
    });
    alert
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn save_question_answers() {
        big_testkit::harness::require_gtk_test(|| {
            for (response, expected) in [
                ("save", SaveAnswer::Save),
                ("discard", SaveAnswer::Discard),
                ("cancel", SaveAnswer::Cancel),
                ("review", SaveAnswer::Review),
            ] {
                let got = Rc::new(Cell::new(None));
                let labels = SaveLabels {
                    cancel: "Cancel",
                    review: Some("View Changes"),
                    discard: "Discard",
                    save: "Save",
                };
                let alert = ask_to_save(None::<&gtk::Widget>, "Save?", "", labels, {
                    let got = Rc::clone(&got);
                    move |answer| got.set(Some(answer))
                });
                let dialog = alert.dialog();
                assert_eq!(dialog.default_response().as_deref(), Some("save"));
                assert_eq!(dialog.close_response().as_str(), "cancel");
                dialog.emit_by_name::<()>("response", &[&response]);
                assert_eq!(got.get(), Some(expected), "{response}");
            }
        });
    }
}
