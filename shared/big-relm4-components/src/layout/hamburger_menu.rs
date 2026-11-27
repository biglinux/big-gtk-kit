// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Standard BigLinux hamburger menu builder.
//!
//! The framework contract is narrow: a hamburger menu is a native
//! `GtkMenuButton` with explicit action rows. Apps provide labels and action
//! IDs; the framework owns tooltip labels, popover behavior, and accessible
//! action rows.

use crate::layout::widget_container::subtree;
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::prelude::*;

const STRUCTURED_MENU_CONTENT_ID: &str = "big-structured-menu-content";
/// Marks a `PopoverMenu` the structured builder has populated at least once.
const STRUCTURED_MENU_CSS_CLASS: &str = "big-structured-menu";
const STRUCTURED_STACK_CSS_CLASS: &str = "big-structured-menu-stack";

#[path = "hamburger_menu/action_row.rs"]
mod action_row;
#[path = "hamburger_menu/cascade.rs"]
mod cascade;
#[path = "hamburger_menu/context_menu.rs"]
mod context_menu;
#[path = "hamburger_menu/menu_model.rs"]
mod menu_model;
#[path = "hamburger_menu/rich_row.rs"]
mod rich_row;
#[path = "hamburger_menu/search.rs"]
mod search;

use action_row::{
    align_labels_past_check_marks, build_action_group_menu_row, build_action_menu_row,
    build_hierarchy_action_menu_row, insert_inheritable_action_group,
    install_menu_accelerator_refresh, install_menu_css, install_menu_page_keyboard_navigation,
};
pub use cascade::SUBMENU_CSS_CLASS;
pub use context_menu::{
    BigContextMenu, BigContextMenuContent, BigMenuFocusReturn, CONTEXT_MENU_CONTROLLER,
    attach_context_menu, connect_context_menu_request, context_menu_request_controllers,
    is_context_menu_key,
};
pub use menu_model::{
    BIG_COLOR_ATTRIBUTE, BIG_DESTRUCTIVE_ATTRIBUTE, BIG_ICON_ATTRIBUTE, BIG_KEYWORDS_ATTRIBUTE,
    BIG_KIND_ATTRIBUTE, BIG_KIND_INLINE, BIG_KIND_SCALE, BIG_KIND_SPLIT, BIG_KIND_STRIP,
    BIG_KIND_SWITCH, BIG_OFF_REASON_ATTRIBUTE, BIG_PRIMARY_ATTRIBUTE, BIG_RANGE_ATTRIBUTE,
    BIG_SUBTITLE_ATTRIBUTE, action_groups_from_menu_model, append_inline_section,
    append_split_section, append_strip_section, described_item, flat_action_items_from_menu_model,
    mark_destructive, mark_primary, menu_action_names, menu_model_rows_key, scale_item,
    set_keywords, switch_item,
};
use menu_model::{
    menu_model_action_item, menu_model_door_item, menu_model_hidden, menu_model_kind,
    menu_model_label_attribute, menu_model_range,
};

/// Pages taller than the screen, less this, scroll: a menu that grows with a
/// long "Open With" or "Move to" list must stop short of the screen instead of
/// being dismissed by the compositor the frame it maps. The margin is a panel
/// and the window's header above the menu, with room to spare.
const SCREEN_MARGIN: i32 = 140;
/// The ceiling when the screen is not known yet, or is smaller than a menu
/// should ever be squeezed to.
const PAGE_CEILING: i32 = 520;

