// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The headings over a list with columns.
//!
//! Real buttons, one per column: a heading is a control a person presses to
//! sort, tabs to, and hears named, and there are only ever a handful. They are
//! laid out where the view draws its columns and move with its sideways scroll.
//! The header only asks: sorting, resizing and moving a column are the
//! consumer's to decide and to remember, and each is a signal.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::ItemView;

/// How close to a column's edge a press starts resizing it.
const EDGE: f64 = 5.0;
/// How far a heading moves before it is being carried rather than pressed.
const CARRY: f64 = 8.0;
/// The narrowest a column can be made.
const NARROWEST: i32 = 48;

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// Resizing the column at `index`, from `width`.
    Resize { index: usize, width: f32 },
    /// Pressed on the heading at `index`; carried once it moves.
    Carry {
        index: usize,
        dx: f64,
        carrying: bool,
    },
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ColumnHeader {
        pub(super) view: glib::WeakRef<ItemView>,
        pub(super) buttons: RefCell<Vec<gtk::Button>>,
        /// What the buttons were built for: each column's id, title and arrow.
        pub(super) built_for: RefCell<Vec<(u32, String, Option<gtk::SortType>)>>,
        pub(super) scroll: RefCell<Option<(gtk::Adjustment, glib::SignalHandlerId)>>,
        pub(super) drag: Cell<Option<Drag>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ColumnHeader {
        const NAME: &'static str = "BigItemViewHeader";
        // One per module copy of this crate (see `ItemView`).
        const ALLOW_NAME_CONFLICT: bool = true;
        type Type = super::ColumnHeader;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("itemheader");
            klass.set_accessible_role(gtk::AccessibleRole::Row);
        }
    }

    impl ObjectImpl for ColumnHeader {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                use glib::subclass::Signal;
                let (u, i) = (u32::static_type(), i32::static_type());
                vec![
                    Signal::builder("sort-requested").param_types([u]).build(),
                    Signal::builder("column-resized")
                        .param_types([u, i])
                        .build(),
                    Signal::builder("column-moved").param_types([u, u]).build(),
                    Signal::builder("context-menu")
                        .param_types([u, f64::static_type(), f64::static_type()])
                        .build(),
                ]
            })
        }

        fn dispose(&self) {
            if let Some((adjustment, handler)) = self.scroll.take() {
                adjustment.disconnect(handler);
            }
            for button in self.buttons.take() {
                button.unparent();
            }
        }
    }

    impl WidgetImpl for ColumnHeader {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                return (0, 0, -1, -1);
            }
            let tallest = self
                .buttons
                .borrow()
                .iter()
                .map(|button| button.measure(orientation, -1).1)
                .max()
                .unwrap_or(0);
            (tallest, tallest, -1, -1)
        }

        fn size_allocate(&self, _width: i32, height: i32, _baseline: i32) {
            let Some(view) = self.view.upgrade() else {
                return;
            };
            let spans = view.column_spans();
            let offset = self
                .scroll
                .borrow()
                .as_ref()
                .map_or(0.0, |(adjustment, _)| adjustment.value() as f32);
            let carried = match self.drag.get() {
                Some(Drag::Carry {
                    index,
                    dx,
                    carrying: true,
                }) => Some((index, dx as f32)),
                _ => None,
            };
            for (index, (button, (x, width))) in self.buttons.borrow().iter().zip(spans).enumerate()
            {
                let shift = carried
                    .filter(|(carried, _)| *carried == index)
                    .map_or(0.0, |(_, dx)| dx);
                let transform = gtk::gsk::Transform::new()
                    .translate(&gtk::graphene::Point::new(x - offset + shift, 0.0));
                button.allocate(width.round() as i32, height, -1, Some(transform));
            }
        }
    }
}

