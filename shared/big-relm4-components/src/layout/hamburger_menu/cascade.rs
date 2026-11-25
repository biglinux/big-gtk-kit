// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Submenus that open beside their row, the way KDE's menus and GTK's nested
//! menus do: resting the pointer on a row with an arrow opens its submenu to
//! the side, resting on another row closes it again, and a click or the Right
//! key opens it at once with the keyboard inside. Escape closes one level.
//! A submenu's own submenus open beside it the same way.
//!
//! GDK closes the whole menu at a press in a popup whose parent popup does
//! not hold the grab (`check_autohide`), and only the menu holds it: a
//! submenu taking one would replace it at every switch between siblings, and
//! a grab asked for on no fresh input is answered by dismissing the chain. So
//! a submenu of a submenu hangs, in GDK, from the menu: its popover is
//! parented to the row its submenu hangs from, and placed beside its own row
//! across the two surfaces ([`place`]).

use crate::layout::widget_container::{children, descendants};
use std::time::Duration;

use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::graphene;
use relm4::gtk::prelude::*;

use super::action_row::{
    focus_first_page_menu_item, focus_is_inside_rich_row, is_submenu_back_key,
    is_submenu_forward_key, submenu_door_row,
};
use super::{BigMenuActionItem, StructuredMenuBuild, build_structured_menu_stack};
use crate::feedback::popover_lifecycle::unparent_popover_on_parent_teardown;
use big_gtk_kit::timer::OnceTimer;

const OPENER_CSS_CLASS: &str = "big-menu-cascade-opener";
/// The data key a door keeps its submenu's popover under ([`popover_of`]).
const SUBMENU_KEY: &str = "big-menu-cascade-submenu";
/// Every submenu popover, for the program to style as its menus.
pub const SUBMENU_CSS_CLASS: &str = "big-menu-submenu";
/// The page (with its header band) whose rows share one open submenu.
pub(super) const SCOPE_CSS_CLASS: &str = "big-menu-hover-scope";
/// How long the pointer rests on a row before its submenu opens. Long enough
/// that moving down the menu flashes nothing.
const HOVER_DELAY: Duration = Duration::from_millis(150);
/// How long an open submenu waits before giving way to the row now under the
/// pointer: the pointer on its way into the submenu crosses rows diagonally,
/// and the macOS "safe triangle" is what this approximates without tracking
/// the pointer: a crossing is shorter than this, a rest is longer.
const INTENT_DELAY: Duration = Duration::from_millis(300);

/// The row for `door`, owning the popover that shows `submenu` beside it.
pub(super) fn build_opener(
    build: &StructuredMenuBuild<'_>,
    door: &BigMenuActionItem,
    submenu: &gio::MenuModel,
) -> gtk::Button {
    let row = submenu_door_row(door);
    attach(build, &row, door, submenu);
    row
}

