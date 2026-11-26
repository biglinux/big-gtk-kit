// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The suite's right-click menu, one component for every product.
//!
//! Every surface that offers a context menu opens it the same way (secondary
//! click, a long press on a touch screen, the Menu key or Shift+F10) and shows
//! it with the same rows as the hamburger menu, so no surface differs in section
//! titles, in when it opens, or in having a keyboard path.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

use super::{
    BigMenuActionGroupBinding, BigMenuModelPopoverLabels, insert_inheritable_action_group,
    populate_menu_model_action_popover_menu,
};
use crate::feedback::popover_lifecycle::unparent_popover_on_parent_teardown;

/// What a context menu shows when it opens.
pub struct BigContextMenuContent {
    /// The menu, built for the item it was opened on.
    pub model: gio::MenuModel,
    /// Accessible name of the menu.
    pub labels: BigMenuModelPopoverLabels,
    /// Action groups the rows resolve against, besides the host's ancestry.
    pub action_groups: Vec<BigMenuActionGroupBinding>,
}

impl BigContextMenuContent {
    /// A menu whose actions resolve through the host's ancestry.
    #[must_use]
    pub fn new(model: impl IsA<gio::MenuModel>, labels: BigMenuModelPopoverLabels) -> Self {
        Self {
            model: model.upcast(),
            labels,
            action_groups: Vec::new(),
        }
    }
}

/// One context-menu popover, parented to its host for the host's life.
///
/// The host must sit outside any scrolled viewport the menu opens over: a
/// popover whose parent lives in a viewport is clipped by it.
#[derive(Clone)]
pub struct BigContextMenu {
    popover: gtk::PopoverMenu,
    host: glib::WeakRef<gtk::Widget>,
    focus_return: BigMenuFocusReturn,
}

/// Gives the keyboard back to the widget a menu was opened on when the menu
/// closes, for a menu popover that is not a [`BigContextMenu`].
///
/// GTK hands it to the popover's parent instead. A parent that does not take
/// the focus leaves it where it was, on a row of the closed menu or on
/// whatever took it while the menu was open.
#[derive(Clone)]
pub struct BigMenuFocusReturn {
    // The focus inside the widget the menu was asked about, so Escape hands
    // the keyboard back to it rather than to the host the popover hangs from.
    anchor: Rc<RefCell<glib::WeakRef<gtk::Widget>>>,
    // Whether Escape closed the menu, which nothing but the menu answers.
    dismissed: Rc<Cell<bool>>,
}

impl BigMenuFocusReturn {
    /// Watch `popover` for as long as it lives.
    #[must_use]
    pub fn install(popover: &gtk::PopoverMenu) -> Self {
        let anchor: Rc<RefCell<glib::WeakRef<gtk::Widget>>> = Rc::default();
        let dismissed = Rc::new(Cell::new(false));
        {
            let dismissed = Rc::clone(&dismissed);
            let escape = gtk::EventControllerKey::new();
            escape.set_propagation_phase(gtk::PropagationPhase::Capture);
            escape.connect_key_pressed(move |controller, key, _, _| {
                // A search field takes Escape for itself, back to the menu.
                let in_search = controller
                    .widget()
                    .and_then(|popover| popover.root())
                    .and_then(|root| root.focus())
                    .is_some_and(|focus| focus.ancestor(gtk::SearchEntry::static_type()).is_some());
                if key == gtk::gdk::Key::Escape && !in_search {
                    dismissed.set(true);
                }
                glib::Propagation::Proceed
            });
            popover.add_controller(escape);
        }
        {
            let (anchor, dismissed) = (Rc::clone(&anchor), Rc::clone(&dismissed));
            // `gtk_popover_popdown` hands the focus to the popover's parent
            // after `closed`, so the grab waits for it to return. A parent
            // that does not take the focus would pass it to its first child
            // that does, a row nobody asked about, so it takes the focus for
            // that turn. A closing submenu hands it to its door instead, a row
            // of the closed menu that no key reaches the window from. Escape
            // always goes back; a menu item that moved the focus to something
            // on screen keeps it there.
            popover.connect_closed(move |popover| {
                let anchor = anchor.borrow().clone();
                let host = popover.parent();
                let lent = host.clone().filter(|host| !host.is_focusable());
                if let Some(host) = &lent {
                    host.set_focusable(true);
                }
                let host = host.map(|host| host.downgrade());
                let lent = lent.map(|host| host.downgrade());
                let dismissed = dismissed.replace(false);
                glib::idle_add_local_once(move || {
                    let host = host.and_then(|host| host.upgrade());
                    if let Some(anchor) = anchor.upgrade() {
                        let focus = anchor.root().and_then(|root| root.focus());
                        if dismissed
                            || focus.is_none_or(|focus| {
                                !focus.is_mapped() || Some(&focus) == host.as_ref()
                            })
                        {
                            let _ = anchor.grab_focus();
                        }
                    }
                    if let Some(host) = lent.and_then(|host| host.upgrade()) {
                        host.set_focusable(false);
                    }
                });
            });
        }
        Self { anchor, dismissed }
    }