glib::wrapper! {
    /// The headings of an [`ItemView`] with columns: put it above the view's
    /// scroller. Built from the view's [`crate::Geometry::columns`], rebuilt when
    /// they change, and scrolled with the view.
    pub struct ColumnHeader(ObjectSubclass<imp::ColumnHeader>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ColumnHeader {
    /// The headings of `view`.
    #[must_use]
    pub fn new(view: &ItemView) -> Self {
        let header: Self = glib::Object::new();
        header.imp().view.set(Some(view));
        let weak = header.downgrade();
        view.connect_closure(
            "geometry-changed",
            false,
            glib::closure_local!(move |_: ItemView| {
                if let Some(header) = weak.upgrade() {
                    header.rebuild();
                }
            }),
        );
        let weak = header.downgrade();
        view.connect_notify_local(Some("hadjustment"), move |view, _| {
            if let Some(header) = weak.upgrade() {
                header.follow(view);
            }
        });
        header.follow(view);
        header.rebuild();
        header.install_drag();
        header
    }

    /// Run `f` with a column's id when its heading is pressed: sort by it.
    pub fn connect_sort_requested<F: Fn(&Self, u32) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "sort-requested",
            false,
            glib::closure_local!(move |header: &Self, id: u32| f(header, id)),
        )
    }

    /// Run `f` with a column's id and new width when a resize ends. The view
    /// already shows the new width.
    pub fn connect_column_resized<F: Fn(&Self, u32, i32) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "column-resized",
            false,
            glib::closure_local!(move |header: &Self, id: u32, width: i32| f(header, id, width)),
        )
    }

    /// Run `f` with a column's id and the place it was carried to.
    pub fn connect_column_moved<F: Fn(&Self, u32, u32) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "column-moved",
            false,
            glib::closure_local!(move |header: &Self, id: u32, to: u32| f(header, id, to)),
        )
    }

    /// Run `f` with the id of the column a menu was asked for over (the
    /// secondary button, the Menu key or Shift+F10 on a heading), and where, in
    /// the header's coordinates: the columns' own menu (which are shown).
    pub fn connect_context_menu<F: Fn(&Self, u32, f64, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "context-menu",
            false,
            glib::closure_local!(move |header: &Self, id: u32, x: f64, y: f64| f(header, id, x, y)),
        )
    }

    /// Keep up with the view's sideways scroll.
    fn follow(&self, view: &ItemView) {
        let imp = self.imp();
        if let Some((old, handler)) = imp.scroll.take() {
            old.disconnect(handler);
        }
        if let Some(adjustment) = view.hadjustment() {
            let weak = self.downgrade();
            let handler = adjustment.connect_value_changed(move |_| {
                if let Some(header) = weak.upgrade() {
                    header.queue_allocate();
                }
            });
            imp.scroll.replace(Some((adjustment, handler)));
        }
        self.queue_allocate();
    }

    /// A button per column, when the columns are not the ones already shown.
    fn rebuild(&self) {
        let Some(view) = self.imp().view.upgrade() else {
            return;
        };
        let columns = view.geometry().columns;
        let wanted: Vec<(u32, String, Option<gtk::SortType>)> = columns
            .iter()
            .map(|column| (column.id, column.title.clone(), column.sort))
            .collect();
        if *self.imp().built_for.borrow() == wanted {
            self.queue_allocate();
            return;
        }
        for button in self.imp().buttons.take() {
            button.unparent();
        }
        let mut buttons = Vec::with_capacity(columns.len());
        for column in &columns {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let title = gtk::Label::builder()
                .label(&column.title)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .hexpand(true)
                .xalign(column.xalign)
                .build();
            content.append(&title);
            let sort = match column.sort {
                None => gtk::AccessibleSort::None,
                Some(gtk::SortType::Ascending) => gtk::AccessibleSort::Ascending,
                Some(_) => gtk::AccessibleSort::Descending,
            };
            if let Some(order) = column.sort {
                content.append(&gtk::Image::from_icon_name(match order {
                    gtk::SortType::Ascending => "pan-up-symbolic",
                    _ => "pan-down-symbolic",
                }));
            }
            let button = gtk::Button::builder()
                .child(&content)
                .accessible_role(gtk::AccessibleRole::ColumnHeader)
                .build();
            button.update_property(&[
                gtk::accessible::Property::Label(&column.title),
                gtk::accessible::Property::Sort(sort),
            ]);
            let (weak, id) = (self.downgrade(), column.id);
            button.connect_clicked(move |_| {
                if let Some(header) = weak.upgrade() {
                    header.emit_by_name::<()>("sort-requested", &[&id]);
                }
            });
            // The column's menu from the keyboard, on the heading that has it.
            let keys = gtk::EventControllerKey::new();
            let (weak, id) = (self.downgrade(), column.id);
            keys.connect_key_pressed(move |controller, key, _, state| {
                let menu = key == gtk::gdk::Key::Menu
                    || (key == gtk::gdk::Key::F10
                        && state.contains(gtk::gdk::ModifierType::SHIFT_MASK));
                if let (true, Some(header), Some(button)) =
                    (menu, weak.upgrade(), controller.widget())
                {
                    let at = button
                        .compute_point(
                            &header,
                            &gtk::graphene::Point::new(0.0, button.height() as f32),
                        )
                        .unwrap_or(gtk::graphene::Point::zero());
                    header.emit_by_name::<()>(
                        "context-menu",
                        &[&id, &f64::from(at.x()), &f64::from(at.y())],
                    );
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            button.add_controller(keys);
            button.set_parent(self);
            buttons.push(button);
        }
        self.imp().buttons.replace(buttons);
        self.imp().built_for.replace(wanted);
        self.queue_resize();
    }

    /// Where the column at `index` sits on screen, and whether `x` is on its
    /// resizing edge.
    fn column_at(&self, x: f64) -> Option<(usize, bool)> {
        let view = self.imp().view.upgrade()?;
        let offset = self
            .imp()
            .scroll
            .borrow()
            .as_ref()
            .map_or(0.0, |(adjustment, _)| adjustment.value());
        let rtl = view.direction() == gtk::TextDirection::Rtl;
        let spans = view.column_spans();
        // The edge first: a press just past a column's end is on the next
        // column's heading, and it still means that edge.
        for (index, (start, width)) in spans.iter().enumerate() {
            let edge = if rtl {
                f64::from(*start) - offset
            } else {
                f64::from(start + width) - offset
            };
            if (x - edge).abs() <= EDGE {
                return Some((index, true));
            }
        }
        spans
            .iter()
            .enumerate()
            .find_map(|(index, (start, width))| {
                let from = f64::from(*start) - offset;
                (x >= from && x < from + f64::from(*width)).then_some((index, false))
            })
    }

    fn install_drag(&self) {
        let secondary = gtk::GestureClick::new();
        secondary.set_button(gtk::gdk::BUTTON_SECONDARY);
        let weak = self.downgrade();
        secondary.connect_pressed(move |gesture, _, x, y| {
            let Some(header) = weak.upgrade() else {
                return;
            };
            let id = header.column_at(x).and_then(|(index, _)| {
                header
                    .imp()
                    .view
                    .upgrade()
                    .and_then(|view| view.geometry().columns.get(index).map(|column| column.id))
            });
            if let Some(id) = id {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                header.emit_by_name::<()>("context-menu", &[&id, &x, &y]);
            }
        });
        self.add_controller(secondary);

        let motion = gtk::EventControllerMotion::new();
        let weak = self.downgrade();
        motion.connect_motion(move |_, x, _| {
            if let Some(header) = weak.upgrade() {
                let resizing = matches!(header.imp().drag.get(), Some(Drag::Resize { .. }));
                let on_edge = header.column_at(x).is_some_and(|(_, edge)| edge);
                header.set_cursor_from_name((resizing || on_edge).then_some("col-resize"));
            }
        });
        self.add_controller(motion);

        let drag = gtk::GestureDrag::new();
        // Before the buttons: a press on an edge is a resize, never a sort.
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        drag.connect_drag_begin(move |gesture, x, _| {
            let Some(header) = weak.upgrade() else {
                return;
            };
            let Some((index, edge)) = header.column_at(x) else {
                return;
            };
            if edge {
                let width = header
                    .imp()
                    .view
                    .upgrade()
                    .and_then(|view| view.column_spans().get(index).map(|span| span.1))
                    .unwrap_or(0.0);
                header.imp().drag.set(Some(Drag::Resize { index, width }));
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else {
                header.imp().drag.set(Some(Drag::Carry {
                    index,
                    dx: 0.0,
                    carrying: false,
                }));
            }
        });
        let weak = self.downgrade();
        drag.connect_drag_update(move |gesture, dx, _| {
            let Some(header) = weak.upgrade() else {
                return;
            };
            let Some(view) = header.imp().view.upgrade() else {
                return;
            };
            let rtl = view.direction() == gtk::TextDirection::Rtl;
            match header.imp().drag.get() {
                Some(Drag::Resize { index, width }) => {
                    let grown = if rtl { -dx } else { dx };
                    let mut geometry = view.geometry();
                    if let Some(column) = geometry.columns.get_mut(index) {
                        column.width = ((f64::from(width) + grown).round() as i32).max(NARROWEST);
                        view.set_geometry(geometry);
                    }
                }
                Some(Drag::Carry {
                    index, carrying, ..
                }) => {
                    let carrying = carrying || dx.abs() > CARRY;
                    if carrying {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                    header.imp().drag.set(Some(Drag::Carry {
                        index,
                        dx,
                        carrying,
                    }));
                    header.queue_allocate();
                }
                None => {}
            }
        });
        let weak = self.downgrade();
        drag.connect_drag_end(move |_, dx, _| {
            let Some(header) = weak.upgrade() else {
                return;
            };
            let drag = header.imp().drag.take();
            let Some(view) = header.imp().view.upgrade() else {
                return;
            };
            let columns = view.geometry().columns;
            match drag {
                Some(Drag::Resize { index, .. }) => {
                    if let Some(column) = columns.get(index) {
                        header.emit_by_name::<()>("column-resized", &[&column.id, &column.width]);
                    }
                }
                Some(Drag::Carry {
                    index,
                    carrying: true,
                    ..
                }) => {
                    // Where the middle of the carried heading was let go.
                    let spans = view.column_spans();
                    let Some((start, width)) = spans.get(index).copied() else {
                        return;
                    };
                    let middle = f64::from(start + width / 2.0) + dx;
                    let to = spans
                        .iter()
                        .position(|(x, w)| middle >= f64::from(*x) && middle < f64::from(x + w))
                        .unwrap_or(if dx < 0.0 { 0 } else { spans.len() - 1 });
                    header.queue_allocate();
                    if to != index
                        && let Some(column) = columns.get(index)
                    {
                        header.emit_by_name::<()>("column-moved", &[&column.id, &(to as u32)]);
                    }
                }
                _ => header.queue_allocate(),
            }
        });
        self.add_controller(drag);
    }
}
