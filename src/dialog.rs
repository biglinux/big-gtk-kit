// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `AdwDialog`s that are released when they close, and [`Dialog`]: one asked
//! again instead of one left behind per opening.
//!
//! libadwaita 1.9 gives every dialog three breakpoints the first time it is
//! presented in a window and never releases them, about 1.8 KB an opening for
//! the life of the program; a dialog presented again makes none.

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Take the keyboard focus out of every `AdwDialog` as it closes. Called by
/// [`crate::install`].
///
/// A dialog opened over a window where nothing had the focus is given back no
/// focus when it closes (libadwaita 1.9 restores the earlier focus only when
/// there was one), so the focus stays inside the closing sheet. GTK 4.22 then
/// keeps a reference to the focused widget for a deferred move and loses it:
/// the dialog's whole widget tree stayed alive, 17 objects for a dialog with
/// one label and over 120 per `AdwShortcutsDialog`, per opening.
pub(crate) fn install() {
    if INSTALLED.replace(true) {
        return;
    }
    crate::on_every_emission("closed", |dialog: adw::Dialog| {
        if let Some(window) = dialog.root().and_downcast::<gtk::Window>()
            && GtkWindowExt::focus(&window).is_some_and(|focus| focus.is_ancestor(&dialog))
        {
            GtkWindowExt::set_focus(&window, gtk::Widget::NONE);
        }
    });
}

/// Present the dialog `build` makes on `parent`, building it only the first
/// time for `parent`'s window and presenting that one again afterwards.
///
/// For a dialog that cannot be released when it closes:
/// `AdwShortcutsDialog` (libadwaita 1.9) keeps its sections, their shortcuts
/// and its filter models after disposal, so one built per opening grows the
/// program by a whole set every time. Kept, it costs one set for as long as
/// the window lives. A dialog that releases cleanly should be built per
/// opening instead: kept, it would cost memory while closed.
pub fn present_kept<D: IsA<adw::Dialog>>(
    parent: &impl IsA<gtk::Widget>,
    build: impl FnOnce() -> D,
) {
    let parent = parent.as_ref();
    let Some(window) = parent.root().and_downcast::<gtk::Window>() else {
        build().present(Some(parent));
        return;
    };
    // One key per dialog type, each written and read as that type only.
    let key = format!("big-gtk-kit-kept-{}", D::static_type().name());
    // SAFETY: `key` holds a `D` (see above).
    let kept = unsafe { window.data::<D>(&key).map(|dialog| dialog.as_ref().clone()) };
    let dialog = kept.unwrap_or_else(|| {
        let dialog = build();
        // SAFETY: as above; the window drops it when it finalizes.
        unsafe { window.set_data(&key, dialog.clone()) };
        dialog
    });
    dialog.present(Some(parent));
}

/// What one opening of a reused dialog connected to it, and which opening is
/// the dialog's now: a handle kept from an earlier one no longer reaches it.
#[derive(Default)]
pub(crate) struct Opening {
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    controllers: RefCell<Vec<gtk::EventController>>,
    number: Cell<u64>,
    /// Closed over a window, so it may go back once it leaves it.
    reusable: Cell<bool>,
}

impl Opening {
    pub(crate) fn keep(&self, handler: glib::SignalHandlerId) {
        self.handlers.borrow_mut().push(handler);
    }

    pub(crate) fn number(&self) -> u64 {
        self.number.get()
    }

    pub(crate) fn add_controller(
        &self,
        dialog: &impl IsA<gtk::Widget>,
        controller: impl IsA<gtk::EventController>,
    ) {
        let controller = controller.upcast();
        self.controllers.borrow_mut().push(controller.clone());
        dialog.add_controller(controller);
    }

    /// Disconnect what this opening connected and start the next.
    pub(crate) fn end(&self, dialog: &impl IsA<gtk::Widget>) {
        for handler in self.handlers.take() {
            dialog.disconnect(handler);
        }
        for controller in self.controllers.take() {
            dialog.remove_controller(&controller);
        }
        self.reusable.set(false);
        self.number.set(self.number.get() + 1);
    }
}

/// Hand `dialog` to `give_back` once it has closed and left its window: at the
/// end of its closing, or with its window. Not at `closed`, which comes as the
/// closing starts and before the `response` that closed an alert; in an idle,
/// because one closing without animation leaves before that `response` too.
///
/// One presented in a window of its own is never given back: libadwaita 1.9.3
/// leaves its content parented to it and sets that parent again at the next
/// presentation, which GTK reports as a critical. Its `force_close` says no `closed` either, and
/// libadwaita then keeps pointing at that destroyed window.
pub(crate) fn give_back_once_left<D: IsA<adw::Dialog> + IsA<gtk::Widget>>(
    dialog: &D,
    opening: &Rc<Opening>,
    give_back: impl Fn(D, u64) + 'static,
) {
    let closing = Rc::clone(opening);
    dialog
        .upcast_ref::<adw::Dialog>()
        .connect_closed(move |dialog| {
            let windowed =
                WidgetExt::parent(dialog).is_some_and(|parent| parent.is::<gtk::Window>());
            closing.reusable.set(!windowed);
        });
    let give_back = Rc::new(give_back);
    let leaving = Rc::clone(opening);
    dialog.connect_notify_local(Some("parent"), move |dialog, _| {
        if WidgetExt::parent(dialog).is_some() || !leaving.reusable.get() {
            return;
        }
        // Held from here: the window was its last owner.
        let (dialog, opening) = (dialog.clone(), Rc::clone(&leaving));
        let give_back = Rc::clone(&give_back);
        let number = opening.number();
        glib::idle_add_local_once(move || {
            if WidgetExt::parent(&dialog).is_none() && opening.number() == number {
                give_back(dialog, number);
            }
        });
    });
}