    /// The menu is opening on `anchor`: the focus goes back to the widget
    /// focused inside it when the menu closes, or to `anchor` itself.
    pub fn remember(&self, anchor: &impl IsA<gtk::Widget>) {
        self.dismissed.set(false);
        // Inside a list the keyboard goes back to the row it was on. The list
        // itself would hand the focus to its first child or its scroller.
        let mut back = anchor.as_ref().clone();
        while let Some(child) = back.focus_child() {
            back = child;
        }
        self.anchor.replace(back.downgrade());
    }
}

impl BigContextMenu {
    /// Create the menu on `host`; it is unparented when the host goes away.
    #[must_use]
    pub fn new(host: &impl IsA<gtk::Widget>) -> Self {
        let host = host.as_ref();
        let popover = super::popover_menu(None);
        popover.set_has_arrow(false);
        // A corner at the point asked about, as menus open everywhere else; a
        // popover is centered on it otherwise. Flipped above it where there is
        // no room below.
        popover.set_halign(gtk::Align::Start);
        popover.set_parent(host);
        unparent_popover_on_parent_teardown(host, &popover);
        let focus_return = BigMenuFocusReturn::install(&popover);
        Self {
            popover,
            host: host.downgrade(),
            focus_return,
        }
    }

    /// Show `content` at `(x, y)` in `anchor`'s coordinates.
    ///
    /// The focus goes back to the widget focused inside `anchor` when the menu
    /// closes. In a recycling list that is the row widget, so a model that
    /// replaces its items while the menu is open can hand the focus to a row
    /// now showing another item; keep the item for the menu's life.
    pub fn popup(
        &self,
        anchor: &impl IsA<gtk::Widget>,
        x: f64,
        y: f64,
        content: &BigContextMenuContent,
    ) {
        let Some(host) = self.host.upgrade() else {
            return;
        };
        // A popover with no window under it cannot be shown, and trying
        // aborts the process.
        if host.root().is_none() {
            return;
        }
        if self.popover.parent().is_none() {
            self.popover.set_parent(&host);
        }
        let anchor = anchor.as_ref();
        self.popover.popdown();
        populate_menu_model_action_popover_menu(
            &self.popover,
            &content.model,
            &content.labels,
            &content.action_groups,
        );
        for binding in &content.action_groups {
            insert_inheritable_action_group(&self.popover, &binding.prefix, &binding.group);
        }
        self.focus_return.remember(anchor);

        #[expect(
            clippy::cast_possible_truncation,
            reason = "widget coordinates fit in f32"
        )]
        let (point_x, point_y) = anchor
            .compute_point(&host, &gtk::graphene::Point::new(x as f32, y as f32))
            .map_or((x as f32, y as f32), |point| (point.x(), point.y()));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "rounded widget coordinates"
        )]
        let point = gtk::gdk::Rectangle::new(
            (point_x.round() as i32).clamp(1, host.width().max(1)),
            (point_y.round() as i32).clamp(1, host.height().max(1)),
            1,
            1,
        );
        self.popover.set_pointing_to(Some(&point));
        self.popover.popup();
        // Below the pointer there may be no room, and GTK has nowhere to flip a
        // menu to either: its last rows would sit past the bottom of the window
        // with no way to reach them. The height is only known once it is up.
        let height = self.popover.measure(gtk::Orientation::Vertical, -1).1;
        let highest_top = (host.height() - height).max(1);
        if point.y() > highest_top {
            self.popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                point.x(),
                highest_top,
                1,
                1,
            )));
        }
    }

    /// Close the menu if it is open.
    pub fn popdown(&self) {
        self.popover.popdown();
    }

    /// The popover, for callers that need to watch it open or close.
    #[must_use]
    pub fn popover(&self) -> &gtk::PopoverMenu {
        &self.popover
    }
}

