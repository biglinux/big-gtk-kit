// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

use crate::layout::widget_container::{children, subtree};
use std::rc::Rc;

use super::*;
use relm4::gtk::glib;
use relm4::gtk::glib::variant::ToVariant;

fn popover_labels() -> BigMenuModelPopoverLabels {
    BigMenuModelPopoverLabels::new("Opcoes da barra")
}

fn string_target_item(label: &str, action: &str, target: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
    item
}

fn toolbar_menu_model() -> gio::Menu {
    let menu = gio::Menu::new();
    let display = gio::Menu::new();
    display.append_item(&string_target_item(
        "Band Default (Icon Only)",
        "bigtoolbar.item-mode",
        "",
    ));
    menu.append_section(None, &display);

    let move_targets = gio::Menu::new();
    move_targets.append_item(&string_target_item(
        "New Toolbar",
        "bigtoolbar.move-item",
        "",
    ));
    let move_section = gio::Menu::new();
    move_section.append_submenu(Some("Move to"), &move_targets);
    menu.append_section(None, &move_section);

    let positions = gio::Menu::new();
    positions.append_item(&string_target_item("Top", "bigtoolbar.position", "top"));
    positions.append_item(&string_target_item(
        "Bottom",
        "bigtoolbar.position",
        "bottom",
    ));
    let toolbar_section = gio::Menu::new();
    toolbar_section.append_submenu(Some("Toolbar Position"), &positions);
    toolbar_section.append(Some("Hide Toolbar"), Some("bigtoolbar.hide"));
    menu.append_section(None, &toolbar_section);
    menu
}

fn descendant_label(widget: &gtk::Widget) -> Option<String> {
    subtree(widget)
        .find_map(|widget| widget.downcast::<gtk::Label>().ok())
        .map(|label| label.text().to_string())
}

fn collect_actionable_labels(widget: &gtk::Widget, labels: &mut Vec<String>) {
    let actionable = subtree(widget).filter(|widget| {
        matches!(
            widget.accessible_role(),
            gtk::AccessibleRole::Button
                | gtk::AccessibleRole::MenuItem
                | gtk::AccessibleRole::MenuItemCheckbox
                | gtk::AccessibleRole::MenuItemRadio
        )
    });
    labels.extend(actionable.filter_map(|widget| descendant_label(&widget)));
}

fn find_button_with_label(widget: &gtk::Widget, expected: &str) -> Option<gtk::Button> {
    subtree(widget)
        .filter(|widget| descendant_label(widget).as_deref() == Some(expected))
        .find_map(|widget| widget.downcast::<gtk::Button>().ok())
}

fn find_stack(widget: &gtk::Widget) -> Option<gtk::Stack> {
    find_widget(widget, &|widget| {
        widget.is::<gtk::Stack>() && widget.has_css_class("big-structured-menu-stack")
    })
    .and_downcast()
}

fn find_label_with_css_class(widget: &gtk::Widget, css_class: &str) -> Option<gtk::Label> {
    find_widget(widget, &|widget| {
        widget.is::<gtk::Label>() && widget.has_css_class(css_class)
    })
    .and_downcast()
}

/// The scroller of the stack's visible page, under the box that also carries
/// the page's header strip when it has one.
fn visible_scroller(stack: &gtk::Stack) -> Option<gtk::ScrolledWindow> {
    find_widget(&stack.visible_child()?, &|widget| {
        widget.is::<gtk::ScrolledWindow>()
    })
    .and_downcast()
}

/// The page box of the stack's visible page, under its scroller and viewport.
fn visible_page(stack: &gtk::Stack) -> Option<gtk::Box> {
    let scroller = visible_scroller(stack)?;
    find_widget(scroller.upcast_ref(), &|widget| widget.is::<gtk::Box>()).and_downcast()
}

fn find_widget(
    widget: &gtk::Widget,
    matches: &dyn Fn(&gtk::Widget) -> bool,
) -> Option<gtk::Widget> {
    subtree(widget).find(|widget| matches(widget))
}

fn vertical_separator_count(widget: &gtk::Widget) -> usize {
    let here = usize::from(
        widget
            .downcast_ref::<gtk::Separator>()
            .is_some_and(|separator| separator.orientation() == gtk::Orientation::Vertical),
    );
    let below: usize = children(widget)
        .map(|node| vertical_separator_count(&node))
        .sum();
    here + below
}

fn direct_separator_count(widget: &gtk::Widget) -> usize {
    let mut count = 0;
    for node in children(widget) {
        count += usize::from(node.is::<gtk::Separator>());
    }
    count
}

#[test]
fn flat_population_exposes_named_buttons() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let popover = gtk::Popover::new();
        let items = [
            BigMenuActionItem::new("Saved Profile", "win.open-profile('saved')"),
            BigMenuActionItem::new("Quick Connect", "win.quick-connect"),
        ];

        populate_flat_action_popover(&popover, &action_context, &items);

        let list = popover
            .child()
            .and_then(|child| child.downcast::<gtk::ListBox>().ok())
            .expect("flat action popover owns an accessible list");
        assert!(gtk::test_accessible_has_role(
            &list,
            gtk::AccessibleRole::List
        ));
        for label in ["Saved Profile", "Quick Connect"] {
            let button = find_button_with_label(list.upcast_ref(), label)
                .expect("each flat action remains a named native button");
            assert!(gtk::test_accessible_has_role(
                &button,
                gtk::AccessibleRole::Button
            ));
        }
        assert_eq!(popover.accessible_role(), gtk::AccessibleRole::Group);
    });
}

/// The context is held from the click on: the popover's `closed` handlers,
/// which run as it closes, may drop its last other reference.
#[test]
fn row_acts_on_context_released_at_close() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let actions = gio::SimpleActionGroup::new();
        let action = gio::SimpleAction::new("export", None);
        let activations = Rc::new(std::cell::Cell::new(0));
        action.connect_activate({
            let activations = Rc::clone(&activations);
            move |_, _| activations.set(activations.get() + 1)
        });
        actions.add_action(&action);
        action_context.insert_action_group("win", Some(&actions));

        let anchor = gtk::Button::new();
        let window = gtk::Window::builder().child(&anchor).build();
        window.present();
        let popover = build_flat_action_popover(
            &action_context,
            &[BigMenuActionItem::new("Export", "win.export")],
        );
        popover.set_parent(&anchor);
        let held = Rc::new(std::cell::RefCell::new(Some(action_context)));
        popover.connect_closed({
            let held = Rc::clone(&held);
            move |_| drop(held.borrow_mut().take())
        });
        popover.popup();
        assert!(big_testkit::harness::pump_until(|| popover.is_mapped()));
        let child = popover.child().expect("flat popover owns its list");
        let button = find_button_with_label(&child, "Export").expect("named action button exists");

        button.emit_clicked();

        assert!(held.borrow().is_none(), "closing released the context");
        assert_eq!(activations.get(), 1);
        popover.unparent();
        window.destroy();
    });
}

#[test]
fn flat_population_uses_explicit_context() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let actions = gio::SimpleActionGroup::new();
        let action = gio::SimpleAction::new("export", None);
        let activations = Rc::new(std::cell::Cell::new(0));
        action.connect_activate({
            let activations = Rc::clone(&activations);
            move |_, _| activations.set(activations.get() + 1)
        });
        actions.add_action(&action);
        action_context.insert_action_group("win", Some(&actions));

        let unrelated_parent = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let popover = gtk::Popover::new();
        popover.set_parent(&unrelated_parent);
        populate_flat_action_popover(
            &popover,
            &action_context,
            &[BigMenuActionItem::new("Export", "win.export")],
        );
        let child = popover.child().expect("flat popover owns its list");
        let button = find_button_with_label(&child, "Export").expect("named action button exists");

        button.emit_clicked();

        assert_eq!(activations.get(), 1);
        popover.unparent();
    });
}

