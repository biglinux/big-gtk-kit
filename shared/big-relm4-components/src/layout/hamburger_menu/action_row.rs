// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

use crate::layout::widget_container::{children, subtree};
use std::cell::Cell;

use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::glib::translate::IntoGlib;
use relm4::gtk::prelude::*;

use super::{BigMenuActionItem, BigMenuActionSelection};
use crate::focus::holds_focus;

const ACTION_ACCELERATOR_CSS_CLASS: &str = "action-accelerator";
/// On a button whose model item carries its own accel.
const MODEL_ACCEL_CSS_CLASS: &str = "model-accel";
/// Every row and strip segment a structured menu draws.
pub(super) const MENU_ROW_CSS_CLASS: &str = "big-menu-row";
/// The check mark a stateful row starts with, as wide as an icon.
const INDICATOR_CSS_CLASS: &str = "selection-indicator";
/// What a row's icon or check mark takes before its label.
const LEADING_SIZE: i32 = 16;
/// Between a row's icon and its label.
const CONTENT_SPACING: i32 = 10;

/// Rows drawn to the measure GTK draws a menu's own (`popover.menu
/// modelbutton`): 32 pixels tall and their text 12 pixels in, so a program's
/// main menu and a text field's menu read as the same kind of thing. Above the
/// application's priority: a program's chrome pads its buttons, libadwaita
/// spaces a toolbar's switches, and a menu hanging from that chrome is inside
/// it.
///
/// The page's margin is the only inset around the rows, as in a plain popover,
/// whose contents have none: `GtkPopoverMenu` pads the box it holds a custom
/// child in as well.
const MENU_CSS: &str = "
button.big-menu-row { min-height: 24px; padding: 4px 12px; }
button.big-menu-row switch { margin-top: 0; margin-bottom: 0; }
popover.menu.big-structured-menu > contents > scrolledwindow > viewport > stack > box,
popover.menu.big-structured-menu > contents > stack > box { padding: 0; }
/* White on the disc, small and bold, whatever colour the row's text is. */
.big-menu-mark image, .big-menu-mark label { color: white; }
.big-menu-mark label { font-size: 8px; font-weight: bold; }
";

pub(super) fn build_action_menu_row(
    action_context: &gtk::Widget,
    popover: &gtk::Popover,
    item: &BigMenuActionItem,
) -> gtk::Button {
    let row = build_action_row(item);
    row.set_sensitive(item.enabled);
    apply_action_row_selection(&row, item.selection);

    let action = item.action.clone();
    let weak_action_context = action_context.downgrade();
    let weak_popover = popover.downgrade();
    row.connect_clicked(move |_| {
        if let Some(popover) = weak_popover.upgrade() {
            popover.popdown();
        }
        if let Some(action_context) = weak_action_context.upgrade() {
            let _ = activate_detailed_action(&action_context, &action);
        }
    });
    row
}

pub(super) fn build_hierarchy_action_menu_row(
    popover: &gtk::Popover,
    item: &BigMenuActionItem,
) -> gtk::Button {
    let row = build_action_row(item);
    bind_hierarchy_action(&row, item);
    apply_action_row_selection(&row, item.selection);

    let weak_popover = popover.downgrade();
    row.connect_clicked(move |_| {
        if let Some(popover) = weak_popover.upgrade() {
            popover.popdown();
        }
    });
    row
}

/// The row that opens a submenu: drawn like an action row (icon, label, one-line explanation) with an arrow where the
/// shortcut would be, so a door and a verb line up in the same menu.
pub(super) fn submenu_door_row(door: &BigMenuActionItem) -> gtk::Button {
    let label = door.label.as_str();
    let row_content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row_content.set_hexpand(true);
    if let Some(icon_name) = &door.leading_icon_name {
        row_content.append(&leading_icon(icon_name, false));
    }
    let (column, title) = text_column(door);
    row_content.append(&column);
    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    arrow.set_accessible_role(gtk::AccessibleRole::Presentation);
    row_content.append(&arrow);

    let row = gtk::Button::builder()
        .child(&row_content)
        .css_classes(["flat", "body", MENU_ROW_CSS_CLASS])
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .build();
    row.set_accessible_role(gtk::AccessibleRole::MenuItem);
    row.update_property(&[
        gtk::accessible::Property::Label(label),
        gtk::accessible::Property::HasPopup(true),
    ]);
    if let Some(subtitle) = &door.subtitle {
        row.update_property(&[gtk::accessible::Property::Description(subtitle)]);
    }
    row.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
    big_gtk_kit::accessible::relate(
        &row,
        gtk::AccessibleRelation::LabelledBy,
        &[title.upcast_ref()],
    );
    row
}

