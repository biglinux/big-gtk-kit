// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! List rows a screen reader can act on.

use gtk::prelude::*;

/// Give an activatable `row` a `row.activate` action, which activates it through
/// its `gtk::ListBox` as Enter does. AT-SPI lists a widget's own actions and GTK 4
/// gives a list row none, so without it a screen reader can focus the row and not
/// open or toggle it.
pub fn offer_activation(row: &impl IsA<gtk::ListBoxRow>) {
    let action = gtk::gio::SimpleAction::new("activate", None);
    // Weak: the action must not keep the row alive.
    let weak = row.as_ref().downgrade();
    action.connect_activate(move |_, _| {
        if let Some(row) = weak.upgrade() {
            row.activate();
        }
    });
    let actions = gtk::gio::SimpleActionGroup::new();
    actions.add_action(&action);
    row.as_ref().insert_action_group("row", Some(&actions));
}
