// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Asking an `AdwAlertDialog` for its answer without keeping the dialog forever,
//! and without leaving part of one behind at every question.
//!
//! [`Alert`] is an `AdwAlertDialog` made for the widget it asks over. libadwaita
//! 1.9 gives every dialog three breakpoints the first time it is presented as a
//! sheet inside a window and never releases them, about 1.8 KB a question for
//! the life of the program; a dialog presented again makes none. An `Alert` for
//! a parent inside an `AdwWindow` is taken from those already made and goes back
//! to be asked again once it has closed and left its window, with its
//! responses, its handlers and what was set on it gone.
//!
//! `AlertDialogExtManual::choose` in libadwaita-rs 0.9 hands `self` to
//! `adw_alert_dialog_choose` with `into_glib_ptr`, a reference the C function never
//! takes (`self` is transfer-none there), so every dialog asked that way stayed in
//! memory after it closed: in the file manager, a whole `AdwAlertDialog`,
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
/// An [`Alert`] asks with [`Alert::choose`], on the parent it was made for.
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
    ask(dialog, parent.map(|parent| parent.as_ref().clone()))
}

/// [`choose_future`] with the parent owned, so the future borrows nothing.
fn ask(
    dialog: &adw::AlertDialog,
    parent: Option<gtk::Widget>,
) -> impl Future<Output = glib::GString> + 'static {
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

/// An `AdwAlertDialog` made for the widget it asks over. For a parent inside an
/// `AdwWindow` or `AdwApplicationWindow`, where it shows as a sheet, it is taken
/// from those already made, or made when none is free, and it goes back once it
/// has closed and left its window; the next question starts with none of the
/// last one's responses, `response` handlers, or handlers connected through its
/// own `connect_closed`. For no parent, or any other, it is made for this
/// question alone and released with the window libadwaita makes for it.
///
/// Everything an `AdwAlertDialog` does goes through it (it derefs to one), but
/// it is presented only by [`present`](Self::present), [`choose`](Self::choose)
/// and [`choose_future`](Self::choose_future), on the parent it was made for:
/// a dialog once shown as a sheet cannot be shown in a window of its own.
///
/// An `Alert` kept past its closing may hold the dialog while it asks
/// something else: its [`close`](Self::close) then does nothing, and a
/// [`WeakAlert`] stops upgrading. Everything else reached through the
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

/// The scroll pane the template puts around the body and extra child.
fn body_pane(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    if widget.has_css_class("body-scrolled-window") {
        return widget.downcast_ref().cloned();
    }
    let mut child = widget.first_child();
    while let Some(each) = child {
        if let Some(pane) = body_pane(&each) {
            return Some(pane);
        }
        child = each.next_sibling();
    }
    None
}