/// The keys that ask the focused widget for its context menu: the Menu key and
/// Shift+F10. Lock modifiers do not hide them; with Ctrl, Alt or Super they stay
/// the shortcuts they are.
#[must_use]
pub fn is_context_menu_key(key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> bool {
    let modifiers = modifiers & gtk::accelerator_get_default_mod_mask();
    (key == gtk::gdk::Key::Menu && modifiers.is_empty())
        || (key == gtk::gdk::Key::F10 && modifiers == gtk::gdk::ModifierType::SHIFT_MASK)
}

/// Open `menu` on `widget` for secondary click, long press, Menu and Shift+F10.
///
/// `build` runs as the menu opens, with the widget and the point it was asked
/// at, and returns what to show, or `None` to show nothing, which leaves the
/// event to the widget. For the keyboard the point is the center of the
/// focused item inside the widget, a list's row, or of the widget itself.
pub fn attach_context_menu<F>(widget: &impl IsA<gtk::Widget>, menu: &BigContextMenu, build: F)
where
    F: Fn(&gtk::Widget, f64, f64) -> Option<BigContextMenuContent> + 'static,
{
    let menu = menu.clone();
    connect_context_menu_request(widget, move |widget, at| {
        let (x, y) = at.unwrap_or_else(|| focus_center(widget));
        let Some(content) = build(widget, x, y) else {
            return false;
        };
        menu.popup(widget, x, y, &content);
        true
    });
}

/// The center of the deepest focused descendant of `widget`, in its coordinates.
fn focus_center(widget: &gtk::Widget) -> (f64, f64) {
    let mut focus = widget.clone();
    while let Some(child) = focus.focus_child() {
        focus = child;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "widget sizes are small integers"
    )]
    let center = gtk::graphene::Point::new(focus.width() as f32 / 2.0, focus.height() as f32 / 2.0);
    let point = focus.compute_point(widget, &center).unwrap_or(center);
    (f64::from(point.x()), f64::from(point.y()))
}

/// Call `open` whenever `widget` is asked for its context menu: secondary
/// click, long press, Menu and Shift+F10.
///
/// For a surface whose menu is not a [`BigContextMenu`]. `open` gets the widget
/// and the point the pointer or finger asked at, `None` for the keyboard, and
/// answers whether it opened a menu; when it did not, the event goes on.
pub fn connect_context_menu_request<F>(widget: &impl IsA<gtk::Widget>, open: F)
where
    F: Fn(&gtk::Widget, Option<(f64, f64)>) -> bool + 'static,
{
    for controller in context_menu_request_controllers(open) {
        widget.as_ref().add_controller(controller);
    }
}

/// The name of every controller [`context_menu_request_controllers`] makes,
/// which tells them from the widget's own.
pub const CONTEXT_MENU_CONTROLLER: &str = "big-context-menu";

