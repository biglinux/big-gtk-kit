// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Flat icon-button constructors with accessibility set by construction.
//!
//! Callers receive a plain [`gtk::Button`], [`gtk::ToggleButton`], or
//! [`gtk::MenuButton`] carrying the `flat` CSS class, a tooltip, and an
//! accessible label set via [`gtk::accessible::Property::Label`], so screen
//! readers announce the icon-only control.
//!
//! Callers own i18n: pass an already-translated `label`.

use gtk::prelude::*;

/// Flat icon button wired to `action`, with tooltip and accessible label.
///
/// `icon` is a symbolic icon name, `label` an already-translated string used
/// for both the tooltip and the AT-SPI accessible label, and `action` the full
/// action name (for example `"win.search-files"` or `"app.quit"`).
pub fn flat_icon_button(icon: &str, label: &str, action: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .css_classes(["flat"])
        .build();
    button.set_action_name(Some(action));
    crate::feedback::tooltip::name_control(&button, label);
    button
}

/// Flat icon toggle button wired to `action`, with tooltip and accessible label.
///
/// Same contract as [`flat_icon_button`] but returns a [`gtk::ToggleButton`]
/// for stateful controls (for example a view mode or a panel visibility toggle).
pub fn flat_icon_toggle(icon: &str, label: &str, action: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder()
        .icon_name(icon)
        .css_classes(["flat"])
        .build();
    button.set_action_name(Some(action));
    crate::feedback::tooltip::name_control(&button, label);
    button
}

/// Flat icon menu button with tooltip and accessible label.
///
/// Unlike the action variants this takes no action name: menu buttons carry a
/// menu model or popover, which the caller attaches afterwards (for example via
/// [`gtk::MenuButton::set_menu_model`] or
/// [`gtk::MenuButton::set_popover`]).
pub fn flat_icon_menu_button(icon: &str, label: &str) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .icon_name(icon)
        .css_classes(["flat"])
        .build();
    crate::feedback::tooltip::name_control(&button, label);
    button
}
