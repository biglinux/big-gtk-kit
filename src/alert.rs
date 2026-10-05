// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Asking an `AdwAlertDialog` for its answer without keeping the dialog forever,
//! and without leaving part of one behind at every question.
//!
//! [`Alert`] is an `AdwAlertDialog` taken from those already made. libadwaita 1.9
//! gives every dialog three breakpoints the first time it is presented in a
//! window and never releases them, about 1.8 KB a question for the life of the
//! program; a dialog presented again makes none. An `Alert` goes back to be
//! asked again once it has closed and left its window, with its responses, its
//! handlers and what was set on it gone.
//!
//! `AlertDialogExtManual::choose` in libadwaita-rs 0.9 hands `self` to
//! `adw_alert_dialog_choose` with `into_glib_ptr`, a reference the C function never
//! takes (`self` is transfer-none there), so every dialog asked that way stayed in
//! memory after it closed — measured in the file manager as a whole `AdwAlertDialog`,
//! its entry and ~200 widgets for every "New Folder" opened. `choose_future` goes
//! through the same call. These two do what the binding means to, from a borrow.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::ops::Deref;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib::translate::IntoGlib;
use gtk::{gio, glib};

use crate::dialog::Opening;

/// Present `dialog` on `parent` and call `callback` once with the response.
pub fn choose(
    dialog: &adw::AlertDialog,
    parent: Option<&impl IsA<gtk::Widget>>,
    callback: impl FnOnce(glib::GString) + 'static,
) {
    // Taken on the first answer, so what the callback holds is let go as soon as
    // it has run rather than when the dialog is finalized; and the handler goes
    // too, as an [`Alert`] asks again.
    let callback = Cell::new(Some(callback));
    let handler = Rc::new(Cell::new(None));
    let connected = Rc::clone(&handler);
    let id = dialog.connect_response(None, move |dialog, response| {
        if let Some(handler) = connected.take() {
            dialog.disconnect(handler);
        }
        if let Some(callback) = callback.take() {
            callback(response.into());
        }
    });
    handler.set(Some(id));
    dialog.present(parent);
}

/// [`choose`] as a future. Dropping the future closes the dialog, which answers
/// with its close response, as the binding's own future does.
pub fn choose_future(
    dialog: &adw::AlertDialog,
    parent: Option<&impl IsA<gtk::Widget>>,
) -> impl Future<Output = glib::GString> + 'static {
    let parent = parent.map(|parent| parent.as_ref().clone());
    gio::GioFuture::new(dialog, move |dialog, cancellable, send| {
        let weak = dialog.downgrade();
        cancellable.connect_cancelled_local(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
        choose(dialog, parent.as_ref(), move |response| {
            send.resolve(response)
        });
    })
}

/// An `AdwAlertDialog` taken from those already made, or made when none is free;
/// it goes back once it has closed and left its window. Everything an
/// `AdwAlertDialog` does goes through it (it derefs to one); the next question
/// starts with none of the last one's responses, `response` handlers, or
/// handlers connected through its own `connect_closed`.
///
/// An `Alert` kept past its closing may hold the dialog while it asks
/// something else: its [`close`](Self::close) then does nothing, and a
/// [`WeakAlert`] no longer upgrades. Everything else reached through the
/// deref acts on whatever the dialog asks now.
#[derive(Clone)]
pub struct Alert {
    dialog: adw::AlertDialog,
    kept: Rc<Kept>,
    number: u64,
}

/// What the current question added to its dialog.
#[derive(Default)]
struct Kept {
    responses: RefCell<Vec<String>>,
    opening: Rc<Opening>,
}

/// A weak reference to one question of an [`Alert`].
#[derive(Clone)]
pub struct WeakAlert {
    dialog: glib::WeakRef<adw::AlertDialog>,
    kept: Rc<Kept>,
    number: u64,
}

impl WeakAlert {
    /// The dialog while it still asks the question this was taken from.
    pub fn upgrade(&self) -> Option<Alert> {
        (self.kept.opening.number() == self.number)
            .then(|| self.dialog.upgrade())
            .flatten()
            .map(|dialog| Alert {
                dialog,
                kept: Rc::clone(&self.kept),
                number: self.number,
            })
    }
}

thread_local! {
    /// Dialogs closed and ready to ask again: as many as were ever open at once.
    static FREE: RefCell<Vec<Alert>> = const { RefCell::new(Vec::new()) };
}