/// One visible menu item bound to one detailed action name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigMenuActionItem {
    /// User-visible item label.
    pub label: String,
    /// Detailed action name, for example `win.preferences` or `app.about`.
    pub action: String,
    /// Whether the row can be activated.
    pub enabled: bool,
    /// Optional symbolic icon shown before the label.
    pub leading_icon_name: Option<String>,
    /// Accelerator in GTK spelling (`<Control>i`) the row shows and names to
    /// the screen reader in place of the application's: the model's `accel`.
    pub accel: Option<String>,
    /// Optional one-line explanation shown dimmed under the label.
    pub subtitle: Option<String>,
    /// Why the row is off, said while its action is disabled (`big-off-reason`).
    pub off_reason: Option<String>,
    /// Whether the row destroys something; drawn in the error color.
    pub destructive: bool,
    /// Stateful action presentation exposed to sighted and assistive users.
    pub selection: BigMenuActionSelection,
    /// `#rrggbb` of a swatch drawn in place of an icon (`big-color`).
    pub color: Option<String>,
    /// Other words the menu's search finds this item by (`big-keywords`).
    pub keywords: Option<String>,
}

/// Accessible presentation for one stateful menu action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BigMenuActionSelection {
    /// Ordinary action without a persistent selected state.
    #[default]
    None,
    /// Boolean action rendered as a checked menu item.
    Checkbox {
        /// Whether the action's current state is enabled.
        is_checked: bool,
    },
    /// Targeted stateful action rendered as one radio choice.
    Radio {
        /// Whether the action state equals this row's target.
        is_selected: bool,
    },
}

/// Localized labels owned by a structured action popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigMenuModelPopoverLabels {
    /// Accessible name for the popover group.
    pub accessible_label: String,
    /// Placeholder of the search a right-click menu offers as soon as a
    /// letter is typed, and what it says when nothing matches; no search
    /// when unset.
    pub search_placeholder: Option<(String, String)>,
}

impl BigMenuModelPopoverLabels {
    /// Create the complete localized label set for one structured popover.
    #[must_use]
    pub fn new(accessible_label: impl Into<String>) -> Self {
        Self {
            accessible_label: accessible_label.into(),
            search_placeholder: None,
        }
    }

    /// Let typing search every action of the menu, submenus included.
    #[must_use]
    pub fn with_search(
        mut self,
        placeholder: impl Into<String>,
        nothing_found: impl Into<String>,
    ) -> Self {
        self.search_placeholder = Some((placeholder.into(), nothing_found.into()));
        self
    }
}

/// One action-prefix binding that resolves enabled and selected menu state.
#[derive(Debug, Clone)]
pub struct BigMenuActionGroupBinding {
    prefix: String,
    group: gio::ActionGroup,
}

/// A [`BigMenuActionGroupBinding`] that does not keep its group alive.
#[derive(Clone, Debug)]
pub struct BigMenuActionGroupWeakBinding {
    prefix: String,
    group: gtk::glib::WeakRef<gio::ActionGroup>,
}

impl BigMenuActionGroupWeakBinding {
    /// The binding again, while its group still exists.
    #[must_use]
    pub fn upgrade(&self) -> Option<BigMenuActionGroupBinding> {
        self.group.upgrade().map(|group| BigMenuActionGroupBinding {
            prefix: self.prefix.clone(),
            group,
        })
    }
}

impl BigMenuActionGroupBinding {
    /// Bind detailed actions such as `bigtoolbar.position` to `group`.
    #[must_use]
    pub fn new(prefix: impl Into<String>, group: &impl IsA<gio::ActionGroup>) -> Self {
        Self {
            prefix: prefix.into(),
            group: group.as_ref().clone(),
        }
    }

    /// The same binding without keeping its group alive, for a menu that is
    /// built later: a window that stores its own binding keeps itself alive.
    #[must_use]
    pub fn downgrade(&self) -> BigMenuActionGroupWeakBinding {
        BigMenuActionGroupWeakBinding {
            prefix: self.prefix.clone(),
            group: self.group.downgrade(),
        }
    }

    fn action<'binding, 'name>(
        &'binding self,
        detailed_name: &'name str,
    ) -> Option<(&'name str, &'binding str, &'binding gio::ActionGroup)> {
        let (prefix, action_name) = detailed_name.split_once('.')?;
        (prefix == self.prefix).then_some((action_name, self.prefix.as_str(), &self.group))
    }
}

