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
/// The list is the popover inside the row, not one the row itself sits in.
fn fit_where_shown(label: &gtk::Label) {
    let in_list = match (
        label.ancestor(gtk::Popover::static_type()),
        label.ancestor(adw::ComboRow::static_type()),
    ) {
        (Some(popover), Some(row)) => popover.is_ancestor(&row),
        _ => false,
    };
    label.set_wrap(in_list);
    if in_list {
        label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        label.set_max_width_chars(LIST_WRAP_CHARS);
    } else {
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(-1);
    }
}

/// GTK opens the popup with the focus on its first item; once the popover has
/// shown, the chosen item takes it, or only scrolls into view where a search
/// field keeps the keyboard.
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
/// item; naming it keeps the factory's look. An item the factory named
/// itself, which binds first, keeps that name.
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
            // The property, not `accessible_label()`, whose binding cannot
            // return the NULL an unnamed item has.
            let named = item
                .property::<Option<gtk::glib::GString>>("accessible-label")
                .is_some_and(|name| !name.is_empty());
            if !named && let Some(value) = item.item().and_downcast::<gtk::StringObject>() {
                item.set_accessible_label(&value.string());
            }
        });
        // A reused item starts unnamed, so the name above never outlives its choice.
        factory.connect_unbind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_accessible_label("");
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
            assert_eq!(name_mismatch(&widget, choice), None, "{choice:?}");
            item = widget.next_sibling();
        }
        assert!(item.is_none());
    }

    /// What `widget`'s accessible name is when it is not `name`.
    fn name_mismatch(widget: &gtk::Widget, name: &std::ffi::CStr) -> Option<gtk::glib::GString> {
        // SAFETY: the widget outlives the call, and the label property's
        // variadic value is a NUL-terminated string.
        unsafe {
            gtk::glib::translate::from_glib_full(gtk::ffi::gtk_test_accessible_check_property(
                gtk::glib::translate::ToGlibPtr::to_glib_none(
                    widget.upcast_ref::<gtk::Accessible>(),
                )
                .0,
                gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
                name.as_ptr(),
            ))
        }
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

    /// The popup's list of `dropdown`, built from its own model and factory.
    fn list_of(dropdown: &gtk::DropDown) -> gtk::ListView {
        gtk::ListView::new(
            dropdown
                .model()
                .map(|model| gtk::NoSelection::new(Some(model))),
            dropdown.factory(),
        )
    }

    /// A factory that names its items keeps its names.
    #[test]
    fn factory_names_are_kept() {
        big_testkit::harness::require_gtk_test(|| {
            let shown_as = |code: &str| match code {
                "pt_BR" => "Portuguese (Brazil)",
                _ => "English",
            };
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, item| {
                if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                    item.set_child(Some(&gtk::Label::new(None)));
                }
            });
            factory.connect_bind(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                if let Some(code) = item.item().and_downcast::<gtk::StringObject>() {
                    let name = shown_as(&code.string());
                    item.child()
                        .and_downcast::<gtk::Label>()
                        .expect("the label setup made")
                        .set_label(name);
                    item.set_accessible_label(name);
                }
            });
            let languages = gtk::StringList::new(&["pt_BR", "en"]);
            let named =
                build_drop_down(gtk::DropDown::builder().model(&languages).factory(&factory));
            let list = list_of(&named);
            let window = gtk::Window::builder().child(&list).build();
            window.present();
            assert_named(&list, &[c"Portuguese (Brazil)", c"English"]);
            window.destroy();
        });
    }

    /// The automatic name follows an item the list binds to another choice.
    #[test]
    fn reused_item_is_renamed() {
        big_testkit::harness::require_gtk_test(|| {
            // More choices than the 200 items a list view makes, so scrolling
            // binds the items it made to other choices.
            let choices: Vec<String> = (0..1000).map(|n| format!("Choice {n}")).collect();
            let dropdown = from_strings(&choices.iter().map(String::as_str).collect::<Vec<_>>());
            let list = list_of(&dropdown);
            let scrolled = gtk::ScrolledWindow::builder()
                .child(&list)
                .min_content_height(120)
                .build();
            let window = gtk::Window::builder().child(&scrolled).build();
            window.present();
            let shows = |text: &str| {
                children(&list).any(|item| {
                    item.is_mapped()
                        && descendants(&item).any(|widget| {
                            widget
                                .downcast::<gtk::Label>()
                                .is_ok_and(|label| label.label() == text)
                        })
                })
            };
            assert!(big_testkit::harness::pump_until(|| shows("Choice 0")));
            list.scroll_to(999, gtk::ListScrollFlags::NONE, None);
            assert!(big_testkit::harness::pump_until(|| shows("Choice 999")));
            for item in children(&list) {
                let shown = descendants(&item)
                    .find_map(|widget| widget.downcast::<gtk::Label>().ok())
                    .expect("the label the item shows")
                    .label();
                let shown = std::ffi::CString::new(shown.as_str()).expect("no NUL");
                assert_eq!(name_mismatch(&item, &shown), None, "{shown:?}");
            }
            window.destroy();
        });
    }

    /// A row inside another popover still shows its choice on one line; only
    /// its own open list wraps.
    #[test]
    fn row_in_popover_keeps_one_line() {
        big_testkit::harness::require_gtk_test(|| {
            let long = "A choice far longer than the row's twenty characters";
            let row = choice_row("Theme", &[long]);
            let list = gtk::ListBox::new();
            list.append(&row);
            let outer = gtk::Popover::builder().child(&list).build();
            let button = gtk::MenuButton::builder().popover(&outer).build();
            let window = gtk::Window::builder().child(&button).build();
            window.present();
            big_testkit::harness::pump();
            let label_in = |popover: &gtk::Widget| {
                descendants(row.upcast_ref::<gtk::Widget>())
                    .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
                    .find(|label| {
                        label.label().as_str() == long
                            && label.ancestor(gtk::Popover::static_type()).as_ref() == Some(popover)
                    })
            };
            let shown = label_in(outer.upcast_ref()).expect("the chosen label on the row");
            assert!(!shown.wraps());
            assert_eq!(shown.ellipsize(), gtk::pango::EllipsizeMode::End);

            button.popup();
            assert!(big_testkit::harness::pump_until(|| outer.is_mapped()));
            gtk::prelude::WidgetExt::activate(&row);
            let own = first_descendant::<gtk::Popover>(row.upcast_ref()).expect("popup");
            let own = own.upcast::<gtk::Widget>();
            assert!(big_testkit::harness::pump_until(|| label_in(&own).is_some()));
            assert!(label_in(&own).is_some_and(|label| label.wraps()));
            window.destroy();
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
