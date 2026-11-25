// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! A compact navigation trail shared by file panes and previews.
//!
//! Products supply buttons with their own navigation, middle-click and edit
//! actions. This module owns only fitting, separators and the ancestor menu.

use big_gtk_kit::tooltip::name_control;
use gtk::prelude::*;

/// A trail that keeps the current location visible and moves older locations
/// into an overflow menu. Clones refer to the same widget tree.
#[derive(Clone)]
pub struct BigBreadcrumbBar {
    root: gtk::ScrolledWindow,
    bar: gtk::Box,
    overflow: gtk::MenuButton,
    menu: gtk::Box,
}

impl BigBreadcrumbBar {
    /// `overflow_label` is the translated name of the ancestor-menu action.
    #[must_use]
    pub fn new(overflow_label: &str) -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        // The class GTK's own path bar carries, which themes keep unfaced.
        bar.add_css_class("path-bar");
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let popover = gtk::Popover::builder().child(&menu).build();
        let overflow = gtk::MenuButton::builder()
            .icon_name("view-more-horizontal-symbolic")
            .popover(&popover)
            .visible(false)
            .css_classes(["flat"])
            .build();
        name_control(&overflow, overflow_label);
        bar.append(&overflow);
        let root = gtk::ScrolledWindow::builder()
            .child(&bar)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .hexpand(true)
            .build();
        let trail = Self {
            root,
            bar,
            overflow,
            menu,
        };
        let root = trail.root.downgrade();
        let overflow = trail.overflow.downgrade();
        let menu = trail.menu.downgrade();
        trail.root.hadjustment().connect_page_size_notify(move |_| {
            if let (Some(root), Some(overflow), Some(menu)) =
                (root.upgrade(), overflow.upgrade(), menu.upgrade())
            {
                fit(&overflow, &menu, root.width());
            }
        });
        trail
    }

    /// The mountable widget; it continues fitting after this handle is dropped.
    #[must_use]
    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }

    /// Replace the trail, oldest first. Each label names the corresponding
    /// button in the overflow menu; buttons must be unparented. Menu activation
    /// closes the popover before invoking the original button's clicked signal.
    /// A crumb that had the focus hands it to the new last crumb.
    pub fn set_items(&self, items: Vec<(String, gtk::Button)>) {
        self.overflow.popdown();
        // GTK keeps a removed focused crumb's ancestors pointing at the trail
        // when the focus moves on before it settles, and Tab then enters
        // there, past the controls before the trail.
        let had_focus = self
            .bar
            .focus_child()
            .is_some_and(|child| child != self.overflow);
        while let Some(child) = self.overflow.next_sibling() {
            self.bar.remove(&child);
        }
        while let Some(child) = self.menu.first_child() {
            self.menu.remove(&child);
        }
        for (index, (label, button)) in items.into_iter().enumerate() {
            if index > 0 {
                let separator = gtk::Label::builder()
                    .label("›")
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build();
                self.bar.append(&separator);
            }
            let entry = gtk::Button::builder()
                .label(&label)
                .css_classes(["flat"])
                .build();
            entry.set_tooltip_text(button.tooltip_text().as_deref());
            if let Some(label) = entry.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_max_width_chars(40);
            }
            button
                .bind_property("sensitive", &entry, "sensitive")
                .sync_create()
                .build();
            let original = button.downgrade();
            let overflow = self.overflow.downgrade();
            entry.connect_clicked(move |_| {
                if let Some(overflow) = overflow.upgrade() {
                    overflow.popdown();
                }
                if let Some(button) = original.upgrade() {
                    button.emit_clicked();
                }
            });
            self.bar.append(&button);
            self.menu.append(&entry);
        }
        if had_focus && let Some(last) = self.bar.last_child() {
            last.grab_focus();
        }
        fit(&self.overflow, &self.menu, self.root.width());
    }
}