#[test]
fn structured_population_replaces_rows() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let popover = gtk::Popover::new();
        let stale_menu = gio::Menu::new();
        stale_menu.append(Some("Stale Action"), Some("bigtoolbar.stale"));
        populate_menu_model_action_popover(
            &popover,
            &action_context,
            &stale_menu,
            &popover_labels(),
            &[],
        );

        populate_menu_model_action_popover(
            &popover,
            &action_context,
            &toolbar_menu_model(),
            &popover_labels(),
            &[],
        );

        let child = popover.child().expect("structured popover owns a stack");
        assert!(child.is::<gtk::Stack>());
        let root_page = child
            .downcast_ref::<gtk::Stack>()
            .and_then(visible_page)
            .expect("structured popover shows its root page");
        assert_eq!(direct_separator_count(root_page.upcast_ref()), 2);
        let mut actionable_labels = Vec::new();
        collect_actionable_labels(&child, &mut actionable_labels);
        assert!(actionable_labels.contains(&"Band Default (Icon Only)".to_owned()));
        assert!(actionable_labels.contains(&"Move to".to_owned()));
        assert!(actionable_labels.contains(&"Toolbar Position".to_owned()));
        assert!(actionable_labels.contains(&"Hide Toolbar".to_owned()));
        assert!(!actionable_labels.contains(&"Stale Action".to_owned()));
        assert_eq!(popover.accessible_role(), gtk::AccessibleRole::Group);
        assert!(popover.has_css_class("menu"));
        let submenu =
            find_button_with_label(&child, "Move to").expect("submenu opener is a native button");
        assert_eq!(submenu.type_().name(), "GtkButton");
        assert!(submenu.activate(), "native submenu button activates");
    });
}

#[test]
fn shown_popover_refills_without_critical() {
    big_testkit::harness::require_gtk_test(|| {
        // GTK logs through the structured writer, which `log_set_handler`
        // never sees; a process takes one writer, so this test owns it.
        static CRITICALS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        glib::log_set_writer_func(|level, fields| {
            if level == glib::LogLevel::Critical
                && let Some(message) = fields
                    .iter()
                    .find(|field| field.key() == "MESSAGE")
                    .and_then(glib::LogField::value_str)
            {
                CRITICALS
                    .lock()
                    .expect("log capture lock")
                    .push(message.to_owned());
            }
            glib::log_writer_default(level, fields)
        });
        let anchor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let window = gtk::Window::new();
        window.set_child(Some(&anchor));
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        let popover = gtk::Popover::new();
        popover.set_parent(&anchor);
        popover.set_autohide(false);
        for _ in 0..2 {
            populate_menu_model_action_popover(
                &popover,
                &anchor,
                &toolbar_menu_model(),
                &popover_labels(),
                &[],
            );
            popover.popup();
            assert!(big_testkit::harness::pump_until(|| popover.is_visible()));
            // What assistive technology reading the popover does: its context
            // is realized, after which GTK refuses any new role.
            popover.update_platform_state(gtk::AccessiblePlatformState::Focused);
            popover.popdown();
        }
        popover.unparent();
        window.close();
        assert_eq!(
            CRITICALS.lock().expect("log capture lock").as_slice(),
            &[] as &[String]
        );
        assert_eq!(popover.accessible_role(), gtk::AccessibleRole::Group);
    });
}

#[test]
fn structured_popover_follows_its_model() {
    big_testkit::harness::require_gtk_test(|| {
        let labels_of = |popover: &gtk::Popover| {
            let mut labels = Vec::new();
            collect_actionable_labels(&popover.child().expect("filled popover"), &mut labels);
            labels
        };
        let anchor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let window = gtk::Window::new();
        window.set_child(Some(&anchor));
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        let popover = gtk::Popover::new();
        popover.set_parent(&anchor);
        popover.set_autohide(false);
        let section = gio::Menu::new();
        section.append(Some("First"), Some("bigtoolbar.first"));
        let menu = gio::Menu::new();
        menu.append_section(None, &section);
        populate_menu_model_action_popover(&popover, &anchor, &menu, &popover_labels(), &[]);
        // The caller may keep only the parts it changes.
        drop(menu);
        let built = popover.child();

        // Closed, it is filled again only when it opens.
        section.append(Some("Added"), Some("bigtoolbar.added"));
        assert_eq!(popover.child(), built);
        popover.popup();
        assert!(big_testkit::harness::pump_until(|| popover.is_visible()));
        assert_eq!(labels_of(&popover), ["First", "Added"]);

        // Open, at once.
        section.remove(0);
        assert_eq!(labels_of(&popover), ["Added"]);
        popover.popdown();
        popover.unparent();
        window.close();
    });
}

#[test]
fn shown_menu_leaves_parent_size() {
    big_testkit::harness::require_gtk_test(|| {
        let anchor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        anchor.append(&gtk::Button::with_label("Open"));
        let window = gtk::Window::new();
        window.set_child(Some(&anchor));
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        let popover = gtk::Popover::new();
        popover.set_parent(&anchor);
        popover.set_autohide(false);
        populate_menu_model_action_popover(
            &popover,
            &anchor,
            &toolbar_menu_model(),
            &popover_labels(),
            &[],
        );
        popover.popup();
        assert!(big_testkit::harness::pump_until(|| popover.is_visible()));
        // The bar keeps its width while its menu is open.
        assert!(!anchor.compute_expand(gtk::Orientation::Horizontal));
        assert!(!anchor.compute_expand(gtk::Orientation::Vertical));
        popover.popdown();
        popover.unparent();
        window.close();
    });
}

#[test]
fn repopulation_releases_replaced_widgets() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let popover = gtk::Popover::new();
        popover.set_parent(&action_context);
        let actions = gio::SimpleActionGroup::new();
        let position = gio::SimpleAction::new_stateful(
            "position",
            Some(glib::VariantTy::STRING),
            &"top".to_variant(),
        );
        actions.add_action(&position);
        let binding = BigMenuActionGroupBinding::new("bigtoolbar", &actions);
        let mut previous_stack = None;

        for selected in ["top", "bottom", "top", "bottom"] {
            position.set_state(&selected.to_variant());
            populate_menu_model_action_popover(
                &popover,
                &action_context,
                &toolbar_menu_model(),
                &popover_labels(),
                std::slice::from_ref(&binding),
            );
            while glib::MainContext::default().iteration(false) {}
            if let Some(previous_stack) = previous_stack.take() {
                assert!(
                    glib::WeakRef::<gtk::Stack>::upgrade(&previous_stack).is_none(),
                    "repopulation must release the prior stack and its signal owners"
                );
            }
            let stack = popover
                .child()
                .and_downcast::<gtk::Stack>()
                .expect("structured population owns the current stack");
            let selected_label = if selected == "top" { "Top" } else { "Bottom" };
            let selected_row = find_button_with_label(stack.upcast_ref(), selected_label)
                .expect("current radio row remains present");
            assert!(selected_row.has_css_class("checked"));
            previous_stack = Some(stack.downgrade());
        }

        popover.unparent();
    });
}

