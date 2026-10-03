// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Many items in an [`ItemView`], for measuring and looking at.
//!
//! `cargo run -p big-item-view --example item_view_story -- [grid|compact|rows|details] [N]`

use std::rc::Rc;

use big_item_view::{
    Badge, Column, ColumnHeader, ColumnShows, Corner, Flow, Geometry, Glyph, IconSource,
    ItemContent, ItemPresenter, ItemView, TickMode,
};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

struct Names;

impl ItemPresenter for Names {
    fn content(&self, position: u32, item: &glib::Object, out: &mut ItemContent) {
        let Some(name) = item
            .downcast_ref::<gtk::StringObject>()
            .map(gtk::StringObject::string)
        else {
            return;
        };
        let folder = position.is_multiple_of(3);
        out.icon = IconSource::Gicon(
            gio::ThemedIcon::new(if folder { "folder" } else { "text-x-generic" }).upcast(),
        );
        out.title = name.to_string();
        out.dimmed = position % 13 == 5;
        if position.is_multiple_of(7) {
            out.badges.push(Badge {
                corner: Corner::BottomEnd,
                fill: gdk::RGBA::new(0.2, 0.5, 0.9, 1.0),
                glyph: Glyph::Text("M".into()),
            });
        }
        out.cells = vec![
            format!("{} KiB", (position * 37) % 9000),
            if folder {
                "Folder".into()
            } else {
                "Text".into()
            },
        ];
        out.accessible_label = Some(format!(
            "{}: {name}",
            if folder { "Folder" } else { "File" }
        ));
    }

    fn tooltip(&self, position: u32, _item: &glib::Object) -> Option<String> {
        Some(format!("Item {position}"))
    }
}

fn column(
    id: u32,
    title: &str,
    shows: ColumnShows,
    width: i32,
    expand: bool,
    xalign: f32,
) -> Column {
    Column {
        id,
        title: title.into(),
        shows,
        width,
        expand,
        xalign,
        sort: None,
    }
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).cloned().unwrap_or_else(|| "grid".into());
    let n: u32 = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(2000);
    let app = gtk::Application::builder()
        .application_id("br.com.biglinux.ItemViewStory")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        let strings: Vec<String> = (0..n)
            .map(|i| format!("item-{i:05} a longer name that has to wrap somewhere"))
            .collect();
        let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
        let model = gtk::MultiSelection::new(Some(gtk::StringList::new(&refs)));
        let mut geometry = match mode.as_str() {
            "rows" => Geometry::rows(22),
            "details" => Geometry {
                columns: vec![
                    column(0, "Name", ColumnShows::Primary, 240, true, 0.0),
                    column(1, "Size", ColumnShows::Cell(0), 100, false, 1.0),
                    column(2, "Kind", ColumnShows::Cell(1), 140, false, 0.0),
                ],
                ..Geometry::rows(22)
            },
            "compact" => Geometry {
                flow: Flow::ColumnMajor,
                cell_width: 22 + 180,
                ..Geometry::rows(22)
            },
            _ => Geometry::grid(112, 64),
        };
        geometry.tick = TickMode::Selection;
        let view = ItemView::new(Rc::new(Names), geometry);
        view.set_model(Some(&model));
        view.set_enable_rubberband(true);
        view.connect_activate(|_, position| println!("activate {position}"));
        view.connect_context_menu(|_, position, x, y| {
            println!("context-menu {position} at {x:.0},{y:.0}")
        });
        view.connect_visible_range_changed(|_, range| println!("visible {range:?}"));
        let (h, v) = match mode.as_str() {
            "compact" => (gtk::PolicyType::Automatic, gtk::PolicyType::Never),
            "details" => (gtk::PolicyType::Automatic, gtk::PolicyType::Automatic),
            _ => (gtk::PolicyType::Never, gtk::PolicyType::Automatic),
        };
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(h)
            .vscrollbar_policy(v)
            .vexpand(true)
            .child(&view)
            .build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if mode == "details" {
            let header = ColumnHeader::new(&view);
            let sorted = view.downgrade();
            header.connect_sort_requested(move |_, id| {
                println!("sort by {id}");
                let Some(view) = sorted.upgrade() else { return };
                let mut geometry = view.geometry();
                for column in &mut geometry.columns {
                    column.sort = (column.id == id).then_some(match column.sort {
                        Some(gtk::SortType::Ascending) => gtk::SortType::Descending,
                        _ => gtk::SortType::Ascending,
                    });
                }
                view.set_geometry(geometry);
            });
            header.connect_column_resized(|_, id, width| println!("column {id} is {width} wide"));
            let moved = view.downgrade();
            header.connect_column_moved(move |_, id, to| {
                println!("column {id} moved to {to}");
                let Some(view) = moved.upgrade() else { return };
                let mut geometry = view.geometry();
                if let Some(from) = geometry.columns.iter().position(|c| c.id == id) {
                    let column = geometry.columns.remove(from);
                    geometry.columns.insert(to as usize, column);
                    view.set_geometry(geometry);
                }
            });
            page.append(&header);
        }
        page.append(&scroller);
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("ItemView story")
            .default_width(1100)
            .default_height(760)
            .child(&page)
            .build();
        // F9 restyles the view at run time, the way a theme or accent change does:
        // the next frame has to draw with the new colours.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(|_, key, _, _| {
            if key == gdk::Key::F9 {
                let provider = gtk::CssProvider::new();
                provider.load_from_string(
                    "itemview > probe.hover-fill { color: alpha(red, 0.5); } \
                     itemview > probe.selected-fill { color: alpha(green, 0.5); }",
                );
                if let Some(display) = gdk::Display::default() {
                    gtk::style_context_add_provider_for_display(&display, &provider, 800);
                }
                println!("restyled");
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(keys);
        window.present();
    });
    app.run_with_args::<&str>(&[])
}