impl Alert {
    /// A dialog with `heading` and `body`, as `adw::AlertDialog::new`.
    pub fn new(heading: Option<&str>, body: Option<&str>) -> Self {
        let alert = FREE.with_borrow_mut(Vec::pop).unwrap_or_else(Self::make);
        // Never unset once made (libadwaita refuses NULL): empty shows none.
        alert.dialog.set_heading(Some(heading.unwrap_or_default()));
        alert.dialog.set_body(body.unwrap_or_default());
        Self {
            number: alert.kept.opening.number(),
            ..alert
        }
    }

    fn make() -> Self {
        // A dialog the default handler of `response` closes, as a new one.
        #[expect(clippy::disallowed_methods, reason = "the one place alerts are made")]
        let dialog = adw::AlertDialog::new(None, None);
        let kept = Rc::<Kept>::default();
        let handed = Rc::clone(&kept);
        crate::dialog::give_back_once_left(&dialog, &kept.opening, move |dialog, number| {
            Alert {
                dialog,
                kept: Rc::clone(&handed),
                number,
            }
            .give_back();
        });
        Self {
            dialog,
            kept,
            number: 0,
        }
    }

    /// Forget this question and wait for the next.
    fn give_back(self) {
        let dialog = &self.dialog;
        self.kept.opening.end(dialog);
        // Every one, `choose`'s and those connected through the deref too:
        // none may answer the next question.
        if let Some(response) =
            glib::subclass::SignalId::lookup("response", adw::AlertDialog::static_type())
        {
            // SAFETY: `dialog` is a live object, and a match on the signal id
            // alone disconnects only its handlers of that signal.
            unsafe {
                glib::gobject_ffi::g_signal_handlers_disconnect_matched(
                    dialog.as_ptr().cast(),
                    glib::gobject_ffi::G_SIGNAL_MATCH_ID,
                    response.into_glib(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
        }
        // Before the responses go: the default is kept by name, so a later
        // question with a response of that name would answer it on Enter.
        self.set_default_response(None);
        // Any the program removed itself are gone already.
        for response in self.kept.responses.take() {
            if dialog.has_response(&response) {
                dialog.remove_response(&response);
            }
        }
        dialog.set_heading(Some(""));
        dialog.set_heading_use_markup(false);
        dialog.set_body("");
        dialog.set_body_use_markup(false);
        dialog.set_extra_child(gtk::Widget::NONE);
        dialog.set_prefer_wide_layout(false);
        dialog.set_close_response("close");
        dialog.set_title("");
        dialog.set_can_close(true);
        dialog.set_follows_content_size(false);
        dialog.set_content_width(-1);
        dialog.set_content_height(-1);
        dialog.set_presentation_mode(adw::DialogPresentationMode::Auto);
        FREE.with_borrow_mut(|free| free.push(self));
    }

    /// The dialog itself, for what takes an `adw::AlertDialog`.
    pub fn dialog(&self) -> &adw::AlertDialog {
        &self.dialog
    }

    /// Whether the dialog still asks this question.
    pub fn is_current(&self) -> bool {
        self.kept.opening.number() == self.number
    }

    /// A weak reference to this question, as `ObjectExt::downgrade`.
    pub fn downgrade(&self) -> WeakAlert {
        WeakAlert {
            dialog: self.dialog.downgrade(),
            kept: Rc::clone(&self.kept),
            number: self.number,
        }
    }

    /// As `AdwDialogExt::close`, when the dialog still asks this question.
    pub fn close(&self) -> bool {
        self.is_current() && self.dialog.close()
    }

    /// As `AdwDialogExt::force_close`, when the dialog still asks this question.
    pub fn force_close(&self) {
        if self.is_current() {
            self.dialog.force_close();
        }
    }

    /// As `AlertDialogExt::set_default_response`, where `None` is safe:
    /// libadwaita 1.9.3 looks a NULL name up, and crashes, once a default was
    /// set.
    pub fn set_default_response(&self, response: Option<&str>) {
        self.dialog
            .set_default_response(Some(response.unwrap_or_default()));
    }

    /// As `AlertDialogExt::add_response`, removed when the dialog has left its
    /// window.
    pub fn add_response(&self, id: &str, label: &str) {
        let mut responses = self.kept.responses.borrow_mut();
        if !responses.iter().any(|kept| kept == id) {
            responses.push(id.to_owned());
        }
        self.dialog.add_response(id, label);
    }

    /// As `AlertDialogExtManual::add_responses`, removed when the dialog has
    /// left its window.
    pub fn add_responses(&self, responses: &[(&str, &str)]) {
        for (id, label) in responses {
            self.add_response(id, label);
        }
    }

    /// As `AdwDialogExt::connect_closed`, disconnected when the dialog has
    /// left its window.
    pub fn connect_closed<F: Fn(&adw::AlertDialog) + 'static>(&self, f: F) {
        self.kept.opening.keep(self.dialog.connect_closed(f));
    }

    /// As `adw::AlertDialog::builder`, for the properties a question sets.
    pub fn builder() -> AlertBuilder {
        AlertBuilder::default()
    }
}

/// [`Alert::builder`]: what `adw::AlertDialog::builder` was used for.
#[derive(Default)]
#[must_use = "an alert is made by `build`"]
pub struct AlertBuilder {
    heading: Option<glib::GString>,
    body: Option<glib::GString>,
    body_use_markup: bool,
    extra_child: Option<gtk::Widget>,
    default_response: Option<glib::GString>,
    close_response: Option<glib::GString>,
}

impl AlertBuilder {
    pub fn heading(mut self, heading: impl Into<glib::GString>) -> Self {
        self.heading = Some(heading.into());
        self
    }

    pub fn body(mut self, body: impl Into<glib::GString>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn body_use_markup(mut self, markup: bool) -> Self {
        self.body_use_markup = markup;
        self
    }

    pub fn extra_child(mut self, child: &impl IsA<gtk::Widget>) -> Self {
        self.extra_child = Some(child.clone().upcast());
        self
    }

    pub fn default_response(mut self, id: impl Into<glib::GString>) -> Self {
        self.default_response = Some(id.into());
        self
    }

    pub fn close_response(mut self, id: impl Into<glib::GString>) -> Self {
        self.close_response = Some(id.into());
        self
    }

    pub fn build(self) -> Alert {
        let alert = Alert::new(self.heading.as_deref(), self.body.as_deref());
        alert.set_body_use_markup(self.body_use_markup);
        alert.set_extra_child(self.extra_child.as_ref());
        if let Some(id) = &self.default_response {
            alert.set_default_response(Some(id));
        }
        if let Some(id) = &self.close_response {
            alert.set_close_response(id);
        }
        alert
    }
}

impl Deref for Alert {
    type Target = adw::AlertDialog;

    fn deref(&self) -> &adw::AlertDialog {
        &self.dialog
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::testing::{init, pump, watch};

    /// One dialog asks every question, each with only its own responses and
    /// handlers, and lets go of what the last one showed.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn alerts_ask_again_with_nothing_of_the_last_question() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.present();
        let dialogs = Rc::new(RefCell::new(Vec::<adw::AlertDialog>::new()));
        let heard = Rc::new(Cell::new(0));
        let closed = Rc::new(Cell::new(0));
        let children = Rc::new(Cell::new(0));
        let mut last: Option<(Alert, WeakAlert)> = None;
        for question in 0..25 {
            let entry = gtk::Entry::new();
            watch(&entry, &children);
            // Every other question has a default, as most here do.
            let builder = Alert::builder()
                .heading("Delete?")
                .extra_child(&entry)
                .close_response("cancel");
            let alert = if question % 2 == 0 {
                builder.default_response("cancel").build()
            } else {
                builder.build()
            };
            let id = format!("delete-{question}");
            alert.add_responses(&[("cancel", "Cancel"), (id.as_str(), "Delete")]);
            if question % 2 == 1 {
                assert!(
                    alert.default_widget().is_none(),
                    "the last question's default is gone"
                );
            }
            if question > 0 {
                let last = format!("delete-{}", question - 1);
                assert!(
                    !alert.has_response(&last),
                    "the last question's response is gone"
                );
            }
            let counted = heard.clone();
            alert.connect_response(None, move |_, _| counted.set(counted.get() + 1));
            let counted = closed.clone();
            alert.connect_closed(move |_| counted.set(counted.get() + 1));
            if !dialogs.borrow().contains(alert.dialog()) {
                dialogs.borrow_mut().push(alert.dialog().clone());
            }
            alert.present(Some(&window));
            pump();
            if let Some((stale, weak)) = last.take() {
                assert!(weak.upgrade().is_none(), "the last question is gone");
                assert!(!stale.close(), "an old handle leaves this one open");
                assert!(alert.is_current());
            }
            last = Some((alert.clone(), alert.downgrade()));
            // Closed as Escape closes it: with its close response.
            alert.close();
            while alert.parent().is_some() {
                pump();
            }
            drop((alert, entry));
            pump();
        }
        drop(last);
        window.destroy();
        pump();
        assert_eq!(dialogs.borrow().len(), 1, "one dialog asked them all");
        assert_eq!(
            (heard.get(), closed.get()),
            (25, 25),
            "each handler ran once"
        );
        assert_eq!(children.get(), 25, "every extra child let go");
    }

    /// A response chosen reaches the program, with or without the closing
    /// animation, and the dialog stays as it was until it has left its window.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn a_chosen_response_reaches_the_program_and_the_dialog_goes_back_whole() {
        init();
        crate::dialog::install();
        let settings = gtk::Settings::default().expect("settings");
        let window = adw::Window::new();
        window.present();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let finalized = Rc::new(Cell::new(0));
        let mut dialogs = Vec::new();
        for animate in [false, true, false, true] {
            settings.set_gtk_enable_animations(animate);
            let alert = Alert::builder().heading("Replace?").build();
            alert.add_responses(&[("cancel", "_Cancel"), ("replace", "_Replace")]);
            let answers = heard.clone();
            alert.connect_response(None, move |_, response| {
                answers.borrow_mut().push(response.to_owned());
            });
            if dialogs.is_empty() {
                watch(alert.dialog(), &finalized);
            }
            // Alive in the pool, so an address seen twice is one dialog.
            dialogs.push(alert.dialog().as_ptr() as usize);
            alert.present(Some(&window));
            // Opened, as a person sees it before answering.
            for _ in 0..10 {
                pump();
            }
            let replace = buttons(alert.dialog().upcast_ref())
                .into_iter()
                .find(|button| button.label().as_deref() == Some("_Replace"))
                .expect("the response's button");
            // Held by its window only, as a program leaves it.
            let shown = alert.dialog().downgrade();
            drop(alert);
            replace.emit_clicked();
            pump();
            if let Some(dialog) = shown.upgrade()
                && dialog.parent().is_some()
            {
                assert_eq!(
                    dialog.heading().as_deref(),
                    Some("Replace?"),
                    "a closing dialog keeps what it showed"
                );
            }
            while shown
                .upgrade()
                .is_some_and(|dialog| dialog.parent().is_some())
            {
                pump();
            }
            pump();
        }
        settings.set_gtk_enable_animations(true);
        window.destroy();
        pump();
        assert_eq!(*heard.borrow(), ["replace"; 4]);
        assert_eq!(finalized.get(), 0, "the dialog waits in the pool");
        dialogs.dedup();
        assert_eq!(dialogs.len(), 1, "one dialog asked them all");
    }

    /// One shut by `force_close` in a window of its own says no `closed`, and
    /// libadwaita still points at that window: it is not asked again.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn an_alert_forced_shut_in_its_own_window_is_not_asked_again() {
        init();
        let first = Alert::new(Some("Once"), None);
        first.present(gtk::Widget::NONE);
        pump();
        let shown = first.dialog().clone();
        first.force_close();
        drop(first);
        pump();
        let next = Alert::new(Some("Again"), None);
        assert_ne!(next.dialog(), &shown);
        next.present(gtk::Widget::NONE);
        pump();
        assert!(next.parent().is_some(), "presented in a window of its own");
        next.force_close();
        pump();
    }

    /// A dialog never presented is let go with its last handle.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn an_alert_never_presented_is_released() {
        init();
        let finalized = Rc::new(Cell::new(0));
        let alert = Alert::new(Some("Unasked"), None);
        watch(alert.dialog(), &finalized);
        drop(alert);
        pump();
        assert_eq!(finalized.get(), 1);
    }

