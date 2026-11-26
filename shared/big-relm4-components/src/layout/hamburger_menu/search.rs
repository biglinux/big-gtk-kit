// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Typing in a right-click menu searches it.
//!
//! The first letter typed while the menu is open turns it into a search over
//! every action it holds, submenus included, each shown with the path to where
//! it lives ("More Actions › Git"), so a person who finds it this way also
//! learns where it is. Labels are matched first, then the words an item names
//! itself by besides its label (`big-keywords`: "zip" for Compress), then the
//! path. Enter runs the first result that destroys nothing: typing "del" must
//! never make Delete Permanently the default. Escape, or emptying the field,
//! goes back to the menu as it was.

use std::cell::RefCell;
use std::rc::Rc;

use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

use super::{
    StructuredMenuBuild, action_row, menu_model_action_item, menu_model_door_item,
    menu_model_label_attribute, resolve_action_presentation,
};

const SEARCH_PAGE: &str = "search";
/// Results shown at once: the page is a list to pick from, not a catalog.
const MOST_RESULTS: usize = 12;

struct Found {
    row: gtk::Widget,
    label: String,
    keywords: String,
    path: String,
    destructive: bool,
}

/// Add the search page to the menu's stack and let a typed letter open it.
pub(super) fn install(
    build: &StructuredMenuBuild<'_>,
    popover: &gtk::Popover,
    menu: &gio::MenuModel,
    placeholder: &str,
    nothing_found: &str,
) {
    let mut found = Vec::new();
    collect(build, menu, &[], &mut found);
    if found.is_empty() {
        return;
    }
    let found = Rc::new(found);

    let entry = gtk::SearchEntry::builder()
        .placeholder_text(placeholder)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label(placeholder)]);
    let results = gtk::Box::new(gtk::Orientation::Vertical, 0);
    results.set_margin_start(6);
    results.set_margin_end(6);
    results.set_margin_bottom(6);
    let nothing = gtk::Label::new(Some(nothing_found));
    nothing.add_css_class("dimmed");
    nothing.set_margin_top(12);
    nothing.set_margin_bottom(12);
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&entry);
    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&results);
    build.stack.add_named(&page, Some(SEARCH_PAGE));

    let shown: Rc<RefCell<Vec<usize>>> = Rc::default();
    {
        let (found, shown, results, nothing) = (
            Rc::clone(&found),
            Rc::clone(&shown),
            results.clone(),
            nothing,
        );
        let stack = build.stack.downgrade();
        entry.connect_search_changed(move |entry| {
            let query = fold(&entry.text());
            while let Some(child) = results.first_child() {
                results.remove(&child);
            }
            if query.trim().is_empty() {
                shown.borrow_mut().clear();
                if let Some(stack) = stack.upgrade() {
                    back_to_menu(&stack);
                }
                return;
            }
            let mut ranked: Vec<(u8, usize)> = found
                .iter()
                .enumerate()
                .filter_map(|(index, item)| rank(item, &query).map(|rank| (rank, index)))
                .collect();
            ranked.sort_by_key(|(rank, index)| (*rank, *index));
            ranked.truncate(MOST_RESULTS);
            let order: Vec<usize> = ranked.into_iter().map(|(_, index)| index).collect();
            if order.is_empty() {
                results.append(&nothing);
            }
            for index in &order {
                results.append(&found[*index].row);
            }
            *shown.borrow_mut() = order;
        });
    }
    {
        let (found, shown) = (Rc::clone(&found), Rc::clone(&shown));
        entry.connect_activate(move |_| {
            let first = shown
                .borrow()
                .iter()
                .copied()
                .find(|index| !found[*index].destructive);
            if let Some(button) =
                first.and_then(|index| found[index].row.downcast_ref::<gtk::Button>().cloned())
            {
                button.emit_clicked();
            }
        });
    }
    {
        let stack = build.stack.downgrade();
        entry.connect_stop_search(move |entry| {
            entry.set_text("");
            if let Some(stack) = stack.upgrade() {
                back_to_menu(&stack);
            }
        });
    }
    {
        // Down from the field goes into the results, as it would in a list.
        let keys = gtk::EventControllerKey::new();
        let results = results.downgrade();
        keys.connect_key_pressed(move |_, keyval, _, _| {
            if !matches!(keyval, gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down) {
                return glib::Propagation::Proceed;
            }
            if let Some(first) = results.upgrade().and_then(|results| results.first_child()) {
                first.grab_focus();
            }
            glib::Propagation::Stop
        });
        entry.add_controller(keys);
    }

    // The first printable key typed on the menu starts the search with it.
    let typed = gtk::EventControllerKey::new();
    typed.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_stack = build.stack.downgrade();
    let weak_entry = entry.downgrade();
    typed.connect_key_pressed(move |controller, keyval, _, modifiers| {
        let (Some(stack), Some(entry)) = (weak_stack.upgrade(), weak_entry.upgrade()) else {
            return glib::Propagation::Proceed;
        };
        if stack.visible_child_name().as_deref() == Some(SEARCH_PAGE)
            || modifiers
                .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let Some(letter) = keyval.to_unicode().filter(|c| c.is_alphanumeric()) else {
            return glib::Propagation::Proceed;
        };
        let focus_in_text = controller
            .widget()
            .and_then(|widget| widget.root())
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus.is::<gtk::Text>() || focus.is::<gtk::TextView>());
        if focus_in_text {
            return glib::Propagation::Proceed;
        }
        super::cascade::close_all(stack.upcast_ref());
        stack.set_visible_child_full(SEARCH_PAGE, gtk::StackTransitionType::None);
        entry.set_text(&letter.to_string());
        entry.grab_focus();
        entry.set_position(-1);
        glib::Propagation::Stop
    });
    popover.add_controller(typed);
    let weak_entry = entry.downgrade();
    popover.connect_closed(move |_| {
        if let Some(entry) = weak_entry.upgrade() {
            entry.set_text("");
        }
    });
}

