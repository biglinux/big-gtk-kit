// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! GMenuModel conversion helpers for explicit action popovers.
//!
//! The model stays the single source of a menu: what tests walk, what carries
//! the accelerators and what the screen reader names. The `big-*` attributes
//! below only say how the structured popover draws an item, so a menu that is
//! rendered by plain `GtkPopoverMenu` still works, minus the richer rows.

use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::glib::variant::ToVariant;
use relm4::gtk::prelude::*;

use super::{BigMenuActionGroup, BigMenuActionGroupBinding, BigMenuActionItem};

/// Attribute naming a rich presentation for a section or an item.
pub const BIG_KIND_ATTRIBUTE: &str = "big-kind";
/// `big-kind` of a section drawn as one row of side-by-side icon-and-label buttons.
pub const BIG_KIND_STRIP: &str = "strip";
/// `big-kind` of an item drawn as a caption over a horizontal slider.
pub const BIG_KIND_SCALE: &str = "scale";
/// `big-kind` of an item drawn as a switch row.
pub const BIG_KIND_SWITCH: &str = "switch";
/// Dimmed text under the label saying what the action does; it wraps.
pub const BIG_SUBTITLE_ATTRIBUTE: &str = "big-subtitle";
/// Why the item is off, said while its action is disabled: the row's tooltip and
/// accessible description then, and nothing while it can run.
pub const BIG_OFF_REASON_ATTRIBUTE: &str = "big-off-reason";
/// Symbolic icon shown before the label.
pub const BIG_ICON_ATTRIBUTE: &str = "big-icon";
/// `(ddd)` minimum, maximum and step of a `scale` item.
pub const BIG_RANGE_ATTRIBUTE: &str = "big-range";
/// Boolean: the row destroys something and is colored as such.
pub const BIG_DESTRUCTIVE_ATTRIBUTE: &str = "big-destructive";
/// `big-kind` of a section drawn as one row: its first item fills the row and
/// its second is a smaller button at the row's end, a variant of the same
/// verb ("Move to Trash" | "Delete Permanently…").
pub const BIG_KIND_SPLIT: &str = "split";
/// `big-kind` of a section drawn as one row named by the section's label, its
/// items small buttons along it (application icons, color swatches) and a
/// submenu among them, if any, a door at its end.
pub const BIG_KIND_INLINE: &str = "inline";
/// Boolean: in an inline row, this item is the row itself (its label and
/// icon fill the row and run it), and the others are the choices beside it
/// ("Open", then the applications it can open with).
pub const BIG_PRIMARY_ATTRIBUTE: &str = "big-primary";
/// `#rrggbb`: the item is drawn as a round swatch of this color.
pub const BIG_COLOR_ATTRIBUTE: &str = "big-color";
/// Other words a person might type for this item, space separated: what the
/// menu's search matches besides the label ("zip rar" for Compress).
pub const BIG_KEYWORDS_ATTRIBUTE: &str = "big-keywords";

/// One action row with an optional icon and a short explanation.
#[must_use]
pub fn described_item(
    label: &str,
    subtitle: Option<&str>,
    icon_name: Option<&str>,
    detailed_action: &str,
) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(detailed_action));
    if let Some(subtitle) = subtitle {
        item.set_attribute_value(BIG_SUBTITLE_ATTRIBUTE, Some(&subtitle.to_variant()));
    }
    if let Some(icon_name) = icon_name {
        item.set_attribute_value(BIG_ICON_ATTRIBUTE, Some(&icon_name.to_variant()));
    }
    item
}

/// Color `item` as destructive. Put it last in its menu.
pub fn mark_destructive(item: &gio::MenuItem) {
    item.set_attribute_value(BIG_DESTRUCTIVE_ATTRIBUTE, Some(&true.to_variant()));
}

/// A switch row bound to a boolean stateful action.
#[must_use]
pub fn switch_item(label: &str, subtitle: Option<&str>, detailed_action: &str) -> gio::MenuItem {
    let item = described_item(label, subtitle, None, detailed_action);
    item.set_attribute_value(BIG_KIND_ATTRIBUTE, Some(&BIG_KIND_SWITCH.to_variant()));
    item
}

