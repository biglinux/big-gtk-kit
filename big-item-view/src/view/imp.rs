// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The subclass: its state, the GTK virtual methods, the model and the
//! accessible window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::a11y;
use crate::draw;
use crate::icons::Icons;
use crate::layout::{self, Flow, Metrics};
use crate::palette::{Palette, Probes};
use crate::{Geometry, ItemContent, ItemPresenter};

use super::drawing::Cache;
use super::{HEADING_AIR, Pending, tree_of};

/// The narrowest a list with columns asks to be when it scrolls sideways.
const COLUMNS_MIN_WIDTH: f32 = 240.0;

#[derive(glib::Properties)]
#[properties(wrapper_type = super::ItemView)]
pub struct ItemView {
    #[property(type = Option<gtk::Adjustment>, override_interface = gtk::Scrollable, get = Self::hadjustment, set = Self::set_hadjustment)]
    pub(super) hadjustment: RefCell<Option<(gtk::Adjustment, glib::SignalHandlerId)>>,
    #[property(type = Option<gtk::Adjustment>, override_interface = gtk::Scrollable, get = Self::vadjustment, set = Self::set_vadjustment)]
    pub(super) vadjustment: RefCell<Option<(gtk::Adjustment, glib::SignalHandlerId)>>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    pub(super) hscroll_policy: Cell<gtk::ScrollablePolicy>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    pub(super) vscroll_policy: Cell<gtk::ScrollablePolicy>,

    pub(crate) presenter: RefCell<Option<Rc<dyn ItemPresenter>>>,
    pub(crate) geometry: RefCell<Geometry>,
    pub(super) geometry_rev: Cell<u64>,
    pub(super) model: RefCell<
        Option<(
            gtk::SelectionModel,
            glib::SignalHandlerId,
            glib::SignalHandlerId,
        )>,
    >,
    pub(crate) metrics: RefCell<Metrics>,
    pub(crate) cursor: Cell<Option<u32>>,
    /// Whether the keyboard put the cursor where it is, which is what the
    /// ring shows. GTK's own focus-visible flag is not enough: it lapses a
    /// few seconds after the last key the window itself saw, and a popover
    /// consumer that walks the arrows in a capture handler never lets the
    /// window see one.
    pub(crate) keyboard_cursor: Cell<bool>,
    pub(super) cursor_item: glib::WeakRef<glib::Object>,
    pub(crate) anchor: Cell<Option<u32>>,
    pub(crate) hover: Cell<Option<u32>>,
    pub(crate) drop_target: Cell<Option<u32>>,
    pub(crate) band: Cell<Option<layout::Rect>>,
    pub(crate) pending: Cell<Option<Pending>>,
    pub(crate) single_click: Cell<bool>,
    /// Every other row shaded, for a long list of rows read across.
    pub(crate) stripes: Cell<bool>,
    pub(crate) rubberband: Cell<bool>,
    pub(super) cache: RefCell<Cache>,
    pub(super) probes: RefCell<Option<Probes>>,
    pub(super) palette: RefCell<Palette>,
    pub(super) palette_rev: Cell<u64>,
    pub(super) fonts: Cell<(u32, u32)>,
    pub(super) line_heights: Cell<(f32, f32)>,
    pub(super) icons: RefCell<Icons>,
    pub(super) icon_loader: Cell<Option<crate::IconLoader>>,
    pub(super) a11y: RefCell<a11y::Window>,
    pub(super) last_range: Cell<(u32, u32)>,
    range_idle: RefCell<Option<glib::SourceId>>,
    pub(super) scratch: RefCell<ItemContent>,
    pub(crate) drop_installed: Cell<bool>,
    /// The width a line was last laid out at, for the column headings.
    line_width: Cell<f32>,
    /// Whether runs of items get a heading, and where the runs are.
    pub(crate) sectioned: Cell<bool>,
    pub(crate) sections: RefCell<layout::Sections>,
    sections_handler: RefCell<Option<glib::SignalHandlerId>>,
    /// The item whose name is being edited (where it was last seen, and
    /// the item itself, which a change to the model may move) and the
    /// editor laid over it.
    pub(super) editing: RefCell<Option<(Cell<u32>, glib::Object, gtk::Widget)>>,
    /// How wide each item would like to be, by position, in a column flow:
    /// what its columns are fitted to. Made when first needed after the
    /// model, the fonts or the geometry change, not at every allocation.
    pub(super) natural_widths: RefCell<Option<Rc<[f32]>>>,
    /// How wide each item's name is on one line, for the grid's rows to
    /// be as tall as their longest name needs. Made when the model, the
    /// fonts or the geometry change. Widths only: the few names too wide
    /// for one line are asked of the presenter again when laid out, and a
    /// copy of every name per open view was most of what this cost.
    pub(super) titles: RefCell<Option<Rc<[f32]>>>,
    /// The widths of the characters names and subtitles are written
    /// with, in the title's and the caption's fonts: kept across changes
    /// of the model, since only the fonts make them wrong.
    pub(super) title_chars: RefCell<crate::text_width::CharWidths>,
    pub(super) caption_chars: RefCell<crate::text_width::CharWidths>,
    /// How many lines each name takes at the width it was last laid out
    /// in: that width, and the counts.
    pub(super) title_lines: RefCell<Option<(i32, Rc<[u8]>)>>,
    /// The controller `input::install_wheel` put on the enclosing
    /// scrolled window.
    pub(crate) wheel: RefCell<Option<gtk::EventControllerScroll>>,
}

