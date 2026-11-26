// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! One searchable list dialog: type, see the best matches, press Enter.
//!
//! Shared by the editor's command palette, Open File by Name and Set Syntax.
//! Matches go into a `GtkListView`, which builds widgets only for the rows on
//! screen, and a query shows at most [`MAX_ROWS`] of them.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

/// The most rows one query shows.
pub const MAX_ROWS: usize = 200;

/// One choice: what it says, and what the dialog hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRow {
    /// The line the query matches against.
    pub title: String,
    /// The dimmed line under it; empty shows none.
    pub subtitle: String,
    /// What the dialog hands back when this row is chosen.
    pub key: String,
}

/// What the dialog says, already translated.
pub struct PickLabels {
    /// The dialog's title.
    pub title: String,
    /// Shown in the empty search entry.
    pub placeholder: String,
    /// The search entry's accessible name.
    pub entry_label: String,
    /// The list's accessible name.
    pub list_label: String,
    /// Shown when a query matches nothing; `{query}` is replaced.
    pub no_match: String,
    /// A line under the list, such as the keys it takes.
    pub hint: Option<String>,
    /// The content's width, in pixels.
    pub width: i32,
    /// The content's height, in pixels.
    pub height: i32,
}

impl PickLabels {
    /// A program's command palette, in the words every BigLinux program uses,
    /// read through the caller's catalog, which lists this file in its
    /// `POTFILES`.
    #[must_use]
    pub fn command_palette(gettext: impl Fn(&str) -> String) -> Self {
        Self {
            title: gettext("Command Palette"),
            placeholder: gettext("Type a command…"),
            entry_label: gettext("Command"),
            list_label: gettext("Commands"),
            no_match: gettext("No command matches “{query}”."),
            hint: None,
            width: 520,
            height: 420,
        }
    }
}

/// How the choice was made: Enter, or Shift+Enter for "beside".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Enter.
    Plain,
    /// Shift+Enter.
    Beside,
}

/// An open dialog, for owners whose choices arrive after it is on screen.
pub struct Picker {
    dialog: big_gtk_kit::dialog::WeakDialog,
    refresh: Rc<dyn Fn()>,
    empty_text: Rc<std::cell::RefCell<String>>,
}

impl Picker {
    /// Run the query again, for choices that changed underneath it.
    pub fn refresh(&self) {
        (self.refresh)();
    }

    /// Say why there is nothing to choose from yet (or at all).
    pub fn set_empty_text(&self, text: &str) {
        text.clone_into(&mut self.empty_text.borrow_mut());
        (self.refresh)();
    }

    /// Run `closed` when the dialog goes away.
    pub fn connect_closed(&self, closed: impl Fn() + 'static) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.connect_closed(move |_| closed());
        }
    }
}

