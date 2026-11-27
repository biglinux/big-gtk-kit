// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Menu rows that are more than a label: a strip of side-by-side buttons, a
//! switch row and a slider. Each is still driven by the `GMenuModel` item it
//! came from, so the model remains what tests walk and what the screen reader
//! names; only the drawing differs from a plain row.

use std::cell::Cell;

use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::glib::variant::ToVariant;
use relm4::gtk::prelude::*;

use super::action_row::{
    MENU_ROW_CSS_CLASS, activate_detailed_action, bind_hierarchy_action,
    insert_inheritable_action_group, leading_icon, name_model_accel, parse_detailed_action,
    text_column,
};
use super::{
    BigMenuActionItem, BigMenuActionSelection, StructuredMenuBuild, action_row,
    menu_model_action_item, menu_model_door_item, menu_model_label_attribute,
    resolve_action_presentation,
};

/// Marks a row whose focus lives in a descendant and which owns its own
/// horizontal arrow keys. The page's keyboard navigation treats one as a
/// single Up/Down stop and leaves Left/Right to it.
pub(super) const RICH_ROW_CSS_CLASS: &str = "big-menu-rich-row";
/// Marks the strip that is a page's header band rather than one of its rows.
pub(super) const HEADER_STRIP_CSS_CLASS: &str = "big-menu-header-strip";
/// Marks the caption over a slider, which starts in the page's label column.
pub(super) const SCALE_CAPTION_CSS_CLASS: &str = "big-menu-scale-caption";
/// The rows and strips of one page share this inset with the plain rows'
/// own padding, so their edges line up.
const ROW_INSET: i32 = 12;

/// Where a strip sits, which is the whole difference in how it is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StripPlacement {
    /// One of the page's rows: a linked card, inset like every other row,
    /// when its buttons are a choice (radios); one row cut by rules into its
    /// verbs, each icon beside its name, when they are actions. A choice is
    /// one control and looks like one; verbs side by side are one row.
    Row,
    /// The band across the top of the page: edge to edge, split by vertical
    /// rules, with no outline of its own.
    Header,
}

/// One row of side-by-side icon-and-label buttons, one per item of `section`.
///
/// A bound radio action becomes a toggle that shows the current choice and
/// leaves the menu open; anything else is a plain button that closes it.
pub(super) fn build_strip(
    build: &StructuredMenuBuild<'_>,
    section: &gio::MenuModel,
    placement: StripPlacement,
) -> gtk::Box {
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    strip.add_css_class(RICH_ROW_CSS_CLASS);
    strip.set_homogeneous(true);
    strip.set_hexpand(true);
    strip.set_accessible_role(gtk::AccessibleRole::Group);
    let is_choice = (0..section.n_items()).any(|index| {
        menu_model_action_item(section, index).is_some_and(|mut item| {
            resolve_action_presentation(&mut item, build.action_groups);
            matches!(item.selection, BigMenuActionSelection::Radio { .. })
        })
    });
    let verbs = placement == StripPlacement::Row && !is_choice;
    match placement {
        StripPlacement::Row if verbs => strip.add_css_class("big-menu-verb-row"),
        StripPlacement::Row => {
            strip.add_css_class("linked");
            strip.set_margin_start(ROW_INSET);
            strip.set_margin_end(ROW_INSET);
        }
        StripPlacement::Header => strip.add_css_class(HEADER_STRIP_CSS_CLASS),
    }

    for index in 0..section.n_items() {
        // A submenu in a strip is a door to the variants of the strip's verbs
        // ("Copy/Move ▸"), opening beside it like any submenu.
        if let (Some(submenu), Some(door)) = (
            section.item_link(index, "submenu"),
            menu_model_door_item(section, index),
        ) {
            let content = if verbs {
                verb_content(&door)
            } else {
                strip_content(&door, placement)
            };
            let arrow = gtk::Image::from_icon_name("go-next-symbolic");
            arrow.set_pixel_size(12);
            arrow.set_accessible_role(gtk::AccessibleRole::Presentation);
            if let Some(label) = content.last_child() {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                content.remove(&label);
                line.append(&label);
                line.append(&arrow);
                content.append(&line);
            }
            let button = gtk::Button::builder().child(&content).build();
            super::cascade::attach(build, &button, &door, &submenu);
            place_in_strip(&strip, button.upcast_ref(), placement, verbs);
            continue;
        }
        let Some(mut item) = menu_model_action_item(section, index) else {
            continue;
        };
        let action_group = resolve_action_presentation(&mut item, build.action_groups);
        let content = if verbs {
            verb_content(&item)
        } else {
            strip_content(&item, placement)
        };
        let button = action_button(build, &item, action_group, &content);
        place_in_strip(&strip, &button, placement, verbs);
    }
    strip
}