#[test]
fn structured_population_reflects_action_state() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let actions = gio::SimpleActionGroup::new();
        let position = gio::SimpleAction::new_stateful(
            "position",
            Some(glib::VariantTy::STRING),
            &"top".to_variant(),
        );
        let activated_target = Rc::new(std::cell::RefCell::new(None));
        position.connect_activate({
            let activated_target = Rc::clone(&activated_target);
            move |action, parameter| {
                let target = parameter
                    .and_then(|value| value.get::<String>())
                    .expect("radio target remains a string");
                action.set_state(&target.to_variant());
                *activated_target.borrow_mut() = Some(target);
            }
        });
        actions.add_action(&position);
        let hide = gio::SimpleAction::new("hide", None);
        hide.set_enabled(false);
        let hide_activations = Rc::new(std::cell::Cell::new(0));
        hide.connect_activate({
            let hide_activations = Rc::clone(&hide_activations);
            move |_, _| hide_activations.set(hide_activations.get() + 1)
        });
        actions.add_action(&hide);
        action_context.insert_action_group("bigtoolbar", Some(&actions));

        let popover = gtk::Popover::new();
        popover.set_parent(&action_context);
        let binding = BigMenuActionGroupBinding::new("bigtoolbar", &actions);
        populate_menu_model_action_popover(
            &popover,
            &action_context,
            &toolbar_menu_model(),
            &popover_labels(),
            std::slice::from_ref(&binding),
        );
        let child = popover.child().expect("structured popover owns a stack");
        let top = find_button_with_label(&child, "Top").expect("selected radio exists");
        let bottom = find_button_with_label(&child, "Bottom").expect("radio target exists");
        assert_eq!(top.accessible_role(), gtk::AccessibleRole::MenuItemRadio);
        assert_eq!(top.action_name().as_deref(), Some("bigtoolbar.position"));
        assert_eq!(
            top.action_target_value()
                .and_then(|target| target.get::<String>()),
            Some("top".to_owned())
        );
        assert!(top.has_css_class("checked"));
        assert_eq!(bottom.accessible_role(), gtk::AccessibleRole::MenuItemRadio);
        assert!(!bottom.has_css_class("checked"));

        let hide_button = find_button_with_label(&child, "Hide Toolbar")
            .expect("disabled action remains visible");
        assert!(!hide_button.is_sensitive());
        hide_button.emit_clicked();
        assert_eq!(hide_activations.get(), 0);

        hide.set_enabled(true);
        while glib::MainContext::default().iteration(false) {}
        assert!(hide_button.is_sensitive());
        hide_button.emit_clicked();
        assert_eq!(hide_activations.get(), 1);

        bottom.emit_clicked();

        assert_eq!(activated_target.borrow().as_deref(), Some("bottom"));
        assert_eq!(
            position
                .state()
                .and_then(|state| state.get::<String>())
                .as_deref(),
            Some("bottom")
        );
        while glib::MainContext::default().iteration(false) {}
        assert!(!top.has_css_class("checked"));
        assert!(bottom.has_css_class("checked"));
        popover.unparent();
    });
}

#[test]
fn native_menu_survives_reopening() {
    big_testkit::harness::require_gtk_test(|| {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&gio::SimpleAction::new(
            "item-mode",
            Some(glib::VariantTy::STRING),
        ));
        actions.add_action(&gio::SimpleAction::new(
            "move-item",
            Some(glib::VariantTy::STRING),
        ));
        let position = gio::SimpleAction::new_stateful(
            "position",
            Some(glib::VariantTy::STRING),
            &"top".to_variant(),
        );
        actions.add_action(&position);
        let hide = gio::SimpleAction::new("hide", None);
        hide.set_enabled(false);
        let hide_activations = Rc::new(std::cell::Cell::new(0));
        hide.connect_activate({
            let hide_activations = Rc::clone(&hide_activations);
            move |_, _| hide_activations.set(hide_activations.get() + 1)
        });
        actions.add_action(&hide);

        let button = gtk::MenuButton::new();
        set_menu_model_action_popover(
            &button,
            &toolbar_menu_model(),
            &popover_labels(),
            &[BigMenuActionGroupBinding::new("bigtoolbar", &actions)],
        );
        let window = gtk::Window::new();
        window.set_child(Some(&button));
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));

        let popover = button
            .popover()
            .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
            .expect("menu button owns a native popover menu");
        assert_eq!(popover.accessible_role(), gtk::AccessibleRole::Menu);
        // Without input the compositor dismisses a grabbing popup (see
        // `custom_children_stay_in_their_page`).
        popover.set_autohide(false);
        let stack = find_stack(popover.upcast_ref()).expect("native custom menu owns its stack");
        assert_eq!(stack.visible_child_name().as_deref(), Some("root"));
        button.popup();
        // `iteration(false)` returns as soon as nothing is pending, which under
        // a loaded machine can be before the popover has been mapped at all.
        assert!(big_testkit::harness::pump_until(|| popover.is_visible()));

        let move_to = find_button_with_label(popover.upcast_ref(), "Move to")
            .expect("submenu opener remains explicitly named");
        let new_toolbar = find_button_with_label(popover.upcast_ref(), "New Toolbar")
            .expect("submenu action remains explicitly named");
        assert_eq!(move_to.accessible_role(), gtk::AccessibleRole::MenuItem);
        assert!(gtk::test_accessible_has_property(
            &move_to,
            gtk::AccessibleProperty::HasPopup
        ));
        assert!(gtk::test_accessible_has_state(
            &move_to,
            gtk::AccessibleState::Expanded
        ));
        assert!(move_to.has_css_class("flat"));
        assert!(move_to.has_css_class("body"));
        let move_to_content = move_to
            .child()
            .and_downcast::<gtk::Box>()
            .expect("menu row owns its content");
        assert_eq!(move_to_content.margin_top(), 0);
        assert_eq!(move_to_content.margin_bottom(), 0);
        assert_eq!(move_to_content.margin_start(), 0);
        assert_eq!(move_to_content.margin_end(), 0);
        assert!(move_to.is_visible());
        assert!(new_toolbar.is_sensitive());

        let top = find_button_with_label(popover.upcast_ref(), "Top")
            .expect("selected radio remains explicitly named");
        let bottom = find_button_with_label(popover.upcast_ref(), "Bottom")
            .expect("radio target remains explicitly named");
        assert_eq!(top.accessible_role(), gtk::AccessibleRole::MenuItemRadio);
        assert!(top.has_css_class("checked"));
        assert!(!bottom.has_css_class("checked"));
        position.set_state(&"bottom".to_variant());
        while glib::MainContext::default().iteration(false) {}
        assert!(!top.has_css_class("checked"));
        assert!(bottom.has_css_class("checked"));

        let hide_button = find_button_with_label(popover.upcast_ref(), "Hide Toolbar")
            .expect("action row remains explicitly named");
        assert_eq!(hide_button.accessible_role(), gtk::AccessibleRole::MenuItem);
        assert!(!hide_button.is_sensitive());
        hide_button.emit_clicked();
        assert_eq!(hide_activations.get(), 0);

        hide.set_enabled(true);
        while glib::MainContext::default().iteration(false) {}
        assert!(hide_button.is_sensitive());
        hide_button.emit_clicked();
        assert_eq!(hide_activations.get(), 1);
        // Closing and reopening are mapping changes like the first popup, and
        // draining what is pending once can land before either happened.
        assert!(big_testkit::harness::pump_until(|| !popover.is_visible()));
        button.popup();
        assert!(big_testkit::harness::pump_until(|| popover.is_visible()));

        move_to.emit_clicked();
        assert!(big_testkit::harness::pump_until(|| new_toolbar.is_mapped()));
        assert!(big_testkit::harness::pump_until(|| new_toolbar.is_focus()));
        assert!(
            new_toolbar
                .native()
                .and_downcast::<gtk::Popover>()
                .is_some_and(|native| native.has_css_class(SUBMENU_CSS_CLASS)),
            "the submenu opens in its own popover beside the menu"
        );
        assert!(
            popover.is_visible(),
            "the menu stays open beside its submenu"
        );

        button.popdown();
        assert!(big_testkit::harness::pump_until(|| !popover.is_visible()));
        assert!(!new_toolbar.is_mapped(), "the submenu closed with its menu");
        button.popup();
        assert!(big_testkit::harness::pump_until(|| popover.is_visible()));
        assert!(
            !new_toolbar.is_mapped(),
            "it reopens with its submenus closed"
        );

        button.popdown();
        window.close();
    });
}

#[test]
fn custom_children_stay_in_their_page() {
    big_testkit::harness::require_gtk_test(|| {
        let menu = gio::Menu::new();
        let custom = gio::MenuItem::new(None, None);
        custom.set_attribute_value("custom", Some(&"owner-header".to_variant()));
        menu.append_item(&custom);
        let submenu = gio::Menu::new();
        submenu.append(Some("Nested Action"), Some("win.nested"));
        menu.append_submenu(Some("More"), &submenu);
        let owner_header = gtk::Label::new(Some("Owner Header"));
        let popover = build_menu_model_action_popover_menu(
            &menu,
            &popover_labels(),
            &[(owner_header.upcast_ref(), "owner-header")],
            &[],
        );
        // A popup that grabs asks with the serial of the last input event, and
        // with none in a test the compositor dismisses it a frame later.
        popover.set_autohide(false);
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        let window = gtk::Window::new();
        window.set_child(Some(&button));
        window.present();
        button.popup();
        assert!(big_testkit::harness::pump_until(
            || window.is_mapped() && popover.is_visible()
        ));

        let stack = find_stack(popover.upcast_ref()).expect("native custom menu owns its stack");
        let root = stack
            .child_by_name("root")
            .expect("structured menu owns a root page");
        assert!(owner_header.is_ancestor(&root));
        // The popover is its own surface: its content maps a round trip after
        // the popover itself becomes visible.
        assert!(big_testkit::harness::pump_until(|| owner_header.is_mapped()));

        let more = find_button_with_label(popover.upcast_ref(), "More")
            .expect("submenu opener remains present");
        more.emit_clicked();
        let nested = find_button_with_label(popover.upcast_ref(), "Nested Action")
            .expect("the submenu's row is built");
        assert!(big_testkit::harness::pump_until(|| nested.is_mapped()));
        assert_eq!(stack.visible_child_name().as_deref(), Some("root"));
        assert!(
            owner_header.is_mapped(),
            "the menu stays beside its submenu"
        );

        button.popdown();
        window.close();
    });
}

