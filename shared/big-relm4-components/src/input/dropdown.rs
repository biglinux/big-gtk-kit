// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Small dropdown helpers for dialog controls.

use crate::layout::widget_container::{children, descendants};
use adw::prelude::*;
use relm4::gtk;

/// Build a native choice row for preferences and dialog forms.
///
/// Labels are already translated plain text. The explicit combo-box role keeps
/// the selector reachable through AT-SPI in the consuming applications.
#[must_use]
pub fn choice_row(title: &str, labels: &[&str]) -> adw::ComboRow {
    build(
        adw::ComboRow::builder()
            .accessible_role(gtk::AccessibleRole::ComboBox)
            .title(title)
            .use_markup(false)
            .model(&gtk::StringList::new(labels)),
    )
}

/// The choice row `builder` describes, its choices shown whole and named.
/// `clippy.toml` sends every choice row of the suite through here.
#[must_use]
pub fn build(builder: adw::builders::ComboRowBuilder) -> adw::ComboRow {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let row = builder.build();
    show_whole_choices(&row);
    name_list_items(row.factory(), row.list_factory());
    focus_chosen_item_on_open(&row, |row| row.selected());
    row
}

/// The drop-down `builder` describes, its choices named and the chosen one
/// focused when it opens. `clippy.toml` sends every drop-down of the suite
/// through here.
#[must_use]
pub fn build_drop_down(builder: gtk::builders::DropDownBuilder) -> gtk::DropDown {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let dropdown = builder.build();
    name_list_items(dropdown.factory(), dropdown.list_factory());
    focus_chosen_item_on_open(&dropdown, gtk::DropDown::selected);
    dropdown
}

/// A drop-down offering `labels`, its choices named.
#[must_use]
pub fn from_strings(labels: &[&str]) -> gtk::DropDown {
    build_drop_down(gtk::DropDown::builder().model(&gtk::StringList::new(labels)))
}

/// The widest a choice in the open list grows before it wraps, in characters.
const LIST_WRAP_CHARS: i32 = 40;

/// Show `row`'s choices whole. `AdwComboRow`'s own factory cuts every label at 20
/// characters, in the open list and on the row alike. In the list a long choice
/// wraps; on the row it takes the room the row has, is cut only past that, and
/// then shows itself whole in a tooltip.
///
/// Only that factory's labels change; a row with a factory of its own keeps it.
fn show_whole_choices(row: &adw::ComboRow) {
    for factory in [row.factory(), row.list_factory()].into_iter().flatten() {
        if let Some(factory) = factory.downcast_ref::<gtk::SignalListItemFactory>() {
            // After the row's own setup, which made the label.
            factory.connect_setup(|_, item| {
                if let Some(label) = item
                    .downcast_ref::<gtk::ListItem>()
                    .and_then(gtk::ListItem::child)
                    .and_then(|child| child.first_child())
                    .and_downcast::<gtk::Label>()
                {
                    show_whole(&label);
                }
            });
        }
    }
    // The items made before this call.
    let mut pending = vec![row.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = pending.pop() {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            show_whole(label);
        }
        for node in children(&widget) {
            pending.push(node);
        }
    }
}

/// Take a label of `AdwComboRow`'s factory, which ellipsizes at 20 characters,
/// and let it show its whole text; any other label is left as it is.
fn show_whole(label: &gtk::Label) {
    if label.max_width_chars() != 20 || label.ellipsize() != gtk::pango::EllipsizeMode::End {
        return;
    }
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_has_tooltip(true);
    label.connect_query_tooltip(|label, _, _, _, tooltip| {
        let cut = label.layout().is_ellipsized();
        if cut {
            tooltip.set_text(Some(&label.text()));
        }
        cut
    });
    fit_where_shown(label);
    label.connect_root_notify(fit_where_shown);
}

/// Wrapped in the open list, one line on the row: the same factory draws both.
fn fit_where_shown(label: &gtk::Label) {
    let in_list = label.ancestor(gtk::Popover::static_type()).is_some();
    label.set_wrap(in_list);
    if in_list {
        label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        label.set_max_width_chars(LIST_WRAP_CHARS);
    } else {
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(-1);
    }
}

/// GTK opens the popup with focus on its first item, the first time since the
/// list has no items yet when the popup maps, and on a drop-down every time;
/// the first key goes to that item. Once the popover is shown, which is when
/// it has focused a child, the chosen item takes the focus, or, where the
/// popup has a search field, is only scrolled to: typing searches.
fn focus_chosen_item_on_open<W: IsA<gtk::Widget>>(owner: &W, selected: fn(&W) -> u32) {
    let Some(popover) = first_descendant::<gtk::Popover>(owner.upcast_ref()) else {
        return;
    };
    let owner = owner.downgrade();
    popover.connect_show(move |popover| {
        let Some(owner) = owner.upgrade() else {
            return;
        };
        let chosen = selected(&owner);
        if chosen != gtk::INVALID_LIST_POSITION
            && let Some(list) = first_descendant::<gtk::ListView>(popover.upcast_ref())
        {
            let searching = first_descendant::<gtk::SearchEntry>(popover.upcast_ref())
                .is_some_and(|search| search.is_visible());
            let flags = if searching {
                gtk::ListScrollFlags::NONE
            } else {
                gtk::ListScrollFlags::FOCUS
            };
            list.scroll_to(chosen, flags, None);
        }
    });
}

fn first_descendant<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<T> {
    descendants(widget).find_map(|widget| widget.downcast::<T>().ok())
}