impl BigMenuActionItem {
    /// Create a menu item from a localized label and detailed action name.
    #[must_use]
    pub fn new(label: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            action: action.into(),
            enabled: true,
            leading_icon_name: None,
            accel: None,
            subtitle: None,
            off_reason: None,
            destructive: false,
            selection: BigMenuActionSelection::None,
            color: None,
            keywords: None,
        }
    }

    /// Set the explanation shown under the label; long text wraps.
    #[must_use]
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Draw the row in the error color because it destroys something.
    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// Return a menu item that is visible but cannot be activated.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// One visual menu group with an optional heading and ordered action rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigMenuActionGroup {
    /// Optional visible heading for this group.
    pub heading: Option<String>,
    /// Action rows in visual order.
    pub items: Vec<BigMenuActionItem>,
}

impl BigMenuActionGroup {
    /// Create a menu group.
    #[must_use]
    pub fn new(heading: Option<String>, items: Vec<BigMenuActionItem>) -> Self {
        Self { heading, items }
    }
}

/// A `PopoverMenu` whose submenus open beside their row, the parent menu left
/// open, as GTK's nested menus do. Every popover menu of the suite starts
/// here: `clippy.toml` refuses GTK's constructors, whose submenus slide over
/// the menu that opened them.
///
/// A native model's submenus are GTK's own popovers, and GTK 4.24 cascades
/// their popdown: Left or Escape in one closes the whole menu. The structured
/// builder closes one level.
///
/// GTK 4.24 labels a native item by a child label whose role is presentation,
/// and its name computation skips such labels, so every item would reach a
/// screen reader unnamed; each is named by its text when the menu opens.
#[must_use]
pub fn popover_menu(model: Option<&gio::MenuModel>) -> gtk::PopoverMenu {
    // The rows' style reaches the display with the first menu, which a window
    // makes as it is built: a provider added later restyles every window.
    install_menu_css();
    #[expect(clippy::disallowed_methods, reason = "the one constructor")]
    let popover = gtk::PopoverMenu::builder()
        .flags(gtk::PopoverMenuFlags::NESTED)
        .build();
    popover.set_menu_model(model);
    popover.connect_map(|menu| name_menu_items(menu.upcast_ref()));
    popover
}

/// Name each native menu item under `menu` by the text of its label; the
/// structured rows name themselves.
fn name_menu_items(menu: &gtk::Widget) {
    for item in big_gtk_kit::widget_tree::descendants(menu) {
        let is_menu_item = item.type_().name() == "GtkModelButton"
            && matches!(
                item.accessible_role(),
                gtk::AccessibleRole::MenuItem
                    | gtk::AccessibleRole::MenuItemCheckbox
                    | gtk::AccessibleRole::MenuItemRadio
            );
        let label = big_gtk_kit::widget_tree::children(&item)
            .find_map(|part| part.downcast::<gtk::Label>().ok());
        if is_menu_item && let Some(label) = label {
            item.reset_relation(gtk::AccessibleRelation::LabelledBy);
            item.update_property(&[gtk::accessible::Property::Label(&label.text())]);
        }
    }
}

/// Attach a structured action popover copied from `menu` with named action rows.
pub fn set_menu_model_action_popover(
    button: &gtk::MenuButton,
    menu: &impl IsA<gio::MenuModel>,
    labels: &BigMenuModelPopoverLabels,
    action_groups: &[BigMenuActionGroupBinding],
) {
    let popover = build_menu_model_action_popover_menu(menu, labels, &[], action_groups);
    for binding in action_groups {
        insert_inheritable_action_group(&popover, &binding.prefix, &binding.group);
    }
    button.set_popover(Some(&popover));
}

/// Name a [`gtk::MenuButton`] while retaining GTK's documented `Button` role.
///
/// GTK delegates focus and activation to toolkit-owned internals. Callers must
/// treat the complete menu button as one widget and never traverse that private
/// hierarchy.
pub fn set_menu_button_accessible_label(button: &gtk::MenuButton, label: &str) {
    if button.accessible_role() != gtk::AccessibleRole::Button {
        button.set_accessible_role(gtk::AccessibleRole::Button);
    }
    button.update_property(&[gtk::accessible::Property::Label(label)]);
}