#[test]
fn menu_button_is_the_named_control() {
    big_testkit::harness::require_gtk_test(|| {
        let button = gtk::MenuButton::new();
        button.update_property(&[gtk::accessible::Property::Label("Stale wrapper label")]);

        set_menu_button_accessible_label(&button, "Application Menu");

        assert_eq!(button.accessible_role(), gtk::AccessibleRole::Button);
        assert!(gtk::test_accessible_has_property(
            &button,
            gtk::AccessibleProperty::Label
        ));
    });
}

#[test]
fn action_rows_refresh_accelerator_labels() {
    big_testkit::harness::require_gtk_test(|| {
        let application = gtk::Application::builder()
            .application_id("br.com.biglinux.MenuAcceleratorTest")
            .build();
        let target = "pdf".to_variant();
        let detailed_action = gio::Action::print_detailed_name("win.export", Some(&target));
        application.set_accels_for_action(&detailed_action, &["<Ctrl>e"]);
        let popover = gtk::Popover::new();
        let row = build_hierarchy_action_menu_row(
            &popover,
            &BigMenuActionItem::new("Export PDF", "win.export('pdf')"),
        );

        super::action_row::refresh_action_row_accelerators(row.upcast_ref(), &application);

        let shortcut = find_label_with_css_class(row.upcast_ref(), "action-accelerator")
            .expect("native action row reserves a standard accelerator label");
        let (key, modifiers) = gtk::accelerator_parse("<Ctrl>e").expect("test accelerator parses");
        assert!(shortcut.is_visible());
        assert_eq!(shortcut.text(), gtk::accelerator_get_label(key, modifiers));
        assert!(gtk::test_accessible_has_property(
            &row,
            gtk::AccessibleProperty::KeyShortcuts
        ));

        application.set_accels_for_action(&detailed_action, &[]);
        super::action_row::refresh_action_row_accelerators(row.upcast_ref(), &application);
        assert!(!shortcut.is_visible());
        assert!(!gtk::test_accessible_has_property(
            &row,
            gtk::AccessibleProperty::KeyShortcuts
        ));
    });
}

/// `None` when `widget`'s `KEY_SHORTCUTS` is `expected`, else what GTK found.
fn key_shortcuts_mismatch(
    widget: &gtk::Button,
    expected: &std::ffi::CStr,
) -> Option<glib::GString> {
    // SAFETY: the widget outlives the call, and the property's variadic value
    // is a NUL-terminated string.
    unsafe {
        glib::translate::from_glib_full(gtk::ffi::gtk_test_accessible_check_property(
            glib::translate::ToGlibPtr::to_glib_none(widget.upcast_ref::<gtk::Accessible>()).0,
            gtk::ffi::GTK_ACCESSIBLE_PROPERTY_KEY_SHORTCUTS,
            expected.as_ptr(),
        ))
    }
}

#[test]
fn rows_name_the_shortcut_shown() {
    big_testkit::harness::require_gtk_test(|| {
        let application = gtk::Application::builder()
            .application_id("br.com.biglinux.MenuShortcutNameTest")
            .build();
        application.set_accels_for_action(
            "win.properties",
            &["<Primary>I", "<Alt>Return", "<Alt>KP_Enter"],
        );
        application.set_accels_for_action("win.details", &["<Alt>Return", "<Alt>KP_Enter"]);
        let menu = gio::Menu::new();
        let rename = described_item("Rename…", None, None, "win.rename");
        rename.set_attribute_value("accel", Some(&"F2".to_variant()));
        append_strip_section(&menu, None, &[rename]);
        let trash = described_item("Move to Trash", None, None, "win.trash");
        trash.set_attribute_value("accel", Some(&"Delete".to_variant()));
        let delete = described_item("Delete Permanently…", None, None, "win.delete");
        delete.set_attribute_value("accel", Some(&"<Shift>Delete".to_variant()));
        append_split_section(&menu, &trash, &delete);
        let properties = described_item("Properties", None, None, "win.properties");
        properties.set_attribute_value("accel", Some(&"<Control>i".to_variant()));
        menu.append_item(&properties);
        menu.append_item(&described_item("Details", None, None, "win.details"));
        let popover = build_menu_model_action_popover_menu(&menu, &popover_labels(), &[], &[]);

        super::action_row::refresh_action_row_accelerators(popover.upcast_ref(), &application);

        // ARIA spelling, one key per row: the one the row shows.
        for (label, expected) in [
            ("Rename…", c"F2"),
            ("Move to Trash", c"Delete"),
            ("Delete Permanently…", c"Shift+Delete"),
            ("Properties", c"Control+I"),
            ("Details", c"Alt+Enter"),
        ] {
            let row = find_button_with_label(popover.upcast_ref(), label)
                .expect("every item is a named row");
            assert_eq!(key_shortcuts_mismatch(&row, expected), None, "{label}");
        }
    });
}

#[test]
fn model_attributes_reach_the_row() {
    big_testkit::harness::require_gtk_test(|| {
        let menu = gio::Menu::new();
        let rename = described_item(
            "Rename…",
            Some("Give the file another name"),
            Some("document-edit-symbolic"),
            "win.rename",
        );
        rename.set_attribute_value("accel", Some(&"F2".to_variant()));
        menu.append_item(&rename);
        let delete = described_item("Delete Permanently…", None, None, "win.delete");
        mark_destructive(&delete);
        menu.append_item(&delete);

        let popover = build_menu_model_action_popover_menu(&menu, &popover_labels(), &[], &[]);

        let rename_row = find_button_with_label(popover.upcast_ref(), "Rename…")
            .expect("described item is a named row");
        let (key, modifiers) = gtk::accelerator_parse("F2").expect("F2 parses");
        let shortcut = find_widget(rename_row.upcast_ref(), &|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text() == gtk::accelerator_get_label(key, modifiers))
        });
        assert!(shortcut.is_some(), "the model's accel is shown as a hint");
        let subtitle = find_widget(rename_row.upcast_ref(), &|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text() == "Give the file another name")
        })
        .expect("the subtitle is drawn under the label");
        assert_eq!(
            subtitle.accessible_role(),
            gtk::AccessibleRole::Label,
            "the subtitle can be told apart from an off reason"
        );
        assert!(
            find_widget(rename_row.upcast_ref(), &|widget| widget
                .downcast_ref::<gtk::Image>()
                .is_some_and(
                    |image| image.icon_name().as_deref() == Some("document-edit-symbolic")
                ))
            .is_some(),
            "the icon leads the row"
        );
        assert!(gtk::test_accessible_has_property(
            &rename_row,
            gtk::AccessibleProperty::Description
        ));

        let delete_row = find_button_with_label(popover.upcast_ref(), "Delete Permanently…")
            .expect("destructive item is a named row");
        let label = find_widget(delete_row.upcast_ref(), &|widget| widget.is::<gtk::Label>())
            .expect("row owns its label");
        assert!(label.has_css_class("error"));
    });
}