/// Make `row` the door of `submenu`: a popover beside it that the pointer
/// resting on it, a click or the Right key opens. For a menu row, and for a
/// button in a strip or an inline row.
pub(super) fn attach(
    build: &StructuredMenuBuild<'_>,
    row: &gtk::Button,
    door: &BigMenuActionItem,
    submenu: &gio::MenuModel,
) {
    row.add_css_class(OPENER_CSS_CLASS);

    let child = gtk::Popover::new();
    child.add_css_class("menu");
    child.add_css_class(SUBMENU_CSS_CLASS);
    child.set_has_arrow(false);
    // The menu it belongs to holds the pointer and keyboard grab. A submenu
    // taking one of its own would replace it with every switch between
    // siblings, and the compositor answers a grab asked for on no fresh input
    // by dismissing the whole chain.
    child.set_autohide(false);
    child.set_position(gtk::PositionType::Right);
    // Its first row level with the door, as KDE and GTK place a submenu,
    // rather than centred on it.
    child.set_valign(gtk::Align::Start);
    child.set_accessible_role(gtk::AccessibleRole::Menu);
    child.update_property(&[gtk::accessible::Property::Label(&door.label)]);
    // Rows inside still close the whole menu: `build.popover` is the root.
    let stack = build_structured_menu_stack(
        build.popover,
        submenu,
        build.labels,
        build.custom_children,
        build.action_groups,
        build.action_context,
    );
    child.set_child(Some(&stack));
    child.set_parent(row);
    set_popover(row.upcast_ref(), &child);
    // GTK does not take a popover off its parent when the parent goes.
    unparent_popover_on_parent_teardown(row.upcast_ref(), &child);
    let door = row.downgrade();
    child.connect_map(move |_| set_expanded(&door, true));
    let door = row.downgrade();
    child.connect_unmap(move |_| set_expanded(&door, false));
    // Escape or Left closed it: the keyboard goes back to its door. Not when
    // the whole menu is closing, which leaves the door unmapped. Its own
    // submenus, which hang from the menu, close with it.
    let door = row.downgrade();
    child.connect_closed(move |child| {
        close_openers(child.upcast_ref(), None);
        let Some(row) = door.upgrade().filter(WidgetExt::is_mapped) else {
            return;
        };
        let focus = row.root().and_then(|root| root.focus());
        if focus.is_none_or(|focus| focus.is_ancestor(child)) {
            let _ = row.grab_focus();
        }
    });

    let back = gtk::EventControllerKey::new();
    back.set_propagation_phase(gtk::PropagationPhase::Capture);
    back.connect_key_pressed(|controller, keyval, _, _| {
        let Some(child) = controller.widget().and_downcast::<gtk::Popover>() else {
            return glib::Propagation::Proceed;
        };
        // Inside a strip or on a slider Left is the control's own.
        if !is_submenu_back_key(child.upcast_ref(), keyval) || focus_is_inside_rich_row(&child) {
            return glib::Propagation::Proceed;
        }
        child.popdown();
        glib::Propagation::Stop
    });
    child.add_controller(back);

    row.connect_clicked(|row| open(row.upcast_ref(), true));
    let forward = gtk::EventControllerKey::new();
    forward.connect_key_pressed(|controller, keyval, _, _| {
        let Some(row) = controller.widget() else {
            return glib::Propagation::Proceed;
        };
        if !is_submenu_forward_key(&row, keyval) {
            return glib::Propagation::Proceed;
        }
        open(&row, true);
        glib::Propagation::Stop
    });
    row.add_controller(forward);
}