impl Default for ItemView {
    fn default() -> Self {
        Self {
            hadjustment: RefCell::new(None),
            vadjustment: RefCell::new(None),
            hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            presenter: RefCell::new(None),
            geometry: RefCell::new(Geometry::grid(112, 48)),
            geometry_rev: Cell::new(0),
            model: RefCell::new(None),
            metrics: RefCell::new(Metrics::default()),
            cursor: Cell::new(None),
            keyboard_cursor: Cell::new(false),
            cursor_item: glib::WeakRef::new(),
            anchor: Cell::new(None),
            hover: Cell::new(None),
            drop_target: Cell::new(None),
            band: Cell::new(None),
            pending: Cell::new(None),
            single_click: Cell::new(false),
            stripes: Cell::new(false),
            rubberband: Cell::new(true),
            cache: RefCell::new(Cache::default()),
            probes: RefCell::new(None),
            palette: RefCell::new(Palette::default()),
            palette_rev: Cell::new(0),
            fonts: Cell::new((0, 0)),
            line_heights: Cell::new((17.0, 15.0)),
            icons: RefCell::new(Icons::default()),
            icon_loader: Cell::new(None),
            a11y: RefCell::default(),
            last_range: Cell::new((u32::MAX, 0)),
            range_idle: RefCell::new(None),
            scratch: RefCell::new(ItemContent::default()),
            drop_installed: Cell::new(false),
            line_width: Cell::new(0.0),
            sectioned: Cell::new(false),
            sections: RefCell::new(layout::Sections::default()),
            sections_handler: RefCell::new(None),
            editing: RefCell::new(None),
            natural_widths: RefCell::new(None),
            titles: RefCell::new(None),
            title_chars: RefCell::default(),
            caption_chars: RefCell::default(),
            title_lines: RefCell::new(None),
            wheel: RefCell::new(None),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for ItemView {
    const NAME: &'static str = "BigItemView";
    // Every hosted module links its own copy of this crate, and GTypes are
    // per process: the second module to draw a view would register the name
    // again and abort the host. Each copy takes the next free name instead.
    const ALLOW_NAME_CONFLICT: bool = true;
    type Type = super::ItemView;
    type ParentType = gtk::Widget;
    type Interfaces = (gtk::Scrollable, gtk::Accessible);

    fn class_init(klass: &mut Self::Class) {
        klass.set_css_name("itemview");
        klass.set_accessible_role(gtk::AccessibleRole::Grid);
    }
}

#[glib::derived_properties]
impl ObjectImpl for ItemView {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.add_css_class("view");
        obj.set_focusable(true);
        // GTK sets `has-focus` after the focus controllers and the state
        // flags have seen the focus arrive, so the cursor item can only
        // take the focus from here.
        obj.connect_has_focus_notify(|view| view.imp().sync_focus());
        obj.set_overflow(gtk::Overflow::Hidden);
        // Before the probes are ever styled: installed later, at realize, the
        // first styling happened without them and the colours never changed.
        if let Some(display) = gdk::Display::default() {
            crate::palette::install_defaults(&display);
        }
        // The probes wait for the view to be shown: a view kept in a stack
        // for another mode (bigfiles keeps three per tab) never pays for them.
        crate::input::install(&obj);
    }

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            use glib::subclass::Signal;
            let u = u32::static_type();
            let f = f64::static_type();
            vec![
                Signal::builder("activate").param_types([u]).build(),
                Signal::builder("context-menu")
                    .param_types([u, f, f])
                    .build(),
                Signal::builder("middle-click").param_types([u]).build(),
                Signal::builder("hover")
                    .param_types([u, f, f, f, f])
                    .build(),
                Signal::builder("visible-range-changed")
                    .param_types([u, u])
                    .build(),
                Signal::builder("check-toggled").param_types([u]).build(),
                Signal::builder("cell-pressed")
                    .param_types([u, u, u])
                    .build(),
                Signal::builder("geometry-changed").build(),
            ]
        })
    }

    fn dispose(&self) {
        self.disconnect_model();
        if let Some(probes) = self.probes.take() {
            probes.unparent();
        }
        while let Some(child) = self.obj().first_child() {
            child.unparent();
        }
        if let Some(source) = self.range_idle.take() {
            big_gtk_kit::source::remove(source);
        }
        self.presenter.replace(None);
        self.cache.borrow_mut().entries.clear();
        self.a11y.borrow_mut().clear();
    }
}

