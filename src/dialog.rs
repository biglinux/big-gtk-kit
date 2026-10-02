// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `AdwDialog`s that are released when they close.

use std::cell::Cell;

use adw::prelude::*;

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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{init, pump, watch};

    /// The tree of a dialog closed over a window with no focus, finalized.
    #[test]
    #[ignore = "needs a display"]
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

    #[test]
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