/// Name each item the button's and the popup's factories make after the string
/// it shows. GTK's and libadwaita's default factories draw a label and a check
/// mark but leave the list item unnamed, so a screen reader reads an empty
/// item; naming it keeps the factory's look.
fn name_list_items(
    factory: Option<gtk::ListItemFactory>,
    list_factory: Option<gtk::ListItemFactory>,
) {
    let list_factory = list_factory.filter(|list| Some(list) != factory.as_ref());
    for factory in [factory, list_factory].into_iter().flatten() {
        let Ok(factory) = factory.downcast::<gtk::SignalListItemFactory>() else {
            continue;
        };
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if let Some(value) = item.item().and_downcast::<gtk::StringObject>() {
                item.set_accessible_label(&value.string());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each item `list` shows is named by the choice of the same index, so a
    /// screen reader reads the choice and not an empty row.
    fn assert_named(list: &gtk::ListView, choices: &[&std::ffi::CStr]) {
        // The compositor closes every popup when another window takes the
        // activation, which another test's window may do at any time, so the
        // wait is for the items to have been shown, not to be shown still.
        let shows_items =
            |list: &gtk::ListView| list.first_child().is_some_and(|item| item.is_mapped());
        let shown = std::rc::Rc::new(std::cell::Cell::new(false));
        list.connect_map({
            let shown = std::rc::Rc::clone(&shown);
            move |list| {
                if shows_items(list) {
                    shown.set(true);
                }
            }
        });
        assert!(big_testkit::harness::pump_until(
            || shown.get() || shows_items(list)
        ));
        let mut item = list.first_child();
        for choice in choices {
            let widget = item.expect("an item per choice");
            // SAFETY: the item outlives the call, and the label
            // property's variadic value is a NUL-terminated string.
            let mismatch: Option<gtk::glib::GString> = unsafe {
                gtk::glib::translate::from_glib_full(gtk::ffi::gtk_test_accessible_check_property(
                    gtk::glib::translate::ToGlibPtr::to_glib_none(
                        widget.upcast_ref::<gtk::Accessible>(),
                    )
                    .0,
                    gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
                    choice.as_ptr(),
                ))
            };
            assert_eq!(mismatch, None, "{choice:?}");
            item = widget.next_sibling();
        }
        assert!(item.is_none());
    }

    #[test]
    fn choice_row_names_its_choices() {
        big_testkit::harness::require_gtk_test(|| {
            let row = choice_row("Theme", &["Light", "Dark"]);
            // The popup's list, built from the row's own model and factory.
            let list = gtk::ListView::new(
                row.model().map(|model| gtk::NoSelection::new(Some(model))),
                row.factory(),
            );
            let window = gtk::Window::builder().child(&list).build();
            window.present();
            assert_named(&list, &[c"Light", c"Dark"]);
            window.destroy();
        });
    }

    #[test]
    fn drop_down_names_its_choices() {
        big_testkit::harness::require_gtk_test(|| {
            for dropdown in [
                from_strings(&["Light", "Dark"]),
                // A popup list of its own, as the greeter's session list has.
                build_drop_down(
                    gtk::DropDown::builder()
                        .model(&gtk::StringList::new(&["Light", "Dark"]))
                        .list_factory(&gtk::SignalListItemFactory::new()),
                ),
            ] {
                let window = gtk::Window::builder().child(&dropdown).build();
                window.present();
                big_testkit::harness::pump();
                gtk::prelude::WidgetExt::activate(&dropdown);
                let popover =
                    first_descendant::<gtk::Popover>(dropdown.upcast_ref()).expect("popup");
                let list = first_descendant::<gtk::ListView>(popover.upcast_ref()).expect("list");
                assert_named(&list, &[c"Light", c"Dark"]);
                window.destroy();
            }
        });
    }

    #[test]
    fn choice_popup_focuses_current_item() {
        big_testkit::harness::require_gtk_test(|| {
            let labels = ["Light", "Dark", "Auto"];
            for row in [
                choice_row("Theme", &labels),
                build(adw::ComboRow::builder().model(&gtk::StringList::new(&labels))),
            ] {
                row.set_selected(1);
                let group = adw::PreferencesGroup::new();
                group.add(&row);
                let window = adw::Window::builder().content(&group).build();
                window.present();
                big_testkit::harness::pump();
                gtk::prelude::WidgetExt::activate(&row);
                let popover = first_descendant::<gtk::Popover>(row.upcast_ref()).expect("popup");
                let list = first_descendant::<gtk::ListView>(popover.upcast_ref()).expect("list");
                let focused = || children(&list).position(|widget| widget.has_focus());
                assert!(big_testkit::harness::pump_until(|| focused().is_some()));
                assert_eq!(focused(), Some(1));
                window.destroy();
            }
            let dropdown = from_strings(&labels);
            dropdown.set_selected(2);
            let window = gtk::Window::builder().child(&dropdown).build();
            window.present();
            big_testkit::harness::pump();
            for opening in 0..2 {
                gtk::prelude::WidgetExt::activate(&dropdown);
                let popover =
                    first_descendant::<gtk::Popover>(dropdown.upcast_ref()).expect("popup");
                let list = first_descendant::<gtk::ListView>(popover.upcast_ref()).expect("list");
                let focused = || children(&list).position(|widget| widget.has_focus());
                assert!(big_testkit::harness::pump_until(|| focused().is_some()));
                assert_eq!(focused(), Some(2), "opening {opening}");
                popover.popdown();
                assert!(big_testkit::harness::pump_until(|| !popover.is_mapped()));
            }
            window.destroy();
        });
    }
}