/// Whether a widget inside `widget` takes the focus, or will once shown and
/// enabled.
fn holds_stop(widget: &gtk::Widget) -> bool {
    if !widget.can_focus() {
        return false;
    }
    if widget.is_focusable() {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(each) = child {
        if holds_stop(&each) {
            return true;
        }
        child = each.next_sibling();
    }
    false
}

/// Keep the body's pane in the Tab chain for what it holds. GtkScrolledWindow
/// takes the focus itself when nothing inside it can, which here is a nameless
/// Tab stop, and with `focusable` off it still claims that Tab and focuses
/// nothing. So the pane is a stop of its own only while its content is taller
/// than it, for the keys to scroll it, and nameless then too: GTK gives its
/// `generic` role no name. Otherwise it stays in the chain only for a stop
/// inside: an entry, a link in the heading or body. With `can-focus` off
/// nothing under it takes the focus, not even on a click, so a hidden or
/// insensitive entry counts as a stop: it may be shown or enabled later.
fn follow_body(pane: &gtk::ScrolledWindow) {
    let adjustment = pane.vadjustment();
    let scrolls = adjustment.upper() > adjustment.page_size();
    pane.set_focusable(scrolls);
    pane.set_can_focus(scrolls || pane.child().is_some_and(|child| holds_stop(&child)));
}

thread_local! {
    /// Dialogs closed and ready to ask again: as many as were ever open at once.
    static FREE: RefCell<Vec<Alert>> = const { RefCell::new(Vec::new()) };
}

impl Alert {
    /// A dialog with `heading` and `body` to ask over `parent`, as
    /// `adw::AlertDialog::new`; `None` asks in a window of its own. `parent`
    /// has to be in its window already: that window decides whether the dialog
    /// is reused. It is held weakly.
    pub fn new(
        parent: Option<&impl IsA<gtk::Widget>>,
        heading: Option<&str>,
        body: Option<&str>,
    ) -> Self {
        let parent = parent.map(AsRef::as_ref);
        let alert = if crate::dialog::presents_in_window(parent) {
            FREE.with_borrow_mut(Vec::pop)
                .unwrap_or_else(|| Self::make(true))
        } else {
            Self::make(false)
        };
        alert.kept.opening.begin(parent);
        // Never unset once made (libadwaita refuses NULL): empty shows none.
        alert.dialog.set_heading(Some(heading.unwrap_or_default()));
        alert.dialog.set_body(body.unwrap_or_default());
        Self {
            number: alert.kept.opening.number(),
            ..alert
        }
    }

    fn make(pooled: bool) -> Self {
        // A dialog the default handler of `response` closes, as a new one.
        #[expect(clippy::disallowed_methods, reason = "the one place alerts are made")]
        let dialog = adw::AlertDialog::new(None, None);
        let kept = Rc::<Kept>::default();
        if let Some(pane) = dialog.child().as_ref().and_then(body_pane) {
            let weak = pane.downgrade();
            pane.vadjustment().connect_changed(move |_| {
                if let Some(pane) = weak.upgrade() {
                    follow_body(&pane);
                }
            });
            for property in [
                "extra-child",
                "body",
                "body-use-markup",
                "heading",
                "heading-use-markup",
            ] {
                let pane = pane.clone();
                dialog.connect_notify_local(Some(property), move |_, _| follow_body(&pane));
            }
            // Ahead of the map handler of `give_back_once_left`: the focus it
            // gives a reused dialog needs the pane open for an entry.
            dialog.connect_map(move |_| follow_body(&pane));
        }
        if pooled {
            let handed = Rc::clone(&kept);
            crate::dialog::give_back_once_left(&dialog, &kept.opening, move |dialog, number| {
                Alert {
                    dialog,
                    kept: Rc::clone(&handed),
                    number,
                }
                .give_back();
            });
        }
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
        // An alert's own defaults (adw-alert-dialog.ui), not `AdwDialog`'s:
        // sized to its content and floating. With `AdwDialog`'s it opens a few
        // pixels tall, its heading cut and its body scrolled away.
        dialog.set_follows_content_size(true);
        dialog.set_content_width(-1);
        dialog.set_content_height(-1);
        dialog.set_presentation_mode(adw::DialogPresentationMode::Floating);
        FREE.with_borrow_mut(|free| free.push(self));
    }

    /// The dialog itself, for what takes an `adw::AlertDialog`.
    pub fn dialog(&self) -> &adw::AlertDialog {
        &self.dialog
    }

    /// Show the question on the parent it was made for; nothing once the
    /// parent is gone or the dialog asks something else.
    pub fn present(&self) {
        if let Some(parent) = self.kept.opening.target(self.number) {
            self.dialog.present(parent.as_ref());
        }
    }

    /// Ask on the parent the question was made for and call `callback` once
    /// with the response, as [`choose`]. Once the parent is gone or the dialog
    /// asks something else, `callback` runs at once with the close response.
    pub fn choose(&self, callback: impl FnOnce(glib::GString) + 'static) {
        match self.kept.opening.target(self.number) {
            Some(parent) => choose(&self.dialog, parent.as_ref(), callback),
            None => callback(self.dialog.close_response()),
        }
    }

    /// [`Self::choose`] as a future, as [`choose_future`].
    pub fn choose_future(&self) -> impl Future<Output = glib::GString> + 'static {
        let asked = self
            .kept
            .opening
            .target(self.number)
            .map(|parent| ask(&self.dialog, parent));
        let close = self.dialog.close_response();
        async move {
            match asked {
                Some(answer) => answer.await,
                None => close,
            }
        }
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

    /// As `adw::AlertDialog::builder`, for the properties a question sets, to
    /// ask over `parent` (see [`Self::new`]).
    pub fn builder(parent: Option<&impl IsA<gtk::Widget>>) -> AlertBuilder {
        AlertBuilder {
            parent: parent.map(|parent| parent.as_ref().clone()),
            ..AlertBuilder::default()
        }
    }
}

/// [`Alert::builder`]: what `adw::AlertDialog::builder` was used for.
#[derive(Default)]
#[must_use = "an alert is made by `build`"]
pub struct AlertBuilder {
    parent: Option<gtk::Widget>,
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
        let alert = Alert::new(
            self.parent.as_ref(),
            self.heading.as_deref(),
            self.body.as_deref(),
        );
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::testing::{init, pump, watch};

    /// One dialog asks every question, each with only its own responses and
    /// handlers, and lets go of what the last one showed.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn reused_alert_forgets_last_question() {
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
            let builder = Alert::builder(Some(&window))
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
            assert!(
                alert.follows_content_size(),
                "sized to its content like a new alert"
            );
            assert_eq!(
                alert.presentation_mode(),
                adw::DialogPresentationMode::Floating,
                "floating like a new alert"
            );
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
            alert.present();
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
    fn chosen_response_returns_whole_dialog() {
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
            let alert = Alert::builder(Some(&window)).heading("Replace?").build();
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
            alert.present();
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

    /// A dialog asked again after a button answered it opens with the focus
    /// on its default response, as the first time.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn answered_alert_reopens_focused() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.present();
        for round in 0..3 {
            let alert = Alert::builder(Some(&window))
                .heading("Delete?")
                .default_response("cancel")
                .build();
            alert.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete")]);
            alert.present();
            for _ in 0..10 {
                pump();
            }
            let cancel = buttons(alert.dialog().upcast_ref())
                .into_iter()
                .find(|button| button.label().as_deref() == Some("_Cancel"))
                .expect("the response's button");
            assert_eq!(
                GtkWindowExt::focus(&window).as_ref(),
                Some(cancel.upcast_ref()),
                "round {round}: the default response lacks the focus"
            );
            let shown = alert.dialog().clone();
            drop(alert);
            cancel.emit_clicked();
            while shown.parent().is_some() {
                pump();
            }
            pump();
        }
        window.destroy();
        pump();
    }

    /// One shut by `force_close` in a window of its own says no `closed`, and
    /// libadwaita still points at that window: it is not asked again.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn force_closed_alert_is_retired() {
        init();
        let first = Alert::new(gtk::Widget::NONE, Some("Once"), None);
        first.present();
        pump();
        let shown = first.dialog().clone();
        first.force_close();
        drop(first);
        pump();
        let next = Alert::new(gtk::Widget::NONE, Some("Again"), None);
        assert_ne!(next.dialog(), &shown);
        next.present();
        pump();
        assert!(next.parent().is_some(), "presented in a window of its own");
        next.force_close();
        pump();
    }

    /// A question with no parent, as the shell asks, or over a plain
    /// `GtkWindow` gets a dialog of its own, never one a sheet showed before:
    /// libadwaita would set a parent on what the sheet still holds.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn parentless_alerts_ask_again_cleanly() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.present();
        let sheet = Alert::new(Some(&window), Some("Keep?"), None);
        let pooled = sheet.dialog().clone();
        sheet.present();
        pump();
        assert!(sheet.close());
        drop(sheet);
        while pooled.parent().is_some() {
            pump();
        }
        pump();
        let plain = gtk::Window::new();
        plain.present();
        let criticals = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&criticals);
        let handler = glib::log_set_handler(
            Some("Gtk"),
            glib::LogLevels::LEVEL_CRITICAL,
            false,
            false,
            move |_, _, message| {
                eprintln!("{message}");
                counted.fetch_add(1, Ordering::Relaxed);
            },
        );
        let parents = [None, None, Some(plain.upcast_ref::<gtk::Widget>())];
        for parent in parents {
            let alert = Alert::new(parent, Some("End?"), None);
            assert_ne!(
                alert.dialog(),
                &pooled,
                "the sheet's dialog stays for sheets"
            );
            alert.present();
            pump();
            assert!(alert.root().is_some(), "presented");
            let shown = alert.dialog().downgrade();
            assert!(alert.close());
            drop(alert);
            while shown
                .upgrade()
                .is_some_and(|dialog| dialog.parent().is_some())
            {
                pump();
            }
            pump();
            assert!(shown.upgrade().is_none(), "let go with its window");
        }
        glib::log_remove_handler(Some("Gtk"), handler);
        plain.destroy();
        window.destroy();
        pump();
        assert_eq!(criticals.load(Ordering::Relaxed), 0);
    }

    /// A dialog never presented is let go with its last handle.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn an_alert_never_presented_is_released() {
        init();
        let finalized = Rc::new(Cell::new(0));
        let alert = Alert::new(gtk::Widget::NONE, Some("Unasked"), None);
        watch(alert.dialog(), &finalized);
        drop(alert);
        pump();
        assert_eq!(finalized.get(), 1);
    }

    /// What Tab lands on: a button's label, `"entry"` for the text inside an
    /// entry, `"link"` for a label, `"body"` for the pane, `None` when nothing
    /// has the focus.
    fn tab_stop(window: &adw::Window) -> Option<String> {
        pump();
        let focus = GtkWindowExt::focus(window)?;
        if focus.ancestor(gtk::Entry::static_type()).is_some() {
            return Some("entry".into());
        }
        if focus.is::<gtk::Label>() {
            return Some("link".into());
        }
        if focus.is::<gtk::ScrolledWindow>() {
            return Some("body".into());
        }
        Some(focus.downcast::<gtk::Button>().ok()?.label()?.into())
    }

    /// Tab, or Shift+Tab, goes round `stops` and never lands on nothing.
    fn assert_tab_cycles(window: &adw::Window, stops: &[&str], back: bool) {
        let direction = if back {
            gtk::DirectionType::TabBackward
        } else {
            gtk::DirectionType::TabForward
        };
        let mut seen = vec![tab_stop(window).unwrap_or_default()];
        for _ in 0..2 * stops.len() {
            window.emit_by_name::<()>("move-focus", &[&direction]);
            seen.push(tab_stop(window).unwrap_or_default());
        }
        let mut order = stops.to_vec();
        if back {
            order.reverse();
        }
        let start = order
            .iter()
            .position(|stop| *stop == seen[0])
            .expect("a stop");
        order.rotate_left(start);
        let expected: Vec<_> = order.iter().cycle().take(seen.len()).copied().collect();
        assert_eq!(seen, expected);
    }

    /// The body's scroll pane is no Tab stop: Tab goes over the buttons only.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn tab_skips_the_body_pane() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.present();
        let alert = Alert::new(Some(&window), Some("Delete?"), Some("It cannot be undone."));
        alert.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        alert.set_default_response(Some("cancel"));
        alert.present();
        pump();
        assert_tab_cycles(&window, &["Cancel", "Delete"], false);
        assert_tab_cycles(&window, &["Cancel", "Delete"], true);
        alert.force_close();
        window.destroy();
        pump();
    }

    /// An extra child inside the pane stays a Tab stop.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn tab_reaches_the_extra_child() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        // Room for the entry: in a smaller window the content scrolls, and the
        // pane is a stop too.
        window.set_default_size(640, 480);
        window.present();
        let entry = gtk::Entry::new();
        let alert = Alert::builder(Some(&window))
            .heading("New Folder")
            .extra_child(&entry)
            .default_response("create")
            .build();
        alert.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
        alert.present();
        pump();
        assert_tab_cycles(&window, &["Cancel", "Create", "entry"], false);
        assert_tab_cycles(&window, &["Cancel", "Create", "entry"], true);
        alert.force_close();
        window.destroy();
        pump();
    }

    /// A reused dialog's entry, shown and enabled later, takes the focus.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn reused_alert_reaches_late_entry() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.set_default_size(640, 480);
        window.present();
        let first = Alert::new(Some(&window), Some("Delete?"), None);
        first.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        first.present();
        pump();
        let shown = first.dialog().clone();
        assert!(first.close());
        drop(first);
        while shown.parent().is_some() {
            pump();
        }
        let entry = gtk::Entry::new();
        entry.set_sensitive(false);
        entry.set_visible(false);
        let again = Alert::new(Some(&window), Some("Rename"), None);
        assert_eq!(again.dialog(), &shown);
        again.set_extra_child(Some(&entry));
        again.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
        again.present();
        pump();
        entry.set_visible(true);
        entry.set_sensitive(true);
        assert!(entry.grab_focus());
        assert_eq!(tab_stop(&window).as_deref(), Some("entry"));
        assert_tab_cycles(&window, &["Cancel", "Rename", "entry"], false);
        again.force_close();
        window.destroy();
        pump();
    }

    /// An extra child with nothing to focus leaves no stop in the pane.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn tab_ignores_an_inert_extra_child() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.present();
        let alert = Alert::builder(Some(&window))
            .heading("Replace?")
            .extra_child(&gtk::Label::new(Some("a.txt")))
            .default_response("cancel")
            .build();
        alert.add_responses(&[("cancel", "Cancel"), ("replace", "Replace")]);
        alert.present();
        pump();
        assert_tab_cycles(&window, &["Cancel", "Replace"], false);
        assert_tab_cycles(&window, &["Cancel", "Replace"], true);
        alert.force_close();
        window.destroy();
        pump();
    }

    /// A link in the body is still a Tab stop.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn tab_reaches_a_body_link() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.present();
        let alert = Alert::builder(Some(&window))
            .heading("Update")
            .body("See <a href=\"https://example.org\">the notes</a>.")
            .body_use_markup(true)
            .default_response("cancel")
            .build();
        alert.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
        alert.present();
        pump();
        assert_tab_cycles(&window, &["Cancel", "OK", "link"], false);
        assert_tab_cycles(&window, &["Cancel", "OK", "link"], true);
        alert.force_close();
        window.destroy();
        pump();
    }

    /// A link put in the heading after the dialog shows is a Tab stop.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn tab_reaches_a_late_heading_link() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.set_default_size(640, 480);
        window.present();
        let alert = Alert::new(Some(&window), Some("Update"), None);
        alert.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
        alert.set_default_response(Some("cancel"));
        alert.present();
        pump();
        alert.set_heading_use_markup(true);
        alert.set_heading(Some("See <a href=\"https://example.org\">the notes</a>"));
        pump();
        assert_tab_cycles(&window, &["Cancel", "OK", "link"], false);
        alert.force_close();
        window.destroy();
        pump();
    }

    /// A body taller than the dialog keeps its pane as a stop to scroll.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn tab_reaches_a_long_body() {
        init();
        crate::dialog::install();
        let window = adw::Window::new();
        window.set_default_size(360, 240);
        window.present();
        let body = vec!["A line of the release notes."; 80].join("\n");
        let alert = Alert::new(Some(&window), Some("Update"), Some(&body));
        alert.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
        alert.set_default_response(Some("cancel"));
        alert.present();
        pump();
        assert_tab_cycles(&window, &["Cancel", "OK", "body"], false);
        assert_tab_cycles(&window, &["Cancel", "OK", "body"], true);
        alert.force_close();
        window.destroy();
        pump();
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
