// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The widget.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use crate::a11y;
use crate::draw;
use crate::icons::{IconKey, IconState, Icons};
use crate::layout::{self, Flow, Metrics};
use crate::palette::{Palette, Probes};
use crate::{Composition, Geometry, IconFrame, IconSource, ItemContent, ItemPresenter, TickMode};

/// Room around a section heading's words: above, and below them.
const HEADING_AIR: f32 = 16.0;
const HEADING_BELOW: f32 = 4.0;

/// The narrowest a list with columns asks to be when it scrolls sideways.
const COLUMNS_MIN_WIDTH: f32 = 240.0;

/// Past this many items the drawing cache starts evicting the least recently
/// drawn; a screenful is a few hundred.
const MIN_CACHE: usize = 256;
/// How far a summed width ([`crate::text_width::CharWidths`]) may fall
/// short of the laid-out one: a name within this of its box is laid out to
/// tell whether it fits on one line.
const SUMMED_SHORT_BY: f32 = 2.0;

/// One item's cached drawing.
struct Entry {
    /// Held so the pointer the entry is keyed by cannot be reused meanwhile.
    _object: glib::Object,
    content: ItemContent,
    icon: Option<IconKey>,
    title: Option<pango::Layout>,
    subtitle: Option<pango::Layout>,
    /// The other columns' words, one per column.
    cells: Vec<Option<pango::Layout>>,
    /// The font serials, text width and row width the layouts were made for.
    layouts_for: (u32, u32, i32, i32),
    node: Option<gsk::RenderNode>,
    /// State, palette revision, geometry revision and size the node was drawn for.
    node_for: (draw::State, u64, u64, (i32, i32)),
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<usize, Entry>,
    tick: u64,
}

impl Cache {
    fn evict(&mut self, keep: usize) {
        if self.entries.len() <= keep {
            return;
        }
        let mut ages: Vec<(u64, usize)> = self.entries.iter().map(|(k, e)| (e.used, *k)).collect();
        ages.sort_unstable();
        for (_, key) in ages.into_iter().take(self.entries.len() - keep) {
            self.entries.remove(&key);
        }
    }
}

/// A press deferred to its release: what it does depends on whether the pointer
/// moved (a drag keeps the selection it started from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    SelectOnly(u32),
    Toggle(u32),
}