/// Add `button` to `strip`: a choice's buttons side by side, a header's and
/// a row of verbs' split by vertical rules.
fn place_in_strip(strip: &gtk::Box, button: &gtk::Widget, placement: StripPlacement, verbs: bool) {
    if placement == StripPlacement::Row && !verbs {
        strip.append(button);
        return;
    }
    button.add_css_class("flat");
    button.set_hexpand(true);
    if verbs {
        // A menu row's height and weight, not a push button's bold.
        button.add_css_class("body");
        button.add_css_class(MENU_ROW_CSS_CLASS);
    }
    // The rule goes in the cell, not between cells: a separator that were a
    // child of a homogeneous box would claim a button's width.
    let cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if strip.first_child().is_some() {
        cell.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    }
    cell.append(button);
    strip.append(&cell);
}

/// A button for `item` holding `content`: a bound radio becomes a toggle that
/// leaves the menu open, anything else runs its action and closes it.
fn action_button(
    build: &StructuredMenuBuild<'_>,
    item: &BigMenuActionItem,
    action_group: Option<(&str, &gio::ActionGroup)>,
    content: &impl IsA<gtk::Widget>,
) -> gtk::Widget {
    let button: gtk::Widget = match item.selection {
        BigMenuActionSelection::Radio { .. } => {
            let toggle = gtk::ToggleButton::builder().child(content).build();
            bind_hierarchy_action(&toggle, item);
            if let Some((prefix, group)) = action_group {
                insert_inheritable_action_group(&toggle, prefix, group);
            }
            toggle.upcast()
        }
        BigMenuActionSelection::None | BigMenuActionSelection::Checkbox { .. } => {
            let button = gtk::Button::builder().child(content).build();
            let weak_popover = build.popover.downgrade();
            match (action_group, build.action_context) {
                (Some((prefix, group)), _) => {
                    bind_hierarchy_action(&button, item);
                    insert_inheritable_action_group(&button, prefix, group);
                    button.connect_clicked(move |_| popdown(&weak_popover));
                }
                (None, Some(action_context)) => {
                    button.set_sensitive(item.enabled);
                    let action = item.action.clone();
                    let weak_action_context = action_context.downgrade();
                    button.connect_clicked(move |_| {
                        popdown(&weak_popover);
                        if let Some(action_context) = weak_action_context.upgrade() {
                            let _ = activate_detailed_action(&action_context, &action);
                        }
                    });
                }
                (None, None) => {
                    bind_hierarchy_action(&button, item);
                    button.connect_clicked(move |_| popdown(&weak_popover));
                }
            }
            button.upcast()
        }
    };
    button.update_property(&[gtk::accessible::Property::Label(&item.label)]);
    if let Some(subtitle) = &item.subtitle {
        button.update_property(&[gtk::accessible::Property::Description(subtitle)]);
    }
    name_model_accel(&button, item);
    action_row::say_why_off(&button, item);
    button
}

/// One row holding two variants of a verb: the first fills the row as an
/// ordinary menu row, the second is a smaller button at its end.
///
/// "Move to Trash" is what a person wants nearly every time and can be undone;
/// "Delete Permanently…" is the same intent, stronger, and asks first. Side by
/// side they read as one decision, and the stronger one is never the big
/// target under the pointer.
pub(super) fn build_split(build: &StructuredMenuBuild<'_>, section: &gio::MenuModel) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class(RICH_ROW_CSS_CLASS);
    row.add_css_class("big-menu-split-row");
    row.set_accessible_role(gtk::AccessibleRole::Group);
    for index in 0..section.n_items() {
        let Some(mut item) = menu_model_action_item(section, index) else {
            continue;
        };
        let action_group = resolve_action_presentation(&mut item, build.action_groups);
        if row.first_child().is_none() {
            let main = action_row(build, &item, action_group);
            main.set_hexpand(true);
            row.append(&main);
            continue;
        }
        // The same type as the row it shares: a smaller, paler line beside a
        // full one reads as a footnote.
        let label = gtk::Label::new(Some(&item.label));
        if item.destructive {
            label.add_css_class("error");
        }
        let variant = action_button(build, &item, action_group, &label);
        variant.add_css_class("flat");
        variant.add_css_class("body");
        variant.add_css_class(MENU_ROW_CSS_CLASS);
        variant.set_valign(gtk::Align::Fill);
        variant.set_tooltip_text(item.subtitle.as_deref());
        // The menu's separators keep their distance from the edges, which
        // makes this one a short rule inside the row.
        let rule = gtk::Separator::new(gtk::Orientation::Vertical);
        rule.set_margin_start(4);
        rule.set_margin_end(4);
        row.append(&rule);
        row.append(&variant);
    }
    row
}

