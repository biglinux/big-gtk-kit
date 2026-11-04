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

/// Drags and drops over rows; every row can be picked up and dropped on.
struct Draggable;

impl ItemPresenter for Draggable {
    fn content(&self, _position: u32, item: &glib::Object, out: &mut ItemContent) {
        out.title = item
            .downcast_ref::<gtk::StringObject>()
            .map(|name| name.string().to_string())
            .unwrap_or_default();
    }

    fn drag_content(&self, positions: &[u32]) -> Option<gdk::ContentProvider> {
        Some(gdk::ContentProvider::for_value(
            &format!("{positions:?}").to_value(),
        ))
    }

    fn accepts_drop(&self, _target: Option<u32>) -> bool {
        true
    }
}

fn shown_view() -> (ItemView, gtk::MultiSelection, gtk::Window) {
    shown_view_of(Rc::new(Draggable))
}

fn shown_view_of(presenter: Rc<dyn ItemPresenter>) -> (ItemView, gtk::MultiSelection, gtk::Window) {
    let view = ItemView::new(presenter, Geometry::rows(16));
    let selection = gtk::MultiSelection::new(Some(gtk::StringList::new(&["a", "b", "c"])));
    view.set_model(Some(&selection));
    let window = gtk::Window::builder()
        .default_width(400)
        .default_height(300)
        .child(&view)
        .build();
    window.present();
    pump();
    (view, selection, window)
}

fn controller<T: IsA<gtk::EventController> + IsA<glib::Object>>(view: &ItemView) -> T {
    view.observe_controllers()
        .into_iter()
        .filter_map(Result::ok)
        .find_map(|controller| controller.downcast::<T>().ok())
        .expect("the view has the controller")
}

/// The middle of the row at `position`.
fn middle(view: &ItemView, position: u32) -> (f64, f64) {
    let rect = view.item_rect(position).expect("the row is shown");
    (
        f64::from(rect.x() + rect.width() / 2.0),
        f64::from(rect.y() + rect.height() / 2.0),
    )
}

/// Before the dragged value loads, enter and motion over a row answer the move.
#[gtk::test]
#[ignore = "needs a display"]
fn enter_and_motion_move_before_load() {
    let (view, _selection, window) = shown_view();
    view.set_drop_types(
        &[String::static_type()],
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    let target: gtk::DropTarget = controller(&view);
    let (x, y) = middle(&view, 0);
    let entered: gdk::DragAction = target.emit_by_name("enter", &[&x, &y]);
    let moved: gdk::DragAction = target.emit_by_name("motion", &[&x, &y]);
    assert_eq!(
        (entered, moved),
        (gdk::DragAction::MOVE, gdk::DragAction::MOVE)
    );
    window.destroy();
}

/// Copies what comes from elsewhere, and takes drops on item 1 alone.
struct Across;

impl ItemPresenter for Across {
    fn content(&self, _position: u32, _item: &glib::Object, _out: &mut ItemContent) {}

    fn accepts_drop(&self, target: Option<u32>) -> bool {
        target == Some(1)
    }

    fn drop_action(&self, _target: Option<u32>, value: &glib::Value) -> gdk::DragAction {
        if value.get::<&str>() == Ok("elsewhere") {
            gdk::DragAction::COPY
        } else {
            gdk::DragAction::MOVE
        }
    }
}

#[test]
fn hover_answers_presenter_action() {
    let elsewhere = "elsewhere".to_value();
    assert_eq!(
        answer(&Across, Some(1), Some(&elsewhere)),
        gdk::DragAction::COPY
    );
    assert_eq!(answer(&Across, Some(1), None), gdk::DragAction::MOVE);
    assert_eq!(
        answer(&Across, None, Some(&elsewhere)),
        gdk::DragAction::empty()
    );
}

/// Without preload the dragged value waits for the drop, whichever of the two
/// calls comes first.
#[gtk::test]
#[ignore = "needs a display"]
fn preload_is_asked_for() {
    let (view, _selection, window) = shown_view();
    view.set_drop_types(&[String::static_type()], gdk::DragAction::MOVE);
    let target: gtk::DropTarget = controller(&view);
    assert!(!target.is_preload());
    view.set_drop_preload(true);
    assert!(target.is_preload());
    window.destroy();

    let (view, _selection, window) = shown_view();
    view.set_drop_preload(true);
    view.set_drop_types(&[String::static_type()], gdk::DragAction::MOVE);
    assert!(controller::<gtk::DropTarget>(&view).is_preload());
    window.destroy();
}

/// Takes drops on items alone, and counts what reaches it.
#[derive(Default)]
struct OnItems {
    received: std::cell::Cell<u32>,
}

impl ItemPresenter for OnItems {
    fn content(&self, _position: u32, _item: &glib::Object, _out: &mut ItemContent) {}

    fn accepts_drop(&self, target: Option<u32>) -> bool {
        target.is_some()
    }

    fn receive_drop(
        &self,
        _target: Option<u32>,
        _value: &glib::Value,
        _action: gdk::DragAction,
    ) -> bool {
        self.received.set(self.received.get() + 1);
        true
    }
}

#[gtk::test]
#[ignore = "needs a display"]
fn background_refuses_drop() {
    let presenter = Rc::new(OnItems::default());
    let (view, _selection, window) = shown_view_of(presenter.clone());
    view.set_drop_types(&[String::static_type()], gdk::DragAction::MOVE);
    let target: gtk::DropTarget = controller(&view);
    // The signal carries the value boxed in a GValue.
    let value = glib::BoxedValue("x".to_value());
    let (x, y) = middle(&view, 0);
    let on_item: bool = target.emit_by_name("drop", &[&value, &x, &y]);
    let below: bool = target.emit_by_name("drop", &[&value, &x, &250.0f64]);
    assert_eq!((on_item, below, presenter.received.get()), (true, false, 1));
    window.destroy();
}