/// Build a native menu popover that keeps sections/submenus from a `GMenuModel`.
///
/// Submenus open beside their rows, also when the pointer rests on them
/// (see `cascade`).
#[must_use]
pub fn build_menu_model_action_popover_menu(
    menu: &impl IsA<gio::MenuModel>,
    labels: &BigMenuModelPopoverLabels,
    custom_children: &[(&gtk::Widget, &str)],
    action_groups: &[BigMenuActionGroupBinding],
) -> gtk::PopoverMenu {
    let popover = popover_menu(None);
    populate_menu_model_action_popover_menu_with_children(
        &popover,
        menu,
        labels,
        custom_children,
        action_groups,
    );
    popover
}

/// Replace the rows of a `PopoverMenu` that stays parented for its life.
///
/// For a menu whose model changes between openings (the tab strip's, a file
/// tree's), call this before each `popup()`. The popover keeps the `menu`
/// accessible role a right-click menu owes the accessibility gate, and the
/// previous rows are released.
pub fn populate_menu_model_action_popover_menu(
    popover: &gtk::PopoverMenu,
    menu: &impl IsA<gio::MenuModel>,
    labels: &BigMenuModelPopoverLabels,
    action_groups: &[BigMenuActionGroupBinding],
) {
    populate_menu_model_action_popover_menu_with_children(
        popover,
        menu,
        labels,
        &[],
        action_groups,
    );
}

fn populate_menu_model_action_popover_menu_with_children(
    popover: &gtk::PopoverMenu,
    menu: &impl IsA<gio::MenuModel>,
    labels: &BigMenuModelPopoverLabels,
    custom_children: &[(&gtk::Widget, &str)],
    action_groups: &[BigMenuActionGroupBinding],
) {
    let _span = big_os_kit::bench::span("menu.rows");
    if let Some(previous) = structured_stack_of(popover.upcast_ref()) {
        popover.remove_child(&previous);
    }
    popover.set_menu_model(Some(&structured_popover_menu_model()));
    popover.add_css_class("menu");
    popover.update_property(&[gtk::accessible::Property::Label(&labels.accessible_label)]);
    let stack = build_structured_menu_stack(
        popover.upcast_ref(),
        menu.as_ref(),
        labels,
        custom_children,
        action_groups,
        None,
    );
    if let Some((placeholder, nothing_found)) = labels.search_placeholder.as_ref() {
        let build = StructuredMenuBuild {
            stack: &stack,
            action_context: None,
            popover: popover.upcast_ref(),
            labels,
            custom_children,
            action_groups,
        };
        search::install(
            &build,
            popover.upcast_ref(),
            menu.as_ref(),
            placeholder,
            nothing_found,
        );
    }
    let added = popover.add_child(&stack, STRUCTURED_MENU_CONTENT_ID);
    debug_assert!(
        added,
        "structured content id must match the popover menu model"
    );
    // Back to the first page for the next opening. Connected once; the
    // handler finds whichever stack the popover holds at the time.
    if !popover.has_css_class(STRUCTURED_MENU_CSS_CLASS) {
        popover.add_css_class(STRUCTURED_MENU_CSS_CLASS);
        popover.connect_closed(|popover| {
            if let Some(stack) = structured_stack_of(popover.upcast_ref()) {
                stack.set_visible_child_name("root");
                cascade::close_all(stack.upcast_ref());
            }
        });
    }
}

fn structured_stack_of(widget: &gtk::Widget) -> Option<gtk::Stack> {
    subtree(widget)
        .filter_map(|widget| widget.downcast::<gtk::Stack>().ok())
        .find(|stack| stack.has_css_class(STRUCTURED_STACK_CSS_CLASS))
}