mod imp {
    use super::*;

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
        probes: RefCell<Option<Probes>>,
        palette: RefCell<Palette>,
        palette_rev: Cell<u64>,
        fonts: Cell<(u32, u32)>,
        pub(super) line_heights: Cell<(f32, f32)>,
        pub(super) icons: RefCell<Icons>,
        pub(super) icon_loader: Cell<Option<crate::IconLoader>>,
        a11y: RefCell<a11y::Window>,
        last_range: Cell<(u32, u32)>,
        range_idle: RefCell<Option<glib::SourceId>>,
        scratch: RefCell<ItemContent>,
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
        natural_widths: RefCell<Option<std::rc::Rc<[f32]>>>,
        /// How wide each item's name is on one line, for the grid's rows to
        /// be as tall as their longest name needs. Made when the model, the
        /// fonts or the geometry change. Widths only: the few names too wide
        /// for one line are asked of the presenter again when laid out, and a
        /// copy of every name per open view was most of what this cost.
        titles: RefCell<Option<std::rc::Rc<[f32]>>>,
        /// The widths of the characters names and subtitles are written
        /// with, in the title's and the caption's fonts: kept across changes
        /// of the model, since only the fonts make them wrong.
        title_chars: RefCell<crate::text_width::CharWidths>,
        caption_chars: RefCell<crate::text_width::CharWidths>,
        /// How many lines each name takes at the width it was last laid out
        /// in: that width, and the counts.
        title_lines: RefCell<Option<(i32, std::rc::Rc<[u8]>)>>,
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
                a11y: RefCell::new(a11y::Window::new(gtk::AccessibleRole::GridCell)),
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
            // its `item_rect`) is placed by its parent's allocation.
            let mut child = obj.first_child();
            while let Some(widget) = child {
                if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
                    popover.present();
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
            if self.line_width.replace(pitch_x) != pitch_x
                && !self.geometry.borrow().columns.is_empty()
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
            self.parent_first_accessible_child()
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
            self.a11y.borrow_mut().clear();
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

        /// Draw heading `index`: the presenter's words for the run under it.
        fn draw_heading(
            &self,
            snapshot: &gtk::Snapshot,
            sections: &layout::Sections,
            metrics: &Metrics,
            index: usize,
            scroll: (f32, f32),
        ) {
            let (Some(rect), Some(model), Some(presenter)) = (
                sections.heading(metrics, index),
                self.model(),
                self.presenter.borrow().clone(),
            ) else {
                return;
            };
            let start = sections.starts[index];
            let Some(title) = model
                .item(start)
                .and_then(|item| presenter.section_title(start, &crate::view::presented(&item)))
            else {
                return;
            };
            let probes = self.probes.borrow();
            let Some(probes) = probes.as_ref() else {
                return;
            };
            let layout = pango::Layout::new(&probes.title_context());
            let mut font = probes
                .title_context()
                .font_description()
                .unwrap_or_default();
            font.set_weight(pango::Weight::Bold);
            layout.set_font_description(Some(&font));
            layout.set_text(&title);
            let (_, logical) = layout.pixel_extents();
            let geometry = self.geometry.borrow();
            let x = (geometry.margin.1 + geometry.padding) as f32;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                rect.x + x - scroll.0,
                rect.y + rect.height - logical.height() as f32 - HEADING_BELOW - scroll.1,
            ));
            snapshot.append_layout(
                &layout,
                &self.palette.borrow().get(crate::palette::Role::Text),
            );
            snapshot.restore();
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
            self.describe_window(true);
            self.last_range.set((u32::MAX, 0));
            self.find_sections();
            self.obj().queue_resize();
        }

        fn selection_changed(&self) {
            self.describe_window(true);
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

        /// Drops what the items measured of themselves; it is stale.
        fn forget_measures(&self) {
            self.natural_widths.replace(None);
            self.titles.replace(None);
            self.title_lines.replace(None);
        }

        /// `metrics` with each row as tall as its tallest item, in an icon
        /// grid whose names may take several lines.
        fn fitted_rows(&self, metrics: Metrics) -> Metrics {
            let geometry = self.geometry.borrow().clone();
            // Under headings every line is the cell's height: the runs are
            // laid out by `layout::Sections`, which does not fit rows.
            if !matches!(geometry.flow, Flow::RowMajor { .. })
                || geometry.composition != Composition::Stacked
                || geometry.title_lines < 2
                || !self.sections.borrow().starts.is_empty()
            {
                return metrics;
            }
            let (pitch_x, pitch_y) = metrics.cell_size();
            let words = draw::text_box(&geometry, (pitch_x, pitch_y), false).width();
            let Some(lines) = self.lines_at(&geometry, words) else {
                return metrics;
            };
            let (title, caption) = self.line_heights.get();
            // Tallest first: the height of every count of lines, once.
            let heights: Vec<f32> = (1..=geometry.title_lines)
                .map(|count| {
                    let mut shape = geometry.clone();
                    shape.title_lines = count;
                    draw::cell_height(&shape, title, caption)
                })
                .collect();
            metrics.fit_rows(|position| {
                let count = lines
                    .get(position as usize)
                    .copied()
                    .unwrap_or(geometry.title_lines);
                heights[usize::from(count.clamp(1, geometry.title_lines)) - 1]
            })
        }

        /// How many lines each name takes in words `width` pixels wide.
        ///
        /// A name narrower than the width on one line is one line, which is
        /// most names; only the others are laid out at the width, so a window
        /// being resized pays for its long names rather than for all of them.
        fn lines_at(&self, geometry: &Geometry, width: f32) -> Option<std::rc::Rc<[u8]>> {
            let key = width.round() as i32;
            if let Some((at, lines)) = self.title_lines.borrow().as_ref()
                && *at == key
            {
                return Some(lines.clone());
            }
            let titles = self.titles()?;
            let (model, presenter) = (self.model()?, self.presenter.borrow().clone()?);
            let context = self
                .probes
                .borrow()
                .as_ref()
                .map_or_else(|| self.obj().pango_context(), Probes::title_context);
            let lines: std::rc::Rc<[u8]> = (0_u32..)
                .zip(titles.iter())
                .map(|(position, single)| {
                    if *single + SUMMED_SHORT_BY <= width {
                        return 1;
                    }
                    let Some(item) = model.item(position) else {
                        return 1;
                    };
                    let title = presenter.title(position, &crate::view::presented(&item));
                    let layout = draw::title_layout(&context, geometry, &title, width);
                    u8::try_from(layout.line_count()).unwrap_or(u8::MAX)
                })
                .collect();
            self.title_lines.replace(Some((key, lines.clone())));
            Some(lines)
        }

        /// How wide each item's name is on one line.
        fn titles(&self) -> Option<std::rc::Rc<[f32]>> {
            if let Some(titles) = self.titles.borrow().clone() {
                return Some(titles);
            }
            let (model, presenter) = (self.model()?, self.presenter.borrow().clone()?);
            let context = self
                .probes
                .borrow()
                .as_ref()
                .map_or_else(|| self.obj().pango_context(), Probes::title_context);
            let layout = pango::Layout::new(&context);
            let mut widths = self.title_chars.borrow_mut();
            let titles: std::rc::Rc<[f32]> = (0..model.n_items())
                .map(|position| {
                    model.item(position).map_or(0.0, |item| {
                        widths.measure(
                            &layout,
                            &presenter.title(position, &crate::view::presented(&item)),
                        )
                    })
                })
                .collect();
            drop(widths);
            self.titles.replace(Some(titles.clone()));
            Some(titles)
        }

        /// `metrics` with each column as wide as its widest item, in a column
        /// flow; as it is otherwise. Never wider than the viewport, where a
        /// name that long is ellipsized anyway.
        fn fitted(&self, metrics: Metrics) -> Metrics {
            let metrics = self.fitted_rows(metrics);
            if self.geometry.borrow().flow != Flow::ColumnMajor {
                return metrics;
            }
            let Some(widths) = self.natural_widths() else {
                return metrics;
            };
            let widest = self.obj().width() as f32;
            metrics.fit_columns(|position| {
                let width = widths.get(position as usize).copied().unwrap_or(1.0);
                if widest > 0.0 {
                    width.min(widest)
                } else {
                    width
                }
            })
        }

        /// How wide each item would be with its whole name on one line.
        fn natural_widths(&self) -> Option<std::rc::Rc<[f32]>> {
            if let Some(widths) = self.natural_widths.borrow().clone() {
                return Some(widths);
            }
            let (model, presenter) = (self.model()?, self.presenter.borrow().clone()?);
            let probes = self.probes.borrow();
            let (title_context, caption_context) = match probes.as_ref() {
                Some(probes) => (probes.title_context(), probes.caption_context()),
                None => (self.obj().pango_context(), self.obj().pango_context()),
            };
            drop(probes);
            let geometry = self.geometry.borrow().clone();
            let (picture, _) = geometry.picture();
            // The words' box starts after the margin, the padding, the picture
            // and the gap, and ends a padding and a margin before the cell does
            // (`draw::text_box`); a pixel more for the rounding of each.
            let around = 2.0 * geometry.margin.1 as f32
                + 2.0 * geometry.padding as f32
                + picture
                + geometry.gap as f32
                + 2.0;
            let title_layout = pango::Layout::new(&title_context);
            let caption_layout = pango::Layout::new(&caption_context);
            let mut title_chars = self.title_chars.borrow_mut();
            let mut caption_chars = self.caption_chars.borrow_mut();
            let widths: std::rc::Rc<[f32]> = (0..model.n_items())
                .map(|position| {
                    let Some(item) = model.item(position) else {
                        return 1.0;
                    };
                    let item = crate::view::presented(&item);
                    let title =
                        title_chars.measure(&title_layout, &presenter.title(position, &item));
                    let subtitle = if geometry.subtitle {
                        presenter
                            .subtitle(position, &item)
                            .map_or(0.0, |text| caption_chars.measure(&caption_layout, &text))
                    } else {
                        0.0
                    };
                    around + title.max(subtitle)
                })
                .collect();
            drop((title_chars, caption_chars));
            self.natural_widths.replace(Some(widths.clone()));
            Some(widths)
        }

        pub(crate) fn cell_size(&self) -> (f32, f32) {
            let geometry = self.geometry.borrow();
            let (title, caption) = self.line_heights.get();
            // A list with columns is as wide as its columns.
            let width = if geometry.columns.is_empty() {
                geometry.cell_width
            } else {
                geometry
                    .columns
                    .iter()
                    .map(|column| column.width)
                    .sum::<i32>()
                    + 2 * geometry.margin.1
            };
            (
                width.max(1) as f32,
                draw::cell_height(&geometry, title, caption),
            )
        }

        pub(crate) fn state_of(&self, position: u32) -> draw::State {
            let obj = self.obj();
            let focused = obj.has_focus();
            draw::State {
                hovered: self.hover.get() == Some(position),
                selected: self
                    .model()
                    .is_some_and(|model| model.is_selected(position)),
                cursor: self.cursor.get() == Some(position),
                focus_visible: focused
                    && (self.keyboard_cursor.get()
                        || obj.state_flags().contains(gtk::StateFlags::FOCUS_VISIBLE)),
                view_focused: focused,
                drop_target: self.drop_target.get() == Some(position),
                striped: self.stripes.get() && position % 2 == 1,
                editing: self
                    .editing
                    .borrow()
                    .as_ref()
                    .is_some_and(|(edited, _, _)| edited.get() == position),
                tree: self
                    .model()
                    .and_then(|model| model.item(position))
                    .and_then(|item| tree_of(&item)),
            }
        }

        /// The item's node, from the cache or drawn now.
        fn item_node(&self, position: u32, size: (f32, f32)) -> Option<gsk::RenderNode> {
            let model = self.model()?;
            let item = model.item(position)?;
            let key = item.as_ptr() as usize;
            let state = self.state_of(position);
            let size_key = (size.0.round() as i32, size.1.round() as i32);
            let node_for = (
                state,
                self.palette_rev.get(),
                self.geometry_rev.get(),
                size_key,
            );

            // 1. Content, asked once per item and kept.
            let icon_source = {
                let mut cache = self.cache.borrow_mut();
                cache.tick += 1;
                let tick = cache.tick;
                let entry = match cache.entries.entry(key) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(vacant) => {
                        let presenter = self.presenter.borrow().clone()?;
                        let mut scratch = self.scratch.borrow_mut();
                        *scratch = ItemContent::default();
                        presenter.content(position, &crate::view::presented(&item), &mut scratch);
                        vacant.insert(Entry {
                            _object: item.clone(),
                            content: scratch.clone(),
                            icon: None,
                            title: None,
                            subtitle: None,
                            cells: Vec::new(),
                            layouts_for: (0, 0, 0, 0),
                            node: None,
                            node_for,
                            used: tick,
                        })
                    }
                };
                entry.used = tick;
                if entry.node_for == node_for && entry.node.is_some() {
                    return entry.node.clone();
                }
                entry.content.icon.clone()
            };

            // 2. The picture: may arrive right now, and it re-enters `icon_arrived`.
            let obj = self.obj();
            let scale = obj.scale_factor().max(1);
            let icon_size = self.geometry.borrow().icon_size;
            let (icon_key, icon_state) = match &icon_source {
                IconSource::Gicon(gicon) => {
                    let key = Icons::key(gicon, icon_size, scale);
                    (Some(key.clone()), self.icon(gicon, key))
                }
                _ => (None, None),
            };

            // 3. Words and drawing.
            let mut cache = self.cache.borrow_mut();
            let entry = cache.entries.get_mut(&key)?;
            entry.icon = icon_key;
            let geometry = self.geometry.borrow();
            let rtl = obj.direction() == gtk::TextDirection::Rtl;
            let outer = draw::primary_span(&geometry, size, rtl);
            let primary = match state.tree {
                Some((depth, _)) => draw::tree_span(outer, depth, rtl),
                None => outer,
            };
            let words = draw::text_box(&geometry, primary.1, rtl);
            let fonts = self.fonts.get();
            let layouts_for = (
                fonts.0,
                fonts.1,
                words.width().round() as i32,
                size.0.round() as i32,
            );
            if entry.layouts_for != layouts_for || entry.title.is_none() {
                let probes = self.probes.borrow();
                let probes = probes.as_ref()?;
                // A mark at the end of the name keeps its room.
                let trailing = entry
                    .content
                    .badges
                    .iter()
                    .any(|badge| badge.corner == crate::Corner::Trailing);
                let title_width = if trailing {
                    (words.width() - draw::TRAILING_BADGE - geometry.gap as f32).max(1.0)
                } else {
                    words.width()
                };
                entry.title = Some(draw::title_layout(
                    &probes.title_context(),
                    &geometry,
                    &entry.content.title,
                    title_width,
                ));
                entry.cells = geometry
                    .columns
                    .iter()
                    .zip(draw::column_spans(&geometry, size.0, rtl))
                    .map(|(column, (_, width))| match column.shows {
                        crate::ColumnShows::Primary
                        | crate::ColumnShows::Stars(_)
                        | crate::ColumnShows::Button(_) => None,
                        crate::ColumnShows::Cell(index) => Some(draw::cell_layout(
                            &probes.title_context(),
                            &geometry,
                            entry.content.cells.get(index).map_or("", String::as_str),
                            width,
                        )),
                    })
                    .collect();
                // Only where the shape keeps a line for it: a caption drawn in
                // a cell with no room for one lands on the next row.
                entry.subtitle = entry
                    .content
                    .subtitle
                    .as_deref()
                    .filter(|_| geometry.subtitle)
                    .map(|text| {
                        draw::subtitle_layout(
                            &probes.caption_context(),
                            &geometry,
                            text,
                            words.width(),
                        )
                    });
                entry.layouts_for = layouts_for;
            }
            let picture = match (&entry.content.icon, &icon_state) {
                (IconSource::Texture(texture), _) => draw::Picture::Texture(texture),
                (_, Some(IconState::Texture(texture))) => draw::Picture::Texture(texture),
                (_, Some(IconState::Paintable(paintable))) => draw::Picture::Paintable(paintable),
                (IconSource::Gicon(_), _) => draw::Picture::Placeholder,
                (IconSource::None, _) if entry.content.frame == IconFrame::Cover => {
                    draw::Picture::Placeholder
                }
                _ => draw::Picture::None,
            };
            let palette = self.palette.borrow();
            let display = obj.display();
            let node = draw::item_node(&draw::Item {
                size,
                primary,
                outer,
                cells: &entry.cells,
                geometry: &geometry,
                content: &entry.content,
                title: entry.title.as_ref()?,
                subtitle: entry.subtitle.as_ref(),
                picture,
                palette: &palette,
                state,
                rtl,
                display: &display,
                scale,
            });
            entry.node = node.clone();
            entry.node_for = node_for;
            drop(geometry);
            drop(palette);
            let keep = (self
                .last_range
                .get()
                .1
                .saturating_sub(self.last_range.get().0) as usize
                * 2)
            .max(MIN_CACHE);
            cache.evict(keep);
            node
        }

        /// What is known of `gicon` at this size, asking for it the first time.
        fn icon(&self, gicon: &gtk::gio::Icon, key: IconKey) -> Option<IconState> {
            if let Some(state) = self.icons.borrow().get(&key) {
                return Some(state);
            }
            if !self.icons.borrow_mut().begin(key.clone()) {
                return self.icons.borrow().get(&key);
            }
            let obj = self.obj();
            let (size, scale) = (key.1, key.2);
            let view = obj.downgrade();
            let wanted = key.clone();
            let fallback = gicon.clone();
            let deliver = move |texture: Option<gdk::Texture>| {
                let Some(view) = view.upgrade() else {
                    return;
                };
                let state = match texture {
                    Some(texture) => IconState::Texture(texture),
                    None => IconState::Paintable(crate::icons::paintable(
                        &view.display(),
                        &fallback,
                        size,
                        scale,
                        view.direction() == gtk::TextDirection::Rtl,
                    )),
                };
                view.imp().icon_arrived(wanted, state);
            };
            match self.icon_loader.get() {
                Some(load) => load(&obj.display(), gicon, size, scale, Box::new(deliver)),
                None => deliver(None),
            }
            self.icons.borrow().get(&key)
        }

        /// Whether `item`, as last drawn, has no button. Only drawn items
        /// are pressed.
        pub(crate) fn hides_button(&self, item: &glib::Object) -> bool {
            self.cache
                .borrow()
                .entries
                .get(&(item.as_ptr() as usize))
                .is_some_and(|entry| entry.content.hides_button)
        }

        fn icon_arrived(&self, key: IconKey, state: IconState) {
            self.icons.borrow_mut().settle(key.clone(), state);
            // Only the items drawing this icon are drawn again. The cache may be
            // borrowed when a cached icon answers at once, inside `item_node`;
            // that item picks the icon up itself.
            if let Ok(mut cache) = self.cache.try_borrow_mut() {
                for entry in cache.entries.values_mut() {
                    if entry.icon.as_ref() == Some(&key) {
                        entry.node = None;
                    }
                }
            }
            self.obj().queue_draw();
        }

        /// Forget what `positions` showed, so the presenter is asked again.
        pub(crate) fn refresh(&self, positions: Range<u32>) {
            let Some(model) = self.model() else {
                return;
            };
            {
                let mut cache = self.cache.borrow_mut();
                for position in positions.clone() {
                    if let Some(item) = model.item(position) {
                        cache.entries.remove(&(item.as_ptr() as usize));
                    }
                }
            }
            let window = self.a11y.borrow().range();
            if positions.start < window.end && window.start < positions.end {
                self.describe_window(true);
            }
            self.obj().queue_draw();
        }

        pub(crate) fn refresh_all(&self) {
            self.cache.borrow_mut().entries.clear();
            self.describe_window(true);
            self.obj().queue_draw();
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
            let stale = self
                .a11y
                .borrow_mut()
                .cover(self.obj().upcast_ref(), window);
            self.describe(&stale);
            self.sync_focus();
            self.obj().emit_by_name::<()>(
                "visible-range-changed",
                &[&range.start, &(range.end - range.start)],
            );
        }

        fn describe_window(&self, all: bool) {
            let positions: Vec<u32> = if all {
                self.a11y.borrow().range().collect()
            } else {
                Vec::new()
            };
            self.describe(&positions);
        }

        /// Put what the presenter says about `positions` on their proxies.
        fn describe(&self, positions: &[u32]) {
            let (Some(model), Some(presenter)) = (self.model(), self.presenter.borrow().clone())
            else {
                return;
            };
            let n = model.n_items();
            let mut content = ItemContent::default();
            for &position in positions {
                let (Some(item), Some(proxy)) = (
                    model.item(position),
                    self.a11y.borrow().get(position).cloned(),
                ) else {
                    continue;
                };
                content = ItemContent::default();
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
            drop(content);
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
}

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

    /// Whether only what an item draws is the item, and the rest of its cell
    /// the view's background (`Geometry::hit_drawn_only`).
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