/// Open and close the page's submenus as the pointer rests on its rows.
///
/// Watched on each row's crossing, not on the page's motion: while a submenu
/// is open it holds the pointer grab, and GTK hands every motion to it, but
/// entering and leaving are still told to the rows under the pointer. The rows
/// are the page's own and the buttons of its strips and inline rows, header
/// band included: `scope` holds them all.
pub(super) fn install_hover(scope: &gtk::Box, page: &gtk::Box) {
    scope.add_css_class(SCOPE_CSS_CLASS);
    // The timer that will make its row's submenu the page's open one (a row
    // with no submenu: close the open one).
    let pending = OnceTimer::new();
    let mut rows: Vec<gtk::Widget> = children(page).collect();
    collect_buttons(scope.upcast_ref(), &mut rows);
    let weak_scope = scope.downgrade();
    for row in rows {
        let motion = gtk::EventControllerMotion::new();
        {
            let pending = pending.clone();
            let weak_scope = weak_scope.clone();
            motion.connect_enter(move |controller, _, _| {
                pending.cancel();
                let (Some(row), Some(scope)) = (controller.widget(), weak_scope.upgrade()) else {
                    return;
                };
                let is_opener = row.has_css_class(OPENER_CSS_CLASS);
                let showing = open_opener(scope.upcast_ref());
                if showing.as_ref() == Some(&row) {
                    return;
                }
                // A row that holds the open door (the strip around it) is
                // where the pointer already is, not a new row.
                if showing.as_ref().is_some_and(|door| door.is_ancestor(&row)) {
                    return;
                }
                if !is_opener && showing.is_none() {
                    return;
                }
                let delay = if showing.is_some() {
                    INTENT_DELAY
                } else {
                    HOVER_DELAY
                };
                let row = row.downgrade();
                let scope = scope.downgrade();
                pending.arm(delay, move || {
                    let (Some(row), Some(scope)) = (row.upgrade(), scope.upgrade()) else {
                        return;
                    };
                    if is_opener {
                        open(&row, false);
                    } else {
                        close_openers(scope.upcast_ref(), None);
                    }
                });
            });
        }
        {
            // Gone into the submenu, or out of the menu: what is open stays.
            let pending = pending.clone();
            motion.connect_leave(move |_| pending.cancel());
        }
        row.add_controller(motion);
    }
    // Escape closes the submenu the pointer opened, and only it: the keyboard
    // is still on this page. With the keyboard inside the submenu, the
    // submenu's own Escape closes it before this hears the key.
    let escape = gtk::EventControllerKey::new();
    escape.connect_key_pressed(|controller, keyval, _, _| {
        let Some(scope) = controller.widget() else {
            return glib::Propagation::Proceed;
        };
        if keyval != gtk::gdk::Key::Escape {
            return glib::Propagation::Proceed;
        }
        let Some(door) = open_opener(&scope) else {
            return glib::Propagation::Proceed;
        };
        close_openers(&scope, None);
        let _ = door.grab_focus();
        glib::Propagation::Stop
    });
    scope.add_controller(escape);
}

/// The buttons inside the rich rows of `widget` (strips, inline and split
/// rows), not those of submenus hanging from it.
fn collect_buttons(widget: &gtk::Widget, into: &mut Vec<gtk::Widget>) {
    for child in children(widget) {
        if child.is::<gtk::Popover>() {
            continue;
        }
        if child.is::<gtk::Button>() && rich_ancestor(&child) {
            into.push(child);
            continue;
        }
        collect_buttons(&child, into);
    }
}

fn rich_ancestor(widget: &gtk::Widget) -> bool {
    std::iter::successors(widget.parent(), WidgetExt::parent)
        .take_while(|parent| !parent.has_css_class(SCOPE_CSS_CLASS))
        .any(|parent| parent.has_css_class(super::rich_row::RICH_ROW_CSS_CLASS))
}

/// Close every submenu under `widget`, nested ones included: the menu closed.
pub(super) fn close_all(widget: &gtk::Widget) {
    for popover in descendants(widget).filter_map(|widget| widget.downcast::<gtk::Popover>().ok()) {
        popover.popdown();
    }
}

/// Show the submenu of `row`, closing the others of its page.
fn open(row: &gtk::Widget, focus: bool) {
    let Some(child) = popover_of(row) else {
        return;
    };
    if let Some(scope) = scope_of(row) {
        close_openers(&scope, Some(&child));
    }
    place(row, &child);
    child.popup();
    if focus && let Some(stack) = child.child().and_downcast::<gtk::Stack>() {
        focus_first_page_menu_item(&stack);
    }
}

/// The hover scope `row` belongs to (`install_hover`).
fn scope_of(row: &gtk::Widget) -> Option<gtk::Widget> {
    std::iter::successors(row.parent(), WidgetExt::parent)
        .take_while(|parent| !parent.is::<gtk::Popover>())
        .find(|parent| parent.has_css_class(SCOPE_CSS_CLASS))
}

fn close_openers(scope: &gtk::Widget, keep: Option<&gtk::Popover>) {
    for child in openers(scope).iter().filter_map(popover_of) {
        if Some(&child) != keep {
            child.popdown();
        }
    }
}

