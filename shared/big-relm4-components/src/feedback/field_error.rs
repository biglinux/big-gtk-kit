// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The line under a form field that says why its value is refused, or what is
//! worth knowing about it.
//!
//! Visible under the field it is about, read as an alert, tied to the field as
//! its description, with the field itself marked invalid. A tooltip or a line
//! below the whole form reaches neither the keyboard nor a screen reader in
//! the field.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use super::announce::announce;

/// A refusal line and the field it speaks for. Its clones are the same line.
#[derive(Clone)]
pub struct FieldError {
    field: gtk::glib::WeakRef<gtk::Widget>,
    line: gtk::Label,
    /// What else describes the field, refused or not, such as a hint under it:
    /// shared by the clones, so a hint added to one stays when another marks
    /// the field.
    hints: Rc<RefCell<Vec<gtk::Accessible>>>,
}

impl FieldError {
    /// A hidden line for `field`; the caller places [`Self::line`] right under
    /// the field, and clears it when the value is accepted again.
    #[must_use]
    pub fn under(field: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>)) -> Self {
        let line = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .css_classes(["error", "caption"])
            .accessible_role(gtk::AccessibleRole::Alert)
            .build();
        Self {
            field: field.upcast_ref::<gtk::Widget>().downgrade(),
            line,
            hints: Rc::default(),
        }
    }

    /// `hint` describes the field too, whether or not the line shows.
    #[must_use]
    pub fn also_described_by(self, hint: &impl IsA<gtk::Accessible>) -> Self {
        self.hints.borrow_mut().push(hint.clone().upcast());
        self.describe();
        self
    }

    /// The label to place under the field.
    #[must_use]
    pub fn line(&self) -> &gtk::Label {
        &self.line
    }

    /// The line in a list row of its own that shows while the line does, to add
    /// right after the field's row: a widget added to an `adw::PreferencesGroup`
    /// directly is drawn below the whole list, away from its field.
    #[must_use]
    pub fn list_row(&self) -> adw::PreferencesRow {
        let line = &self.line;
        line.set_margin_top(6);
        line.set_margin_bottom(6);
        line.set_margin_start(12);
        line.set_margin_end(12);
        let row = adw::PreferencesRow::builder()
            .activatable(false)
            .focusable(false)
            .child(line)
            .visible(line.get_visible())
            .build();
        line.bind_property("visible", &row, "visible").build();
        row
    }

    /// Refuse the value with `message`, said aloud when the line appears or
    /// its reason changes: a check run on every keystroke repeats nothing
    /// while the same reason holds.
    pub fn show(&self, message: &str) {
        self.say(message, true);
    }

    /// Say something worth knowing about a value that is accepted, such as a
    /// name that hides its item, without marking the field.
    pub fn note(&self, message: &str) {
        self.say(message, false);
    }

    /// Take the refusal or the note back.
    pub fn clear(&self) {
        if self.line.get_visible() {
            self.line.set_visible(false);
            self.mark(false);
        }
    }

    fn say(&self, message: &str, refused: bool) {
        let new_reason = !self.line.get_visible()
            || self.line.label() != message
            || self.line.has_css_class("error") != refused;
        self.line.set_label(message);
        self.line.set_visible(true);
        if refused {
            self.line.add_css_class("error");
            self.line.remove_css_class("dimmed");
        } else {
            self.line.add_css_class("dimmed");
            self.line.remove_css_class("error");
        }
        self.mark(refused);
        if new_reason {
            announce(
                &self.line,
                message,
                if refused {
                    gtk::AccessibleAnnouncementPriority::High
                } else {
                    gtk::AccessibleAnnouncementPriority::Medium
                },
            );
        }
    }

    fn mark(&self, invalid: bool) {
        let Some(field) = self.field.upgrade() else {
            return;
        };
        if invalid {
            field.add_css_class("error");
        } else {
            field.remove_css_class("error");
        }
        self.describe();
        // Reset rather than set to false: a valid field carries no invalid
        // state at all, which is what an assistive technology reads as valid.
        for marked in marked(&field) {
            if invalid {
                marked.update_state(&[gtk::accessible::State::Invalid(
                    gtk::AccessibleInvalidState::True,
                )]);
            } else {
                marked.reset_state(gtk::AccessibleState::Invalid);
            }
        }
    }

    /// Tie the field to the line while it shows, and to the hints: a hidden
    /// line left as a description is still read out as the reason.
    fn describe(&self) {
        let Some(field) = self.field.upgrade() else {
            return;
        };
        let line = self.line.get_visible().then(|| self.line.upcast_ref());
        let hints = self.hints.borrow();
        let targets: Vec<&gtk::Accessible> = line.into_iter().chain(hints.iter()).collect();
        for described in marked(&field) {
            if targets.is_empty() {
                described.reset_relation(gtk::AccessibleRelation::DescribedBy);
            } else {
                big_gtk_kit::accessible::relate(
                    &described,
                    gtk::AccessibleRelation::DescribedBy,
                    &targets,
                );
            }
        }
    }
}