fn structured_popover_menu_model() -> gio::Menu {
    use relm4::gtk::glib::variant::ToVariant;

    let custom_menu = gio::Menu::new();
    let content_item = gio::MenuItem::new(None, None);
    content_item.set_attribute_value("custom", Some(&STRUCTURED_MENU_CONTENT_ID.to_variant()));
    custom_menu.append_item(&content_item);
    custom_menu
}

/// Replace a popover's child with structured, explicitly named action rows.
///
/// Sections, submenus, and detailed action targets are copied from `menu`.
/// The rows follow `menu`, its sections and its submenus as they change: at
/// once while the popover is open, when it next opens while it is closed.
/// The caller owns the popover's accessible label, anchoring, and lifecycle.
pub fn populate_menu_model_action_popover(
    popover: &gtk::Popover,
    action_context: &impl IsA<gtk::Widget>,
    menu: &impl IsA<gio::MenuModel>,
    labels: &BigMenuModelPopoverLabels,
    action_groups: &[BigMenuActionGroupBinding],
) {
    prepare_menu_popover(popover);
    popover.update_property(&[gtk::accessible::Property::Label(&labels.accessible_label)]);

    let stack = build_structured_menu_stack(
        popover,
        menu.as_ref(),
        labels,
        &[],
        action_groups,
        Some(action_context.as_ref()),
    );
    popover.set_child(Some(&stack));
    refill_on_model_change(
        popover,
        &stack,
        action_context.as_ref(),
        menu.as_ref(),
        labels,
        action_groups,
    );
}

/// Fill `popover` again from `menu` when it or a model linked from it changes.
///
/// The handlers live as long as `stack`, the rows they describe: filling the
/// popover again replaces the stack and so disconnects them. They hold `menu`,
/// which the caller may have let go once the rows were built.
fn refill_on_model_change(
    popover: &gtk::Popover,
    stack: &gtk::Stack,
    action_context: &gtk::Widget,
    menu: &gio::MenuModel,
    labels: &BigMenuModelPopoverLabels,
    action_groups: &[BigMenuActionGroupBinding],
) {
    use std::cell::Cell;
    use std::rc::Rc;

    let refill = {
        let (popover, action_context) = (popover.downgrade(), action_context.downgrade());
        let (menu, labels, action_groups) = (menu.clone(), labels.clone(), action_groups.to_vec());
        Rc::new(move || {
            if let (Some(popover), Some(action_context)) =
                (popover.upgrade(), action_context.upgrade())
            {
                populate_menu_model_action_popover(
                    &popover,
                    &action_context,
                    &menu,
                    &labels,
                    &action_groups,
                );
            }
        })
    };
    let stale = Rc::new(Cell::new(false));
    let mut models = vec![menu.clone()];
    let mut index = 0;
    while let Some(model) = models.get(index).cloned() {
        for item in 0..model.n_items() {
            let links = model.iterate_item_links(item);
            while let Some((_, linked)) = links.next() {
                models.push(linked);
            }
        }
        index += 1;
    }
    let handlers: Vec<_> = models
        .into_iter()
        .map(|model| {
            let (refill, stale, popover) =
                (Rc::clone(&refill), Rc::clone(&stale), popover.downgrade());
            let id = model.connect_items_changed(move |_, _, removed, added| {
                if removed == 0 && added == 0 {
                    return;
                }
                match popover.upgrade() {
                    Some(popover) if popover.is_visible() => refill(),
                    Some(_) => stale.set(true),
                    None => {}
                }
            });
            (model.downgrade(), id)
        })
        .collect();
    let shown = popover.connect_show(move |_| {
        if stale.get() {
            refill();
        }
    });
    let handlers = Cell::new(Some((handlers, shown)));
    let popover = popover.downgrade();
    stack.connect_destroy(move |_| {
        let Some((handlers, shown)) = handlers.take() else {
            return;
        };
        for (model, id) in handlers {
            if let Some(model) = model.upgrade() {
                model.disconnect(id);
            }
        }
        if let Some(popover) = popover.upgrade() {
            popover.disconnect(shown);
        }
    });
}