pub(super) fn install_menu_page_keyboard_navigation(page: &gtk::Box) {
    let keyboard = gtk::EventControllerKey::new();
    keyboard.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_page = page.downgrade();
    keyboard.connect_key_pressed(move |_, keyval, _, _| {
        let Some(page) = weak_page.upgrade() else {
            return glib::Propagation::Proceed;
        };
        // Inside a strip or on a slider the arrows are the control's own:
        // Left moves to the previous button or lowers the value, it does not
        // leave the page.
        if focus_is_inside_rich_row(&page) && is_horizontal_arrow(keyval) {
            return glib::Propagation::Proceed;
        }

        let focus_target = match keyval {
            gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => {
                relative_page_menu_item(&page, PageNavigation::Previous)
            }
            gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => {
                relative_page_menu_item(&page, PageNavigation::Next)
            }
            gtk::gdk::Key::Home | gtk::gdk::Key::KP_Home => {
                relative_page_menu_item(&page, PageNavigation::First)
            }
            gtk::gdk::Key::End | gtk::gdk::Key::KP_End => {
                relative_page_menu_item(&page, PageNavigation::Last)
            }
            _ => None,
        };
        focus_target.map_or(glib::Propagation::Proceed, |target| {
            focus_menu_item(&target);
            // GTK draws the focus only once it knows the keyboard is moving it,
            // and it learns that from its own Tab and arrow bindings, which
            // this handler stands in for: without this the arrows would walk an
            // invisible focus.
            if let Some(window) = target.root().and_downcast::<gtk::Window>() {
                window.set_focus_visible(true);
            }
            glib::Propagation::Stop
        })
    });
    page.add_controller(keyboard);
}

#[derive(Clone, Copy)]
enum PageNavigation {
    Previous,
    Next,
    First,
    Last,
}

pub(super) fn is_submenu_forward_key(widget: &gtk::Widget, keyval: gtk::gdk::Key) -> bool {
    match widget.direction() {
        gtk::TextDirection::Rtl => keyval == gtk::gdk::Key::Left,
        _ => keyval == gtk::gdk::Key::Right,
    }
}

pub(super) fn is_submenu_back_key(widget: &gtk::Widget, keyval: gtk::gdk::Key) -> bool {
    match widget.direction() {
        gtk::TextDirection::Rtl => keyval == gtk::gdk::Key::Right,
        _ => keyval == gtk::gdk::Key::Left,
    }
}

fn is_horizontal_arrow(keyval: gtk::gdk::Key) -> bool {
    matches!(
        keyval,
        gtk::gdk::Key::Left
            | gtk::gdk::Key::Right
            | gtk::gdk::Key::KP_Left
            | gtk::gdk::Key::KP_Right
    )
}

/// Whether the window's focus sits in one of `page`'s rich rows (a strip of
/// buttons or a slider), which own their horizontal arrows.
pub(super) fn focus_is_inside_rich_row(page: &impl IsA<gtk::Widget>) -> bool {
    let page = page.as_ref();
    let Some(focused) = page.root().and_then(|root| root.focus()) else {
        return false;
    };
    let mut node = Some(focused);
    while let Some(widget) = node {
        if widget == *page {
            return false;
        }
        if widget.has_css_class(super::rich_row::RICH_ROW_CSS_CLASS) {
            return true;
        }
        node = widget.parent();
    }
    false
}

pub(super) fn focus_first_page_menu_item(stack: &gtk::Stack) {
    let Some(page) = stack.visible_child() else {
        return;
    };
    let mut menu_items = Vec::new();
    collect_page_menu_items(&page, &mut menu_items);
    if let Some(menu_item) = menu_items.first() {
        focus_menu_item(menu_item);
    }
}

