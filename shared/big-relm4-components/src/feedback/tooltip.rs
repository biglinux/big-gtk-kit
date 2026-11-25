// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Icon button builders with tooltip and accessible label set together.
//!
//! `big_gtk_kit::tooltip` draws every tooltip of the process as the BigLinux
//! card; widgets set an ordinary `tooltip-text`.

use adw::prelude::*;
use relm4::gtk;

/// Give `widget` one name: the tooltip a person reads and the label a screen
/// reader announces.
pub fn name_control(widget: &impl IsA<gtk::Widget>, label: &str) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    widget.set_tooltip_text(Some(label));
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

/// Build an icon-only button with tooltip and accessible label set together.
#[must_use]
pub fn icon_button(icon_name: &str, label: &str, classes: &[&str]) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(&gtk::Image::from_icon_name(icon_name))
        .css_classes(classes)
        .build();
    name_control(&button, label);
    button
}

/// Build an icon-only menu button with tooltip and accessible label set together.
#[must_use]
pub fn icon_menu_button(icon_name: &str, label: &str, classes: &[&str]) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .icon_name(icon_name)
        .css_classes(classes)
        .build();
    name_control(&button, label);
    button
}

/// Build a menu button with app-owned child content and a shared tooltip and
/// accessible label policy.
#[must_use]
pub fn custom_menu_button(
    child: &impl IsA<gtk::Widget>,
    label: &str,
    classes: &[&str],
) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .child(child)
        .css_classes(classes)
        .build();
    name_control(&button, label);
    button
}

/// Build a button with an icon, text label, tooltip, and accessible label.
#[must_use]
pub fn labeled_icon_button(
    icon_name: &str,
    label: &str,
    tooltip: &str,
    classes: &[&str],
) -> gtk::Button {
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    inner.append(&gtk::Image::from_icon_name(icon_name));
    inner.append(&gtk::Label::new(Some(label)));

    let button = gtk::Button::builder()
        .child(&inner)
        .css_classes(classes)
        .build();
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}