/// A caption over a slider bound to a numeric stateful action.
///
/// The slider only appears when the popover is built with a
/// [`super::BigMenuActionGroupBinding`] for the action's prefix, because that
/// is where the number is read from and written to; without one the item is
/// drawn as an ordinary row.
#[must_use]
pub fn scale_item(
    label: &str,
    detailed_action: &str,
    minimum: f64,
    maximum: f64,
    step: f64,
) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(detailed_action));
    item.set_attribute_value(BIG_KIND_ATTRIBUTE, Some(&BIG_KIND_SCALE.to_variant()));
    item.set_attribute_value(
        BIG_RANGE_ATTRIBUTE,
        Some(&(minimum, maximum, step).to_variant()),
    );
    item
}

/// Append `items` to `menu` as one section drawn as a row of side-by-side
/// icon-and-label buttons.
///
/// Items whose action is a bound radio (a stateful action with a target) are
/// toggles that show the current choice and leave the menu open; the others
/// are plain buttons that close it, the way a menu row does.
pub fn append_strip_section(menu: &gio::Menu, heading: Option<&str>, items: &[gio::MenuItem]) {
    let section = gio::Menu::new();
    for item in items {
        section.append_item(item);
    }
    let section_item = gio::MenuItem::new_section(heading, &section);
    section_item.set_attribute_value(BIG_KIND_ATTRIBUTE, Some(&BIG_KIND_STRIP.to_variant()));
    menu.append_item(&section_item);
}

/// Append `primary` and `variant` to `menu` as one row: the everyday verb
/// filling it, and its stronger or rarer variant at its end.
pub fn append_split_section(menu: &gio::Menu, primary: &gio::MenuItem, variant: &gio::MenuItem) {
    let section = gio::Menu::new();
    section.append_item(primary);
    section.append_item(variant);
    let section_item = gio::MenuItem::new_section(None, &section);
    section_item.set_attribute_value(BIG_KIND_ATTRIBUTE, Some(&BIG_KIND_SPLIT.to_variant()));
    menu.append_item(&section_item);
}

/// Append one row named `label` whose `items` are choices along it: small
/// buttons, each an icon (`big-icon`) or a swatch (`big-color`) named by its
/// label. A submenu item among them becomes the door at the row's end, for
/// everything that did not fit.
pub fn append_inline_section(
    menu: &gio::Menu,
    label: &str,
    icon_name: Option<&str>,
    items: &[gio::MenuItem],
) {
    let section = gio::Menu::new();
    for item in items {
        section.append_item(item);
    }
    let section_item = gio::MenuItem::new_section(Some(label), &section);
    section_item.set_attribute_value(BIG_KIND_ATTRIBUTE, Some(&BIG_KIND_INLINE.to_variant()));
    if let Some(icon_name) = icon_name {
        section_item.set_attribute_value(BIG_ICON_ATTRIBUTE, Some(&icon_name.to_variant()));
    }
    menu.append_item(&section_item);
}

/// Make `item` the row an inline section is drawn as (`BIG_PRIMARY_ATTRIBUTE`).
pub fn mark_primary(item: &gio::MenuItem) {
    item.set_attribute_value(BIG_PRIMARY_ATTRIBUTE, Some(&true.to_variant()));
}

/// Words besides the label that find `item` in the menu's search.
pub fn set_keywords(item: &gio::MenuItem, keywords: &str) {
    item.set_attribute_value(BIG_KEYWORDS_ATTRIBUTE, Some(&keywords.to_variant()));
}

/// Collect detailed action names from a menu model recursively.
#[must_use]
pub fn menu_action_names(menu: &impl IsA<gio::MenuModel>) -> Vec<String> {
    let model = menu.as_ref();
    let mut actions = Vec::new();
    for index in 0..model.n_items() {
        if let Some(action) = menu_item_string(model, index, "action") {
            actions.push(action);
        }
        if let Some(section) = model.item_link(index, "section") {
            actions.extend(menu_action_names(&section));
        }
        if let Some(submenu) = model.item_link(index, "submenu") {
            actions.extend(menu_action_names(&submenu));
        }
    }
    actions
}