/// One row named by the section's label with its choices along it: an icon
/// button per application, a swatch per color, and the section's submenu, if
/// it has one, as a door at the end.
///
/// A choice among three applications, or eight colors, is one glance and one
/// click instead of a submenu; the door keeps the rest one step away.
pub(super) fn build_inline(
    build: &StructuredMenuBuild<'_>,
    parent: &gio::MenuModel,
    parent_index: i32,
    section: &gio::MenuModel,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.add_css_class(RICH_ROW_CSS_CLASS);
    row.add_css_class("big-menu-inline-row");
    // Where a plain row's icon sits: the button's own padding, not the inset.
    row.set_margin_start(ROW_INSET - 2);
    // Its door's arrow ends where every other row's arrow and shortcut end.
    row.set_margin_end(0);
    row.set_accessible_role(gtk::AccessibleRole::Group);
    let name = menu_model_label_attribute(parent, parent_index).unwrap_or_default();
    row.update_property(&[gtk::accessible::Property::Label(&name)]);
    let primary = (0..section.n_items()).find(|index| {
        section
            .item_attribute_value(*index, super::BIG_PRIMARY_ATTRIBUTE, None)
            .and_then(|value| value.get::<bool>())
            .unwrap_or(false)
    });
    match primary.and_then(|index| menu_model_action_item(section, index)) {
        // The row runs its own verb: "Open" fills it, the applications that
        // can open instead sit at its end.
        Some(mut item) => {
            row.set_margin_start(0);
            let action_group = resolve_action_presentation(&mut item, build.action_groups);
            let main = action_row(build, &item, action_group);
            main.set_hexpand(true);
            row.append(&main);
        }
        None => {
            if let Some(icon) = parent
                .item_attribute_value(parent_index, super::BIG_ICON_ATTRIBUTE, None)
                .and_then(|value| value.get::<String>())
            {
                row.append(&leading_icon(&icon, false));
            }
            let title = gtk::Label::new(Some(&name));
            title.set_xalign(0.0);
            title.set_hexpand(true);
            title.set_margin_start(6);
            title.set_margin_end(8);
            row.append(&title);
        }
    }
    for index in 0..section.n_items() {
        if Some(index) == primary {
            continue;
        }
        if let (Some(submenu), Some(door)) = (
            section.item_link(index, "submenu"),
            menu_model_door_item(section, index),
        ) {
            let arrow = gtk::Image::from_icon_name("go-next-symbolic");
            let button = gtk::Button::builder()
                .child(&arrow)
                .css_classes(["flat"])
                .build();
            button.set_tooltip_text(Some(&door.label));
            button.set_accessible_role(gtk::AccessibleRole::MenuItem);
            button.update_property(&[gtk::accessible::Property::Label(&door.label)]);
            super::cascade::attach(build, &button, &door, &submenu);
            row.append(&button);
            continue;
        }
        let Some(mut item) = menu_model_action_item(section, index) else {
            continue;
        };
        let action_group = resolve_action_presentation(&mut item, build.action_groups);
        let content: gtk::Widget = match (&item.color, &item.leading_icon_name) {
            (Some(color), _) => swatch(color).upcast(),
            (None, Some(icon)) => {
                let image = leading_icon(icon, item.destructive);
                image.set_pixel_size(20);
                image.upcast()
            }
            (None, None) => gtk::Label::new(Some(&item.label)).upcast(),
        };
        let button = action_button(build, &item, action_group, &content);
        button.add_css_class("flat");
        button.add_css_class("circular");
        button.set_tooltip_text(Some(&item.label));
        button.set_valign(gtk::Align::Center);
        row.append(&button);
    }
    row
}

/// A round swatch of `#rrggbb`, 16 pixels across.
fn swatch(color: &str) -> gtk::DrawingArea {
    let fill = gtk::gdk::RGBA::parse(color).unwrap_or(gtk::gdk::RGBA::BLACK);
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    area.set_accessible_role(gtk::AccessibleRole::Presentation);
    area.set_draw_func(move |_, cr, width, height| {
        let radius = f64::from(width.min(height)) / 2.0;
        cr.arc(
            f64::from(width) / 2.0,
            f64::from(height) / 2.0,
            radius - 0.5,
            0.0,
            std::f64::consts::TAU,
        );
        cr.set_source_rgba(
            f64::from(fill.red()),
            f64::from(fill.green()),
            f64::from(fill.blue()),
            f64::from(fill.alpha()),
        );
        let _ = cr.fill_preserve();
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
    });
    area
}