/// Open the dialog over `window`.
///
/// `rows` answers a query with the best matches first; `choose` gets the row
/// picked, the query as typed and how it was picked, after the dialog closes.
pub fn present(
    window: &gtk::Window,
    labels: PickLabels,
    empty_text: &str,
    rows: impl Fn(&str) -> Vec<PickRow> + 'static,
    choose: impl Fn(&gtk::Window, PickRow, String, Pick) + 'static,
) -> Picker {
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some(&labels.placeholder));
    entry.update_property(&[gtk::accessible::Property::Label(&labels.entry_label)]);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    let list = gtk::ListView::new(Some(selection.clone()), Some(row_factory()));
    list.add_css_class("navigation-sidebar");
    list.update_property(&[gtk::accessible::Property::Label(&labels.list_label)]);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let empty = crate::feedback::empty_state::line(empty_text);
    let body = gtk::Stack::new();
    body.add_named(&scroller, Some("rows"));
    body.add_named(&empty, Some("empty"));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&entry);
    content.append(&body);
    if let Some(hint) = &labels.hint {
        content.append(
            &gtk::Label::builder()
                .label(hint)
                .xalign(0.0)
                .css_classes(["caption", "dimmed"])
                .build(),
        );
    }

    // A header bar, so the dialog carries a close button.
    let framed = adw::ToolbarView::builder().content(&content).build();
    framed.add_top_bar(&crate::layout::window_handle::header_bar(
        adw::HeaderBar::builder(),
    ));
    let dialog = big_gtk_kit::dialog::Dialog::builder(Some(window))
        .title(&labels.title)
        .content_width(labels.width)
        .content_height(labels.height)
        .child(&framed)
        .build();

    // Weak everywhere a handler inside the dialog reaches the dialog or its own
    // widgets: a strong capture keeps every closed dialog alive.
    let empty_text = Rc::new(std::cell::RefCell::new(empty_text.to_owned()));
    let refresh: Rc<dyn Fn()> = Rc::new({
        let empty_text = Rc::clone(&empty_text);
        let (entry, body, empty, selection) = (
            entry.downgrade(),
            body.downgrade(),
            empty.downgrade(),
            selection.downgrade(),
        );
        let no_match = labels.no_match;
        let store = store.downgrade();
        move || {
            let (Some(entry), Some(body), Some(empty), Some(selection), Some(store)) = (
                entry.upgrade(),
                body.upgrade(),
                empty.upgrade(),
                selection.upgrade(),
                store.upgrade(),
            ) else {
                return;
            };
            let query = entry.text().to_string();
            let found: Vec<glib::BoxedAnyObject> = rows(&query)
                .into_iter()
                .take(MAX_ROWS)
                .map(glib::BoxedAnyObject::new)
                .collect();
            store.splice(0, store.n_items(), &found);
            if found.is_empty() {
                if query.trim().is_empty() {
                    empty.set_label(&empty_text.borrow());
                } else {
                    empty.set_label(&no_match.replace("{query}", &query));
                }
                body.set_visible_child_name("empty");
            } else {
                selection.set_selected(0);
                body.set_visible_child_name("rows");
            }
        }
    });
    refresh();
    entry.connect_search_changed({
        let refresh = Rc::clone(&refresh);
        move |_| refresh()
    });

    let choose = Rc::new(choose);
    let pick: Rc<dyn Fn(u32, Pick)> = Rc::new({
        let (dialog, window, entry, store) = (
            dialog.downgrade(),
            window.downgrade(),
            entry.downgrade(),
            store.downgrade(),
        );
        move |position, how| {
            let (Some(dialog), Some(window), Some(entry), Some(store)) = (
                dialog.upgrade(),
                window.upgrade(),
                entry.upgrade(),
                store.upgrade(),
            ) else {
                return;
            };
            let Some(row) = store
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
                .map(|item| item.borrow::<PickRow>().clone())
            else {
                return;
            };
            let query = entry.text().to_string();
            dialog.close();
            // After the dialog closes, so what the choice opens is not
            // fighting a dialog still on screen for focus.
            let choose = Rc::clone(&choose);
            glib::idle_add_local_once(move || choose(&window, row, query, how));
        }
    });
    list.connect_activate({
        let pick = Rc::clone(&pick);
        move |_, position| pick(position, Pick::Plain)
    });

    // On the whole content, so the keys hold in the list as in the entry, and
    // in capture: a GtkSearchEntry eats Escape for its own "stop search".
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let (list, entry, dialog, selection) = (
            list.downgrade(),
            entry.downgrade(),
            dialog.downgrade(),
            selection.downgrade(),
        );
        move |_, key, _, modifiers| {
            let (Some(list), Some(entry), Some(dialog), Some(selection)) = (
                list.upgrade(),
                entry.upgrade(),
                dialog.upgrade(),
                selection.upgrade(),
            ) else {
                return glib::Propagation::Proceed;
            };
            let in_list = big_gtk_kit::focus::holds_focus(&list);
            match key {
                gtk::gdk::Key::Down if !in_list => {
                    list.grab_focus();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Up if in_list && selection.selected() == 0 => {
                    entry.grab_focus();
                    entry.set_position(-1);
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Escape => {
                    dialog.close();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter
                    if selection.selected() != gtk::INVALID_LIST_POSITION =>
                {
                    let how = if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                        Pick::Beside
                    } else {
                        Pick::Plain
                    };
                    pick(selection.selected(), how);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    });
    content.add_controller(keys);
    entry.set_key_capture_widget(Some(&content));

    dialog.present();
    entry.grab_focus();
    Picker {
        dialog: dialog.downgrade(),
        refresh,
        empty_text,
    }
}

/// Title over a dimmed subtitle, built only for the rows on screen.
fn row_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["caption", "dimmed"])
            .build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
        column.set_margin_top(6);
        column.set_margin_bottom(6);
        column.append(&title);
        column.append(&subtitle);
        // One click opens a row. Not `single-click-activate`: that also makes
        // hovering select, so Enter would pick the row under the pointer.
        let click = gtk::GestureClick::new();
        let weak_item = item.downgrade();
        click.connect_released(move |gesture, presses, _, _| {
            let (Some(item), Some(widget)) = (weak_item.upgrade(), gesture.widget()) else {
                return;
            };
            if presses == 1 {
                let _ = widget
                    .activate_action("list.activate-item", Some(&item.position().to_variant()));
            }
        });
        column.add_controller(click);
        item.set_child(Some(&column));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(row), Some(column)) = (
            item.item().and_downcast::<glib::BoxedAnyObject>(),
            item.child().and_downcast::<gtk::Box>(),
        ) else {
            return;
        };
        let row = row.borrow::<PickRow>();
        let (Some(title), Some(subtitle)) = (
            column.first_child().and_downcast::<gtk::Label>(),
            column.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        title.set_label(&row.title);
        subtitle.set_label(&row.subtitle);
        subtitle.set_visible(!row.subtitle.is_empty());
        item.set_accessible_label(&if row.subtitle.is_empty() {
            row.title.clone()
        } else {
            format!("{}, {}", row.title, row.subtitle)
        });
    });
    factory
}

