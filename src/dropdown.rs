// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Choice rows and drop-downs that name each choice for a screen reader.
//!
//! GTK's and libadwaita's own factories leave every choice unnamed, and
//! `AdwComboRow` cuts each one at 20 characters. `clippy.toml` sends every
//! choice row and drop-down of the suite through [`build`] and
//! [`build_drop_down`].

use crate::widget_tree::{children, descendants, subtree};
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

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
    for factory in [row.factory(), row.list_factory()] {
        name_items(factory);
    }
    // libadwaita makes a new default factory when the expression changes.
    row.connect_factory_notify(|row| {
        show_whole_in(row.factory());
        name_items(row.factory());
    });
    row.connect_list_factory_notify(|row| {
        show_whole_in(row.list_factory());
        name_items(row.list_factory());
    });
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
    for factory in [dropdown.factory(), dropdown.list_factory()] {
        name_items(factory);
    }
    // GTK makes new default factories when the expression changes.
    dropdown.connect_factory_notify(|dropdown| name_items(dropdown.factory()));
    dropdown.connect_list_factory_notify(|dropdown| name_items(dropdown.list_factory()));
    focus_chosen_item_on_open(&dropdown, gtk::DropDown::selected);
    dropdown
}

/// A drop-down offering `labels`, its choices named.
#[must_use]
pub fn from_strings(labels: &[&str]) -> gtk::DropDown {
    build_drop_down(gtk::DropDown::builder().model(&gtk::StringList::new(labels)))
}

/// The drop-down `builder` describes, as [`build_drop_down`] makes it, read by
/// a screen reader as the field `name` and its choice: "`name`: choice", or
/// `name` alone while nothing is chosen. `name` is already translated.
///
/// The choice is what the expression gives for the chosen item, followed as
/// the item's properties change; GTK's own factory shows the text as it was
/// when the item was bound, so items that change need a factory that binds it.
#[must_use]
pub fn named_drop_down(builder: gtk::builders::DropDownBuilder, name: &str) -> gtk::DropDown {
    let dropdown = build_drop_down(builder);
    // GTK names the drop-down, and through it its button, after the item the
    // button shows, by a labelled-by relation that outranks any label.
    dropdown.reset_relation(gtk::AccessibleRelation::LabelledBy);
    let field: Rc<str> = name.into();
    // The shown text also changes when a property the expression reads
    // changes on the chosen item, which neither notify below reports.
    let watch = Rc::new(RefCell::new(None::<gtk::ExpressionWatch>));
    let follow = Rc::new({
        let watch = Rc::clone(&watch);
        move |dropdown: &gtk::DropDown| {
            if let Some(old) = watch.take() {
                old.unwatch();
            }
            label_with_choice(dropdown, &field);
            if let (Some(expression), Some(item)) =
                (dropdown.expression(), dropdown.selected_item())
            {
                let weak = dropdown.downgrade();
                let field = Rc::clone(&field);
                watch.replace(Some(expression.watch(Some(&item), move || {
                    if let Some(dropdown) = weak.upgrade() {
                        label_with_choice(&dropdown, &field);
                    }
                })));
            }
        }
    });
    follow(&dropdown);
    dropdown.connect_selected_item_notify({
        let follow = Rc::clone(&follow);
        move |dropdown| follow(dropdown)
    });
    dropdown.connect_expression_notify(move |dropdown| follow(dropdown));
    // The chosen item may outlive the drop-down, and its watch with it.
    dropdown.connect_destroy(move |_| {
        if let Some(old) = watch.take() {
            old.unwatch();
        }
    });
    dropdown
}