/// An action's icon beside its name, in the row's own text color: a verb
/// sharing a line with its neighbors reads like the rows above and below it.
fn verb_content(item: &BigMenuActionItem) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_halign(gtk::Align::Center);
    if let Some(icon_name) = &item.leading_icon_name {
        content.append(&leading_icon(icon_name, item.destructive));
    }
    let label = gtk::Label::new(Some(&item.label));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&label);
    content
}

fn strip_content(item: &BigMenuActionItem, placement: StripPlacement) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_halign(gtk::Align::Center);
    if placement == StripPlacement::Header {
        content.set_margin_top(6);
        content.set_margin_bottom(6);
    }
    if let Some(icon_name) = &item.leading_icon_name {
        content.append(&leading_icon(icon_name, item.destructive));
    }
    let label = gtk::Label::new(Some(&item.label));
    if item.destructive {
        label.add_css_class("error");
    }
    content.append(&label);
    content
}

fn popdown(popover: &glib::WeakRef<gtk::Popover>) {
    if let Some(popover) = popover.upgrade() {
        popover.popdown();
    }
}

/// A menu row with a switch at its end, bound to a boolean stateful action.
///
/// `GtkSwitch` is actionable, so GTK keeps it in step with the action in both
/// directions. The row is the one focusable, named control (a checkbox menu
/// item to assistive tools), and pressing anywhere on it moves the switch.
pub(super) fn build_switch_row(
    item: &BigMenuActionItem,
    action_group: Option<(&str, &gio::ActionGroup)>,
) -> gtk::Button {
    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);
    switch.set_can_focus(false);
    switch.set_accessible_role(gtk::AccessibleRole::Presentation);
    bind_hierarchy_action(&switch, item);
    if let Some((prefix, group)) = action_group {
        insert_inheritable_action_group(&switch, prefix, group);
    }

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.set_hexpand(true);
    if let Some(icon_name) = &item.leading_icon_name {
        content.append(&leading_icon(icon_name, item.destructive));
    }
    let (column, label) = text_column(item);
    content.append(&column);
    content.append(&switch);

    let row = gtk::Button::builder()
        .child(&content)
        .css_classes(["flat", "body", MENU_ROW_CSS_CLASS])
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .accessible_role(gtk::AccessibleRole::MenuItemCheckbox)
        .build();
    // The switch is what GTK binds to the action, and it goes insensitive
    // with it; the row the person presses goes with the switch.
    switch
        .bind_property("sensitive", &row, "sensitive")
        .sync_create()
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&item.label)]);
    if let Some(subtitle) = &item.subtitle {
        row.update_property(&[gtk::accessible::Property::Description(subtitle)]);
    }
    action_row::say_why_off(row.upcast_ref(), item);
    big_gtk_kit::accessible::relate(
        &row,
        gtk::AccessibleRelation::LabelledBy,
        &[label.upcast_ref()],
    );
    reflect_checked(&row, switch.is_active());
    {
        let weak_row = row.downgrade();
        switch.connect_active_notify(move |switch| {
            if let Some(row) = weak_row.upgrade() {
                reflect_checked(&row, switch.is_active());
            }
        });
    }
    {
        let weak_switch = switch.downgrade();
        row.connect_clicked(move |_| {
            if let Some(switch) = weak_switch.upgrade() {
                big_gtk_kit::switch::set_active(&switch, !switch.is_active());
            }
        });
    }
    row
}

fn reflect_checked(row: &gtk::Button, checked: bool) {
    row.update_state(&[gtk::accessible::State::Checked(if checked {
        gtk::AccessibleTristate::True
    } else {
        gtk::AccessibleTristate::False
    })]);
}