/// Whether `query` should show `label`.
///
/// Every typed character has to appear, in order, but not next to each other:
/// "gtl" finds "Go to Line…" the way a person expects a palette to work.
/// An empty query matches everything, so the list opens showing all of it.
#[must_use]
pub fn matches(label: &str, query: &str) -> bool {
    let mut wanted = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase);
    let mut next = wanted.next();
    for available in label.chars().flat_map(char::to_lowercase) {
        if next == Some(available) {
            next = wanted.next();
        }
    }
    next.is_none()
}

/// How well `label` answers `query`, lower is better, `None` for no match.
///
/// A ranking beyond the filter puts "Sobre" (About) before "Perguntar à IA
/// sobre a seleção" for the query "sobre".
#[must_use]
pub fn rank(label: &str, query: &str) -> Option<u8> {
    let query = query.trim().to_lowercase();
    let label = label.to_lowercase();
    if query.is_empty() {
        return Some(4);
    }
    let words_start = label
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(&query));
    if label == query {
        Some(0)
    } else if label.starts_with(&query) {
        Some(1)
    } else if words_start {
        Some(2)
    } else if label.contains(&query) {
        Some(3)
    } else {
        matches(&label, &query).then_some(4)
    }
}

/// A window action a command palette offers.
pub struct PaletteAction {
    /// What the row says.
    pub label: String,
    /// Under it: the action's state and keys, or nothing.
    pub described: String,
    /// The action's full name, `win.` included.
    pub action: String,
    /// Its string target, for an action that takes one.
    pub target: Option<String>,
}