#[test]
fn off_row_says_why() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let actions = gio::SimpleActionGroup::new();
        let close = gio::SimpleAction::new("close-pane", None);
        close.set_enabled(false);
        actions.add_action(&close);
        action_context.insert_action_group("win", Some(&actions));
        let menu = gio::Menu::new();
        let item = gio::MenuItem::new(Some("Close pane"), Some("win.close-pane"));
        item.set_attribute_value(
            BIG_OFF_REASON_ATTRIBUTE,
            Some(&"There is only one pane".to_variant()),
        );
        menu.append_item(&item);

        let popover = gtk::Popover::new();
        popover.set_parent(&action_context);
        let binding = BigMenuActionGroupBinding::new("win", &actions);
        populate_menu_model_action_popover(
            &popover,
            &action_context,
            &menu,
            &popover_labels(),
            std::slice::from_ref(&binding),
        );
        while glib::MainContext::default().iteration(false) {}
        let row = find_button_with_label(popover.upcast_ref(), "Close pane").expect("named row");
        assert!(!row.is_sensitive());
        assert_eq!(
            row.tooltip_text().as_deref(),
            Some("There is only one pane")
        );
        assert!(gtk::test_accessible_has_property(
            &row,
            gtk::AccessibleProperty::Description
        ));

        close.set_enabled(true);
        while glib::MainContext::default().iteration(false) {}
        assert!(row.is_sensitive());
        assert_eq!(row.tooltip_text(), None);
        assert!(!gtk::test_accessible_has_property(
            &row,
            gtk::AccessibleProperty::Description
        ));
        popover.unparent();
    });
}

#[test]
fn off_switch_says_why() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let actions = gio::SimpleActionGroup::new();
        let preview = gio::SimpleAction::new_stateful("preview", None, &false.to_variant());
        preview.set_enabled(false);
        actions.add_action(&preview);
        action_context.insert_action_group("win", Some(&actions));
        let menu = gio::Menu::new();
        let item = switch_item("Preview", None, "win.preview");
        item.set_attribute_value(
            BIG_OFF_REASON_ATTRIBUTE,
            Some(&"Only a Markdown document has a preview".to_variant()),
        );
        menu.append_item(&item);

        let popover = gtk::Popover::new();
        popover.set_parent(&action_context);
        let binding = BigMenuActionGroupBinding::new("win", &actions);
        populate_menu_model_action_popover(
            &popover,
            &action_context,
            &menu,
            &popover_labels(),
            std::slice::from_ref(&binding),
        );
        while glib::MainContext::default().iteration(false) {}
        let row = find_button_with_label(popover.upcast_ref(), "Preview").expect("switch row");
        assert!(!row.is_sensitive());
        assert_eq!(
            row.tooltip_text().as_deref(),
            Some("Only a Markdown document has a preview")
        );
        assert!(gtk::test_accessible_has_property(
            &row,
            gtk::AccessibleProperty::Description
        ));

        preview.set_enabled(true);
        while glib::MainContext::default().iteration(false) {}
        assert!(row.is_sensitive());
        assert_eq!(row.tooltip_text(), None);
        popover.unparent();
    });
}

#[test]
fn off_strip_button_says_why() {
    big_testkit::harness::require_gtk_test(|| {
        let action_context = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let actions = gio::SimpleActionGroup::new();
        let copy = gio::SimpleAction::new("copy", None);
        copy.set_enabled(false);
        actions.add_action(&copy);
        action_context.insert_action_group("win", Some(&actions));
        let menu = gio::Menu::new();
        let item = described_item("Copy", None, Some("edit-copy-symbolic"), "win.copy");
        item.set_attribute_value(
            BIG_OFF_REASON_ATTRIBUTE,
            Some(&"Nothing is selected".to_variant()),
        );
        append_strip_section(&menu, None, &[item]);

        let popover = gtk::Popover::new();
        popover.set_parent(&action_context);
        let binding = BigMenuActionGroupBinding::new("win", &actions);
        populate_menu_model_action_popover(
            &popover,
            &action_context,
            &menu,
            &popover_labels(),
            std::slice::from_ref(&binding),
        );
        while glib::MainContext::default().iteration(false) {}
        let button = find_button_with_label(popover.upcast_ref(), "Copy").expect("strip button");
        assert!(!button.is_sensitive());
        assert_eq!(
            button.tooltip_text().as_deref(),
            Some("Nothing is selected")
        );

        copy.set_enabled(true);
        while glib::MainContext::default().iteration(false) {}
        assert!(button.is_sensitive());
        assert_eq!(button.tooltip_text(), None);
        popover.unparent();
    });
}

#[test]
fn long_subtitle_wraps_in_row() {
    big_testkit::harness::require_gtk_test(|| {
        let menu = gio::Menu::new();
        let reason = "It keeps the desktop or the login running, so it cannot be stopped, restarted or disabled here. ".repeat(2);
        menu.append_item(&described_item("Stop", Some(&reason), None, "win.stop"));

        let popover = build_menu_model_action_popover_menu(&menu, &popover_labels(), &[], &[]);

        let row = find_button_with_label(popover.upcast_ref(), "Stop").expect("named row");
        let (_, natural, _, _) = row.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            natural < 480,
            "a long subtitle widened the row to {natural}"
        );
    });
}

#[test]
fn pages_scroll() {
    big_testkit::harness::require_gtk_test(|| {
        let popover = build_menu_model_action_popover_menu(
            &toolbar_menu_model(),
            &popover_labels(),
            &[],
            &[],
        );
        let stack = find_stack(popover.upcast_ref()).expect("structured menu owns its stack");
        let scroller = visible_scroller(&stack).expect("each page sits in a scroller");
        assert_eq!(scroller.max_content_height(), PAGE_CEILING);
        assert!(scroller.propagates_natural_height());
        assert_eq!(scroller.hscrollbar_policy(), gtk::PolicyType::Never);
    });
}

fn numeric_group() -> (
    gio::SimpleActionGroup,
    gio::SimpleAction,
    gio::SimpleAction,
    gio::SimpleAction,
) {
    let actions = gio::SimpleActionGroup::new();
    let mode = gio::SimpleAction::new_stateful(
        "mode",
        Some(glib::VariantTy::STRING),
        &"grid".to_variant(),
    );
    mode.connect_activate(|action, parameter| {
        if let Some(parameter) = parameter {
            action.set_state(parameter);
        }
    });
    actions.add_action(&mode);
    let previews = gio::SimpleAction::new_stateful("previews", None, &true.to_variant());
    previews.connect_change_state(|action, state| {
        if let Some(state) = state {
            action.set_state(state);
        }
    });
    actions.add_action(&previews);
    let zoom = gio::SimpleAction::new_stateful("zoom", None, &2_i32.to_variant());
    zoom.connect_change_state(|action, state| {
        if let Some(state) = state {
            action.set_state(state);
        }
    });
    actions.add_action(&zoom);
    (actions, mode, previews, zoom)
}

/// A strip in first place, with nothing named above it, is the page's header:
/// one band across the popover, split by vertical rules, above the scrolling
/// rows and with no line under it.
#[test]
fn leading_strip_becomes_page_header() {
    big_testkit::harness::require_gtk_test(|| {
        let menu = gio::Menu::new();
        append_strip_section(
            &menu,
            None,
            &[
                described_item("Copy", None, Some("edit-copy-symbolic"), "win.copy"),
                described_item("Cut", None, Some("edit-cut-symbolic"), "win.cut"),
            ],
        );
        let rows = gio::Menu::new();
        rows.append(Some("Open"), Some("win.open"));
        menu.append_section(None, &rows);

        let popover = build_menu_model_action_popover_menu(&menu, &popover_labels(), &[], &[]);
        let stack = find_stack(popover.upcast_ref()).expect("structured menu owns its stack");
        let page_root = stack
            .visible_child()
            .and_downcast::<gtk::Box>()
            .expect("a page carries its header above its scroller");
        let header = page_root.first_child().expect("the header comes first");
        assert!(header.has_css_class(rich_row::HEADER_STRIP_CSS_CLASS));
        assert!(
            !header.has_css_class("linked"),
            "a header band is not a card"
        );
        assert_eq!(
            header.margin_start(),
            0,
            "the band reaches the popover edge"
        );
        assert_eq!(header.margin_end(), 0, "the band reaches the popover edge");
        assert_eq!(
            vertical_separator_count(&header),
            1,
            "one rule between two buttons"
        );
        let page = visible_page(&stack).expect("the rows scroll under the header");
        assert_eq!(
            direct_separator_count(page.upcast_ref()),
            0,
            "the header is not underlined"
        );
        assert!(find_button_with_label(popover.upcast_ref(), "Copy").is_some());
    });
}