/// An `AdwDialog` taken from those already made, or made when none is free; it
/// goes back once it has closed and left its window, with its child, the
/// handlers and controllers added through it and what was set on it gone. It
/// derefs to the `adw::Dialog`.
///
/// A `Dialog` kept past its closing may hold the dialog while it shows
/// something else: its [`close`](Self::close) then does nothing, and a
/// [`WeakDialog`] no longer upgrades. Everything else reached through the
/// deref acts on whatever the dialog shows now.
#[derive(Clone)]
pub struct Dialog {
    dialog: adw::Dialog,
    opening: Rc<Opening>,
    number: u64,
}

/// A weak reference to one opening of a [`Dialog`].
#[derive(Clone)]
pub struct WeakDialog {
    dialog: glib::WeakRef<adw::Dialog>,
    opening: Rc<Opening>,
    number: u64,
}

impl WeakDialog {
    /// The dialog while it still shows the opening this was taken from.
    pub fn upgrade(&self) -> Option<Dialog> {
        (self.opening.number() == self.number)
            .then(|| self.dialog.upgrade())
            .flatten()
            .map(|dialog| Dialog {
                dialog,
                opening: Rc::clone(&self.opening),
                number: self.number,
            })
    }
}

thread_local! {
    /// Dialogs closed and ready to show again: as many as were ever open at once.
    static FREE: RefCell<Vec<Dialog>> = const { RefCell::new(Vec::new()) };
}

impl Dialog {
    /// An empty dialog, as `adw::Dialog::new`.
    pub fn new() -> Self {
        let dialog = FREE.with_borrow_mut(Vec::pop).unwrap_or_else(Self::make);
        Self {
            number: dialog.opening.number(),
            ..dialog
        }
    }

    fn make() -> Self {
        #[expect(clippy::disallowed_methods, reason = "the one place dialogs are made")]
        let dialog = adw::Dialog::new();
        let opening = Rc::<Opening>::default();
        let kept = Rc::clone(&opening);
        give_back_once_left(&dialog, &opening, move |dialog, number| {
            Dialog {
                dialog,
                opening: Rc::clone(&kept),
                number,
            }
            .give_back();
        });
        Self {
            dialog,
            opening,
            number: 0,
        }
    }

    /// As `adw::Dialog::builder`, for the properties a dialog here sets.
    pub fn builder() -> DialogBuilder {
        DialogBuilder::default()
    }

    /// Forget this opening and wait for the next.
    fn give_back(self) {
        let dialog = &self.dialog;
        self.opening.end(dialog);
        dialog.set_child(gtk::Widget::NONE);
        dialog.set_default_widget(gtk::Widget::NONE);
        dialog.set_title("");
        dialog.set_content_width(-1);
        dialog.set_content_height(-1);
        dialog.set_follows_content_size(false);
        dialog.set_can_close(true);
        dialog.set_presentation_mode(adw::DialogPresentationMode::Auto);
        FREE.with_borrow_mut(|free| free.push(self));
    }

    /// The dialog itself, for what takes an `adw::Dialog`.
    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    /// Whether the dialog still shows this opening.
    pub fn is_current(&self) -> bool {
        self.opening.number() == self.number
    }

    /// A weak reference to this opening, as `ObjectExt::downgrade`.
    pub fn downgrade(&self) -> WeakDialog {
        WeakDialog {
            dialog: self.dialog.downgrade(),
            opening: Rc::clone(&self.opening),
            number: self.number,
        }
    }

    /// As `AdwDialogExt::close`, when the dialog still shows this opening.
    pub fn close(&self) -> bool {
        self.is_current() && self.dialog.close()
    }

    /// As `AdwDialogExt::force_close`, when the dialog still shows this opening.
    pub fn force_close(&self) {
        if self.is_current() {
            self.dialog.force_close();
        }
    }

    /// As `AdwDialogExt::connect_closed`, disconnected when the dialog has
    /// left its window.
    pub fn connect_closed<F: Fn(&adw::Dialog) + 'static>(&self, f: F) {
        self.opening.keep(self.dialog.connect_closed(f));
    }

    /// As `WidgetExt::add_controller`, removed when the dialog has left its
    /// window.
    pub fn add_controller(&self, controller: impl IsA<gtk::EventController>) {
        self.opening.add_controller(&self.dialog, controller);
    }
}