/// Focus one Up/Down stop. A plain row is focusable itself; a rich row is a
/// box whose focus lives in a descendant, so the focus is moved into it.
fn focus_menu_item(item: &gtk::Widget) {
    if item.has_css_class(super::rich_row::RICH_ROW_CSS_CLASS) {
        // Its first control, whichever of them held the focus last: GTK's own
        // Tab-forward resumes after the remembered child, which would put the
        // keyboard on a strip's last button when the menu opens.
        if let Some(first) = first_focusable(item) {
            let _ = first.grab_focus();
        } else {
            let _ = item.child_focus(gtk::DirectionType::TabForward);
        }
    } else {
        let _ = item.grab_focus();
    }
}

fn first_focusable(widget: &gtk::Widget) -> Option<gtk::Widget> {
    children(widget).find_map(|child| {
        if child.is::<gtk::Popover>() || !child.is_visible() || !child.is_sensitive() {
            return None;
        }
        if child.is_focusable() && child.can_focus() {
            return Some(child);
        }
        first_focusable(&child)
    })
}

/// The rows Up and Down step through: menu-item buttons, plus each rich row as
/// one stop (focusing a strip lands on its first button, a slider row on its
/// slider).
fn collect_page_menu_items(widget: &gtk::Widget, items: &mut Vec<gtk::Widget>) {
    let is_menu_item = matches!(
        widget.accessible_role(),
        gtk::AccessibleRole::MenuItem
            | gtk::AccessibleRole::MenuItemCheckbox
            | gtk::AccessibleRole::MenuItemRadio
    );
    if (is_menu_item || widget.has_css_class(super::rich_row::RICH_ROW_CSS_CLASS))
        && widget.is_sensitive()
        && widget.is_visible()
    {
        items.push(widget.clone());
        return;
    }
    for node in children(widget) {
        collect_page_menu_items(&node, items);
    }
}

fn relative_page_menu_item(page: &gtk::Box, navigation: PageNavigation) -> Option<gtk::Widget> {
    let mut menu_items = Vec::new();
    collect_page_menu_items(page.upcast_ref(), &mut menu_items);
    if menu_items.is_empty() {
        return None;
    }
    let focused_index = menu_items.iter().position(holds_focus);
    let last_index = menu_items.len() - 1;
    let target_index = match navigation {
        PageNavigation::First => 0,
        PageNavigation::Last => last_index,
        PageNavigation::Previous => focused_index.map_or(last_index, |index| {
            index.checked_sub(1).unwrap_or(last_index)
        }),
        PageNavigation::Next => focused_index.map_or(0, |index| {
            index
                .checked_add(1)
                .filter(|next| *next <= last_index)
                .unwrap_or(0)
        }),
    };
    menu_items.get(target_index).cloned()
}

pub(super) fn install_menu_accelerator_refresh(popover: &gtk::Popover, stack: &gtk::Stack) {
    let weak_stack = stack.downgrade();
    let handler_id = popover.connect_map(move |popover| {
        let Some(stack) = weak_stack.upgrade() else {
            return;
        };
        let Some(application) = popover
            .root()
            .and_downcast::<gtk::Window>()
            .and_then(|window| window.application())
        else {
            return;
        };
        refresh_action_row_accelerators(stack.upcast_ref(), &application);
    });
    let handler_id = Cell::new(Some(handler_id));
    let weak_popover = popover.downgrade();
    stack.connect_destroy(move |_| {
        if let Some(popover) = weak_popover.upgrade()
            && let Some(handler_id) = handler_id.take()
        {
            popover.disconnect(handler_id);
        }
    });
}

