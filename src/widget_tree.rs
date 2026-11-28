// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Walking the widgets inside a widget.

use gtk::prelude::*;

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

    #[gtk::test]
    #[ignore = "needs a display"]
    fn walk_is_depth_first_preorder() {
        gtk::init().expect("a display");
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
    }

    #[gtk::test]
    #[ignore = "needs a display"]
    fn loop_may_remove_child() {
        gtk::init().expect("a display");
        let parent = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for name in ["a", "b", "c"] {
            parent.append(&named(name, &[]));
        }
        for child in children(&parent) {
            parent.remove(&child);
        }
        assert!(parent.first_child().is_none());
    }
}