/// A consistently sized trail button. Its owner connects the action and can
/// add product-specific gestures, accessible labels or custom content.
#[must_use]
pub fn breadcrumb_button(label: &str, tooltip: &str, icon: Option<&str>) -> gtk::Button {
    let text = gtk::Label::builder()
        .label(label)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(28)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if let Some(icon) = icon {
        content.append(&gtk::Image::from_icon_name(icon));
    }
    content.append(&text);
    let button = gtk::Button::builder()
        .child(&content)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn fit(overflow: &gtk::MenuButton, menu: &gtk::Box, available: i32) {
    if available <= 0 {
        return;
    }
    let mut crumbs = Vec::new();
    let mut child = overflow.next_sibling();
    while let Some(widget) = child {
        child = widget.next_sibling();
        widget.set_visible(true);
        crumbs.push(widget);
    }
    overflow.set_visible(false);
    let widths: Vec<_> = crumbs
        .iter()
        .map(|widget| widget.measure(gtk::Orientation::Horizontal, -1).1)
        .collect();
    let mut used: i32 = widths.iter().sum();
    let mut hidden = 0;
    if used > available && crumbs.len() > 1 {
        overflow.set_visible(true);
        used += overflow.measure(gtk::Orientation::Horizontal, -1).1;
        // Drop complete button/separator pairs; never leave a leading separator
        // and never remove the final location, even in a narrow split pane.
        while used > available && hidden + 2 < crumbs.len() {
            used -= widths[hidden] + widths[hidden + 1];
            crumbs[hidden].set_visible(false);
            crumbs[hidden + 1].set_visible(false);
            hidden += 2;
        }
    }
    let mut entry = menu.first_child();
    let mut index = 0;
    while let Some(widget) = entry {
        entry = widget.next_sibling();
        widget.set_visible(index < hidden / 2);
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    /// Replacing the trail while one of its crumbs has the focus leaves Tab
    /// starting from the window's first control, not from the trail.
    #[test]
    fn replaced_focus_keeps_tab_order() {
        big_testkit::harness::require_gtk_test(|| {
            let trail = BigBreadcrumbBar::new("Parent folders");
            let crumbs = |names: &[&str]| {
                names
                    .iter()
                    .map(|name| ((*name).to_owned(), breadcrumb_button(name, name, None)))
                    .collect::<Vec<_>>()
            };
            trail.set_items(crumbs(&["Home", "Folder"]));
            let back = gtk::Button::with_label("Back");
            let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            header.append(&back);
            header.append(trail.widget());
            let listing = gtk::Button::with_label("Listing");
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&header);
            content.append(&listing);
            let window = gtk::Window::builder().child(&content).build();
            window.present();
            assert!(big_testkit::harness::pump_until(|| trail.bar.is_mapped()));
            let first = trail.overflow.next_sibling().unwrap();
            assert!(first.grab_focus());
            // As a pane does on opening a folder: a new trail, then the
            // keyboard to the listing, in one turn of the main loop.
            trail.set_items(crumbs(&["Home", "Folder", "Inner"]));
            assert!(listing.grab_focus());
            big_testkit::harness::pump();
            window.child_focus(gtk::DirectionType::TabForward);
            // `has_focus` also needs the window active, which the shared test
            // session may give another test's window at any time.
            assert!(back.is_focus(), "Tab after the listing reaches Back");
            window.destroy();
        });
    }

    #[test]
    fn overflow_keeps_location_and_actions() {
        big_testkit::harness::require_gtk_test(|| {
            let trail = BigBreadcrumbBar::new("Parent folders");
            let activated = Rc::new(Cell::new(usize::MAX));
            let buttons: Vec<_> = ["Home", "Projects", "A much longer current folder"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| {
                    let button = breadcrumb_button(name, name, None);
                    let activated = activated.clone();
                    button.connect_clicked(move |_| activated.set(index));
                    (name.to_owned(), button)
                })
                .collect();
            trail.set_items(buttons.clone());
            for width in [1000, 200, 1, 1000] {
                fit(&trail.overflow, &trail.menu, width);
                assert!(buttons[2].1.get_visible());
                let mut hidden = 0;
                for (_, button) in &buttons[..2] {
                    let separator = button.next_sibling().unwrap();
                    assert_eq!(button.get_visible(), separator.get_visible());
                    hidden += usize::from(!button.get_visible());
                }
                assert_eq!(trail.overflow.get_visible(), hidden > 0);
                let mut entry = trail.menu.first_child();
                let mut index = 0;
                while let Some(widget) = entry {
                    entry = widget.next_sibling();
                    assert_eq!(widget.get_visible(), index < hidden);
                    index += 1;
                }
            }
            fit(&trail.overflow, &trail.menu, 1);
            trail
                .menu
                .first_child()
                .and_downcast::<gtk::Button>()
                .unwrap()
                .emit_clicked();
            assert_eq!(activated.get(), 0);
            let old = buttons[0].1.downgrade();
            drop(buttons);
            trail.set_items(Vec::new());
            assert!(
                old.upgrade().is_none(),
                "replaced locations must be released"
            );
            let root = trail.widget().downgrade();
            let overflow = trail.overflow.downgrade();
            drop(trail);
            assert!(root.upgrade().is_none());
            assert!(overflow.upgrade().is_none());
        });
    }
}