/// Replace a popover's child with flat named action rows, each activating its
/// item's detailed action on `action_context` and closing the popover. Safe to
/// call again on each opening: the popover's role is set once.
pub fn populate_flat_action_popover(
    popover: &gtk::Popover,
    action_context: &impl IsA<gtk::Widget>,
    items: &[BigMenuActionItem],
) {
    prepare_menu_popover(popover);
    install_menu_css();

    let rows = flat_action_rows();

    for item in items {
        rows.append(&build_action_menu_row(
            action_context.as_ref(),
            popover,
            item,
        ));
    }

    popover.set_child(Some(&rows));
}

/// Dress a popover the menu rows are about to fill.
///
/// A popover filled again on each opening keeps the role it was given the
/// first time: GTK refuses a role, even the same one, once the popover has
/// been shown to assistive technology.
///
/// A `GtkBox` adds a shown child's expand to its own, natives included, so
/// the popover does not expand: the bar it hangs from keeps its width while
/// the menu is open.
fn prepare_menu_popover(popover: &gtk::Popover) {
    if popover.accessible_role() != gtk::AccessibleRole::Group {
        popover.set_accessible_role(gtk::AccessibleRole::Group);
    }
    popover.set_hexpand(false);
    popover.set_vexpand(false);
    popover.add_css_class("menu");
}

fn flat_action_rows() -> gtk::ListBox {
    let rows = gtk::ListBox::new();
    rows.set_selection_mode(gtk::SelectionMode::None);
    rows.set_margin_top(6);
    rows.set_margin_bottom(6);
    rows.set_margin_start(6);
    rows.set_margin_end(6);
    rows
}

/// Build a flat action popover with named action rows, as
/// [`populate_flat_action_popover`] fills one. The caller parents it.
#[must_use]
pub fn build_flat_action_popover(
    action_context: &impl IsA<gtk::Widget>,
    items: &[BigMenuActionItem],
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    populate_flat_action_popover(&popover, action_context, items);
    popover
}

struct StructuredMenuBuild<'a> {
    stack: &'a gtk::Stack,
    action_context: Option<&'a gtk::Widget>,
    popover: &'a gtk::Popover,
    labels: &'a BigMenuModelPopoverLabels,
    custom_children: &'a [(&'a gtk::Widget, &'a str)],
    action_groups: &'a [BigMenuActionGroupBinding],
}

fn build_structured_menu_stack(
    popover: &gtk::Popover,
    menu: &gio::MenuModel,
    labels: &BigMenuModelPopoverLabels,
    custom_children: &[(&gtk::Widget, &str)],
    action_groups: &[BigMenuActionGroupBinding],
    action_context: Option<&gtk::Widget>,
) -> gtk::Stack {
    install_menu_css();
    let stack = gtk::Stack::new();
    stack.add_css_class(STRUCTURED_STACK_CSS_CLASS);
    stack.set_interpolate_size(true);
    stack.set_vhomogeneous(false);
    {
        let build = StructuredMenuBuild {
            stack: &stack,
            action_context,
            popover,
            labels,
            custom_children,
            action_groups,
        };
        append_menu_model_page(&build, menu);
    }
    stack.set_visible_child_name("root");
    install_menu_accelerator_refresh(popover, &stack);
    stack
}

