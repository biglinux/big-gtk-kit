// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The widget in a real window: what it asks the presenter, and what it keeps.

use std::cell::Cell;
use std::rc::Rc;

use big_item_view::{Geometry, IconSource, ItemContent, ItemPresenter, ItemView};
use gtk::prelude::*;
use gtk::{gio, glib};

/// Counts how often the view asks about an item.
#[derive(Default)]
struct Counting {
    asked: Cell<u32>,
}

impl ItemPresenter for Counting {
    fn content(&self, position: u32, item: &glib::Object, out: &mut ItemContent) {
        self.asked.set(self.asked.get() + 1);
        out.title = self.title(position, item);
        out.icon = IconSource::Gicon(gio::ThemedIcon::new("folder").upcast());
    }

    /// Answered alone, as a presenter with costly content does: rows fitted
    /// to their names ask it of every item.
    fn title(&self, _position: u32, item: &glib::Object) -> String {
        item.downcast_ref::<gtk::StringObject>()
            .map(|name| name.string().to_string())
            .unwrap_or_default()
    }
}

fn strings(n: u32) -> gtk::StringList {
    let names: Vec<String> = (0..n).map(|i| format!("entry {i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    gtk::StringList::new(&refs)
}

/// A view of `n` items, 800×600, in a window, drawn at least once.
fn shown(n: u32) -> (gtk::Window, ItemView, Rc<Counting>, gtk::MultiSelection) {
    let presenter = Rc::new(Counting::default());
    let view = ItemView::new(presenter.clone(), Geometry::grid(100, 48));
    let model = gtk::MultiSelection::new(Some(strings(n)));
    view.set_model(Some(&model));
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&view)
        .build();
    let window = gtk::Window::builder()
        .default_width(800)
        .default_height(600)
        .child(&scroller)
        .build();
    window.present();
    assert!(
        pump_until(|| view.is_mapped() && view.height() > 0),
        "the view never showed"
    );
    assert!(
        pump_until(|| presenter.asked.get() > 0),
        "nothing was drawn"
    );
    pump();
    (window, view, presenter, model)
}

#[gtk::test]
#[ignore = "needs a display"]
fn only_visible_items_are_asked() {
    let (window, view, presenter, model) = shown(5000);
    let on_screen = view.visible_range().len() as u32;
    assert!(
        on_screen > 0 && on_screen < 200,
        "a screenful, not {on_screen}"
    );
    // Drawing plus the accessible window: a few screenfuls at most.
    assert!(
        presenter.asked.get() <= on_screen * 4,
        "{} asked for {on_screen} on screen",
        presenter.asked.get()
    );

    // A reload: every item replaced in one splice.
    presenter.asked.set(0);
    let list = model.model().and_downcast::<gtk::StringList>().unwrap();
    let names: Vec<String> = (0..5000).map(|i| format!("renamed {i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    list.splice(0, 5000, &refs);
    assert!(pump_until(|| presenter.asked.get() > 0));
    pump();
    assert!(
        presenter.asked.get() <= on_screen * 4,
        "a splice of 5000 asked about {} items",
        presenter.asked.get()
    );
    window.destroy();
}

#[gtk::test]
#[ignore = "needs a display"]
fn scroll_to_selects_and_focuses() {
    let (window, view, _, model) = shown(5000);
    view.scroll_to(
        4000,
        gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
    );
    pump();
    assert!(model.is_selected(4000));
    assert_eq!(model.selection().size(), 1, "only that item");
    assert_eq!(view.cursor(), Some(4000));
    assert!(
        pump_until(|| view.visible_range().contains(&4000)),
        "not brought on screen"
    );
    window.destroy();
}

#[gtk::test]
#[ignore = "needs a display"]
fn cursor_follows_its_item() {
    let (window, view, _, model) = shown(100);
    view.scroll_to(50, gtk::ListScrollFlags::FOCUS);
    let list = model.model().and_downcast::<gtk::StringList>().unwrap();
    list.splice(0, 0, &["new a", "new b", "new c"]);
    pump();
    assert_eq!(view.cursor(), Some(53));
    list.remove(0);
    pump();
    assert_eq!(view.cursor(), Some(52));
    window.destroy();
}

/// The item proxies assistive technology sees, in order.
fn proxies(view: &ItemView) -> Vec<gtk::Accessible> {
    let mut found = Vec::new();
    let mut next = view.first_accessible_child();
    while let Some(proxy) = next {
        if proxy.is::<gtk::Widget>() {
            break;
        }
        next = proxy.next_accessible_sibling();
        found.push(proxy);
    }
    found
}

/// The proxy whose bounds are where `position` is drawn.
fn proxy_at(view: &ItemView, position: u32) -> Option<gtk::Accessible> {
    let rect = view.item_rect(position)?;
    let drawn = (
        rect.x().round() as i32,
        rect.y().round() as i32,
        rect.width().round() as i32,
        rect.height().round() as i32,
    );
    proxies(view)
        .into_iter()
        .find(|proxy| proxy.bounds() == Some(drawn))
}

/// A scroll too small to change which items are on screen still moves them,
/// and the bounds a screen reader is given move with them.
#[gtk::test]
#[ignore = "needs a display"]
fn bounds_follow_scroll() {
    let (window, view, _, _) = shown(1000);
    let before = proxy_at(&view, 10).expect("a proxy for an item on screen");
    let adjustment = view.vadjustment().expect("scrolled");
    adjustment.set_value(adjustment.value() + 7.0);
    pump();
    assert_eq!(
        proxy_at(&view, 10).as_ref(),
        Some(&before),
        "the proxy still says where its item is drawn"
    );
    window.destroy();
}

/// A file arriving before the focused item moves it along one place; its
/// proxy must be the same object afterwards, since that is what a screen
/// reader holds on to.
#[gtk::test]
#[ignore = "needs a display"]
fn items_keep_proxy_on_insert() {
    let (window, view, _, model) = shown(100);
    let before = proxy_at(&view, 10).expect("a proxy for an item on screen");
    let list = model.model().and_downcast::<gtk::StringList>().unwrap();
    list.splice(0, 0, &["new a"]);
    pump();
    assert_eq!(proxy_at(&view, 11).as_ref(), Some(&before));
    list.remove(0);
    pump();
    assert_eq!(proxy_at(&view, 10).as_ref(), Some(&before));
    window.destroy();
}

#[gtk::test]
#[ignore = "needs a display"]
fn items_are_placed_where_they_are_hit() {
    let (window, view, _, _) = shown(100);
    let per_line = view.items_per_line();
    assert!(per_line >= 6, "800 px of 100 px cells: {per_line} per line");
    let rect = view.item_rect(per_line + 1).unwrap();
    let hit = view.position_at(
        f64::from(rect.x() + rect.width() / 2.0),
        f64::from(rect.y() + rect.height() / 2.0),
    );
    assert_eq!(hit, Some(per_line + 1));
    window.destroy();
}

/// With `hit_drawn_only`, the picture and the words are the item and the rest
/// of the cell is background, where a rubber band starts.
#[gtk::test]
#[ignore = "needs a display"]
fn hits_only_drawn_parts() {
    let (window, view, _, _) = shown(100);
    view.set_geometry(Geometry {
        hit_drawn_only: true,
        ..Geometry::grid(100, 48)
    });
    pump();
    let rect = view.item_rect(0).unwrap();
    let (x, y) = (f64::from(rect.x()), f64::from(rect.y()));
    let (w, h) = (f64::from(rect.width()), f64::from(rect.height()));
    // The picture, centred at the top, and beside a short name ("entry
    // 0"): the card is as wide as the cell.
    assert_eq!(view.position_at(x + w / 2.0, y + 30.0), Some(0));
    assert_eq!(view.position_at(x + w / 2.0 - 40.0, y + h - 10.0), Some(0));
    // The room around the card, where a rubber band starts: background.
    assert_eq!(view.position_at(x + 1.0, y + h / 2.0), None);
    assert_eq!(view.position_at(x + w - 1.0, y + h / 2.0), None);
    assert_eq!(view.position_at(x + w / 2.0, y + 1.0), None);
    assert_eq!(view.position_at(x + w / 2.0, y + h - 1.0), None);
    window.destroy();
}

/// A name being edited has its editor laid over it, following the item when
/// the list scrolls, and the editor goes when the edit ends.
#[gtk::test]
#[ignore = "needs a display"]
fn editor_covers_edited_name() {
    let (window, view, _, _) = shown(1000);
    let entry = gtk::Entry::new();
    view.begin_editing(3, &entry);
    // Laid out with the next frame.
    assert!(
        pump_until(|| entry.width() > 0),
        "the editor was never laid out"
    );
    assert_eq!(
        entry.parent().as_ref(),
        Some(view.upcast_ref::<gtk::Widget>())
    );
    let name = view.name_rect(3).unwrap();
    let placed = entry.compute_bounds(&view).unwrap();
    assert!(
        (placed.y() - name.y()).abs() < 24.0 && placed.x() <= name.x() + 1.0,
        "editor at {placed:?}, name at {name:?}"
    );
    view.end_editing();
    pump();
    assert!(entry.parent().is_none(), "the editor stayed in the view");
    window.destroy();
}

#[gtk::test]
#[ignore = "needs a display"]
fn a_view_that_goes_keeps_nothing_alive() {
    let (window, view, presenter, _) = shown(1000);
    let weak = view.downgrade();
    drop(view);
    window.destroy();
    drop(window);
    pump();
    assert!(weak.upgrade().is_none(), "the view outlived its window");
    assert_eq!(
        Rc::strong_count(&presenter),
        1,
        "the view kept its presenter"
    );
}

fn pump() {
    let context = glib::MainContext::default();
    let mut guard = 0;
    while context.pending() && guard < 10_000 {
        context.iteration(false);
        guard += 1;
    }
}

/// Pump until `is_ready`, for at most five seconds.
fn pump_until(is_ready: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !is_ready() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    true
}
