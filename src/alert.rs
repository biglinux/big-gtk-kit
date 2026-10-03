// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Asking an `AdwAlertDialog` for its answer without keeping the dialog forever.
//!
//! `AlertDialogExtManual::choose` in libadwaita-rs 0.9 hands `self` to
//! `adw_alert_dialog_choose` with `into_glib_ptr`, a reference the C function never
//! takes (`self` is transfer-none there), so every dialog asked that way stayed in
//! memory after it closed — measured in the file manager as a whole `AdwAlertDialog`,
//! its entry and ~200 widgets for every "New Folder" opened. `choose_future` goes
//! through the same call. These two do what the binding means to, from a borrow.

use std::cell::Cell;
use std::future::Future;

use adw::prelude::*;
use gtk::{gio, glib};

/// Present `dialog` on `parent` and call `callback` once with the response.
pub fn choose(
    dialog: &adw::AlertDialog,
    parent: Option<&impl IsA<gtk::Widget>>,
    callback: impl FnOnce(glib::GString) + 'static,
) {
    // Taken on the first answer, so what the callback holds is let go as soon as
    // it has run rather than when the dialog is finalized.
    let callback = Cell::new(Some(callback));
    dialog.connect_response(None, move |_, response| {
        if let Some(callback) = callback.take() {
            callback(response.into());
        }
    });
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{init, pump, watch};

    #[gtk::test]
    #[ignore = "needs a display"]
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