fn back_to_menu(stack: &gtk::Stack) {
    if stack.visible_child_name().as_deref() == Some(SEARCH_PAGE) {
        stack.set_visible_child_full("root", gtk::StackTransitionType::None);
        super::action_row::focus_first_page_menu_item(stack);
    }
}

/// Every action of `menu`, submenus and inline rows included, as a row whose
/// line under the label is the path to where it lives.
fn collect(
    build: &StructuredMenuBuild<'_>,
    menu: &gio::MenuModel,
    path: &[String],
    found: &mut Vec<Found>,
) {
    for index in 0..menu.n_items() {
        if let Some(section) = menu.item_link(index, "section") {
            // An inline row's label is the place its choices live ("Open With").
            let inline =
                super::menu_model_kind(menu, index).as_deref() == Some(super::BIG_KIND_INLINE);
            match menu_model_label_attribute(menu, index).filter(|_| inline) {
                Some(label) => {
                    let mut inner = path.to_vec();
                    inner.push(label);
                    collect(build, &section, &inner, found);
                }
                None => collect(build, &section, path, found),
            }
            continue;
        }
        if let Some(submenu) = menu.item_link(index, "submenu") {
            if let Some(door) = menu_model_door_item(menu, index) {
                let mut inner = path.to_vec();
                inner.push(door.label);
                collect(build, &submenu, &inner, found);
            }
            continue;
        }
        if super::menu_model_hidden(menu, index, build.action_groups) {
            continue;
        }
        let Some(mut item) = menu_model_action_item(menu, index) else {
            continue;
        };
        let action_group = resolve_action_presentation(&mut item, build.action_groups);
        // The verb an inline row is named for lives where the row does, not
        // under its own name.
        let primary = menu
            .item_attribute_value(index, super::BIG_PRIMARY_ATTRIBUTE, None)
            .and_then(|value| value.get::<bool>())
            .unwrap_or(false);
        let path = if primary && !path.is_empty() {
            &path[..path.len() - 1]
        } else {
            path
        };
        let joined = path.join(" › ");
        item.subtitle = (!joined.is_empty()).then(|| joined.clone());
        let row = action_row(build, &item, action_group);
        found.push(Found {
            row: row.upcast(),
            label: fold(&item.label),
            keywords: fold(item.keywords.as_deref().unwrap_or_default()),
            path: fold(&joined),
            destructive: item.destructive,
        });
    }
}

/// How well `item` answers `query`, lower first; `None` when a word of the
/// query is found nowhere.
fn rank(item: &Found, query: &str) -> Option<u8> {
    let words: Vec<&str> = query.split_whitespace().collect();
    let starts_a_word = |text: &str, word: &str| {
        text.split(|c: char| !c.is_alphanumeric())
            .any(|part| part.starts_with(word))
    };
    let everywhere = words.iter().all(|word| {
        item.label.contains(word) || item.keywords.contains(word) || item.path.contains(word)
    });
    if !everywhere {
        return None;
    }
    let first = words.first()?;
    Some(if item.label.starts_with(first) {
        0
    } else if starts_a_word(&item.label, first) {
        1
    } else if starts_a_word(&item.keywords, first) {
        2
    } else if item.label.contains(first) {
        3
    } else {
        4
    })
}

/// Lower case without the accents of the Latin alphabets, so "excluir" finds
/// "Excluir" and "acao" finds "Ação".
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(label: &str, keywords: &str, path: &str, destructive: bool) -> Found {
        Found {
            row: gtk::Label::new(None).upcast(),
            label: fold(label),
            keywords: fold(keywords),
            path: fold(path),
            destructive,
        }
    }

    #[test]
    fn labels_come_before_synonyms_and_paths() {
        big_testkit::harness::require_gtk_test(|| {
            let compress = found("Compactar…", "zip rar tar", "", false);
            let git = found("Preparar", "stage", "Mais ações › Git", false);
            assert_eq!(rank(&compress, &fold("comp")), Some(0));
            assert_eq!(rank(&compress, &fold("zip")), Some(2));
            assert_eq!(rank(&git, &fold("git")), Some(4));
            assert_eq!(rank(&git, &fold("git prep")), Some(4));
            assert_eq!(rank(&git, &fold("xyz")), None);
            assert_eq!(fold("Ação Excluída"), "acao excluida");
        });
    }
}