/// `field`, and the child it is typed into when it has one: an `adw::EntryRow`
/// holds the keyboard in its text, which is the node a screen reader reads.
fn marked(field: &gtk::Widget) -> Vec<gtk::Widget> {
    let typed = field
        .dynamic_cast_ref::<gtk::Editable>()
        .and_then(|editable| editable.delegate())
        .and_then(|typed| typed.dynamic_cast::<gtk::Widget>().ok())
        .filter(|typed| typed != field);
    std::iter::once(field.clone()).chain(typed).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::entry_row;

    fn invalid(field: &impl IsA<gtk::Accessible>) -> bool {
        gtk::test_accessible_has_state(field, gtk::AccessibleState::Invalid)
    }

    /// A refused value marks its field and says why under it until it is
    /// cleared.
    #[test]
    fn refusal_marks_field_until_cleared() {
        big_testkit::harness::require_gtk_test(|| {
            let field = entry_row::build(adw::EntryRow::builder().title("Name"));
            let error = FieldError::under(&field);
            error.show("Needs a name");
            assert!(error.line().is_visible());
            assert!(field.has_css_class("error"));
            assert!(invalid(&field));
            assert_eq!(error.line().accessible_role(), gtk::AccessibleRole::Alert);
            field.set_text("x");
            assert!(error.line().is_visible());
            error.clear();
            assert!(!error.line().is_visible());
            assert!(!field.has_css_class("error"));
            assert!(!invalid(&field));
        });
    }

    /// A note under the field is shown without refusing the value, and a
    /// refusal after it marks the field.
    #[test]
    fn note_keeps_field_valid() {
        big_testkit::harness::require_gtk_test(|| {
            let field = gtk::Entry::new();
            let error = FieldError::under(&field);
            error.note("Hidden from the list");
            assert!(error.line().is_visible());
            assert!(error.line().has_css_class("dimmed"));
            assert!(!error.line().has_css_class("error"));
            assert!(!invalid(&field));
            error.show("Taken");
            assert!(error.line().has_css_class("error"));
            assert!(!error.line().has_css_class("dimmed"));
            assert!(invalid(&field));
            error.note("Hidden from the list");
            assert!(!invalid(&field));
        });
    }

    /// A row whose text is typed into a child marks that child too: it is what
    /// holds the keyboard, and what a screen reader reads.
    #[test]
    fn typed_child_is_marked() {
        big_testkit::harness::require_gtk_test(|| {
            let field = entry_row::build(adw::EntryRow::builder());
            let text = field.delegate().expect("an entry row types into a child");
            let error = FieldError::under(&field);
            error.show("Needs a name");
            assert!(invalid(&text));
            error.clear();
            assert!(!invalid(&text));
        });
    }

    /// A field describes itself by its line only while the line shows: a hidden
    /// line would still be read out as the reason.
    #[test]
    fn hidden_line_describes_nothing() {
        big_testkit::harness::require_gtk_test(|| {
            let described = |field: &gtk::Entry| {
                gtk::test_accessible_has_relation(field, gtk::AccessibleRelation::DescribedBy)
            };
            let field = gtk::Entry::new();
            let error = FieldError::under(&field);
            assert!(!described(&field));
            error.show("Taken");
            assert!(described(&field));
            error.clear();
            assert!(!described(&field));

            let hinted = gtk::Entry::new();
            let hint = gtk::Label::new(Some("Type a line number"));
            let error = FieldError::under(&hinted).also_described_by(&hint);
            assert!(described(&hinted), "the hint describes it from the start");
            error.show("Not a number");
            error.clear();
            assert!(described(&hinted), "and still does once the refusal goes");

            // A clone kept by the field's own check, the hint added after it.
            let cloned = gtk::Entry::new();
            let error = FieldError::under(&cloned);
            let checking = error.clone();
            let _error = error.also_described_by(&hint);
            checking.show("Not a number");
            checking.clear();
            assert!(described(&cloned), "the clone keeps the hint");
        });
    }

    /// The list row holding the line shows only while the line does.
    #[test]
    fn list_row_follows_line() {
        big_testkit::harness::require_gtk_test(|| {
            let field = entry_row::build(adw::EntryRow::builder());
            let error = FieldError::under(&field);
            let row = error.list_row();
            assert!(!row.is_visible());
            error.show("Needs a name");
            assert!(row.is_visible());
            error.clear();
            assert!(!row.is_visible());
        });
    }
}