/// Show and name the application's primary accelerator on each action row
/// under `widget`, as `GtkModelButton` does: one key, while the application
/// may bind several, two of them under one name (Return and KP_Enter are both
/// "Enter"). A row with its model's own accel keeps it.
pub(super) fn refresh_action_row_accelerators(
    widget: &gtk::Widget,
    application: &gtk::Application,
) {
    for button in subtree(widget).filter_map(|widget| widget.downcast::<gtk::Button>().ok()) {
        if button.has_css_class(MODEL_ACCEL_CSS_CLASS) {
            continue;
        }
        let Some(action_name) = button.action_name() else {
            continue;
        };
        let detailed_action =
            gio::Action::print_detailed_name(&action_name, button.action_target_value().as_ref());
        let primary = application
            .accels_for_action(&detailed_action)
            .into_iter()
            .find_map(|accelerator| gtk::accelerator_parse(&accelerator));
        match primary {
            Some((key, modifiers)) => {
                button.update_property(&[gtk::accessible::Property::KeyShortcuts(
                    &gtk::accelerator_get_accessible_label(key.into_glib(), modifiers),
                )])
            }
            None => button.reset_property(gtk::AccessibleProperty::KeyShortcuts),
        }
        if let Some(shortcut) = descendant_action_accelerator(button.upcast_ref()) {
            shortcut.set_visible(primary.is_some());
            shortcut.set_label(
                &primary
                    .map(|(key, modifiers)| gtk::accelerator_get_label(key, modifiers))
                    .unwrap_or_default(),
            );
        }
    }
}

fn descendant_action_accelerator(widget: &gtk::Widget) -> Option<gtk::Label> {
    subtree(widget)
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .find(|shortcut| shortcut.has_css_class(ACTION_ACCELERATOR_CSS_CLASS))
}

/// The accelerator `item`'s model gives it, parsed.
fn model_accel(item: &BigMenuActionItem) -> Option<(gtk::gdk::Key, gtk::gdk::ModifierType)> {
    item.accel.as_deref().and_then(gtk::accelerator_parse)
}

/// Name `item`'s model accel on `button` in ARIA spelling, as `GtkModelButton`
/// does, and keep the application's table off it.
pub(super) fn name_model_accel(button: &gtk::Widget, item: &BigMenuActionItem) {
    if let Some((key, modifiers)) = model_accel(item) {
        button.update_property(&[gtk::accessible::Property::KeyShortcuts(
            &gtk::accelerator_get_accessible_label(key.into_glib(), modifiers),
        )]);
        button.add_css_class(MODEL_ACCEL_CSS_CLASS);
    }
}

/// Give `widget` the `prefix` action group, unless the group is itself a widget.
///
/// `gtk_widget_insert_action_group` takes a strong reference. When the group
/// is a window and the widget is a row of that window's own menu, the result is
/// `window → menu button → popover → row → window`: a cycle GTK cannot break,
/// which pins the whole window and everything under it. A widget group is
/// already reached by inheritance up the row's own ancestry (GTK resolves
/// `win.open` from any descendant), so the insertion buys nothing there and
/// costs the window. Groups that are not widgets (a `SimpleActionGroup`, the
/// application) are not inherited and are inserted.
pub(super) fn insert_inheritable_action_group(
    widget: &impl IsA<gtk::Widget>,
    prefix: &str,
    group: &gio::ActionGroup,
) {
    if group.is::<gtk::Widget>() {
        return;
    }
    widget.as_ref().insert_action_group(prefix, Some(group));
}

pub(super) fn build_action_group_menu_row(
    popover: &gtk::Popover,
    item: &BigMenuActionItem,
    prefix: &str,
    action_group: &gio::ActionGroup,
) -> gtk::Button {
    let row = build_action_row(item);
    insert_inheritable_action_group(&row, prefix, action_group);
    bind_hierarchy_action(&row, item);
    apply_action_row_selection(&row, item.selection);
    bind_action_row_selection(&row, item, prefix, action_group);

    let weak_popover = popover.downgrade();
    row.connect_clicked(move |_| {
        if let Some(popover) = weak_popover.upgrade() {
            popover.popdown();
        }
    });
    row
}