impl WidgetImpl for ItemView {
    fn realize(&self) {
        self.parent_realize();
        crate::palette::install_defaults(&self.obj().display());
    }

    fn map(&self) {
        if self.probes.borrow().is_none() {
            self.probes.replace(Some(Probes::new(&*self.obj())));
            // The fonts the probes carry may differ from the guess measured
            // before (`read_fonts`).
            self.obj().queue_resize();
        }
        self.parent_map();
        crate::input::install_wheel(&self.obj());
    }

    fn unroot(&self) {
        crate::input::remove_wheel(&self.obj());
        self.parent_unroot();
    }

    fn unmap(&self) {
        self.parent_unmap();
        // Nothing to keep for a view nobody sees: the next frame draws the
        // screenful again from the model.
        self.cache.borrow_mut().entries.clear();
        self.icons.borrow_mut().clear();
    }

    fn request_mode(&self) -> gtk::SizeRequestMode {
        match self.geometry.borrow().flow {
            Flow::RowMajor { .. } => gtk::SizeRequestMode::HeightForWidth,
            Flow::ColumnMajor => gtk::SizeRequestMode::WidthForHeight,
        }
    }

    fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
        self.read_fonts();
        let (cell_w, cell_h) = self.cell_size();
        let n = self.n_items();
        let flow = self.geometry.borrow().flow;
        let scrolls_here = match orientation {
            gtk::Orientation::Horizontal => self.hadjustment.borrow().is_some(),
            _ => self.vadjustment.borrow().is_some(),
        };
        let (min, nat) = match (flow, orientation) {
            (
                Flow::RowMajor {
                    min_columns,
                    max_columns,
                },
                gtk::Orientation::Horizontal,
            ) => {
                // Columns wider than the room scroll sideways rather than
                // asking the window to grow.
                let columns = !self.geometry.borrow().columns.is_empty();
                let min = if columns && scrolls_here {
                    cell_w.min(COLUMNS_MIN_WIDTH)
                } else {
                    cell_w * min_columns.max(1) as f32
                };
                let nat = cell_w * max_columns.min(n.max(1)).max(min_columns.max(1)) as f32;
                (min, nat)
            }
            (Flow::RowMajor { .. }, _) => {
                let width = if for_size > 0 {
                    for_size as f32
                } else {
                    cell_w
                };
                let metrics = self.fitted(Metrics::compute(
                    flow,
                    (cell_w, cell_h),
                    n,
                    (width, 0.0),
                    false,
                ));
                let full = self.sections.borrow().content_size(&metrics).1;
                (if scrolls_here { cell_h.min(full) } else { full }, full)
            }
            (Flow::ColumnMajor, gtk::Orientation::Vertical) => {
                (cell_h, cell_h * n.clamp(1, 20) as f32)
            }
            (Flow::ColumnMajor, _) => {
                let height = if for_size > 0 {
                    for_size as f32
                } else {
                    cell_h
                };
                let metrics = self.fitted(Metrics::compute(
                    flow,
                    (cell_w, cell_h),
                    n,
                    (0.0, height),
                    false,
                ));
                let full = metrics.content_size().0;
                (if scrolls_here { cell_w.min(full) } else { full }, full)
            }
        };
        (min.ceil() as i32, nat.ceil().max(min.ceil()) as i32, -1, -1)
    }

    fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
        if let Some(probes) = self.probes.borrow().as_ref() {
            probes.allocate();
        }
        let obj = self.obj();
        // A popover a consumer parents here (renaming an item, pointing at
        // its `item_rect`) is placed by its parent's allocation. The item
        // proxies get a size, zero, so they stay mapped.
        let mut child = obj.first_child();
        while let Some(widget) = child {
            if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
                popover.present();
            } else if widget.is::<a11y::ItemProxy>() {
                widget.allocate(0, 0, -1, None);
            }
            child = widget.next_sibling();
        }
        self.read_fonts();
        let rtl = obj.direction() == gtk::TextDirection::Rtl;
        let metrics = self.fitted(Metrics::compute(
            self.geometry.borrow().flow,
            self.cell_size(),
            self.n_items(),
            (width as f32, height as f32),
            rtl,
        ));
        self.metrics.replace(metrics.clone());
        let (content_w, content_h) = self.sections.borrow().content_size(&metrics);
        let (pitch_x, pitch_y) = metrics.cell_size();
        // Headings over the columns move when a line changes width.
        if self.line_width.replace(pitch_x) != pitch_x && !self.geometry.borrow().columns.is_empty()
        {
            let obj = obj.clone();
            glib::idle_add_local_once(move || {
                obj.emit_by_name::<()>("geometry-changed", &[]);
            });
        }
        if let Some((adjustment, _)) = self.hadjustment.borrow().as_ref() {
            configure(adjustment, content_w, width as f32, pitch_x);
        }
        if let Some((adjustment, _)) = self.vadjustment.borrow().as_ref() {
            configure(adjustment, content_h, height as f32, pitch_y);
        }
        self.schedule_range_update();
        obj.place_editor();
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let obj = self.obj();
        self.refresh_palette();
        let metrics = self.metrics.borrow().clone();
        let scroll = self.scroll();
        let (width, height) = (obj.width() as f32, obj.height() as f32);
        snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, width, height));
        let sections = self.sections.borrow().clone();
        let range = sections.visible_range(&metrics, scroll);
        for index in sections.visible_headings(&metrics, scroll) {
            self.draw_heading(snapshot, &sections, &metrics, index, scroll);
        }
        for position in range.clone() {
            let Some(rect) = sections.rect(&metrics, position) else {
                continue;
            };
            let Some(node) = self.item_node(position, (rect.width, rect.height)) else {
                continue;
            };
            snapshot.save();
            snapshot.translate(&graphene::Point::new(rect.x - scroll.0, rect.y - scroll.1));
            snapshot.append_node(&node);
            snapshot.restore();
        }
        if let Some(band) = self.band.get() {
            let palette = self.palette.borrow();
            let rect = graphene::Rect::new(
                band.x - scroll.0,
                band.y - scroll.1,
                band.width,
                band.height,
            );
            snapshot.append_color(&palette.get(crate::palette::Role::RubberbandFill), &rect);
            let edge = palette.get(crate::palette::Role::RubberbandEdge);
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(rect, 0.0),
                &[1.0; 4],
                &[edge; 4],
            );
        }
        if let Some((_, _, editor)) = self.editing.borrow().as_ref() {
            obj.snapshot_child(editor, snapshot);
        }
        snapshot.pop();
        if let Some(probes) = self.probes.borrow().as_ref() {
            probes.snapshot(&*obj, snapshot);
        }
        if self.last_range.get() != (range.start, range.end) {
            self.schedule_range_update();
        }
    }

    fn state_flags_changed(&self, previous: &gtk::StateFlags) {
        self.parent_state_flags_changed(previous);
        let changed = self.obj().state_flags() ^ *previous;
        if changed.intersects(
            gtk::StateFlags::FOCUS_VISIBLE
                | gtk::StateFlags::FOCUS_WITHIN
                | gtk::StateFlags::FOCUSED,
        ) {
            self.obj().queue_draw();
        }
    }

    fn direction_changed(&self, previous: gtk::TextDirection) {
        self.parent_direction_changed(previous);
        self.cache.borrow_mut().entries.clear();
        self.obj().queue_allocate();
    }
}