/// Offer `actions` over `window`, the best match for the query first and equal
/// matches in their own order; the one chosen is activated on the window.
pub fn present_actions(window: &gtk::Window, labels: PickLabels, actions: Vec<PaletteAction>) {
    let actions = Rc::new(actions);
    let offered = Rc::clone(&actions);
    present(
        window,
        labels,
        "",
        move |query| {
            let mut found: Vec<(u8, usize)> = offered
                .iter()
                .enumerate()
                .filter_map(|(index, action)| rank(&action.label, query).map(|rank| (rank, index)))
                .collect();
            found.sort_by_key(|(rank, _)| *rank);
            found
                .into_iter()
                .map(|(_, index)| PickRow {
                    title: offered[index].label.clone(),
                    subtitle: offered[index].described.clone(),
                    key: index.to_string(),
                })
                .collect()
        },
        move |window, row, _, _| {
            let Some(chosen) = row
                .key
                .parse::<usize>()
                .ok()
                .and_then(|index| actions.get(index))
            else {
                return;
            };
            let target = chosen.target.as_deref().map(ToVariant::to_variant);
            let _ = window.activate_action(&chosen.action, target.as_ref());
        },
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use adw::prelude::*;

    use super::*;
    use crate::layout::widget_container::subtree;

    #[test]
    fn chosen_action_runs_with_target() {
        big_testkit::harness::require_gtk_test(|| {
            let window = adw::Window::new();
            window.present();
            let ran = Rc::new(RefCell::new(None));
            let go = gio::SimpleAction::new("go", Some(glib::VariantTy::STRING));
            go.connect_activate({
                let ran = Rc::clone(&ran);
                move |_, target| *ran.borrow_mut() = target.and_then(glib::Variant::get::<String>)
            });
            let group = gio::SimpleActionGroup::new();
            group.add_action(&go);
            window.insert_action_group("win", Some(&group));
            let action = |label: &str, target: &str| PaletteAction {
                label: label.to_owned(),
                described: String::new(),
                action: "win.go".to_owned(),
                target: Some(target.to_owned()),
            };
            present_actions(
                window.upcast_ref(),
                PickLabels {
                    title: "Commands".to_owned(),
                    placeholder: String::new(),
                    entry_label: "Command".to_owned(),
                    list_label: "Commands".to_owned(),
                    no_match: String::new(),
                    hint: None,
                    width: 400,
                    height: 300,
                },
                vec![action("Stop", "stop"), action("Go Home", "home")],
            );
            let dialog = window.visible_dialog().expect("the palette");
            let entry = subtree(&dialog)
                .find_map(|widget| widget.downcast::<gtk::SearchEntry>().ok())
                .expect("the search field");
            let list = subtree(&dialog)
                .find_map(|widget| widget.downcast::<gtk::ListView>().ok())
                .expect("the list");
            entry.set_text("home");
            assert!(big_testkit::harness::pump_until(|| {
                list.model().is_some_and(|model| model.n_items() == 1)
            }));
            list.emit_by_name::<()>("activate", &[&0_u32]);
            assert!(big_testkit::harness::pump_until(|| ran.borrow().is_some()));
            assert_eq!(ran.borrow().as_deref(), Some("home"));
        });
    }

    /// The keys hold with the keyboard in the list: Up from the first row goes
    /// back to the entry, and Shift+Enter picks the selected row beside.
    #[test]
    fn keys_hold_in_the_list() {
        big_testkit::harness::require_gtk_test(|| {
            use gtk::gdk::{Key, ModifierType};
            let window = adw::Window::new();
            window.present();
            let picked = Rc::new(RefCell::new(None));
            let row = |key: &str| PickRow {
                title: key.to_owned(),
                subtitle: String::new(),
                key: key.to_owned(),
            };
            present(
                window.upcast_ref(),
                PickLabels::command_palette(str::to_owned),
                "",
                move |_| vec![row("a.txt"), row("b.txt"), row("notes.md")],
                {
                    let picked = Rc::clone(&picked);
                    move |_, row, _, how| *picked.borrow_mut() = Some((row.key, how))
                },
            );
            let dialog = window.visible_dialog().expect("the picker");
            let entry = subtree(&dialog)
                .find_map(|widget| widget.downcast::<gtk::SearchEntry>().ok())
                .expect("the search field");
            let list = subtree(&dialog)
                .find_map(|widget| widget.downcast::<gtk::ListView>().ok())
                .expect("the list");
            let content = entry.parent().expect("the content");
            let press = |key: Key, modifiers: ModifierType| {
                big_testkit::harness::press_key(
                    &content,
                    Some(gtk::PropagationPhase::Capture),
                    key,
                    modifiers,
                )
            };
            let focused = |widget: &gtk::Widget| big_gtk_kit::focus::holds_focus(widget);
            assert!(press(Key::Down, ModifierType::empty()));
            assert!(big_testkit::harness::pump_until(|| focused(
                list.upcast_ref()
            )));
            assert!(press(Key::Up, ModifierType::empty()));
            assert!(focused(entry.upcast_ref()), "Up on the first row");
            assert!(press(Key::Down, ModifierType::empty()));
            let selection = list
                .model()
                .and_downcast::<gtk::SingleSelection>()
                .expect("one selected row");
            selection.set_selected(1);
            assert!(!press(Key::Up, ModifierType::empty()), "the list moves up");
            assert!(press(Key::Return, ModifierType::SHIFT_MASK));
            assert!(big_testkit::harness::pump_until(|| picked
                .borrow()
                .is_some()));
            assert_eq!(*picked.borrow(), Some(("b.txt".to_owned(), Pick::Beside)));
            window.destroy();
        });
    }
}