fn bind_action_row_selection(
    row: &gtk::Button,
    item: &BigMenuActionItem,
    prefix: &str,
    action_group: &gio::ActionGroup,
) {
    if matches!(item.selection, BigMenuActionSelection::None) {
        return;
    }
    let Ok((detailed_name, target)) = parse_detailed_action(&item.action) else {
        return;
    };
    let Some(action_name) = detailed_name.strip_prefix(&format!("{prefix}.")) else {
        return;
    };
    let is_radio = matches!(item.selection, BigMenuActionSelection::Radio { .. });
    let weak_row = row.downgrade();
    let handler_id =
        action_group.connect_action_state_changed(Some(action_name), move |_, _, state| {
            let Some(row) = weak_row.upgrade() else {
                return;
            };
            let selection = if is_radio {
                BigMenuActionSelection::Radio {
                    is_selected: target.as_ref().is_some_and(|target| target == state),
                }
            } else {
                BigMenuActionSelection::Checkbox {
                    is_checked: state.get::<bool>().unwrap_or(false),
                }
            };
            apply_action_row_selection(&row, selection);
        });
    // Weak group. A strong clone here outlives nothing useful and keeps the
    // group alive from a row inside it: the same cycle the insertion below
    // avoids. Disconnecting a group that is already gone is not needed.
    let action_group = action_group.downgrade();
    let handler_id = Cell::new(Some(handler_id));
    row.connect_destroy(move |_| {
        if let (Some(action_group), Some(handler_id)) = (action_group.upgrade(), handler_id.take())
        {
            action_group.disconnect(handler_id);
        }
    });
}

fn build_action_row(item: &BigMenuActionItem) -> gtk::Button {
    let row_content = build_action_row_content(item);
    let label = row_content.label.clone();

    let row = gtk::Button::builder()
        .child(&row_content.root)
        .css_classes(["flat", "body", MENU_ROW_CSS_CLASS])
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .accessible_role(match item.selection {
            BigMenuActionSelection::None => gtk::AccessibleRole::Button,
            BigMenuActionSelection::Checkbox { .. } => gtk::AccessibleRole::MenuItemCheckbox,
            BigMenuActionSelection::Radio { .. } => gtk::AccessibleRole::MenuItemRadio,
        })
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&item.label)]);
    if let Some(subtitle) = &item.subtitle {
        row.update_property(&[gtk::accessible::Property::Description(subtitle)]);
    }
    name_model_accel(row.upcast_ref(), item);
    say_why_off(row.upcast_ref(), item);
    big_gtk_kit::accessible::relate(
        &row,
        gtk::AccessibleRelation::LabelledBy,
        &[label.upcast_ref()],
    );
    row
}

/// Say `item`'s off reason as `widget`'s tooltip and accessible description
/// while it is insensitive. Sensitivity follows the action's enabled state, so
/// the reason follows it.
pub(super) fn say_why_off(widget: &gtk::Widget, item: &BigMenuActionItem) {
    let Some(reason) = item.off_reason.clone() else {
        return;
    };
    let subtitle = item.subtitle.clone();
    let explain = move |widget: &gtk::Widget| {
        if widget.is_sensitive() {
            widget.set_tooltip_text(None);
            match &subtitle {
                Some(subtitle) => {
                    widget.update_property(&[gtk::accessible::Property::Description(subtitle)]);
                }
                None => widget.reset_property(gtk::AccessibleProperty::Description),
            }
        } else {
            widget.set_tooltip_text(Some(&reason));
            widget.update_property(&[gtk::accessible::Property::Description(&reason)]);
        }
    };
    explain(widget);
    widget.connect_sensitive_notify(explain);
}

pub(super) fn bind_hierarchy_action(row: &impl IsA<gtk::Actionable>, item: &BigMenuActionItem) {
    let row = row.as_ref();
    if !item.enabled {
        row.set_sensitive(false);
        return;
    }

    let Ok((action_name, target)) = parse_detailed_action(&item.action) else {
        row.set_sensitive(false);
        return;
    };
    // Set the target before the name so an already-mounted action group never
    // observes a transient parameterized action with a null target.
    row.set_action_target_value(target.as_ref());
    row.set_action_name(Some(&action_name));
}

/// The icon, label, subtitle and shortcut of one menu row, laid out for a
/// horizontal row: icon, then a text column, then the trailing shortcut.
pub(super) fn text_column(item: &BigMenuActionItem) -> (gtk::Box, gtk::Label) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_hexpand(true);
    column.set_valign(gtk::Align::Center);
    let label = gtk::Label::new(Some(&item.label));
    label.set_xalign(0.0);
    if item.destructive {
        label.add_css_class("error");
    }
    column.append(&label);
    if let Some(subtitle) = &item.subtitle {
        let subtitle = gtk::Label::new(Some(subtitle));
        subtitle.set_xalign(0.0);
        // An ellipsized label asks for its whole text, which would widen the
        // popover to the longest reason; wrapping caps the width instead.
        subtitle.set_wrap(true);
        subtitle.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        subtitle.set_max_width_chars(44);
        subtitle.add_css_class("dimmed");
        subtitle.add_css_class("caption");
        // Kept in the accessible tree, where it tells the subtitle in the row's
        // description apart from the reason an off row gives there.
        column.append(&subtitle);
    }
    (column, label)
}

