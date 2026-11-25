// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The switch at the end of a row.

use gtk::prelude::*;

/// A switch showing `active`, centred on its row and named `label` for a
/// screen reader, which reads the switch apart from the row's title.
///
/// `label` is already translated.
pub fn named_switch(active: bool, label: &str) -> gtk::Switch {
    let switch = gtk::Switch::builder()
        .active(active)
        .valign(gtk::Align::Center)
        .build();
    switch.update_property(&[gtk::accessible::Property::Label(label)]);
    switch
}