impl ScrollableImpl for ItemView {}

impl AccessibleImpl for ItemView {
    fn first_accessible_child(&self) -> Option<gtk::Accessible> {
        if let Some(first) = self.a11y.borrow().first() {
            return Some(first.clone().upcast());
        }
        a11y::first_real_child(self.obj().upcast_ref()).map(Cast::upcast)
    }

    fn platform_state(&self, state: gtk::AccessiblePlatformState) -> bool {
        // While an item holds the cursor, it is what has the focus for
        // assistive technology, and the grid is where it sits.
        if state == gtk::AccessiblePlatformState::Focused && self.cursor_proxy().is_some() {
            return false;
        }
        self.parent_platform_state(state)
    }
}

fn configure(adjustment: &gtk::Adjustment, content: f32, page: f32, step: f32) {
    let upper = content.max(page);
    let value = adjustment
        .value()
        .clamp(0.0, f64::from((upper - page).max(0.0)));
    adjustment.configure(
        value,
        0.0,
        f64::from(upper),
        f64::from(step),
        f64::from(page * 0.9),
        f64::from(page),
    );
}

impl ItemView {
    fn hadjustment(&self) -> Option<gtk::Adjustment> {
        self.hadjustment
            .borrow()
            .as_ref()
            .map(|(adjustment, _)| adjustment.clone())
    }