pub(super) fn leading_icon(icon_name: &str, destructive: bool) -> gtk::Image {
    // Menus other programs install (service menus, Nemo actions) may name an
    // icon by its file.
    let icon = if icon_name.starts_with('/') {
        gtk::Image::from_gicon(&gio::FileIcon::new(&gio::File::for_path(icon_name)))
    } else if icon_name.starts_with(". ") {
        // An application's icon as `g_icon_to_string` writes a themed icon
        // with fallbacks (". GThemedIcon org.app org.app-symbolic").
        gio::Icon::for_string(icon_name).map_or_else(
            |_| gtk::Image::from_icon_name("application-x-executable"),
            |icon| gtk::Image::from_gicon(&icon),
        )
    } else {
        gtk::Image::from_icon_name(icon_name)
    };
    icon.set_pixel_size(LEADING_SIZE);
    icon.set_valign(gtk::Align::Center);
    if destructive {
        icon.add_css_class("error");
    }
    icon
}

struct ActionRowContent {
    root: gtk::Box,
    label: gtk::Label,
}

fn build_action_row_content(action_item: &BigMenuActionItem) -> ActionRowContent {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(CONTENT_SPACING)
        .hexpand(true)
        .build();
    if !matches!(action_item.selection, BigMenuActionSelection::None) {
        let is_selected = match action_item.selection {
            BigMenuActionSelection::Checkbox { is_checked } => is_checked,
            BigMenuActionSelection::Radio { is_selected } => is_selected,
            BigMenuActionSelection::None => false,
        };
        let indicator = gtk::Image::from_icon_name("object-select-symbolic");
        indicator.add_css_class(INDICATOR_CSS_CLASS);
        indicator.set_pixel_size(LEADING_SIZE);
        indicator.set_opacity(if is_selected { 1.0 } else { 0.0 });
        indicator.set_accessible_role(gtk::AccessibleRole::Presentation);
        root.append(&indicator);
    } else if let Some(colour) = &action_item.color {
        root.append(&mark(colour, action_item));
    } else if let Some(icon_name) = &action_item.leading_icon_name {
        root.append(&leading_icon(icon_name, action_item.destructive));
    }

    let (column, label) = text_column(action_item);
    root.append(&column);

    // The row's `KEY_SHORTCUTS` says the key to the screen reader.
    let shortcut = gtk::Label::new(None);
    shortcut.add_css_class("dimmed");
    shortcut.set_xalign(1.0);
    shortcut.set_accessible_role(gtk::AccessibleRole::Presentation);
    match model_accel(action_item) {
        Some((key, modifiers)) => shortcut.set_label(&gtk::accelerator_get_label(key, modifiers)),
        None => {
            shortcut.add_css_class(ACTION_ACCELERATOR_CSS_CLASS);
            shortcut.set_visible(false);
        }
    }
    root.append(&shortcut);

    ActionRowContent { root, label }
}