#[test]
fn strip_section_draws_toggles_and_buttons() {
    big_testkit::harness::require_gtk_test(|| {
        let (actions, mode, _, _) = numeric_group();
        let copies = Rc::new(std::cell::Cell::new(0));
        let copy = gio::SimpleAction::new("copy", None);
        copy.connect_activate({
            let copies = Rc::clone(&copies);
            move |_, _| copies.set(copies.get() + 1)
        });
        actions.add_action(&copy);

        let menu = gio::Menu::new();
        append_strip_section(
            &menu,
            Some("View"),
            &[
                described_item("Icons", None, Some("view-grid-symbolic"), "view.mode::grid"),
                described_item(
                    "Details",
                    None,
                    Some("view-list-symbolic"),
                    "view.mode::list",
                ),
            ],
        );
        append_strip_section(
            &menu,
            None,
            &[described_item(
                "Copy",
                None,
                Some("edit-copy-symbolic"),
                "view.copy",
            )],
        );
        let popover = build_menu_model_action_popover_menu(
            &menu,
            &popover_labels(),
            &[],
            &[BigMenuActionGroupBinding::new("view", &actions)],
        );
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        let window = gtk::Window::new();
        window.set_child(Some(&button));
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));

        let icons = find_widget(popover.upcast_ref(), &|widget| {
            widget.is::<gtk::ToggleButton>() && descendant_label(widget).as_deref() == Some("Icons")
        })
        .and_downcast::<gtk::ToggleButton>()
        .expect("a bound radio is a toggle");
        let details = find_widget(popover.upcast_ref(), &|widget| {
            widget.is::<gtk::ToggleButton>()
                && descendant_label(widget).as_deref() == Some("Details")
        })
        .and_downcast::<gtk::ToggleButton>()
        .expect("a bound radio is a toggle");
        let strip = icons.parent().expect("toggles sit in the strip");
        assert!(strip.has_css_class("linked"));
        assert!(strip.has_css_class("big-menu-rich-row"));
        assert!(gtk::test_accessible_has_property(
            &icons,
            gtk::AccessibleProperty::Label
        ));
        assert!(icons.is_active());
        assert!(!details.is_active());

        details.emit_clicked();
        assert!(big_testkit::harness::pump_until(|| details.is_active()));
        assert_eq!(
            mode.state()
                .and_then(|state| state.get::<String>())
                .as_deref(),
            Some("list")
        );
        assert!(!icons.is_active());

        let copy_button = find_button_with_label(popover.upcast_ref(), "Copy")
            .expect("a plain action is a button");
        assert!(!copy_button.is::<gtk::ToggleButton>());
        copy_button.emit_clicked();
        assert!(big_testkit::harness::pump_until(|| copies.get() == 1));
        window.close();
    });
}

#[test]
fn switch_and_scale_rows_follow_actions() {
    big_testkit::harness::require_gtk_test(|| {
        let (actions, _, previews, zoom) = numeric_group();
        let menu = gio::Menu::new();
        menu.append_item(&switch_item(
            "Previews",
            Some("Draw the file instead of its type icon"),
            "view.previews",
        ));
        menu.append_item(&scale_item("Entry size", "view.zoom", 0.0, 6.0, 1.0));
        let popover = build_menu_model_action_popover_menu(
            &menu,
            &popover_labels(),
            &[],
            &[BigMenuActionGroupBinding::new("view", &actions)],
        );
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        let window = gtk::Window::new();
        window.set_child(Some(&button));
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));

        let row = find_button_with_label(popover.upcast_ref(), "Previews")
            .expect("switch row is a named row");
        assert_eq!(row.accessible_role(), gtk::AccessibleRole::MenuItemCheckbox);
        let switch = find_widget(row.upcast_ref(), &|widget| widget.is::<gtk::Switch>())
            .and_downcast::<gtk::Switch>()
            .expect("row ends in a switch");
        assert!(switch.is_active());
        assert!(!switch.can_focus(), "the row is the one focus stop");
        row.emit_clicked();
        assert!(big_testkit::harness::pump_until(|| !switch.is_active()));
        assert_eq!(
            previews.state().and_then(|state| state.get::<bool>()),
            Some(false)
        );
        previews.set_state(&true.to_variant());
        assert!(big_testkit::harness::pump_until(|| switch.is_active()));
        // An action that cannot run now greys the row out, not only its switch.
        previews.set_enabled(false);
        assert!(big_testkit::harness::pump_until(|| !row.is_sensitive()));
        previews.set_enabled(true);
        assert!(big_testkit::harness::pump_until(|| row.is_sensitive()));

        let scale = find_widget(popover.upcast_ref(), &|widget| widget.is::<gtk::Scale>())
            .and_downcast::<gtk::Scale>()
            .expect("scale item is a slider");
        assert!(gtk::test_accessible_has_property(
            &scale,
            gtk::AccessibleProperty::Label
        ));
        assert!((scale.value() - 2.0).abs() < f64::EPSILON);
        scale.set_value(5.0);
        assert_eq!(zoom.state().and_then(|state| state.get::<i32>()), Some(5));
        zoom.set_state(&1_i32.to_variant());
        assert!((scale.value() - 1.0).abs() < f64::EPSILON);
        // The slider is off while its action is, from the start too.
        zoom.set_enabled(false);
        assert!(!scale.is_sensitive());
        let later = build_menu_model_action_popover_menu(
            &menu,
            &popover_labels(),
            &[],
            &[BigMenuActionGroupBinding::new("view", &actions)],
        );
        let later_scale = find_widget(later.upcast_ref(), &|widget| widget.is::<gtk::Scale>())
            .expect("scale item is a slider");
        assert!(!later_scale.is_sensitive());
        zoom.set_enabled(true);
        assert!(scale.is_sensitive());
        assert!(later_scale.is_sensitive());
        window.close();
    });
}

#[test]
fn repopulating_keeps_menu_role() {
    big_testkit::harness::require_gtk_test(|| {
        let popover = popover_menu(None);
        let first = gio::Menu::new();
        first.append(Some("First Action"), Some("win.first"));
        populate_menu_model_action_popover_menu(&popover, &first, &popover_labels(), &[]);
        // A weak handle only: a strong one here would be the leak being tested.
        let first_stack = find_stack(popover.upcast_ref())
            .expect("first population owns a stack")
            .downgrade();
        assert_eq!(popover.accessible_role(), gtk::AccessibleRole::Menu);
        assert!(find_button_with_label(popover.upcast_ref(), "First Action").is_some());

        let second = gio::Menu::new();
        second.append(Some("Second Action"), Some("win.second"));
        populate_menu_model_action_popover_menu(&popover, &second, &popover_labels(), &[]);
        while glib::MainContext::default().iteration(false) {}

        assert!(
            first_stack.upgrade().is_none(),
            "the replaced rows are released with their stack"
        );
        assert!(find_button_with_label(popover.upcast_ref(), "First Action").is_none());
        assert!(find_button_with_label(popover.upcast_ref(), "Second Action").is_some());
        assert_eq!(popover.accessible_role(), gtk::AccessibleRole::Menu);
    });
}

/// GTK 4.22's own menu tracker keeps every section link it shows, so a native
/// `GtkPopoverMenu` built per window leaks its sections. The structured
/// renderer reads sections itself and hands GTK a single custom row: the
/// sections of the suite's menus finalize with them.
#[test]
fn structured_menus_release_their_sections() {
    big_testkit::harness::require_gtk_test(|| {
        let census = big_testkit::leak::WidgetFinalizeCensus::new();
        for cycle in 0..25 {
            let section = gio::Menu::new();
            section.append(Some("Open"), Some("win.open"));
            census.track(format!("section {cycle}"), &section);
            let menu = gio::Menu::new();
            menu.append_section(None, &section);
            drop(section);
            let popover = build_menu_model_action_popover_menu(&menu, &popover_labels(), &[], &[]);
            let button = gtk::MenuButton::builder().popover(&popover).build();
            let window = gtk::Window::builder().child(&button).build();
            window.present();
            big_testkit::harness::pump();
            button.popup();
            big_testkit::harness::pump();
            button.popdown();
            drop((menu, popover, button));
            window.destroy();
            drop(window);
            big_testkit::harness::pump();
        }
        assert_eq!(census.survivors(), Vec::<String>::new());
    });
}