/// What the structured builder reads of `menu` and `action_groups`, as text:
/// every attribute and link of every item, and whether a group has its action
/// (one that none has is built as a disabled row). Two equal texts build the
/// same rows, so a popover built for one can open again for the other.
#[must_use]
pub fn menu_model_rows_key(
    menu: &impl IsA<gio::MenuModel>,
    action_groups: &[BigMenuActionGroupBinding],
) -> String {
    let mut key = String::new();
    write_rows_key(menu.as_ref(), action_groups, &mut key);
    key
}

fn write_rows_key(
    menu: &gio::MenuModel,
    action_groups: &[BigMenuActionGroupBinding],
    key: &mut String,
) {
    use std::fmt::Write;

    key.push('[');
    for index in 0..menu.n_items() {
        let attributes = menu.iterate_item_attributes(index);
        while let Some((name, value)) = attributes.next() {
            let _ = write!(key, "{name}={value};");
        }
        if let Some(action) = menu_item_string(menu, index, "action") {
            let present = action_groups
                .iter()
                .find_map(|binding| binding.action(&action))
                .is_some_and(|(name, _, group)| group.has_action(name));
            key.push(if present { '+' } else { '-' });
        }
        let links = menu.iterate_item_links(index);
        while let Some((name, linked)) = links.next() {
            let _ = write!(key, "{name}:");
            write_rows_key(&linked, action_groups, key);
        }
        key.push('|');
    }
    key.push(']');
}

/// Flatten a `GMenuModel` into named action rows for accessible popovers.
///
/// Native `GtkPopoverMenu` can expose unnamed `menu item` nodes through AT-SPI
/// in some GTK/portal combinations. This preserves the existing menu model as
/// the source of truth while rendering explicit rows with stable names.
#[must_use]
pub fn flat_action_items_from_menu_model(
    menu: &impl IsA<gio::MenuModel>,
) -> Vec<BigMenuActionItem> {
    let model = menu.as_ref();
    let mut items = Vec::new();
    for index in 0..model.n_items() {
        if let Some(section) = model.item_link(index, "section") {
            items.extend(flat_action_items_from_menu_model(&section));
        }
        if let Some(submenu) = model.item_link(index, "submenu") {
            items.extend(flat_action_items_from_menu_model(&submenu));
        }
        let Some(label) = menu_item_string(model, index, "label") else {
            continue;
        };
        let Some(action_name) = menu_item_string(model, index, "action") else {
            continue;
        };
        let target_value = menu_item_target(model, index);
        let Some(detailed_action) = detailed_action_name(&action_name, target_value.as_ref())
        else {
            continue;
        };
        items.push(BigMenuActionItem::new(label, detailed_action.to_string()));
    }
    items
}

/// Convert a `GMenuModel` into grouped custom action rows.
#[must_use]
pub fn action_groups_from_menu_model(menu: &impl IsA<gio::MenuModel>) -> Vec<BigMenuActionGroup> {
    let mut groups = Vec::new();
    collect_action_groups_from_menu_model(menu.as_ref(), None, &mut groups);
    groups
}

fn menu_item_string(
    menu: &impl IsA<gio::MenuModel>,
    index: i32,
    attribute: &str,
) -> Option<String> {
    menu.item_attribute_value(index, attribute, Some(glib::VariantTy::STRING))
        .and_then(|value| value.get::<String>())
}

fn menu_item_target(menu: &impl IsA<gio::MenuModel>, index: i32) -> Option<glib::Variant> {
    menu.item_attribute_value(index, "target", None)
}

fn collect_action_groups_from_menu_model(
    menu: &gio::MenuModel,
    inherited_heading: Option<String>,
    groups: &mut Vec<BigMenuActionGroup>,
) {
    let mut direct_items = Vec::new();
    for index in 0..menu.n_items() {
        if let Some(section) = menu.item_link(index, "section") {
            let heading =
                menu_model_label_attribute(menu, index).or_else(|| inherited_heading.clone());
            collect_action_groups_from_menu_model(&section, heading, groups);
            continue;
        }
        if let Some(submenu) = menu.item_link(index, "submenu") {
            let heading =
                menu_model_label_attribute(menu, index).or_else(|| inherited_heading.clone());
            let items = flat_action_items_from_menu_model(&submenu);
            if !items.is_empty() {
                groups.push(BigMenuActionGroup::new(heading, items));
            }
            continue;
        }
        if let Some(item) = menu_model_action_item(menu, index) {
            direct_items.push(item);
        }
    }
    if !direct_items.is_empty() {
        groups.push(BigMenuActionGroup::new(inherited_heading, direct_items));
    }
}