/// A row's own mark in place of its icon: a disc of `colour` carrying the
/// item's symbol, or the first letter of its name when it has none. How a tag
/// is drawn in a file manager's sidebar and on its files, so the menu names a
/// tag by the same picture.
fn mark(colour: &str, item: &BigMenuActionItem) -> gtk::Overlay {
    let fill = gtk::gdk::RGBA::parse(colour).unwrap_or(gtk::gdk::RGBA::BLACK);
    let disc = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .build();
    disc.set_draw_func(move |_, cr, width, height| {
        let radius = f64::from(width.min(height)) / 2.0;
        cr.arc(
            f64::from(width) / 2.0,
            f64::from(height) / 2.0,
            radius,
            0.0,
            std::f64::consts::TAU,
        );
        cr.set_source_rgba(
            f64::from(fill.red()),
            f64::from(fill.green()),
            f64::from(fill.blue()),
            f64::from(fill.alpha()),
        );
        let _ = cr.fill();
    });
    let overlay = gtk::Overlay::builder()
        .child(&disc)
        .valign(gtk::Align::Center)
        .css_classes(["big-menu-mark"])
        .build();
    overlay.set_accessible_role(gtk::AccessibleRole::Presentation);
    let symbol: gtk::Widget = match &item.leading_icon_name {
        Some(icon) => {
            let image = gtk::Image::from_icon_name(icon);
            image.set_pixel_size(10);
            image.upcast()
        }
        None => gtk::Label::new(
            item.label
                .chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_uppercase().to_string())
                .as_deref(),
        )
        .upcast(),
    };
    symbol.set_halign(gtk::Align::Center);
    symbol.set_valign(gtk::Align::Center);
    overlay.add_overlay(&symbol);
    overlay
}

/// Start the label of every plain row of `page`, and the caption over each of
/// its sliders, where a check mark puts the others' when the page holds one, as
/// GTK lines up a menu with a check or radio item: the labels of one page read
/// as one column.
pub(super) fn align_labels_past_check_marks(page: &gtk::Box) {
    let rows: Vec<gtk::Widget> = children(page).collect();
    let contents: Vec<gtk::Box> = rows
        .iter()
        .filter(|row| row.has_css_class(MENU_ROW_CSS_CLASS))
        .filter_map(|row| row.downcast_ref::<gtk::Button>()?.child().and_downcast())
        .collect();
    let marked = contents.iter().any(|content| {
        content
            .first_child()
            .is_some_and(|first| first.has_css_class(INDICATOR_CSS_CLASS))
    });
    if !marked {
        return;
    }
    for content in contents {
        // A plain row starts with its text column; an icon or a mark already
        // takes the room.
        let bare = content
            .first_child()
            .and_downcast::<gtk::Box>()
            .is_some_and(|column| column.orientation() == gtk::Orientation::Vertical);
        if bare {
            content.set_margin_start(LEADING_SIZE + CONTENT_SPACING);
        }
    }
    for caption in rows
        .iter()
        .filter_map(WidgetExt::first_child)
        .filter(|caption| caption.has_css_class(super::rich_row::SCALE_CAPTION_CSS_CLASS))
    {
        caption.set_margin_start(LEADING_SIZE + CONTENT_SPACING);
    }
}

/// Install [`MENU_CSS`] on the default display, once.
pub(super) fn install_menu_css() {
    crate::css::install(MENU_CSS, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
}

fn apply_action_row_selection(row: &gtk::Button, selection: BigMenuActionSelection) {
    let checked = match selection {
        BigMenuActionSelection::None => return,
        BigMenuActionSelection::Checkbox { is_checked } => is_checked,
        BigMenuActionSelection::Radio { is_selected } => is_selected,
    };
    row.update_state(&[gtk::accessible::State::Checked(if checked {
        gtk::AccessibleTristate::True
    } else {
        gtk::AccessibleTristate::False
    })]);
    if let Some(indicator) = row
        .child()
        .and_downcast::<gtk::Box>()
        .and_then(|content| content.first_child())
        .and_downcast::<gtk::Image>()
        .filter(|indicator| indicator.has_css_class(INDICATOR_CSS_CLASS))
    {
        indicator.set_opacity(if checked { 1.0 } else { 0.0 });
    }
    if checked {
        row.add_css_class("checked");
    } else {
        row.remove_css_class("checked");
    }
}

pub(super) fn activate_detailed_action(
    action_context: &gtk::Widget,
    detailed_action: &str,
) -> bool {
    match parse_detailed_action(detailed_action) {
        Ok((action_name, target_value)) => action_context
            .activate_action(&action_name, target_value.as_ref())
            .is_ok(),
        Err(_) => action_context
            .activate_action(detailed_action, None)
            .is_ok(),
    }
}

pub(super) fn parse_detailed_action(
    detailed_action: &str,
) -> Result<(String, Option<glib::Variant>), glib::Error> {
    gio::Action::parse_detailed_name(detailed_action)
        .map(|(action_name, target_value)| (action_name.to_string(), target_value))
}