/// The page's row whose submenu is showing.
fn open_opener(page: &gtk::Widget) -> Option<gtk::Widget> {
    openers(page)
        .into_iter()
        .find(|row| popover_of(row).is_some_and(|child| child.is_visible()))
}

/// Every door under `scope`, in its rows and strips, but not in the
/// submenus hanging from them.
fn openers(scope: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut stack: Vec<gtk::Widget> = children(scope).collect();
    while let Some(widget) = stack.pop() {
        if widget.is::<gtk::Popover>() {
            continue;
        }
        if widget.has_css_class(OPENER_CSS_CLASS) {
            found.push(widget);
            continue;
        }
        stack.extend(children(&widget));
    }
    found
}

/// Hang `child`, the submenu of `row`, where it opens beside `row`. A row
/// of a submenu lends it to the widget that submenu hangs from, in the menu's
/// own surface, and shifts it from there to the row's place: the submenu's
/// offset in that surface, the row's in the submenu's. GDK cuts the rectangle
/// a popup points at to its parent's, so the shift is the popup's offset.
fn place(row: &gtk::Widget, child: &gtk::Popover) {
    // Beside the menu rather than over its edge: the row stands in from the
    // card's rim by the card's padding, so the submenu is shifted that far
    // past it, onto the rim the two share.
    let past = room_past(row);
    let Some(host) = row
        .native()
        .and_downcast::<gtk::Popover>()
        .filter(|host| host.has_css_class(SUBMENU_CSS_CLASS))
    else {
        child.set_offset(past, 0);
        return;
    };
    let (Some(anchor), Some(popup)) = (
        host.parent(),
        host.surface().and_dynamic_cast::<gdk::Popup>().ok(),
    ) else {
        return;
    };
    let (Some(menu), Some(bounds)) = (anchor.native(), row.compute_bounds(&host)) else {
        return;
    };
    if child.parent().as_ref() != Some(&anchor) {
        child.unparent();
        child.set_parent(&anchor);
    }
    let (host_x, host_y) = host.surface_transform();
    let (menu_x, menu_y) = menu.surface_transform();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "a menu's place in pixels"
    )]
    let at = graphene::Point::new(
        bounds.x() + (host_x + f64::from(popup.position_x()) - menu_x) as f32,
        bounds.y() + (host_y + f64::from(popup.position_y()) - menu_y) as f32,
    );
    let Some(at) = menu.compute_point(&anchor, &at) else {
        return;
    };
    // Beside the row, its first row level with it, as beside the anchor.
    #[allow(clippy::cast_possible_truncation, reason = "a menu's place in pixels")]
    child.set_offset(
        (at.x() + bounds.width()).round() as i32 + past - anchor.width(),
        at.y().round() as i32,
    );
}

/// How far the rim of the card `row` sits in lies past the row's end: the card
/// is GTK's `contents` node.
fn room_past(row: &gtk::Widget) -> i32 {
    let Some(card) = row
        .native()
        .and_then(|menu| children(&menu).find(|child| child.css_name() == "contents"))
    else {
        return 0;
    };
    let Some(bounds) = row.compute_bounds(&card) else {
        return 0;
    };
    #[allow(clippy::cast_possible_truncation, reason = "a menu's place in pixels")]
    let end = (bounds.x() + bounds.width()).round() as i32;
    (card.width() - end).max(0)
}

fn set_popover(row: &gtk::Widget, child: &gtk::Popover) {
    // SAFETY: `SUBMENU_KEY` only ever holds a `gtk::Popover`, here and in
    // `popover_of`.
    unsafe { row.set_data(SUBMENU_KEY, child.clone()) };
}

/// The popover of `row`'s submenu, wherever it hangs ([`place`]).
fn popover_of(row: &gtk::Widget) -> Option<gtk::Popover> {
    // SAFETY: `SUBMENU_KEY` only ever holds a `gtk::Popover` (`set_popover`).
    unsafe { row.data::<gtk::Popover>(SUBMENU_KEY) }.map(|child| {
        // SAFETY: the row holds the popover while it lives.
        unsafe { child.as_ref() }.clone()
    })
}