/// The menu's root page, named `root`.
fn append_menu_model_page(build: &StructuredMenuBuild<'_>, menu: &gio::MenuModel) {
    // A strip in first place is the page's header: it spans the popover from
    // edge to edge, above the rows and outside their scrolling, so it reads as
    // the band it is instead of a card floating over the first section.
    let header = leading_strip_section(menu)
        .map(|section| rich_row::build_strip(build, &section, rich_row::StripPlacement::Header));

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_margin_top(6);
    page.set_margin_bottom(6);
    page.set_margin_start(6);
    page.set_margin_end(6);

    append_menu_model_rows(build, &page, menu, i32::from(header.is_some()), false);
    align_labels_past_check_marks(&page);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&page)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .propagate_natural_width(true)
        .max_content_height(PAGE_CEILING)
        .build();
    // Realized before the first layout, and by then the window it opens over is
    // on a monitor whose height is known.
    scroller.connect_realize(|scroller| {
        let height = scroller
            .root()
            .and_then(|root| root.surface())
            .and_then(|surface| surface.display().monitor_at_surface(&surface))
            .map(|monitor| monitor.geometry().height());
        if let Some(height) = height {
            scroller.set_max_content_height((height - SCREEN_MARGIN).max(PAGE_CEILING));
        }
    });
    // The header is a keyboard stop of this page like any row, so Up and Down
    // are bound above both it and the scroller.
    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if let Some(header) = header {
        container.append(&header);
    }
    container.append(&scroller);
    cascade::install_hover(&container, &page);
    install_menu_page_keyboard_navigation(&container);
    build.stack.add_named(&container, Some("root"));
}

/// The section a page draws as its header: a strip, first, and unheaded.
///
/// A strip that carries a heading is answering a question the heading asks
/// ("Encoding", "Alignment") and belongs among the rows, under it. The header
/// band is for the strip that names nothing above itself.
fn leading_strip_section(menu: &gio::MenuModel) -> Option<gio::MenuModel> {
    if menu.n_items() == 0
        || menu_model_kind(menu, 0).as_deref() != Some(BIG_KIND_STRIP)
        || menu_model_label_attribute(menu, 0).is_some()
    {
        return None;
    }
    menu.item_link(0, "section")
}