    fn buttons(widget: &gtk::Widget) -> Vec<gtk::Button> {
        let mut found: Vec<gtk::Button> = widget.downcast_ref().cloned().into_iter().collect();
        let mut child = widget.first_child();
        while let Some(widget) = child {
            found.extend(buttons(&widget));
            child = widget.next_sibling();
        }
        found
    }

    #[gtk::test]
    #[ignore = "needs a display"]
    #[expect(
        clippy::disallowed_methods,
        reason = "a dialog made per opening is what this proves releases"
    )]
    fn answered_dialogs_finalize() {
        init();
        // The closing-dialog focus fix every program installs.
        crate::dialog::install();
        let window = adw::Window::new();
        window.present();
        let finalized = Rc::new(Cell::new(0));
        let answers = Rc::new(Cell::new(0));
        for _ in 0..25 {
            let dialog = adw::AlertDialog::new(Some("Delete?"), None);
            dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
            watch(&dialog, &finalized);
            let counted = answers.clone();
            choose(&dialog, Some(&window), move |_| {
                counted.set(counted.get() + 1)
            });
            pump();
            dialog.emit_by_name::<()>("response", &[&"delete"]);
            dialog.force_close();
            drop(dialog);
            pump();
        }
        window.destroy();
        pump();
        assert_eq!((answers.get(), finalized.get()), (25, 25));
    }
}