/// A caption over a horizontal slider, reading and writing the numeric state
/// of the action behind `item` through `group`.
pub(super) fn build_scale_row(
    item: &BigMenuActionItem,
    range: Option<(f64, f64, f64)>,
    prefix: &str,
    group: &gio::ActionGroup,
) -> gtk::Box {
    let (minimum, maximum, step) = sane_range(range);
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class(RICH_ROW_CSS_CLASS);
    row.set_margin_start(ROW_INSET);
    row.set_margin_end(ROW_INSET);
    row.set_accessible_role(gtk::AccessibleRole::Group);

    let caption = gtk::Label::new(Some(&item.label));
    caption.set_xalign(0.0);
    caption.add_css_class("dimmed");
    caption.add_css_class("caption");
    caption.add_css_class(SCALE_CAPTION_CSS_CLASS);
    row.append(&caption);

    let scale =
        big_gtk_kit::slider::with_range(gtk::Orientation::Horizontal, minimum, maximum, step);
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    // Long enough to set a value finely in a menu that holds it alone.
    scale.set_size_request(160, -1);
    scale.set_sensitive(item.enabled);
    scale.update_property(&[gtk::accessible::Property::Label(&item.label)]);
    row.append(&scale);

    let Ok((detailed_name, _)) = parse_detailed_action(&item.action) else {
        scale.set_sensitive(false);
        return row;
    };
    let Some(action_name) = detailed_name
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
        .map(str::to_owned)
    else {
        scale.set_sensitive(false);
        return row;
    };
    let Some(state) = group.action_state(&action_name) else {
        scale.set_sensitive(false);
        return row;
    };
    if let Some(value) = number_of(&state) {
        scale.set_value(value);
    }

    // The slider writes the action, the action writes the slider: each write
    // happens with the other direction blocked or the two would chase each
    // other around the same number.
    let state_type = state.type_().to_owned();
    let write_state = {
        let group = group.downgrade();
        let action_name = action_name.clone();
        scale.connect_value_changed(move |scale| {
            let Some(group) = group.upgrade() else {
                return;
            };
            if let Some(value) = number_like(&state_type, scale.value()) {
                group.change_action_state(&action_name, &value);
            }
        })
    };
    let read_state = {
        let weak_scale = scale.downgrade();
        group.connect_action_state_changed(Some(&action_name), move |_, _, state| {
            let (Some(scale), Some(value)) = (weak_scale.upgrade(), number_of(state)) else {
                return;
            };
            if (scale.value() - value).abs() < f64::EPSILON {
                return;
            }
            scale.block_signal(&write_state);
            scale.set_value(value);
            scale.unblock_signal(&write_state);
        })
    };
    // GTK follows an actionable's action into its sensitivity; this slider
    // writes the state by hand, so it follows the action's switch itself.
    let item_enabled = item.enabled;
    scale.set_sensitive(item_enabled && group.is_action_enabled(&action_name));
    let read_enabled = {
        let weak_scale = scale.downgrade();
        group.connect_action_enabled_changed(Some(&action_name), move |_, _, enabled| {
            if let Some(scale) = weak_scale.upgrade() {
                scale.set_sensitive(item_enabled && enabled);
            }
        })
    };
    let group = group.downgrade();
    let handlers = Cell::new(Some([read_state, read_enabled]));
    scale.connect_destroy(move |_| {
        if let (Some(group), Some(handlers)) = (group.upgrade(), handlers.take()) {
            for handler in handlers {
                group.disconnect(handler);
            }
        }
    });
    row
}

fn sane_range(range: Option<(f64, f64, f64)>) -> (f64, f64, f64) {
    let (mut minimum, mut maximum, mut step) = range.unwrap_or((0.0, 1.0, 0.1));
    if !minimum.is_finite() {
        minimum = 0.0;
    }
    if !maximum.is_finite() || maximum <= minimum {
        maximum = minimum + 1.0;
    }
    if !step.is_finite() || step <= 0.0 {
        step = (maximum - minimum) / 10.0;
    }
    (minimum, maximum, step)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a slider position never needs more than f64 precision"
)]
fn number_of(state: &glib::Variant) -> Option<f64> {
    state
        .get::<f64>()
        .or_else(|| state.get::<i32>().map(f64::from))
        .or_else(|| state.get::<u32>().map(f64::from))
        .or_else(|| state.get::<i64>().map(|value| value as f64))
        .or_else(|| state.get::<u64>().map(|value| value as f64))
}

/// `value` in the same variant type the action already holds, so the write
/// is accepted; a rounded integer for the integer types.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the slider is bounded to the action's own range"
)]
fn number_like(state_type: &glib::VariantTy, value: f64) -> Option<glib::Variant> {
    let variant = match state_type {
        ty if ty == glib::VariantTy::DOUBLE => value.to_variant(),
        ty if ty == glib::VariantTy::INT32 => (value.round() as i32).to_variant(),
        ty if ty == glib::VariantTy::UINT32 => (value.round().max(0.0) as u32).to_variant(),
        ty if ty == glib::VariantTy::INT64 => (value.round() as i64).to_variant(),
        ty if ty == glib::VariantTy::UINT64 => (value.round().max(0.0) as u64).to_variant(),
        _ => return None,
    };
    Some(variant)
}