/// The bounds of `widget` in `popover`'s coordinates.
fn bounds_in(widget: &impl IsA<gtk::Widget>, popover: &gtk::Popover) -> gtk::graphene::Rect {
    widget
        .compute_bounds(popover)
        .expect("the widget is inside the popover")
}

/// A window makes its first menu as it is built, before its first style, so
/// the rows' style is on the display by then: a menu filled at the first
/// opening restyles nothing in the window.
#[test]
fn later_menu_restyles_nothing() {
    big_testkit::harness::require_gtk_test(|| {
        adw::init().expect("libadwaita");
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover_menu(None)));
        let row = gtk::Button::with_label("Copy");
        row.add_css_class(action_row::MENU_ROW_CSS_CLASS);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&button);
        content.append(&row);
        let window = gtk::Window::new();
        window.set_child(Some(&content));
        window.present();
        assert!(big_testkit::harness::pump_frame(&window));
        let styled = row.measure(gtk::Orientation::Horizontal, -1);
        let menu = gio::Menu::new();
        menu.append(Some("Copy"), Some("view.copy"));
        let popover = build_menu_model_action_popover_menu(&menu, &popover_labels(), &[], &[]);
        assert!(big_testkit::harness::pump_frame(&window));
        let later = row.measure(gtk::Orientation::Horizontal, -1);
        drop(popover);
        window.destroy();

        assert_eq!(later, styled, "building the menu restyled the window");
    });
}

/// A row of the suite's menu is drawn to the measure of a row of GTK's own,
/// and the chrome the menu hangs from, which pads its own controls, changes
/// nothing in it.
#[test]
fn rows_measure_as_native_menu() {
    big_testkit::harness::require_gtk_test(|| {
        adw::init().expect("libadwaita");
        let chrome_css = crate::css::provider();
        chrome_css.load_from_string(
            ".padded-chrome button.flat { min-height: 24px; padding: 2px 4px; }
             .padded-chrome switch { margin: 4px 0; }",
        );
        let display = gtk::gdk::Display::default().expect("the test display");
        #[expect(clippy::disallowed_methods, reason = "a test stacks its own sheet")]
        gtk::style_context_add_provider_for_display(
            &display,
            &chrome_css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&gio::SimpleAction::new("copy", None));
        actions.add_action(&gio::SimpleAction::new_stateful(
            "sidebar",
            None,
            &true.to_variant(),
        ));
        let plain = gio::Menu::new();
        plain.append(Some("Copy"), Some("view.copy"));
        let menu = gio::Menu::new();
        menu.append(Some("Copy"), Some("view.copy"));
        menu.append_item(&switch_item("Sidebar", None, "view.sidebar"));
        let structured = || {
            build_menu_model_action_popover_menu(
                &menu,
                &popover_labels(),
                &[],
                &[BigMenuActionGroupBinding::new("view", &actions)],
            )
        };
        let (native, inside, outside) = (
            popover_menu(Some(plain.upcast_ref())),
            structured(),
            structured(),
        );
        let chrome = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        chrome.add_css_class("padded-chrome");
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.insert_action_group("view", Some(&actions));
        content.append(&chrome);
        for (popover, parent) in [(&native, &chrome), (&inside, &chrome), (&outside, &content)] {
            // Three menus open at once, and no input to keep a grab.
            popover.set_autohide(false);
            let button = gtk::MenuButton::new();
            button.set_popover(Some(popover));
            parent.append(&button);
        }
        let window = gtk::Window::builder().child(&content).build();
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        for popover in [&native, &inside, &outside] {
            popover.popup();
        }
        let modelbutton = find_widget(native.upcast_ref(), &|widget| {
            widget.css_name() == "modelbutton"
        })
        .expect("GTK draws its row as a model button");
        let row = |popover: &gtk::PopoverMenu, label: &str| {
            find_button_with_label(popover.upcast_ref(), label).expect("the row")
        };
        let copy = row(&inside, "Copy");
        assert!(big_testkit::harness::pump_until(|| {
            modelbutton.height() > 0 && copy.height() > 0 && row(&outside, "Sidebar").height() > 0
        }));

        let (ours, gtks) = (
            bounds_in(&copy, inside.upcast_ref()),
            bounds_in(&modelbutton, native.upcast_ref()),
        );
        assert_eq!(ours.height(), gtks.height(), "the row is as tall as GTK's");
        assert_eq!(ours.x(), gtks.x(), "the row is inset as GTK's is");
        let text = |widget: &gtk::Widget, popover: &gtk::Popover| {
            let label =
                find_widget(widget, &|widget| widget.is::<gtk::Label>()).expect("the row's label");
            bounds_in(&label, popover).x()
        };
        assert_eq!(
            text(copy.upcast_ref(), inside.upcast_ref()),
            text(&modelbutton, native.upcast_ref()),
            "the label starts where GTK's does"
        );
        assert_eq!(
            bounds_in(&row(&inside, "Sidebar"), inside.upcast_ref()).height(),
            bounds_in(&row(&outside, "Sidebar"), outside.upcast_ref()).height(),
            "a switch row is as tall in a toolbar as anywhere"
        );
        for popover in [&native, &inside, &outside] {
            popover.popdown();
        }
        window.close();
        gtk::style_context_remove_provider_for_display(&display, &chrome_css);
    });
}

/// Verbs side by side are one row cut by a rule: the row's height, and the
/// edges every other row of the page has.
#[test]
fn verb_strip_is_one_row() {
    big_testkit::harness::require_gtk_test(|| {
        adw::init().expect("libadwaita");
        let actions = gio::SimpleActionGroup::new();
        for name in ["open", "cut", "copy"] {
            actions.add_action(&gio::SimpleAction::new(name, None));
        }
        let menu = gio::Menu::new();
        let open = gio::Menu::new();
        open.append(Some("Open"), Some("view.open"));
        menu.append_section(None, &open);
        append_strip_section(
            &menu,
            None,
            &[
                described_item("Cut", None, Some("edit-cut-symbolic"), "view.cut"),
                described_item("Copy", None, Some("edit-copy-symbolic"), "view.copy"),
            ],
        );
        let popover = build_menu_model_action_popover_menu(
            &menu,
            &popover_labels(),
            &[],
            &[BigMenuActionGroupBinding::new("view", &actions)],
        );
        popover.set_autohide(false);
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        let window = gtk::Window::builder().child(&button).build();
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        popover.popup();
        let row = find_button_with_label(popover.upcast_ref(), "Open").expect("the plain row");
        let cut = find_button_with_label(popover.upcast_ref(), "Cut").expect("the first verb");
        let copy = find_button_with_label(popover.upcast_ref(), "Copy").expect("the second verb");
        assert!(big_testkit::harness::pump_until(|| copy.width() > 0));

        let strip = find_widget(popover.upcast_ref(), &|widget| {
            widget.has_css_class("big-menu-verb-row")
        })
        .expect("the verbs' row");
        assert_eq!(
            vertical_separator_count(&strip),
            1,
            "one rule between two verbs"
        );
        let (row, strip) = (
            bounds_in(&row, popover.upcast_ref()),
            bounds_in(&strip, popover.upcast_ref()),
        );
        for verb in [&cut, &copy] {
            assert_eq!(bounds_in(verb, popover.upcast_ref()).height(), row.height());
        }
        assert_eq!(strip.x(), row.x());
        assert_eq!(strip.width(), row.width());
        popover.popdown();
        window.close();
    });
}

/// A strip's door says it opens a menu, and that it is closed, before it ever
/// opened, as a door row does.
#[test]
fn strip_door_is_closed_popup() {
    big_testkit::harness::require_gtk_test(|| {
        let more = gio::Menu::new();
        more.append(Some("Move to…"), Some("view.move"));
        let menu = gio::Menu::new();
        append_strip_section(
            &menu,
            None,
            &[
                described_item("Copy", None, Some("edit-copy-symbolic"), "view.copy"),
                gio::MenuItem::new_submenu(Some("More"), &more),
            ],
        );
        let popover = build_menu_model_action_popover_menu(&menu, &popover_labels(), &[], &[]);
        let door = find_button_with_label(popover.upcast_ref(), "More").expect("the strip's door");
        assert!(gtk::test_accessible_has_property(
            &door,
            gtk::AccessibleProperty::HasPopup
        ));
        assert!(gtk::test_accessible_has_state(
            &door,
            gtk::AccessibleState::Expanded
        ));
    });
}