fn append_menu_model_rows(
    build: &StructuredMenuBuild<'_>,
    page: &gtk::Box,
    menu: &gio::MenuModel,
    start_index: i32,
    nested: bool,
) {
    let mut has_visible_rows = false;
    for index in start_index..menu.n_items() {
        if let Some(custom_id) = menu_model_custom_id(menu, index) {
            if let Some((child, _)) = build
                .custom_children
                .iter()
                .find(|(_, id)| **id == custom_id)
            {
                page.append(*child);
                has_visible_rows = true;
            }
            continue;
        }

        if let Some(section) = menu.item_link(index, "section") {
            if !menu_model_has_rows(&section, build) {
                continue;
            }
            // A heading already says a new group starts here; a line above it
            // as well is one divider too many.
            // An inline row names itself with the section's label, on the row.
            let heading = menu_model_label_attribute(menu, index)
                .filter(|_| menu_model_kind(menu, index).as_deref() != Some(BIG_KIND_INLINE));
            // An inline row, or a strip of verbs inside a group, continues
            // the group it sits in ("Open", then where else it opens).
            let kind = menu_model_kind(menu, index);
            let continues = matches!(kind.as_deref(), Some(BIG_KIND_INLINE | BIG_KIND_STRIP))
                && nested
                && menu_model_label_attribute(menu, index).is_none()
                && has_visible_rows;
            let separates =
                heading.is_none() && !continues && kind.as_deref() != Some(BIG_KIND_INLINE);
            if has_visible_rows && separates {
                page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            append_section_heading(page, heading);
            let kind = menu_model_kind(menu, index);
            if kind.as_deref() == Some(BIG_KIND_STRIP) {
                page.append(&rich_row::build_strip(
                    build,
                    &section,
                    rich_row::StripPlacement::Row,
                ));
            } else if kind.as_deref() == Some(BIG_KIND_SPLIT) {
                page.append(&rich_row::build_split(build, &section));
            } else if kind.as_deref() == Some(BIG_KIND_INLINE) {
                page.append(&rich_row::build_inline(build, menu, index, &section));
            } else {
                append_menu_model_rows(build, page, &section, 0, true);
            }
            has_visible_rows = true;
            continue;
        }

        if let Some(submenu) = menu.item_link(index, "submenu") {
            if let Some(door) = menu_model_door_item(menu, index) {
                page.append(&cascade::build_opener(build, &door, &submenu));
                has_visible_rows = true;
            }
            continue;
        }

        if menu_model_hidden(menu, index, build.action_groups) {
            continue;
        }
        if let Some(mut item) = menu_model_action_item(menu, index) {
            let action_group = resolve_action_presentation(&mut item, build.action_groups);
            match menu_model_kind(menu, index).as_deref() {
                Some(BIG_KIND_SWITCH) => {
                    page.append(&rich_row::build_switch_row(&item, action_group));
                    has_visible_rows = true;
                    continue;
                }
                Some(BIG_KIND_SCALE) => {
                    if let Some((prefix, group)) = action_group {
                        let range = menu_model_range(menu, index);
                        page.append(&rich_row::build_scale_row(&item, range, prefix, group));
                        has_visible_rows = true;
                        continue;
                    }
                }
                _ => {}
            }
            page.append(&action_row(build, &item, action_group));
            has_visible_rows = true;
        }
    }
}

/// The row for one action item, bound the way the popover binds its actions.
fn action_row(
    build: &StructuredMenuBuild<'_>,
    item: &BigMenuActionItem,
    action_group: Option<(&str, &gio::ActionGroup)>,
) -> gtk::Button {
    let row = action_group.map_or_else(
        || {
            build.action_context.map_or_else(
                || build_hierarchy_action_menu_row(build.popover, item),
                |action_context| build_action_menu_row(action_context, build.popover, item),
            )
        },
        |(prefix, group)| build_action_group_menu_row(build.popover, item, prefix, group),
    );
    if matches!(item.selection, BigMenuActionSelection::None) {
        row.set_accessible_role(gtk::AccessibleRole::MenuItem);
    }
    row
}

fn menu_model_has_rows(menu: &gio::MenuModel, build: &StructuredMenuBuild<'_>) -> bool {
    (0..menu.n_items()).any(|index| {
        menu_model_custom_id(menu, index)
            .is_some_and(|custom_id| build.custom_children.iter().any(|(_, id)| *id == custom_id))
            || (menu_model_action_item(menu, index).is_some()
                && !menu_model_hidden(menu, index, build.action_groups))
            || menu
                .item_link(index, "submenu")
                .is_some_and(|_| menu_model_label_attribute(menu, index).is_some())
            || menu
                .item_link(index, "section")
                .is_some_and(|section| menu_model_has_rows(&section, build))
    })
}

fn menu_model_custom_id(menu: &gio::MenuModel, index: i32) -> Option<String> {
    menu.item_attribute_value(index, "custom", Some(relm4::gtk::glib::VariantTy::STRING))
        .and_then(|value| value.get::<String>())
}

fn resolve_action_presentation<'a>(
    item: &mut BigMenuActionItem,
    action_groups: &'a [BigMenuActionGroupBinding],
) -> Option<(&'a str, &'a gio::ActionGroup)> {
    let Ok((action_name, target)) = gio::Action::parse_detailed_name(&item.action) else {
        return None;
    };
    let (action_name, prefix, group) = action_groups
        .iter()
        .find_map(|binding| binding.action(&action_name))?;
    if !group.has_action(action_name) {
        item.enabled = false;
        return None;
    }
    let Some(state) = group.action_state(action_name) else {
        return Some((prefix, group));
    };
    item.selection = if let Some(target) = target {
        BigMenuActionSelection::Radio {
            is_selected: state == target,
        }
    } else if let Some(is_checked) = state.get::<bool>() {
        BigMenuActionSelection::Checkbox { is_checked }
    } else {
        BigMenuActionSelection::None
    };
    Some((prefix, group))
}

fn append_section_heading(page: &gtk::Box, heading: Option<String>) {
    if let Some(heading) = heading {
        let label = gtk::Label::new(Some(&heading));
        label.set_xalign(0.0);
        label.add_css_class("dimmed");
        label.add_css_class("caption-heading");
        label.set_margin_top(8);
        label.set_margin_bottom(4);
        label.set_margin_start(12);
        label.set_margin_end(12);
        page.append(&label);
    }
}

#[cfg(test)]
#[path = "hamburger_menu/tests.rs"]
mod tests;