pub(super) fn menu_model_label_attribute(menu: &gio::MenuModel, index: i32) -> Option<String> {
    menu.item_attribute_value(index, "label", Some(glib::VariantTy::STRING))
        .and_then(|value| value.get::<String>())
}

pub(super) fn menu_model_action_item(
    menu: &gio::MenuModel,
    index: i32,
) -> Option<BigMenuActionItem> {
    let label = menu_model_label_attribute(menu, index)?;
    let action_name = menu_item_string(menu, index, "action")?;
    let target_value = menu_item_target(menu, index);
    let detailed_action = detailed_action_name(&action_name, target_value.as_ref())?;
    let mut item = BigMenuActionItem::new(label, detailed_action.to_string());
    item.leading_icon_name = menu_item_string(menu, index, BIG_ICON_ATTRIBUTE);
    item.subtitle = menu_item_string(menu, index, BIG_SUBTITLE_ATTRIBUTE);
    item.off_reason = menu_item_string(menu, index, BIG_OFF_REASON_ATTRIBUTE);
    item.color = menu_item_string(menu, index, BIG_COLOR_ATTRIBUTE);
    item.keywords = menu_item_string(menu, index, BIG_KEYWORDS_ATTRIBUTE);
    item.destructive = menu
        .item_attribute_value(
            index,
            BIG_DESTRUCTIVE_ATTRIBUTE,
            Some(glib::VariantTy::BOOLEAN),
        )
        .and_then(|value| value.get::<bool>())
        .unwrap_or(false);
    // The model's own `accel` attribute, which `GtkPopoverMenu` would show: keys
    // the application does not register (a view-bound F2 or Delete).
    item.accel = menu_item_string(menu, index, "accel");
    Some(item)
}

/// Whether the item at `index` is left out of the menu: it says
/// `hidden-when="action-missing"`, as `GtkPopoverMenu` reads it, and the group
/// bound to its action's prefix does not have the action.
pub(super) fn menu_model_hidden(
    menu: &gio::MenuModel,
    index: i32,
    action_groups: &[BigMenuActionGroupBinding],
) -> bool {
    if menu_item_string(menu, index, "hidden-when").as_deref() != Some("action-missing") {
        return false;
    }
    menu_item_string(menu, index, "action").is_some_and(|action| {
        action_groups
            .iter()
            .find_map(|binding| binding.action(&action))
            .is_some_and(|(name, _, group)| !group.has_action(name))
    })
}

/// The label, icon and explanation of a submenu opener, as an item with no
/// action: the submenu is what it opens.
pub(super) fn menu_model_door_item(menu: &gio::MenuModel, index: i32) -> Option<BigMenuActionItem> {
    let label = menu_model_label_attribute(menu, index)?;
    let mut door = BigMenuActionItem::new(label, String::new());
    door.leading_icon_name = menu_item_string(menu, index, BIG_ICON_ATTRIBUTE);
    door.subtitle = menu_item_string(menu, index, BIG_SUBTITLE_ATTRIBUTE);
    door.keywords = menu_item_string(menu, index, BIG_KEYWORDS_ATTRIBUTE);
    Some(door)
}

pub(super) fn menu_model_kind(menu: &gio::MenuModel, index: i32) -> Option<String> {
    menu_item_string(menu, index, BIG_KIND_ATTRIBUTE)
}

pub(super) fn menu_model_range(menu: &gio::MenuModel, index: i32) -> Option<(f64, f64, f64)> {
    menu.item_attribute_value(index, BIG_RANGE_ATTRIBUTE, None)
        .and_then(|value| value.get::<(f64, f64, f64)>())
}

fn detailed_action_name(
    action_name: &str,
    target_value: Option<&glib::Variant>,
) -> Option<glib::GString> {
    gio::Action::name_is_valid(action_name)
        .then(|| gio::Action::print_detailed_name(action_name, target_value))
}