    fn vadjustment(&self) -> Option<gtk::Adjustment> {
        self.vadjustment
            .borrow()
            .as_ref()
            .map(|(adjustment, _)| adjustment.clone())
    }

    fn set_hadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        self.replace_adjustment(&self.hadjustment, adjustment);
    }

    fn set_vadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        self.replace_adjustment(&self.vadjustment, adjustment);
    }

    fn replace_adjustment(
        &self,
        slot: &RefCell<Option<(gtk::Adjustment, glib::SignalHandlerId)>>,
        adjustment: Option<gtk::Adjustment>,
    ) {
        if let Some((old, handler)) = slot.take() {
            old.disconnect(handler);
        }
        if let Some(adjustment) = adjustment {
            let view = self.obj().downgrade();
            let handler = adjustment.connect_value_changed(move |_| {
                if let Some(view) = view.upgrade() {
                    view.queue_draw();
                    // An editor laid over a name moves with it.
                    if view.imp().editing.borrow().is_some() {
                        view.queue_allocate();
                    }
                }
            });
            slot.replace(Some((adjustment, handler)));
        }
        self.obj().queue_allocate();
    }

    pub(crate) fn scroll(&self) -> (f32, f32) {
        let value = |slot: &RefCell<Option<(gtk::Adjustment, glib::SignalHandlerId)>>| {
            slot.borrow()
                .as_ref()
                .map_or(0.0, |(adjustment, _)| adjustment.value() as f32)
        };
        (value(&self.hadjustment), value(&self.vadjustment))
    }

    pub(crate) fn adjustment_along(&self) -> Option<gtk::Adjustment> {
        match self.geometry.borrow().flow {
            Flow::RowMajor { .. } => self.vadjustment(),
            Flow::ColumnMajor => self.hadjustment(),
        }
    }

    pub(crate) fn model(&self) -> Option<gtk::SelectionModel> {
        self.model
            .borrow()
            .as_ref()
            .map(|(model, _, _)| model.clone())
    }

    pub(crate) fn n_items(&self) -> u32 {
        self.model().map_or(0, |model| model.n_items())
    }

    fn disconnect_model(&self) {
        if let Some((model, items, selection)) = self.model.take() {
            model.disconnect(items);
            model.disconnect(selection);
            if let Some(handler) = self.sections_handler.take() {
                model.disconnect(handler);
            }
        }
    }

    pub(super) fn set_model(&self, model: Option<gtk::SelectionModel>) {
        if self.model().as_ref() == model.as_ref() {
            return;
        }
        self.disconnect_model();
        self.cache.borrow_mut().entries.clear();
        self.forget_measures();
        self.cursor.set(None);
        self.cursor_item.set(None);
        self.anchor.set(None);
        self.hover.set(None);
        let gone = self.a11y.borrow_mut().clear();
        for proxy in gone {
            proxy.unparent();
        }
        self.last_range.set((u32::MAX, 0));
        if let Some(model) = model {
            let view = self.obj().downgrade();
            let items = model.connect_items_changed(move |_, position, removed, added| {
                if let Some(view) = view.upgrade() {
                    view.imp().items_changed(position, removed, added);
                }
            });
            let view = self.obj().downgrade();
            let selection = model.connect_selection_changed(move |_, _, _| {
                if let Some(view) = view.upgrade() {
                    view.imp().selection_changed();
                }
            });
            if let Some(sectioned) = model.dynamic_cast_ref::<gtk::SectionModel>() {
                let view = self.obj().downgrade();
                let handler = sectioned.connect_sections_changed(move |_, _, _| {
                    if let Some(view) = view.upgrade() {
                        view.imp().find_sections();
                    }
                });
                self.sections_handler.replace(Some(handler));
            }
            self.model.replace(Some((model, items, selection)));
        }
        self.find_sections();
        self.obj().queue_resize();
    }

    /// Where the runs of items start, when they get headings.
    pub(crate) fn find_sections(&self) {
        let mut starts = Vec::new();
        // A column flow reads down and across; a heading has no line there.
        let rows = matches!(self.geometry.borrow().flow, Flow::RowMajor { .. });
        if self.sectioned.get()
            && rows
            && let Some(model) = self.model()
            && let Some(sectioned) = model.dynamic_cast_ref::<gtk::SectionModel>()
        {
            let n = model.n_items();
            let mut position = 0;
            while position < n {
                let (start, end) = sectioned.section(position);
                starts.push(start);
                position = end.max(position + 1);
            }
        }
        let height = self.line_heights.get().0 + HEADING_AIR;
        let sections = layout::Sections { starts, height };
        if *self.sections.borrow() != sections {
            self.sections.replace(sections);
            self.last_range.set((u32::MAX, 0));
            self.obj().queue_resize();
        }
    }

    fn items_changed(&self, position: u32, removed: u32, added: u32) {
        self.forget_measures();
        // A reload replaces everything: the drawings of what left go with it.
        if removed > 0 && removed as usize >= self.cache.borrow().entries.len() {
            self.cache.borrow_mut().entries.clear();
        }
        let remap = |slot: &Cell<Option<u32>>| {
            slot.set(
                slot.get()
                    .and_then(|old| layout::remap(old, position, removed, added)),
            );
        };
        remap(&self.anchor);
        remap(&self.hover);
        remap(&self.drop_target);
        match self
            .cursor
            .get()
            .map(|old| layout::remap(old, position, removed, added))
        {
            Some(Some(new)) => self.cursor.set(Some(new)),
            Some(None) => {
                // A re-sort re-adds the same objects: find the one the cursor was on.
                let found = self.cursor_item.upgrade().and_then(|item| {
                    let model = self.model()?;
                    (position..position + added)
                        .find(|candidate| model.item(*candidate).as_ref() == Some(&item))
                });
                self.cursor.set(found);
            }
            None => {}
        }
        // The proxies move with their items rather than being thrown away:
        // a screen reader following the focused item still holds it after
        // a file arrives. Described again at once, since "57 of 1129" is
        // now another number for every one of them.
        self.a11y.borrow_mut().shift(
            self.obj().upcast_ref(),
            position,
            removed,
            added,
            self.n_items(),
        );
        self.describe_window();
        self.last_range.set((u32::MAX, 0));
        self.find_sections();
        self.obj().queue_resize();
    }

    fn selection_changed(&self) {
        self.describe_window();
        self.obj().queue_draw();
    }

    fn read_fonts(&self) {
        let probes = self.probes.borrow();
        // Before the view is first shown, its own font is the best guess.
        let (title, caption) = match probes.as_ref() {
            Some(probes) => (probes.title_context(), probes.caption_context()),
            None => (self.obj().pango_context(), self.obj().pango_context()),
        };
        let serials = (title.serial(), caption.serial());
        if serials != self.fonts.get() {
            self.fonts.set(serials);
            self.line_heights
                .set((draw::line_height(&title), draw::line_height(&caption)));
            self.cache.borrow_mut().entries.clear();
            self.title_chars.borrow_mut().clear();
            self.caption_chars.borrow_mut().clear();
            self.forget_measures();
        }
    }

    fn refresh_palette(&self) {
        let Some(palette) = self.probes.borrow().as_ref().map(Probes::read) else {
            return;
        };
        if *self.palette.borrow() != palette {
            self.palette.replace(palette);
            self.palette_rev.set(self.palette_rev.get().wrapping_add(1));
        }
    }

    pub(crate) fn set_geometry(&self, geometry: Geometry) {
        if *self.geometry.borrow() == geometry {
            return;
        }
        // The icons stay unless they are wanted at another size: a column
        // being resized redraws every frame, and must not flash placeholders.
        let same_pictures = self.geometry.borrow().picture() == geometry.picture();
        self.geometry.replace(geometry);
        self.forget_measures();
        self.geometry_rev
            .set(self.geometry_rev.get().wrapping_add(1));
        self.cache.borrow_mut().entries.clear();
        if !same_pictures {
            self.icons.borrow_mut().clear();
        }
        // Said again even when the positions on screen are the same: what
        // they need (a picture at the new size) is not.
        self.last_range.set((u32::MAX, 0));
        self.obj().queue_resize();
        self.obj().emit_by_name::<()>("geometry-changed", &[]);
    }

    fn schedule_range_update(&self) {
        if self.range_idle.borrow().is_some() {
            return;
        }
        let view = self.obj().downgrade();
        let source = glib::idle_add_local_once(move || {
            if let Some(view) = view.upgrade() {
                view.imp().range_idle.take();
                view.imp().update_range();
            }
        });
        self.range_idle.replace(Some(source));
    }

    /// The visible range settled: tell the consumer, and move the accessible
    /// window to it.
    fn update_range(&self) {
        let metrics = self.metrics.borrow().clone();
        let range = self
            .sections
            .borrow()
            .visible_range(&metrics, self.scroll());
        if self.last_range.get() == (range.start, range.end) {
            return;
        }
        self.last_range.set((range.start, range.end));
        // A window of a fixed size (the lines a screenful can touch, plus one
        // either side), so the proxies are replaced only when the view is
        // resized. Sized by what is visible, a scroll that shows one more
        // partial line would rebuild every proxy, and a screen reader holding
        // the old ones would find them gone.
        let lanes = metrics.items_per_line().max(1);
        let (pitch_x, pitch_y) = metrics.cell_size();
        let page_lines = match self.geometry.borrow().flow {
            Flow::RowMajor { .. } => (self.obj().height() as f32 / pitch_y).ceil() as u32,
            Flow::ColumnMajor => (self.obj().width() as f32 / pitch_x).ceil() as u32,
        };
        let capacity = (page_lines + 3)
            .saturating_mul(lanes)
            .min(metrics.n_items());
        let start = range
            .start
            .saturating_sub(lanes)
            .min(metrics.n_items().saturating_sub(capacity));
        let window = start..start + capacity;
        let obj = self.obj();
        let cover = self.a11y.borrow_mut().cover(window);
        for proxy in &cover.gone {
            proxy.unparent();
        }
        // Described before they are parented: a proxy not yet in the tree
        // tells assistive technology nothing until it is added.
        self.describe(&cover.stale);
        // First among the children: the real children's accessible siblings
        // follow the widget order and must not lead into the ring.
        for proxy in &cover.added {
            proxy.insert_after(&*obj, None::<&gtk::Widget>);
        }
        self.a11y.borrow().link(obj.upcast_ref());
        self.sync_focus();
        obj.emit_by_name::<()>(
            "visible-range-changed",
            &[&range.start, &(range.end - range.start)],
        );
    }

    /// Describe every proxy again.
    pub(super) fn describe_window(&self) {
        let positions: Vec<u32> = self.a11y.borrow().range().collect();
        self.describe(&positions);
    }

    /// Put what the presenter says about `positions` on their proxies.
    fn describe(&self, positions: &[u32]) {
        let (Some(model), Some(presenter)) = (self.model(), self.presenter.borrow().clone()) else {
            return;
        };
        let n = model.n_items();
        for &position in positions {
            let (Some(item), Some(proxy)) = (
                model.item(position),
                self.a11y.borrow().get(position).cloned(),
            ) else {
                continue;
            };
            let mut content = ItemContent::default();
            presenter.content(position, &crate::view::presented(&item), &mut content);
            a11y::describe(
                &proxy,
                position,
                &a11y::Facts {
                    label: content
                        .accessible_label
                        .as_deref()
                        .unwrap_or(&content.title),
                    description: content.accessible_description.as_deref(),
                    selected: model.is_selected(position),
                    checked: content.checked,
                    tree: tree_of(&item),
                    set_size: n,
                },
            );
        }
    }

    fn cursor_proxy(&self) -> Option<a11y::ItemProxy> {
        let cursor = self.cursor.get()?;
        self.a11y.borrow().get(cursor).cloned()
    }

    /// Tell assistive technology which item has the focus.
    pub(crate) fn sync_focus(&self) {
        let focused = self.obj().has_focus();
        let cursor = self.cursor.get();
        let window = self.a11y.borrow();
        for position in window.range() {
            if let Some(proxy) = window.get(position) {
                proxy.set_focused(focused && cursor == Some(position));
            }
        }
    }

    pub(crate) fn set_cursor(&self, position: Option<u32>) {
        self.cursor.set(position);
        let item = position.and_then(|position| self.model()?.item(position));
        self.cursor_item.set(item.as_ref());
        self.sync_focus();
        self.obj().queue_draw();
    }
}
