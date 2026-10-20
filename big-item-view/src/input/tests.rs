// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::{Column, ColumnShows, Geometry, ItemContent, ItemPresenter};

/// Names its items and rates each one its own length.
#[derive(Default)]
struct Named {
    seen: RefCell<Vec<String>>,
}

impl ItemPresenter for Named {
    fn content(&self, _position: u32, item: &glib::Object, out: &mut ItemContent) {
        let name = item
            .downcast_ref::<gtk::StringObject>()
            .expect("the presenter gets the item a tree row holds")
            .string()
            .to_string();
        self.seen.borrow_mut().push(name.clone());
        out.ratings = vec![u8::try_from(name.len()).unwrap_or(5).min(5)];
        out.hides_button = name == "b";
        out.title = name;
    }
}

fn pump() {
    let context = glib::MainContext::default();
    for _ in 0..20 {
        while context.iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// "a" holds "a1" and "a2"; "b" holds nothing.
fn tree() -> gtk::TreeListModel {
    let root = gtk::StringList::new(&["a", "b"]);
    gtk::TreeListModel::new(root, false, false, |item| {
        let name = item.downcast_ref::<gtk::StringObject>()?.string();
        (name == "a").then(|| gtk::StringList::new(&["a1", "a2"]).upcast())
    })
}

fn row(view: &ItemView, position: u32) -> gtk::TreeListRow {
    view.model()
        .and_then(|model| model.item(position))
        .and_downcast()
        .expect("a tree row")
}

/// A tree opens and closes from its expander and from the keyboard, the
/// presenter only ever sees the items the rows hold, and a press on a
/// star rates the item with that star.
#[gtk::test]
#[ignore = "needs a display"]
fn tree_toggles_and_star_rates() {
    let presenter = Rc::new(Named::default());
    let mut geometry = Geometry::rows(16);
    geometry.columns = vec![
        Column {
            id: 0,
            title: "Name".into(),
            shows: ColumnShows::Primary,
            width: 200,
            expand: true,
            xalign: 0.0,
            sort: None,
        },
        Column {
            id: 9,
            title: "Rating".into(),
            shows: ColumnShows::Stars(0),
            width: 100,
            expand: false,
            xalign: 0.0,
            sort: None,
        },
        Column {
            id: 8,
            title: String::new(),
            shows: ColumnShows::Button("go-next-symbolic"),
            width: 30,
            expand: false,
            xalign: 0.5,
            sort: None,
        },
    ];
    let view = ItemView::new(presenter.clone(), geometry.clone());
    let model = tree();
    view.set_model(Some(&gtk::SingleSelection::new(Some(model.clone()))));
    let window = gtk::Window::builder()
        .default_width(400)
        .default_height(300)
        .child(&view)
        .build();
    window.present();
    pump();
    assert_eq!(model.n_items(), 2);

    // The expander of "a", pressed where it is drawn.
    let cell = view.item_rect(0).expect("a is shown");
    let outer = crate::draw::primary_span(&geometry, (cell.width(), cell.height()), false);
    let spot = crate::draw::expander_box(&geometry, outer, 0, false);
    let (x, y) = (
        f64::from(cell.x() + spot.x() + 2.0),
        f64::from(cell.y() + spot.y() + 2.0),
    );
    match control_at(&view, 0, x, y) {
        Some(Control::Expander(row)) => row.set_expanded(true),
        _ => panic!("the expander of a is under the press"),
    }
    pump();
    assert_eq!(model.n_items(), 4);
    // "b" has no children: no expander to press.
    let cell_b = view.item_rect(3).expect("b is shown");
    assert!(control_at(&view, 3, x, f64::from(cell_b.y() + spot.y() + 2.0)).is_none());

    // The keyboard: from "a1", closing goes to its parent, then closes it.
    view.imp().set_cursor(Some(1));
    assert!(tree_key(&view, false));
    assert_eq!(view.cursor(), Some(0));
    assert!(tree_key(&view, false));
    pump();
    assert!(!row(&view, 0).is_expanded());
    assert_eq!(model.n_items(), 2);
    assert!(tree_key(&view, true));
    assert!(row(&view, 0).is_expanded());
    assert!(!tree_key(&view, true), "an open row opens no further");
    pump();

    // The third star of "a2".
    let cell = view.item_rect(2).expect("a2 is shown");
    let spans = crate::draw::column_spans(&geometry, cell.width(), false);
    let first = crate::draw::cell_icons(
        &geometry,
        &geometry.columns[1],
        spans[1],
        cell.height(),
        5,
        false,
    );
    let x = f64::from(cell.x() + first.x() + 2.0 * crate::draw::CELL_ICON + 2.0);
    let y = f64::from(cell.y() + first.y() + 2.0);
    assert!(matches!(
        control_at(&view, 2, x, y),
        Some(Control::Cell {
            column: 9,
            value: 3
        })
    ));

    // The button of "a2" answers; "b" has none.
    let button = |position: u32| {
        let cell = view.item_rect(position).expect("shown");
        let spans = crate::draw::column_spans(&geometry, cell.width(), false);
        let spot = crate::draw::cell_icons(
            &geometry,
            &geometry.columns[2],
            spans[2],
            cell.height(),
            1,
            false,
        );
        control_at(
            &view,
            position,
            f64::from(cell.x() + spot.x() + 2.0),
            f64::from(cell.y() + spot.y() + 2.0),
        )
    };
    assert!(matches!(
        button(2),
        Some(Control::Cell {
            column: 8,
            value: 0
        })
    ));
    assert!(button(3).is_none(), "b hides its button");

    let seen = presenter.seen.borrow();
    assert!(seen.iter().any(|name| name == "a1"), "{seen:?}");
    drop(seen);
    window.destroy();
}