impl Default for Dialog {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for Dialog {
    type Target = adw::Dialog;

    fn deref(&self) -> &adw::Dialog {
        &self.dialog
    }
}

/// [`Dialog::builder`]: what `adw::Dialog::builder` was used for.
#[derive(Default)]
#[must_use = "a dialog is made by `build`"]
pub struct DialogBuilder {
    title: Option<glib::GString>,
    content_width: Option<i32>,
    content_height: Option<i32>,
    child: Option<gtk::Widget>,
}

impl DialogBuilder {
    pub fn title(mut self, title: impl Into<glib::GString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn content_width(mut self, width: i32) -> Self {
        self.content_width = Some(width);
        self
    }

    pub fn content_height(mut self, height: i32) -> Self {
        self.content_height = Some(height);
        self
    }

    pub fn child(mut self, child: &impl IsA<gtk::Widget>) -> Self {
        self.child = Some(child.clone().upcast());
        self
    }

    pub fn build(self) -> Dialog {
        let dialog = Dialog::new();
        if let Some(title) = &self.title {
            dialog.set_title(title);
        }
        if let Some(width) = self.content_width {
            dialog.set_content_width(width);
        }
        if let Some(height) = self.content_height {
            dialog.set_content_height(height);
        }
        dialog.set_child(self.child.as_ref());
        dialog
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{init, pump, watch};

    /// One dialog shows every opening, each with only its own child and
    /// handlers, and lets go of what the last one showed.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn dialogs_show_again_with_nothing_of_the_last_opening() {
        init();
        install();
        let window = adw::Window::new();
        window.present();
        let dialogs = Rc::new(std::cell::RefCell::new(Vec::<adw::Dialog>::new()));
        let closed = Rc::new(Cell::new(0));
        let children = Rc::new(Cell::new(0));
        let mut controllers = None;
        for _ in 0..25 {
            let child = gtk::Label::new(Some("hello"));
            watch(&child, &children);
            let dialog = Dialog::builder()
                .title("Properties")
                .content_width(300)
                .child(&child)
                .build();
            let counted = closed.clone();
            dialog.connect_closed(move |_| counted.set(counted.get() + 1));
            controllers.get_or_insert(dialog.observe_controllers().n_items());
            dialog.add_controller(gtk::EventControllerKey::new());
            if !dialogs.borrow().contains(dialog.dialog()) {
                dialogs.borrow_mut().push(dialog.dialog().clone());
            }
            dialog.present(Some(&window));
            pump();
            dialog.close();
            while dialog.parent().is_some() {
                pump();
            }
            drop((dialog, child));
            pump();
        }
        window.destroy();
        pump();
        assert_eq!(dialogs.borrow().len(), 1, "one dialog showed them all");
        assert_eq!(closed.get(), 25, "each handler ran once");
        assert_eq!(children.get(), 25, "every child let go");
        assert_eq!(
            Some(dialogs.borrow()[0].observe_controllers().n_items()),
            controllers,
            "every controller removed"
        );
    }

    /// The tree of a dialog closed over a window with no focus, finalized.
    #[gtk::test]
    #[ignore = "needs a display"]
    #[expect(
        clippy::disallowed_methods,
        reason = "a dialog made per opening is what this proves releases"
    )]
    fn dialogs_closed_without_earlier_focus_finalize() {
        init();
        install();
        let window = adw::Window::builder()
            .default_width(800)
            .default_height(600)
            .build();
        window.present();
        pump();
        let finalized = Rc::new(Cell::new(0));
        for _ in 0..25 {
            let entry = gtk::Entry::new();
            watch(&entry, &finalized);
            let dialog = adw::Dialog::builder().child(&entry).build();
            dialog.present(Some(&window));
            pump();
            entry.grab_focus();
            pump();
            dialog.force_close();
            drop((entry, dialog));
            pump();
        }
        window.destroy();
        assert_eq!(finalized.get(), 25);
    }

    #[gtk::test]
    #[ignore = "needs a display"]
    fn a_kept_dialog_is_built_once_per_window() {
        init();
        let window = adw::Window::new();
        window.present();
        pump();
        let built = Rc::new(Cell::new(0));
        for _ in 0..5 {
            let count = built.clone();
            present_kept(&window, || {
                count.set(count.get() + 1);
                adw::ShortcutsDialog::new()
            });
            pump();
            if let Some(dialog) = window.visible_dialog() {
                dialog.force_close();
            }
            pump();
        }
        assert_eq!(built.get(), 1);
        let finalized = Rc::new(Cell::new(0));
        // SAFETY: the key and type `present_kept` uses for this dialog type.
        let kept =
            unsafe { window.data::<adw::ShortcutsDialog>("big-gtk-kit-kept-AdwShortcutsDialog") };
        // SAFETY: as above.
        let kept = kept
            .map(|dialog| unsafe { dialog.as_ref() }.clone())
            .expect("kept");
        watch(&kept, &finalized);
        drop(kept);
        window.destroy();
        drop(window);
        pump();
        assert_eq!(finalized.get(), 1, "the kept dialog outlived its window");
    }
}
