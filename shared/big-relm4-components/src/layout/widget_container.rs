// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Walking and clearing the widgets inside a container.

use adw::prelude::*;
use relm4::gtk;

pub use big_gtk_kit::widget_tree::{children, descendants, subtree};

/// Remove every child from a `GtkBox`.
pub fn clear_box_children(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// Remove every child from a `GtkGrid`.
pub fn clear_grid_children(grid: &gtk::Grid) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
}

/// Put the keyboard on the first control inside `container`, depth first.
///
/// Not `gtk_widget_child_focus`, and not `grab_focus` on the first child.
/// `grab_focus` on a `GtkBox` returns false and does not descend, so focusing
/// "the first row" of a hand-built list that way focuses nothing; and
/// `child_focus` returns false whenever the toplevel is not the active window,
/// which is exactly the case for a panel popover that has just been asked to
/// open by a global shortcut.
///
/// `grab_focus` on the descendant itself works in both situations, so the walk
/// is done here. It passes over what is hidden: `grab_focus` takes a control
/// inside a hidden row, which then swallows every key. Returns whether anything
/// took the focus.
pub fn focus_first_control(container: &impl IsA<gtk::Widget>) -> bool {
    fn walk(widget: &gtk::Widget) -> bool {
        if !widget.is_visible() {
            return false;
        }
        if widget.is_focusable() && widget.is_sensitive() && widget.grab_focus() {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if walk(&current) {
                return true;
            }
            child = current.next_sibling();
        }
        false
    }
    let container = container.as_ref();
    let mut child = container.first_child();
    while let Some(current) = child {
        if walk(&current) {
            return true;
        }
        child = current.next_sibling();
    }
    false
}
