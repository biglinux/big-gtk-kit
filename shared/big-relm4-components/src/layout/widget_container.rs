// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Walking and clearing the widgets inside a container.

use adw::prelude::*;
use relm4::gtk;

/// The widgets directly under `widget`, first to last. Each sibling is read
/// before its widget is yielded, so the loop may remove the widget it holds.
pub fn children<W: IsA<gtk::Widget>>(widget: &W) -> impl Iterator<Item = gtk::Widget> + use<W> {
    std::iter::successors(widget.first_child(), WidgetExt::next_sibling)
}

/// Every widget under `widget`, depth first, each before its children;
/// `widget` itself is left out.
pub fn descendants<W: IsA<gtk::Widget>>(widget: &W) -> impl Iterator<Item = gtk::Widget> + use<W> {
    let root = widget.clone().upcast::<gtk::Widget>();
    let mut next = root.first_child();
    std::iter::from_fn(move || {
        let current = next.take()?;
        next = current.first_child().or_else(|| {
            let mut climbed = current.clone();
            loop {
                if let Some(sibling) = climbed.next_sibling() {
                    return Some(sibling);
                }
                climbed = climbed.parent().filter(|parent| *parent != root)?;
            }
        });
        Some(current)
    })
}

/// `widget` and every widget under it, in the order of [`descendants`].
pub fn subtree<W: IsA<gtk::Widget>>(widget: &W) -> impl Iterator<Item = gtk::Widget> + use<W> {
    std::iter::once(widget.clone().upcast()).chain(descendants(widget))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, inside: &[&gtk::Widget]) -> gtk::Widget {
        let widget = gtk::Box::builder().name(name).build();
        for child in inside {
            widget.append(*child);
        }
        widget.upcast()
    }

    fn names(widgets: impl Iterator<Item = gtk::Widget>) -> Vec<String> {
        widgets
            .map(|widget| widget.widget_name().to_string())
            .collect()
    }

    #[test]
    fn walk_is_depth_first_preorder() {
        big_testkit::harness::require_gtk_test(|| {
            let a = named(
                "a",
                &[&named("a1", &[&named("a1x", &[])]), &named("a2", &[])],
            );
            let root = named(
                "root",
                &[&a, &named("b", &[]), &named("c", &[&named("c1", &[])])],
            );
            assert_eq!(names(children(&root)), ["a", "b", "c"]);
            assert_eq!(
                names(descendants(&root)),
                ["a", "a1", "a1x", "a2", "b", "c", "c1"]
            );
            assert_eq!(
                names(descendants(&a)),
                ["a1", "a1x", "a2"],
                "stays inside its root"
            );
            assert_eq!(names(subtree(&a)), ["a", "a1", "a1x", "a2"]);
            let leaf = named("leaf", &[]);
            assert_eq!(descendants(&leaf).count(), 0);
        });
    }

    #[test]
    fn loop_may_remove_child() {
        big_testkit::harness::require_gtk_test(|| {
            let parent = gtk::Box::new(gtk::Orientation::Vertical, 0);
            for name in ["a", "b", "c"] {
                parent.append(&named(name, &[]));
            }
            for child in children(&parent) {
                parent.remove(&child);
            }
            assert!(parent.first_child().is_none());
        });
    }
}