/// Down passes over a strip whose buttons are all off, which the focus cannot
/// enter, instead of stopping on it for good.
#[test]
fn down_skips_strip_all_off() {
    big_testkit::harness::require_gtk_test(|| {
        let actions = gio::SimpleActionGroup::new();
        for name in ["open", "cut", "close"] {
            let action = gio::SimpleAction::new(name, None);
            action.set_enabled(name != "cut");
            actions.add_action(&action);
        }
        let menu = gio::Menu::new();
        menu.append(Some("Open"), Some("view.open"));
        let cut = described_item("Cut", None, Some("edit-cut-symbolic"), "view.cut");
        append_strip_section(&menu, None, &[cut]);
        menu.append(Some("Close"), Some("view.close"));

        let anchor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let window = gtk::Window::builder().child(&anchor).build();
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        let popover = gtk::Popover::new();
        popover.set_parent(&anchor);
        popover.set_autohide(false);
        populate_menu_model_action_popover(
            &popover,
            &anchor,
            &menu,
            &popover_labels(),
            &[BigMenuActionGroupBinding::new("view", &actions)],
        );
        popover.popup();
        assert!(big_testkit::harness::pump_until(|| popover.is_visible()));
        let cut = find_button_with_label(popover.upcast_ref(), "Cut").expect("the strip");
        assert!(big_testkit::harness::pump_until(|| !cut.is_sensitive()));
        let open = find_button_with_label(popover.upcast_ref(), "Open").expect("first row");
        let close = find_button_with_label(popover.upcast_ref(), "Close").expect("last row");
        assert!(open.grab_focus());
        let page = find_stack(popover.upcast_ref())
            .and_then(|stack| stack.visible_child())
            .expect("the root page");

        assert!(big_testkit::harness::press_key(
            &page,
            Some(gtk::PropagationPhase::Capture),
            gtk::gdk::Key::Down,
            gtk::gdk::ModifierType::empty(),
        ));
        assert!(close.is_focus(), "Down stopped on the strip");
        popover.popdown();
        popover.unparent();
        window.close();
    });
}

/// `hidden-when="action-missing"` leaves the row out while its action is not
/// there, with the line its section would have drawn; without the attribute
/// a missing action is a row that is off.
#[test]
fn missing_action_hides_marked_row() {
    big_testkit::harness::require_gtk_test(|| {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&gio::SimpleAction::new("open", None));
        let menu = gio::Menu::new();
        menu.append(Some("Open"), Some("view.open"));
        let assistant = gio::Menu::new();
        let ask = gio::MenuItem::new(Some("Ask"), Some("view.ask"));
        ask.set_attribute_value("hidden-when", Some(&"action-missing".to_variant()));
        assistant.append_item(&ask);
        menu.append_section(None, &assistant);
        let print = gio::Menu::new();
        print.append(Some("Print"), Some("view.print"));
        menu.append_section(None, &print);
        let bindings = [BigMenuActionGroupBinding::new("view", &actions)];

        let popover = popover_menu(None);
        populate_menu_model_action_popover_menu(&popover, &menu, &popover_labels(), &bindings);
        assert!(find_button_with_label(popover.upcast_ref(), "Ask").is_none());
        let print_row = find_button_with_label(popover.upcast_ref(), "Print").expect("off row");
        assert!(!print_row.is_sensitive());
        let stack = find_stack(popover.upcast_ref()).expect("the menu's stack");
        let page = visible_page(&stack).expect("the root page");
        assert_eq!(direct_separator_count(page.upcast_ref()), 1);

        actions.add_action(&gio::SimpleAction::new("ask", None));
        populate_menu_model_action_popover_menu(&popover, &menu, &popover_labels(), &bindings);
        assert!(find_button_with_label(popover.upcast_ref(), "Ask").is_some());
    });
}

/// Beside a check mark, a plain row's label and a slider's caption start where
/// the checked row's label does, as in GTK's own menus.
#[test]
fn labels_line_up_past_check_marks() {
    big_testkit::harness::require_gtk_test(|| {
        adw::init().expect("libadwaita");
        let (actions, _, _, _) = numeric_group();
        actions.add_action(&gio::SimpleAction::new("open", None));
        let menu = gio::Menu::new();
        menu.append(Some("Open"), Some("view.open"));
        menu.append(Some("Previews"), Some("view.previews"));
        let sliders = gio::Menu::new();
        sliders.append_item(&scale_item("Entry size", "view.zoom", 0.0, 6.0, 1.0));
        menu.append_section(None, &sliders);
        let popover = build_menu_model_action_popover_menu(
            &menu,
            &popover_labels(),
            &[],
            &[BigMenuActionGroupBinding::new("view", &actions)],
        );
        popover.set_autohide(false);
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        let window = gtk::Window::builder().child(&button).build();
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        popover.popup();
        let label = |text: &str| {
            find_widget(popover.upcast_ref(), &|widget| {
                widget
                    .downcast_ref::<gtk::Label>()
                    .is_some_and(|label| label.text() == text)
            })
            .expect("the row's label")
        };
        let (open, previews) = (label("Open"), label("Previews"));
        assert!(big_testkit::harness::pump_until(|| previews.width() > 0));
        let column = bounds_in(&previews, popover.upcast_ref()).x();
        assert_eq!(bounds_in(&open, popover.upcast_ref()).x(), column);
        assert_eq!(
            bounds_in(&label("Entry size"), popover.upcast_ref()).x(),
            column,
            "a slider's caption"
        );
        popover.popdown();
        window.close();
    });
}

/// GTK 4.24 leaves a native item unnamed for a screen reader; the menu names
/// it by its text once it opens.
#[test]
fn native_items_are_named() {
    big_testkit::harness::require_gtk_test(|| {
        let menu = gio::Menu::new();
        menu.append(Some("Open Folder"), Some("win.open-folder"));
        let popover = popover_menu(Some(menu.upcast_ref()));
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        let window = gtk::Window::new();
        window.set_child(Some(&button));
        window.present();
        assert!(big_testkit::harness::pump_until(|| window.is_mapped()));
        popover.set_autohide(false);
        button.popup();
        assert!(big_testkit::harness::pump_until(|| popover.is_visible()));

        let item = subtree(&popover)
            .find(|widget| widget.type_().name() == "GtkModelButton")
            .expect("a native menu draws its items as model buttons");
        // SAFETY: the item outlives the call, and the property's variadic
        // value is a NUL-terminated string.
        let mismatch: Option<glib::GString> = unsafe {
            glib::translate::from_glib_full(gtk::ffi::gtk_test_accessible_check_property(
                glib::translate::ToGlibPtr::to_glib_none(item.upcast_ref::<gtk::Accessible>()).0,
                gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
                c"Open Folder".as_ptr(),
            ))
        };
        assert_eq!(mismatch, None);
        window.destroy();
    });
}

/// A row icon another program names by its file reaches the row as a texture
/// the image sandbox decoded, never through GTK's in-process loader.
#[test]
fn file_icon_loads_sandboxed() {
    big_testkit::harness::require_gtk_test(|| {
        let dir = glib::mkdtemp(std::env::temp_dir().join("big-menu-icon-XXXXXX"))
            .expect("temporary directory");
        let path = dir.join("icon.png");
        let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 32, 32)
            .expect("a surface");
        surface
            .write_to_png(&mut std::fs::File::create(&path).expect("icon file"))
            .expect("a PNG");
        let icon = action_row::leading_icon(path.to_str().expect("a UTF-8 path"), false);
        let decoded =
            big_testkit::harness::pump_until(|| icon.storage_type() == gtk::ImageType::Paintable);
        std::fs::remove_dir_all(dir).expect("temporary directory removed");
        assert!(decoded, "the icon is held as {:?}", icon.storage_type());
    });
}
