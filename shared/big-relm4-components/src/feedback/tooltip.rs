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
///
/// The image child carries no `image-button` class, which
/// `ButtonBuilder::icon_name` adds unless `css_classes` replaces it; pass it in
/// `classes` for that narrower padding.
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

/// Build an icon-only toggle button with tooltip and accessible label set
/// together, `active` as it starts. Its classes follow [`icon_button`]'s.
#[must_use]
pub fn icon_toggle_button(
    icon_name: &str,
    label: &str,
    active: bool,
    classes: &[&str],
) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder()
        .child(&gtk::Image::from_icon_name(icon_name))
        .css_classes(classes)
        .active(active)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An image child sized as `ButtonBuilder::icon_name` sizes it: the same
    /// classes, the same measure, with and without `image-button`.
    #[test]
    fn icon_button_matches_icon_name_builder() {
        big_testkit::harness::require_gtk_test(|| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let mut pairs = Vec::new();
            for classes in [&["flat"][..], &["flat", "circular"], &[]] {
                // `css_classes` replaces the `image-button` class that `icon_name` added.
                let replaced = gtk::Button::builder()
                    .icon_name("list-add-symbolic")
                    .css_classes(classes)
                    .build();
                pairs.push((replaced, icon_button("list-add-symbolic", "Add", classes)));
                let kept = gtk::Button::builder()
                    .icon_name("list-add-symbolic")
                    .build();
                for class in classes {
                    kept.add_css_class(class);
                }
                let mut with_image_class = vec!["image-button"];
                with_image_class.extend_from_slice(classes);
                pairs.push((
                    kept,
                    icon_button("list-add-symbolic", "Add", &with_image_class),
                ));
            }
            for (built, owned) in &pairs {
                row.append(built);
                row.append(owned);
            }
            let window = gtk::Window::builder().child(&row).build();
            window.present();
            big_testkit::harness::pump();
            for (built, owned) in &pairs {
                assert_eq!(built.css_classes(), owned.css_classes());
                for orientation in [gtk::Orientation::Horizontal, gtk::Orientation::Vertical] {
                    assert_eq!(
                        built.measure(orientation, -1),
                        owned.measure(orientation, -1),
                        "{:?}",
                        built.css_classes()
                    );
                }
            }
            window.destroy();
        });
    }
}