/// The controllers [`connect_context_menu_request`] adds, for a widget that
/// removes them again when its menu changes.
#[must_use]
pub fn context_menu_request_controllers<F>(open: F) -> [gtk::EventController; 3]
where
    F: Fn(&gtk::Widget, Option<(f64, f64)>) -> bool + 'static,
{
    // Each controller receives the widget it is on as an argument, so no
    // closure below holds the widget itself.
    let open = Rc::new(open);

    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    {
        let open = Rc::clone(&open);
        click.connect_pressed(move |gesture, _, x, y| {
            if let Some(widget) = gesture.widget()
                && open(&widget, Some((x, y)))
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
    }

    let long_press = gtk::GestureLongPress::new();
    long_press.set_touch_only(true);
    {
        let open = Rc::clone(&open);
        long_press.connect_pressed(move |gesture, x, y| {
            if let Some(widget) = gesture.widget()
                && open(&widget, Some((x, y)))
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
    }

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |controller, key, _, modifiers| {
        match controller
            .widget()
            .filter(|_| is_context_menu_key(key, modifiers))
        {
            Some(widget) if open(&widget, None) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        }
    });
    let controllers: [gtk::EventController; 3] =
        [click.upcast(), long_press.upcast(), keys.upcast()];
    for controller in &controllers {
        controller.set_name(Some(CONTEXT_MENU_CONTROLLER));
    }
    controllers
}

#[cfg(test)]
mod tests {
    use crate::layout::widget_container::children;
    use std::cell::Cell;

    use super::*;
    use big_testkit::harness::{pump, pump_until, require_gtk_test};

    fn content() -> BigContextMenuContent {
        let menu = gio::Menu::new();
        menu.append(Some("Rename"), Some("win.rename"));
        BigContextMenuContent::new(menu, BigMenuModelPopoverLabels::new("File Actions"))
    }

    fn press(widget: &gtk::Widget, key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> bool {
        let keys = widget
            .observe_controllers()
            .into_iter()
            .filter_map(Result::ok)
            .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok());
        keys.is_some_and(|keys| {
            keys.emit_by_name::<bool>("key-pressed", &[&key, &0_u32, &modifiers])
        })
    }

    fn press_menu_key(widget: &gtk::Widget) -> bool {
        press(widget, gtk::gdk::Key::Menu, gtk::gdk::ModifierType::empty())
    }

    #[test]
    fn menu_key_opens_and_escape_returns() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let row = gtk::Button::with_label("report.txt");
            host.append(&row);
            window.set_child(Some(&host));
            window.present();
            pump();

            let menu = BigContextMenu::new(&host);
            attach_context_menu(&row, &menu, |_, _, _| Some(content()));
            assert!(press_menu_key(row.upcast_ref()));
            assert!(pump_until(|| menu.popover().is_mapped()));
            assert_eq!(menu.popover().parent().as_ref(), Some(host.upcast_ref()));

            menu.popdown();
            // The row is the window's focus again; whether the compositor has
            // made the window active by then is its own timing, and in a
            // private session shared by other tests' windows it often has not.
            assert!(pump_until(|| row.is_focus()));
            window.destroy();
        });
    }

    #[test]
    fn modified_menu_keys_stay_shortcuts() {
        use gtk::gdk::{Key, ModifierType};
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            window.set_child(Some(&host));
            window.present();
            pump();

            let menu = BigContextMenu::new(&host);
            attach_context_menu(&host, &menu, |_, _, _| Some(content()));
            let host = host.upcast_ref();
            for (key, modifiers) in [
                (
                    Key::F10,
                    ModifierType::SHIFT_MASK | ModifierType::CONTROL_MASK,
                ),
                (Key::F10, ModifierType::empty()),
                (Key::Menu, ModifierType::CONTROL_MASK),
                (Key::Menu, ModifierType::ALT_MASK),
            ] {
                assert!(!press(host, key, modifiers), "{key:?} with {modifiers:?}");
            }
            pump();
            assert!(!menu.popover().is_visible());
            // Caps Lock does not hide the keys that ask for the menu.
            assert!(press(host, Key::Menu, ModifierType::LOCK_MASK));
            menu.popdown();
            assert!(press(host, Key::F10, ModifierType::SHIFT_MASK));
            menu.popdown();
            window.destroy();
        });
    }

    #[test]
    fn empty_menu_leaves_key_alone() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            window.set_child(Some(&host));
            window.present();
            pump();

            let menu = BigContextMenu::new(&host);
            let asked = Rc::new(Cell::new(0));
            let counter = Rc::clone(&asked);
            attach_context_menu(&host, &menu, move |_, _, _| {
                counter.set(counter.get() + 1);
                None
            });
            assert!(!press_menu_key(host.upcast_ref()));
            pump();
            assert_eq!(asked.get(), 1);
            assert!(!menu.popover().is_visible());
            window.destroy();
        });
    }

    #[test]
    fn keyboard_request_has_no_point() {
        require_gtk_test(|| {
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let asked = Rc::new(Cell::new(None));
            let seen = Rc::clone(&asked);
            connect_context_menu_request(&host, move |_, at| {
                seen.set(Some(at));
                true
            });
            assert!(press_menu_key(host.upcast_ref()));
            assert_eq!(asked.get(), Some(None));
        });
    }

    #[test]
    fn keyboard_menu_opens_at_focus() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let first = gtk::Button::with_label("first");
            let second = gtk::Button::with_label("second");
            host.append(&first);
            host.append(&second);
            window.set_child(Some(&host));
            window.present();
            pump();
            second.grab_focus();

            let menu = BigContextMenu::new(&host);
            let asked = Rc::new(Cell::new(None));
            let seen = Rc::clone(&asked);
            attach_context_menu(&host, &menu, move |_, x, y| {
                seen.set(Some((x, y)));
                None
            });
            press_menu_key(host.upcast_ref());
            let (_, y) = asked.get().expect("the menu was asked for");
            let bounds = second.compute_bounds(&host).expect("laid out");
            assert!(f64::from(bounds.y()) < y && y < f64::from(bounds.y() + bounds.height()));
            window.destroy();
        });
    }

    /// A list's menu hands the keyboard back to the row it was opened on, not
    /// to the list's first child or to the scroller the menu hangs from.
    #[test]
    fn escape_returns_to_the_row() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let first = gtk::Button::with_label("first");
            let second = gtk::Button::with_label("second");
            list.append(&first);
            list.append(&second);
            let host = gtk::ScrolledWindow::builder().child(&list).build();
            window.set_child(Some(&host));
            window.present();
            pump();
            second.grab_focus();

            let menu = BigContextMenu::new(&host);
            attach_context_menu(&list, &menu, |_, _, _| Some(content()));
            assert!(press_menu_key(list.upcast_ref()));
            assert!(pump_until(|| menu.popover().is_mapped()));
            menu.popdown();
            assert!(pump_until(|| second.is_focus()), "the focus left the row");
            window.destroy();
        });
    }

    /// Escape gives the keyboard back to the row even when the popover's
    /// parent hands it to another of its children as the menu closes.
    #[test]
    fn escape_wins_over_first_child() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let first = gtk::Button::with_label("first");
            let second = gtk::Button::with_label("second");
            host.append(&first);
            host.append(&second);
            window.set_child(Some(&host));
            window.present();
            pump();
            second.grab_focus();

            let menu = BigContextMenu::new(&host);
            attach_context_menu(&host, &menu, |_, _, _| Some(content()));
            assert!(press_menu_key(host.upcast_ref()));
            assert!(pump_until(|| menu.popover().is_mapped()));
            menu.popover().connect_closed({
                let window = window.clone();
                move |_| {
                    let (window, first) = (window.clone(), first.clone());
                    glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                        GtkWindowExt::set_focus(&window, Some(&first));
                        glib::ControlFlow::Break
                    });
                }
            });
            for keys in menu
                .popover()
                .observe_controllers()
                .into_iter()
                .filter_map(|controller| {
                    controller.ok()?.downcast::<gtk::EventControllerKey>().ok()
                })
            {
                keys.emit_by_name::<bool>(
                    "key-pressed",
                    &[
                        &gtk::gdk::Key::Escape,
                        &0_u32,
                        &gtk::gdk::ModifierType::empty(),
                    ],
                );
            }
            menu.popdown();
            assert!(pump_until(|| second.is_focus()), "the focus left the row");
            window.destroy();
        });
    }

    /// A menu item closes the menu, and the keyboard goes back to the row,
    /// not to the first child the popover's parent hands it to.
    #[test]
    fn item_returns_to_the_row() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let first = gtk::Button::with_label("first");
            let second = gtk::Button::with_label("second");
            host.append(&first);
            host.append(&second);
            window.set_child(Some(&host));
            window.present();
            pump();
            second.grab_focus();

            let menu = BigContextMenu::new(&host);
            attach_context_menu(&host, &menu, |_, _, _| Some(content()));
            assert!(press_menu_key(host.upcast_ref()));
            assert!(pump_until(|| menu.popover().is_mapped()));
            menu.popdown();
            assert!(pump_until(|| second.is_focus()), "the focus left the row");
            assert!(!host.is_focusable(), "the host keeps the focus it lent");
            window.destroy();
        });
    }

    /// An item of a submenu closes the whole menu, and the keyboard goes back
    /// to the row. GTK hands the focus of a closing submenu to its door, a
    /// row of the closed menu where no key reaches the window's shortcuts,
    /// after the next frame; here that is done before the menu's own idle.
    #[test]
    fn submenu_item_returns_to_the_row() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let actions = gio::SimpleActionGroup::new();
            actions.add_action(&gio::SimpleAction::new("copy-to", None));
            window.insert_action_group("win", Some(&actions));
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let first = gtk::Button::with_label("first");
            let second = gtk::Button::with_label("second");
            list.append(&first);
            list.append(&second);
            let host = gtk::ScrolledWindow::builder().child(&list).build();
            window.set_child(Some(&host));
            window.present();
            pump();
            second.grab_focus();

            let menu = BigContextMenu::new(&host);
            attach_context_menu(&list, &menu, |_, _, _| {
                let more = gio::Menu::new();
                more.append(Some("Copy To…"), Some("win.copy-to"));
                let model = gio::Menu::new();
                model.append_submenu(Some("More Actions"), &more);
                Some(BigContextMenuContent::new(
                    model,
                    BigMenuModelPopoverLabels::new("File Actions"),
                ))
            });
            assert!(press_menu_key(list.upcast_ref()));
            assert!(pump_until(|| menu.popover().is_mapped()));
            let button = |label: &str| {
                let mut stack = vec![menu.popover().clone().upcast::<gtk::Widget>()];
                while let Some(widget) = stack.pop() {
                    if let Some(text) = widget.downcast_ref::<gtk::Label>()
                        && text.label() == label
                        && let Some(button) = text.ancestor(gtk::Button::static_type())
                    {
                        return button.downcast::<gtk::Button>().expect("a button");
                    }
                    for next in children(&widget) {
                        stack.push(next);
                    }
                }
                panic!("no {label} row");
            };
            let door = button("More Actions");
            door.emit_clicked();
            let item = button("Copy To…");
            assert!(
                pump_until(|| item.has_focus()),
                "the submenu took the keyboard"
            );
            menu.popover().connect_closed({
                let window = window.clone();
                move |_| {
                    let window = window.clone();
                    let door = door.clone();
                    glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                        GtkWindowExt::set_focus(&window, Some(&door));
                        glib::ControlFlow::Break
                    });
                }
            });
            item.emit_clicked();
            assert!(pump_until(|| !menu.popover().is_visible()));
            assert!(
                pump_until(|| second.is_focus()),
                "the focus stayed in the menu"
            );
            window.destroy();
        });
    }

    /// Escape that only leaves the menu's search closes nothing: an item
    /// chosen afterwards keeps the focus it moved on purpose.
    #[test]
    fn search_escape_lets_an_item_move_the_focus() {
        use gtk::gdk::{Key, ModifierType};
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let row = gtk::Button::with_label("alpha.txt");
            let elsewhere = gtk::Button::with_label("Elsewhere");
            host.append(&row);
            host.append(&elsewhere);
            window.set_child(Some(&host));
            let actions = gio::SimpleActionGroup::new();
            let open = gio::SimpleAction::new("open", None);
            open.connect_activate({
                let elsewhere = elsewhere.downgrade();
                move |_, _| {
                    if let Some(elsewhere) = elsewhere.upgrade() {
                        elsewhere.grab_focus();
                    }
                }
            });
            actions.add_action(&open);
            window.insert_action_group("win", Some(&actions));
            window.present();
            pump();
            row.grab_focus();

            let menu = BigContextMenu::new(&host);
            attach_context_menu(&row, &menu, |_, _, _| {
                let model = gio::Menu::new();
                model.append(Some("Open"), Some("win.open"));
                Some(BigContextMenuContent::new(
                    model,
                    BigMenuModelPopoverLabels::new("File Actions")
                        .with_search("Search actions", "Nothing found"),
                ))
            });
            assert!(press_menu_key(row.upcast_ref()));
            assert!(pump_until(|| menu.popover().is_mapped()));
            // What the menu sees first: its search field takes Escape after.
            let press_on_menu = |key: Key| {
                for keys in menu
                    .popover()
                    .observe_controllers()
                    .into_iter()
                    .filter_map(|controller| {
                        controller.ok()?.downcast::<gtk::EventControllerKey>().ok()
                    })
                    .filter(|keys| keys.propagation_phase() == gtk::PropagationPhase::Capture)
                {
                    keys.emit_by_name::<bool>(
                        "key-pressed",
                        &[&key, &0_u32, &ModifierType::empty()],
                    );
                }
            };
            press_on_menu(Key::o);
            let entry = GtkWindowExt::focus(&window)
                .and_then(|focus| focus.ancestor(gtk::SearchEntry::static_type()))
                .and_downcast::<gtk::SearchEntry>()
                .expect("the letter opened the search");
            press_on_menu(Key::Escape);
            entry.emit_by_name::<()>("stop-search", &[]);
            let mut pending = vec![menu.popover().clone().upcast::<gtk::Widget>()];
            let mut item = None;
            while let Some(widget) = pending.pop() {
                if let Some(button) = widget.downcast_ref::<gtk::Button>()
                    && button.action_name().as_deref() == Some("win.open")
                    && button.is_mapped()
                {
                    item = Some(button.clone());
                }
                pending.extend(children(&widget));
            }
            item.expect("the menu's Open").emit_clicked();
            assert!(pump_until(|| !menu.popover().is_visible()));
            for _ in 0..10 {
                pump();
            }
            assert!(elsewhere.is_focus(), "the item's focus was pulled back");
            window.destroy();
        });
    }

    /// A host taken out of its window and put back (a tab moved or brought
    /// back) unrealizes and realizes again. The menu has to open on it
    /// afterwards without realizing a popover with no window under it, which makes
    /// GTK abort the process.
    #[test]
    fn menu_reopens_after_host_returns() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame.append(&host);
            window.set_child(Some(&frame));
            window.present();
            pump();

            let menu = BigContextMenu::new(&host);
            frame.remove(&host);
            pump();
            assert!(menu.popover().parent().is_none(), "released with its host");
            // Out of any window: asking for the menu does nothing rather than abort.
            menu.popup(&host, 4.0, 4.0, &content());
            assert!(!menu.popover().is_visible());

            frame.append(&host);
            pump();
            assert_eq!(menu.popover().parent().as_ref(), Some(host.upcast_ref()));
            menu.popup(&host, 4.0, 4.0, &content());
            assert!(pump_until(|| menu.popover().is_mapped()));
            menu.popdown();
            window.destroy();
        });
    }

    #[test]
    fn the_menu_leaves_with_its_host() {
        require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            window.set_child(Some(&host));
            window.present();
            pump();

            let menu = BigContextMenu::new(&host);
            menu.popup(&host, 4.0, 4.0, &content());
            window.destroy();
            pump();
            assert!(menu.popover().parent().is_none());
        });
    }
}