/// Label `dropdown` as the field `name` and its choice: the drop-down's
/// expression evaluated on the chosen item, else a [`gtk::StringObject`]'s
/// string.
fn label_with_choice(dropdown: &gtk::DropDown, name: &str) {
    let shown = dropdown
        .selected_item()
        .and_then(|item| match dropdown.expression() {
            Some(expression) => expression
                .evaluate(Some(&item))
                .and_then(|value| value.get::<Option<String>>().ok().flatten()),
            None => item
                .downcast::<gtk::StringObject>()
                .ok()
                .map(|choice| choice.string().into()),
        })
        .filter(|shown| !shown.is_empty());
    let label = match shown {
        Some(shown) => format!("{name}: {shown}"),
        None => name.to_owned(),
    };
    dropdown.update_property(&[gtk::accessible::Property::Label(&label)]);
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
    for factory in [row.factory(), row.list_factory()] {
        show_whole_in(factory);
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

/// Show whole the labels `factory` makes from now on, once per factory.
fn show_whole_in(factory: Option<gtk::ListItemFactory>) {
    let Some(factory) = factory.and_then(|factory| first_dressing(&factory, Dressing::Whole))
    else {
        return;
    };
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

/// What was connected to a factory.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dressing {
    Whole,
    Named,
}

thread_local! {
    /// Each factory connected to, and to what: a factory set again, or used
    /// for both the button and the popup, is connected once.
    static DRESSED: RefCell<Vec<(gtk::glib::WeakRef<gtk::ListItemFactory>, Dressing)>> =
        const { RefCell::new(Vec::new()) };
}

/// `factory` as a signal factory, the first time it is given `dressing`;
/// `None` after that, or for a factory of another kind.
fn first_dressing(
    factory: &gtk::ListItemFactory,
    dressing: Dressing,
) -> Option<gtk::SignalListItemFactory> {
    let signals = factory
        .downcast_ref::<gtk::SignalListItemFactory>()?
        .clone();
    DRESSED.with_borrow_mut(|dressed| {
        dressed.retain(|(factory, _)| factory.upgrade().is_some());
        let done = dressed
            .iter()
            .any(|(other, kind)| *kind == dressing && other.upgrade().as_ref() == Some(factory));
        if done {
            return None;
        }
        dressed.push((factory.downgrade(), dressing));
        Some(signals)
    })
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

/// The text `item` shows: its factory's label, which the drop-down's
/// expression filled, else a [`gtk::StringObject`]'s string.
fn shown_text(item: &gtk::ListItem) -> Option<String> {
    let label = item
        .child()
        .into_iter()
        .flat_map(|child| subtree(&child))
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .map(|label| label.text().to_string())
        .find(|text| !text.is_empty());
    label.or_else(|| {
        item.item()
            .and_downcast::<gtk::StringObject>()
            .map(|value| value.string().into())
    })
}

/// Name each item `factory` makes after the text it shows, once per factory.
/// GTK's and libadwaita's default factories draw a label and a check
/// mark but leave the list item unnamed, so a screen reader reads an empty
/// item; naming it keeps the factory's look. An item the factory named
/// itself, which binds first, keeps that name.
fn name_items(factory: Option<gtk::ListItemFactory>) {
    let Some(factory) = factory.and_then(|factory| first_dressing(&factory, Dressing::Named))
    else {
        return;
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
        if !named && let Some(shown) = shown_text(item) {
            item.set_accessible_label(&shown);
        }
    });
    // A reused item starts unnamed, so the name above never outlives its choice.
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_accessible_label("");
        }
    });
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

    /// Items of a model shown through the drop-down's expression are named by
    /// the text they show.
    #[test]
    fn expression_choices_are_named() {
        big_testkit::harness::require_gtk_test(|| {
            let choices = gtk::gio::ListStore::new::<gtk::Label>();
            for name in ["Front", "Rear"] {
                choices.append(&gtk::Label::new(Some(name)));
            }
            let dropdown = build_drop_down(gtk::DropDown::builder().model(&choices).expression(
                gtk::PropertyExpression::new(
                    gtk::Label::static_type(),
                    None::<gtk::Expression>,
                    "label",
                ),
            ));
            let window = gtk::Window::builder().child(&dropdown).build();
            window.present();
            big_testkit::harness::pump();
            gtk::prelude::WidgetExt::activate(&dropdown);
            let popover = first_descendant::<gtk::Popover>(dropdown.upcast_ref()).expect("popup");
            let list = first_descendant::<gtk::ListView>(popover.upcast_ref()).expect("list");
            assert_named(&list, &[c"Front", c"Rear"]);
            window.destroy();
        });
    }

    /// The default factories GTK makes again for a new expression name their
    /// items too.
    #[test]
    fn new_expression_keeps_names() {
        big_testkit::harness::require_gtk_test(|| {
            let dropdown = from_strings(&["Front", "Rear"]);
            dropdown.set_expression(Some(gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                None::<gtk::Expression>,
                "string",
            )));
            let window = gtk::Window::builder().child(&dropdown).build();
            window.present();
            big_testkit::harness::pump();
            gtk::prelude::WidgetExt::activate(&dropdown);
            let popover = first_descendant::<gtk::Popover>(dropdown.upcast_ref()).expect("popup");
            let list = first_descendant::<gtk::ListView>(popover.upcast_ref()).expect("list");
            assert_named(&list, &[c"Front", c"Rear"]);
            window.destroy();
        });
    }

    /// The default factory libadwaita makes again for a new expression shows
    /// its choices whole and names them.
    #[test]
    fn row_new_expression_keeps_names() {
        big_testkit::harness::require_gtk_test(|| {
            let row = choice_row("Camera", &["Front", "Rear"]);
            row.set_expression(Some(gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                None::<gtk::Expression>,
                "string",
            )));
            let list = gtk::ListView::new(
                row.model().map(|model| gtk::NoSelection::new(Some(model))),
                row.factory(),
            );
            let window = gtk::Window::builder().child(&list).build();
            window.present();
            assert_named(&list, &[c"Front", c"Rear"]);
            window.destroy();
        });
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

    /// The field's name stays first through every change of what is chosen.
    #[test]
    fn named_drop_down_follows_choice() {
        big_testkit::harness::require_gtk_test(|| {
            let dropdown = named_drop_down(
                gtk::DropDown::builder().model(&gtk::StringList::new(&[])),
                "Camera",
            );
            let reads = |name: &std::ffi::CStr| name_mismatch(dropdown.upcast_ref(), name);
            assert!(!gtk::test_accessible_has_relation(
                &dropdown,
                gtk::AccessibleRelation::LabelledBy
            ));
            assert_eq!(reads(c"Camera"), None);
            dropdown.set_model(Some(&gtk::StringList::new(&["Front", "Rear"])));
            assert_eq!(reads(c"Camera: Front"), None);
            dropdown.set_selected(1);
            assert_eq!(reads(c"Camera: Rear"), None);
            // The drop-down shows what its expression says, not the item's string.
            dropdown.set_expression(Some(gtk::ClosureExpression::with_callback(
                [] as [gtk::Expression; 0],
                |values| {
                    let item = values[0].get::<gtk::StringObject>().expect("a choice");
                    format!("{} lens", item.string())
                },
            )));
            assert_eq!(reads(c"Camera: Rear lens"), None);
            dropdown.set_model(Some(&gtk::StringList::new(&[])));
            assert_eq!(reads(c"Camera"), None);
        });
    }

    /// The name follows a property the expression reads on the chosen item,
    /// changed without replacing the item.
    #[test]
    fn named_drop_down_follows_item_property() {
        big_testkit::harness::require_gtk_test(|| {
            let choices = gtk::gio::ListStore::new::<gtk::Label>();
            let front = gtk::Label::new(Some("Front"));
            choices.append(&front);
            let dropdown = named_drop_down(
                gtk::DropDown::builder()
                    .model(&choices)
                    .expression(gtk::PropertyExpression::new(
                        gtk::Label::static_type(),
                        None::<gtk::Expression>,
                        "label",
                    )),
                "Camera",
            );
            let reads = |name: &std::ffi::CStr| name_mismatch(dropdown.upcast_ref(), name);
            assert_eq!(reads(c"Camera: Front"), None);
            front.set_label("Rear");
            assert_eq!(reads(c"Camera: Rear"), None);
        });
    }

    /// The `(role, name)` of each node of `application` in tree order whose
    /// AT-SPI role is one of `roles`, as a screen reader reads them.
    fn heard(
        bus: &gtk::gio::DBusConnection,
        application: &str,
        roles: &[&str],
    ) -> Vec<(String, String)> {
        let call =
            |path: &str, interface: &str, method: &str, arguments: Option<gtk::glib::Variant>| {
                gtk::glib::MainContext::default()
                    .block_on(bus.call_future(
                        Some(application),
                        path,
                        interface,
                        method,
                        arguments.as_ref(),
                        None,
                        gtk::gio::DBusCallFlags::NONE,
                        5000,
                    ))
                    .expect("the application answers")
            };
        let mut found = Vec::new();
        let mut pending = vec!["/org/a11y/atspi/accessible/root".to_owned()];
        while let Some(path) = pending.pop() {
            let role = call(&path, "org.a11y.atspi.Accessible", "GetRoleName", None)
                .child_value(0)
                .get::<String>()
                .expect("a role name");
            if roles.contains(&role.as_str()) {
                let name = call(
                    &path,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    Some(("org.a11y.atspi.Accessible", "Name").to_variant()),
                )
                .child_value(0)
                .as_variant()
                .and_then(|name| name.get::<String>())
                .expect("a name");
                found.push((role, name));
            }
            let children = call(&path, "org.a11y.atspi.Accessible", "GetChildren", None);
            let children: Vec<String> = children
                .child_value(0)
                .iter()
                .filter_map(|child| child.child_value(1).get::<gtk::glib::variant::ObjectPath>())
                .map(|path| path.as_str().to_owned())
                .collect();
            pending.extend(children.into_iter().rev());
        }
        found
    }

    /// The combo box and the button that takes its focus are both read as the
    /// field and its choice, empty and after a choice made in its list.
    #[test]
    #[ignore = "other-display: needs the AT-SPI bridge: tools/headless-gate.sh --accessible cargo test -p big-gtk-kit -- --ignored"]
    fn named_drop_down_heard_with_choice() {
        big_testkit::harness::require_gtk_test(|| {
            use big_testkit::atspi::{accessibility_bus, settle};
            let empty = named_drop_down(
                gtk::DropDown::builder().model(&gtk::StringList::new(&[])),
                "Microphone",
            );
            let chosen = named_drop_down(
                gtk::DropDown::builder().model(&gtk::StringList::new(&["Front", "Rear"])),
                "Camera",
            );
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.append(&empty);
            column.append(&chosen);
            let window = gtk::Window::builder().child(&column).build();
            window.present();
            settle();
            let (bus, application) =
                accessibility_bus().expect("the AT-SPI bridge (headless-gate.sh --accessible)");
            let read = || heard(&bus, &application, &["combo box", "toggle button"]);
            let expected = |choice: &str| {
                [
                    ("combo box", "Microphone"),
                    ("toggle button", "Microphone"),
                    ("combo box", choice),
                    ("toggle button", choice),
                ]
                .map(|(role, name)| (role.to_owned(), name.to_owned()))
            };
            assert_eq!(read(), expected("Camera: Front"));

            gtk::prelude::WidgetExt::activate(&chosen);
            let popover = first_descendant::<gtk::Popover>(chosen.upcast_ref()).expect("popup");
            let list = first_descendant::<gtk::ListView>(popover.upcast_ref()).expect("list");
            // A click selects the row in the list, then activates it.
            list.model()
                .and_downcast::<gtk::SingleSelection>()
                .expect("the list's selection")
                .set_selected(1);
            list.emit_by_name::<()>("activate", &[&1_u32]);
            settle();
            assert_eq!(chosen.selected(), 1);
            assert_eq!(read(), expected("Camera: Rear"));
            window.destroy();
        });
    }
}