fn set_expanded(door: &glib::WeakRef<gtk::Button>, expanded: bool) {
    if let Some(row) = door.upgrade() {
        row.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);
    }
}

#[cfg(test)]
mod tests {
    use big_testkit::harness::{pump, pump_until, require_gtk_test};

    use super::*;
    use crate::layout::hamburger_menu::{
        BigContextMenu, BigContextMenuContent, BigMenuModelPopoverLabels,
    };
    use crate::layout::widget_container::subtree;

    fn find_opener(widget: &gtk::Widget) -> Option<gtk::Widget> {
        subtree(widget).find(|widget| widget.has_css_class(OPENER_CSS_CLASS))
    }

    #[test]
    fn submenu_opens_beside_its_row() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            window.set_child(Some(&host));
            window.present();
            pump();

            let model = gio::Menu::new();
            let nested = gio::Menu::new();
            nested.append(Some("Nested"), Some("win.nested"));
            model.append_submenu(Some("More"), &nested);
            let menu = BigContextMenu::new(&host);
            menu.popup(
                &host,
                4.0,
                4.0,
                &BigContextMenuContent::new(model, BigMenuModelPopoverLabels::new("Actions")),
            );
            assert!(pump_until(|| menu.popover().is_mapped()));
            let opener = find_opener(menu.popover().upcast_ref()).expect("a row opens the submenu");
            let child = popover_of(&opener).expect("the submenu hangs from its row");
            assert!(!child.is_visible());

            opener
                .downcast_ref::<gtk::Button>()
                .expect("the row is a button")
                .emit_clicked();
            assert!(
                pump_until(|| child.is_mapped()),
                "a click opens it beside the row"
            );
            assert!(menu.popover().is_mapped(), "the menu stays open under it");

            menu.popdown();
            assert!(
                pump_until(|| !child.is_visible()),
                "the submenu closed with its menu"
            );
            window.destroy();
        });
    }

    /// A submenu's own submenu opens beside it, the menu and the submenu
    /// still open, and hangs from the menu's row of that submenu: GDK keeps
    /// the menu open at a press in a popup whose parent holds the grab.
    #[test]
    fn nested_submenu_opens_beside() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            window.set_child(Some(&host));
            window.present();
            pump();

            let model = gio::Menu::new();
            let more = gio::Menu::new();
            let deeper = gio::Menu::new();
            deeper.append(Some("Deep"), Some("win.deep"));
            more.append_submenu(Some("Deeper"), &deeper);
            model.append_submenu(Some("More"), &more);
            let menu = BigContextMenu::new(&host);
            menu.popup(
                &host,
                4.0,
                4.0,
                &BigContextMenuContent::new(model, BigMenuModelPopoverLabels::new("Actions")),
            );
            assert!(pump_until(|| menu.popover().is_mapped()));
            let opener = find_opener(menu.popover().upcast_ref()).expect("a row opens More");
            let child = popover_of(&opener).expect("More hangs from its row");
            let click = |row: &gtk::Widget| {
                row.downcast_ref::<gtk::Button>()
                    .expect("the row is a button")
                    .emit_clicked();
            };
            click(&opener);
            assert!(pump_until(|| child.is_mapped()), "More opens");
            let inner = find_opener(child.upcast_ref()).expect("a row of More opens Deeper");
            let nested = popover_of(&inner).expect("Deeper has a popover");
            click(&inner);
            assert!(pump_until(|| nested.is_mapped()), "Deeper opens");
            assert!(child.is_mapped(), "More stays open beside it");
            assert_eq!(
                nested.parent().as_ref(),
                Some(&opener),
                "it hangs from the menu"
            );

            child.popdown();
            assert!(pump_until(|| !nested.is_visible()), "it closes with More");
            menu.popdown();
            window.destroy();
        });
    }
}
