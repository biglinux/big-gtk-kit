// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The widget.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::draw;
use crate::{Composition, Geometry, ItemContent, ItemPresenter, TickMode};

/// Room around a section heading's words: above, and below them.
const HEADING_AIR: f32 = 16.0;
const HEADING_BELOW: f32 = 4.0;

/// A press deferred to its release: what it does depends on whether the pointer
/// moved (a drag keeps the selection it started from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    SelectOnly(u32),
    Toggle(u32),
}

mod drawing;
mod imp;
mod measure;

glib::wrapper! {
    /// A drawn, virtualized view of many items. See the crate documentation.
    pub struct ItemView(ObjectSubclass<imp::ItemView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl ItemView {
    /// A view whose items `presenter` describes, laid out by `geometry`.
    #[must_use]
    pub fn new(presenter: Rc<dyn ItemPresenter>, geometry: Geometry) -> Self {
        let view: Self = glib::Object::new();
        view.imp().presenter.replace(Some(presenter));
        view.set_geometry(geometry);
        view
    }

    /// Draw coloured icons through `loader`; without one, GTK's icon theme
    /// draws every icon.
    pub fn set_icon_loader(&self, loader: crate::IconLoader) {
        self.imp().icon_loader.set(Some(loader));
        self.imp().icons.borrow_mut().clear();
        self.queue_draw();
    }

    /// Show `model`, or nothing.
    pub fn set_model(&self, model: Option<&impl IsA<gtk::SelectionModel>>) {
        self.imp()
            .set_model(model.map(|model| model.as_ref().clone()));
    }

    /// The model shown.
    #[must_use]
    pub fn model(&self) -> Option<gtk::SelectionModel> {
        self.imp().model()
    }

    /// Lay the items out by `geometry`.
    pub fn set_geometry(&self, geometry: Geometry) {
        self.imp().set_geometry(geometry);
    }

    /// Where each column of a list with columns sits across a line, as (x,
    /// width) in the view's scrolled coordinates.
    #[must_use]
    pub fn column_spans(&self) -> Vec<(f32, f32)> {
        let imp = self.imp();
        let width = imp
            .metrics
            .borrow()
            .clone()
            .cell_size()
            .0
            .max(imp.cell_size().0);
        crate::draw::column_spans(
            &imp.geometry.borrow(),
            width,
            self.direction() == gtk::TextDirection::Rtl,
        )
    }

    /// Run `f` when the geometry changes, or the width a line is laid out at.
    pub fn connect_geometry_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "geometry-changed",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    /// The geometry items are laid out by.
    #[must_use]
    pub fn geometry(&self) -> Geometry {
        self.imp().geometry.borrow().clone()
    }

    /// Activate on a single click instead of a double one.
    pub fn set_single_click_activate(&self, single: bool) {
        self.imp().single_click.set(single);
    }

    /// Shade every other row, which a long list of rows with columns needs for
    /// the eye to follow a line across.
    pub fn set_stripes(&self, stripes: bool) {
        self.imp().stripes.set(stripes);
        self.queue_draw();
    }

    /// Select by dragging a rectangle over the items from empty space.
    pub fn set_enable_rubberband(&self, enable: bool) {
        self.imp().rubberband.set(enable);
    }

    /// Accept drops of `types`; the presenter decides where they may land.
    pub fn set_drop_types(&self, types: &[glib::Type], actions: gdk::DragAction) {
        crate::input::install_drop(self, types, actions);
    }

    /// Bring the item at `position` on screen, and focus or select it as `flags` say.
    pub fn scroll_to(&self, position: u32, flags: gtk::ListScrollFlags) {
        let imp = self.imp();
        if position >= imp.n_items() {
            return;
        }
        if flags.contains(gtk::ListScrollFlags::SELECT)
            && let Some(model) = imp.model()
        {
            model.select_item(position, true);
            imp.anchor.set(Some(position));
        }
        if flags.contains(gtk::ListScrollFlags::FOCUS) {
            imp.set_cursor(Some(position));
            imp.keyboard_cursor.set(true);
            self.grab_focus();
        }
        crate::input::bring_into_view(self, position);
    }

    /// Forget the keyboard cursor and scroll back to the first item: for a
    /// consumer that shows a different listing in the same view (another
    /// category), where following the old cursor's item would land mid-list.
    pub fn start_over(&self) {
        let imp = self.imp();
        imp.set_cursor(None);
        imp.anchor.set(None);
        if let Some(adjustment) = imp.adjustment_along() {
            adjustment.set_value(adjustment.lower());
        }
    }

    /// Move the keyboard cursor one item or line the way `direction` points,
    /// as the arrow key would. `false` at the edge, for a consumer that walks
    /// the keyboard out of the view itself.
    pub fn step_cursor(&self, direction: gtk::DirectionType) -> bool {
        crate::input::step_cursor(self, direction)
    }

    /// The item the keyboard cursor is on.
    #[must_use]
    pub fn cursor(&self) -> Option<u32> {
        self.imp().cursor.get()
    }

    /// How many items one line holds: Up and Down move by this many.
    #[must_use]
    pub fn items_per_line(&self) -> u32 {
        self.imp().metrics.borrow().clone().items_per_line()
    }

    /// Where the item at `position` is drawn, in the view's coordinates.
    #[must_use]
    pub fn item_rect(&self, position: u32) -> Option<graphene::Rect> {
        let imp = self.imp();
        let rect = imp
            .sections
            .borrow()
            .rect(&imp.metrics.borrow().clone(), position)?;
        let scroll = imp.scroll();
        Some(graphene::Rect::new(
            rect.x - scroll.0,
            rect.y - scroll.1,
            rect.width,
            rect.height,
        ))
    }

    /// The item under `(x, y)` in the view's coordinates, if any.
    #[must_use]
    pub fn position_at(&self, x: f64, y: f64) -> Option<u32> {
        let imp = self.imp();
        let scroll = imp.scroll();
        let position = imp.sections.borrow().position_at(
            &imp.metrics.borrow().clone(),
            x as f32 + scroll.0,
            y as f32 + scroll.1,
        )?;
        if !self.hits_drawn_only() {
            return Some(position);
        }
        let point = graphene::Point::new(x as f32, y as f32);
        self.drawn_areas(position)?
            .iter()
            .any(|area| area.contains_point(&point))
            .then_some(position)
    }

    /// Where the item at `position`, `size` big, draws its picture and name:
    /// the primary span, past a tree row's expander.
    pub(crate) fn primary_of(
        &self,
        position: u32,
        geometry: &Geometry,
        size: (f32, f32),
    ) -> (f32, (f32, f32)) {
        let rtl = self.direction() == gtk::TextDirection::Rtl;
        let outer = draw::primary_span(geometry, size, rtl);
        match self
            .imp()
            .model()
            .and_then(|model| model.item(position))
            .and_then(|item| tree_of(&item))
        {
            Some((depth, _)) => draw::tree_span(outer, depth, rtl),
            None => outer,
        }
    }

    /// Whether only what an item draws is the item, and the rest of its cell
    /// the view's background (`Geometry::hit_drawn_only`).
    pub(crate) fn hits_drawn_only(&self) -> bool {
        let geometry = self.imp().geometry.borrow();
        geometry.hit_drawn_only && geometry.columns.is_empty()
    }

    /// What the item at `position` draws, in the view's coordinates: the card
    /// under its picture and words, and its tick.
    pub(crate) fn drawn_areas(&self, position: u32) -> Option<Vec<graphene::Rect>> {
        let imp = self.imp();
        let geometry = imp.geometry.borrow();
        let cell = self.item_rect(position)?;
        let size = (cell.width(), cell.height());
        let rtl = self.direction() == gtk::TextDirection::Rtl;
        let mut areas = Vec::new();
        if geometry.tick != TickMode::None {
            areas.push(draw::tick_box(&geometry, size, rtl));
        }
        // The card the item is drawn on (`draw::card`), from the layouts of the
        // last drawing; a new shape drops them until the next frame, and an
        // item not drawn yet has none, so they are made now from what the item
        // says.
        let (offset, primary) = self.primary_of(position, &geometry, size);
        let words = draw::text_box(&geometry, primary, rtl);
        let item = imp.model().and_then(|model| model.item(position))?;
        let key = item.as_ptr() as usize;
        let drawn = imp.cache.borrow().entries.get(&key).map(|entry| {
            (
                entry.title.clone(),
                entry.subtitle.clone(),
                entry.content.clone(),
            )
        });
        let (title, subtitle, content) = drawn.unwrap_or_else(|| {
            let mut content = ItemContent::default();
            if let Some(presenter) = imp.presenter.borrow().clone() {
                presenter.content(position, &crate::view::presented(&item), &mut content);
            }
            (None, None, content)
        });
        let title = title.unwrap_or_else(|| {
            draw::title_layout(
                &self.pango_context(),
                &geometry,
                &content.title,
                words.width(),
            )
        });
        let subtitle = subtitle.or_else(|| {
            let text = content.subtitle.as_deref().filter(|_| geometry.subtitle)?;
            Some(draw::subtitle_layout(
                &self.pango_context(),
                &geometry,
                text,
                words.width(),
            ))
        });
        areas.push(
            draw::card(&geometry, primary, rtl, &title, subtitle.as_ref()).offset_r(offset, 0.0),
        );
        Some(
            areas
                .into_iter()
                .map(|area| area.offset_r(cell.x(), cell.y()))
                .collect(),
        )
    }

    /// Where the name of the item at `position` is drawn, in the view's
    /// coordinates: the box its words are laid out in, as tall as the lines
    /// a name may take.
    #[must_use]
    pub fn name_rect(&self, position: u32) -> Option<graphene::Rect> {
        let imp = self.imp();
        let cell = self.item_rect(position)?;
        let geometry = imp.geometry.borrow();
        let rtl = self.direction() == gtk::TextDirection::Rtl;
        let (offset, size) = self.primary_of(position, &geometry, (cell.width(), cell.height()));
        let words = draw::text_box(&geometry, size, rtl);
        let height = match geometry.composition {
            Composition::Stacked => {
                imp.line_heights.get().0 * f32::from(geometry.title_lines.max(1))
            }
            Composition::Inline => words.height(),
        };
        Some(graphene::Rect::new(
            cell.x() + offset + words.x(),
            cell.y() + words.y(),
            words.width(),
            height,
        ))
    }

    /// Lay `editor` over the name of the item at `position`, in place of the
    /// name, until [`Self::end_editing`]. It follows the item through
    /// scrolling, and through a change to the model that moves it.
    pub fn begin_editing(&self, position: u32, editor: &impl IsA<gtk::Widget>) {
        self.end_editing();
        let Some(item) = self.imp().model().and_then(|model| model.item(position)) else {
            return;
        };
        editor.set_parent(self);
        self.imp()
            .editing
            .replace(Some((Cell::new(position), item, editor.clone().upcast())));
        self.refresh(position..position + 1);
        self.queue_allocate();
    }

    /// Take the editor away and draw the name again.
    pub fn end_editing(&self) {
        if let Some((position, _, editor)) = self.imp().editing.take() {
            editor.unparent();
            let position = position.get();
            self.refresh(position..position + 1);
        }
    }

    /// Put the editor over its item's name, or out of sight while the item is.
    fn place_editor(&self) {
        let Some((cached, item, editor)) = self
            .imp()
            .editing
            .borrow()
            .as_ref()
            .map(|(at, item, editor)| (at.get(), item.clone(), editor.clone()))
        else {
            return;
        };
        // The item may have moved since: find it again.
        let model = self.imp().model();
        let position = match &model {
            Some(model) if model.item(cached).as_ref() == Some(&item) => Some(cached),
            Some(model) => (0..model.n_items()).find(|at| model.item(*at).as_ref() == Some(&item)),
            None => None,
        };
        if let Some(position) = position
            && position != cached
            && let Some((at, _, _)) = self.imp().editing.borrow().as_ref()
        {
            at.set(position);
            self.refresh(position..position + 1);
        }
        let Some(rect) = position.and_then(|position| self.name_rect(position)) else {
            editor.set_child_visible(false);
            return;
        };
        editor.set_child_visible(true);
        let inline = self.imp().geometry.borrow().composition == Composition::Inline;
        // A little wider than the words, so the text inside the editor's own
        // padding starts where the name did.
        let (grow, width) = (6.0_f32, rect.width() + 12.0);
        let (_, natural, _, _) = editor.measure(gtk::Orientation::Vertical, width as i32);
        let line = self.imp().line_heights.get().0;
        let y = if inline {
            rect.y() + (rect.height() - natural as f32) / 2.0
        } else {
            // Centred on the first line, and growing downwards from there: an
            // editor that wraps a long name must not climb over the icon.
            rect.y() + ((line - natural as f32) / 2.0).max(-4.0)
        };
        editor.size_allocate(
            &gtk::Allocation::new(
                (rect.x() - grow).round() as i32,
                y.round() as i32,
                width.round() as i32,
                natural,
            ),
            -1,
        );
    }

    /// The items on screen.
    #[must_use]
    pub fn visible_range(&self) -> Range<u32> {
        let imp = self.imp();
        imp.sections
            .borrow()
            .visible_range(&imp.metrics.borrow().clone(), imp.scroll())
    }

    /// Give each run of items a heading ([`ItemPresenter::section_title`]),
    /// when the model has sections. In a grid every run starts a line of its
    /// own; a column flow has no headings.
    pub fn set_sections(&self, sectioned: bool) {
        let imp = self.imp();
        if imp.sectioned.replace(sectioned) != sectioned {
            imp.find_sections();
        }
    }

    /// Ask the presenter again about `positions`, e.g. after a thumbnail arrived.
    pub fn refresh(&self, positions: Range<u32>) {
        self.imp().refresh(positions);
    }

    /// Ask the presenter again about every item.
    pub fn refresh_all(&self) {
        self.imp().refresh_all();
    }

    /// Run `f` with the position of an item activated: double click (or single
    /// with [`Self::set_single_click_activate`]), Enter, spring-loaded drop.
    pub fn connect_activate<F: Fn(&Self, u32) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "activate",
            false,
            glib::closure_local!(move |view: &Self, position: u32| f(view, position)),
        )
    }

    /// Run `f` when a context menu is asked for at `(x, y)`, on an item or on
    /// empty space (`gtk::INVALID_LIST_POSITION`).
    pub fn connect_context_menu<F: Fn(&Self, u32, f64, f64) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "context-menu",
            false,
            glib::closure_local!(move |view: &Self, position: u32, x: f64, y: f64| f(
                view, position, x, y
            )),
        )
    }

    /// Run `f` when an item is middle-clicked.
    pub fn connect_middle_click<F: Fn(&Self, u32) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "middle-click",
            false,
            glib::closure_local!(move |view: &Self, position: u32| f(view, position)),
        )
    }

    /// Run `f` when the pointer enters an item (its rectangle in view
    /// coordinates) or leaves them all (`None`).
    pub fn connect_hover<F: Fn(&Self, Option<(u32, graphene::Rect)>) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "hover",
            false,
            glib::closure_local!(move |view: &Self,
                                       position: u32,
                                       x: f64,
                                       y: f64,
                                       w: f64,
                                       h: f64| {
                let hovered = (position != gtk::INVALID_LIST_POSITION).then(|| {
                    (
                        position,
                        graphene::Rect::new(x as f32, y as f32, w as f32, h as f32),
                    )
                });
                f(view, hovered);
            }),
        )
    }

    /// Run `f` with the new first position and count when the items on screen change.
    pub fn connect_visible_range_changed<F: Fn(&Self, Range<u32>) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "visible-range-changed",
            false,
            glib::closure_local!(move |view: &Self, first: u32, count: u32| f(
                view,
                first..first + count
            )),
        )
    }

    /// Run `f` on a press on a [`crate::ColumnShows::Stars`] or
    /// [`crate::ColumnShows::Button`] cell: the item's position, the column's
    /// id, and the star pressed (1 to 5; 0 for a button).
    pub fn connect_cell_pressed<F: Fn(&Self, u32, u32, u32) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "cell-pressed",
            false,
            glib::closure_local!(move |view: &Self, position: u32, column: u32, value: u32| {
                f(view, position, column, value);
            }),
        )
    }

    /// Run `f` when an item's own check is pressed ([`crate::TickMode::Check`]).
    pub fn connect_check_toggled<F: Fn(&Self, u32) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "check-toggled",
            false,
            glib::closure_local!(move |view: &Self, position: u32| f(view, position)),
        )
    }
}

/// A tree row's depth, and whether it is expanded when it can be.
pub(crate) fn tree_of(item: &glib::Object) -> Option<(u16, Option<bool>)> {
    let row = item.downcast_ref::<gtk::TreeListRow>()?;
    let depth = u16::try_from(row.depth()).unwrap_or(u16::MAX);
    Some((depth, row.is_expandable().then(|| row.is_expanded())))
}

/// What the presenter is asked about: the item a tree row holds, else the
/// model's item itself.
pub(crate) fn presented(item: &glib::Object) -> glib::Object {
    item.downcast_ref::<gtk::TreeListRow>()
        .and_then(gtk::TreeListRow::item)
        .unwrap_or_else(|| item.clone())
}
