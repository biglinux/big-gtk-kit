// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! One tooltip look for every BigLinux program.
//!
//! Call [`install`] once, after GTK is initialised and before the first window
//! is realized. From then on a widget's own `tooltip-text` or `tooltip-markup`
//! — set in code, by a builder, in a relm4 `view!` or in a `.ui` file — shows as
//! the BigLinux card instead of GTK's tooltip window. Call sites do not change,
//! and the accessible name and description GTK derives from the tooltip stay.
//!
//! Each window and popover gets one card, made the first time something in it
//! is hovered, rather than a popover per widget. A hovered widget answers
//! GTK's `query-tooltip` with "none" from then on, so the two never show
//! together; `has-tooltip` stays the program's to turn off. A widget whose
//! tooltip changes across it (drawn items) uses [`set_area_tips`]; one that
//! still answers `query-tooltip` itself keeps GTK's tooltip window.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, glib, graphene};

const SHOW_DELAY: Duration = Duration::from_millis(150);
/// Reaching another widget this soon after a card hid shows its card at once.
const BROWSE_WINDOW: Duration = Duration::from_millis(300);
const MAX_WIDTH_CHARS: i32 = 80;
const CARD_CLASS: &str = "big-tooltip";

// qdata keys. `set_data`/`data`/`steal_data` are unsafe because the store is
// untyped; each key below is written and read with one type only, named here,
// and only by this module.
/// `Rc<Engine>` on a window or popover.
const ENGINE_KEY: &str = "big-gtk-kit-tooltip-engine";
/// `bool` on a widget whose GTK tooltip this module answers for.
const SILENCED_KEY: &str = "big-gtk-kit-tooltip-silenced";
/// `String` on a widget whose card shows an icon tile.
const ICON_KEY: &str = "big-gtk-kit-tooltip-icon";
/// `Rc<AreaTips>` on a widget whose tooltip changes across it.
const AREA_KEY: &str = "big-gtk-kit-tooltip-area";

/// The tip at a point of a widget, in its own coordinates: the text and the
/// area of the widget it covers.
type AreaTips = dyn Fn(&gtk::Widget, f64, f64) -> Option<(String, gdk::Rectangle)>;
/// A widget's own tip at the pointer, when [`set_area_tips`] gave it one.
type Area = Option<(String, gdk::Rectangle)>;

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    static SHOW_WHEN_INACTIVE: Cell<bool> = const { Cell::new(false) };
    static FOLLOW_POINTER: Cell<bool> = const { Cell::new(false) };
    /// Every engine, for [`hide_all`].
    static ENGINES: RefCell<Vec<Weak<Engine>>> = const { RefCell::new(Vec::new()) };
}

/// Show every tooltip of this process as the BigLinux card.
///
/// Idempotent. Windows realized before the call are covered too.
pub fn install() {
    if INSTALLED.replace(true) {
        return;
    }
    if let Some(display) = gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        // An application provider answers `@media` from its own properties,
        // not from the user's settings as the theme does.
        let settings = gtk::Settings::for_display(&display);
        for (setting, preference) in [
            ("gtk-interface-color-scheme", "prefers-color-scheme"),
            ("gtk-interface-contrast", "prefers-contrast"),
            ("gtk-interface-reduced-motion", "prefers-reduced-motion"),
        ] {
            // GTK 4.20 added them; an older one keeps the light card.
            if provider.find_property(preference).is_none() {
                continue;
            }
            settings
                .bind_property(setting, &provider, preference)
                .sync_create()
                .build();
        }
        provider.load_from_string(include_str!("tooltip.css"));
        // Process-wide on purpose: the card is the same in every window.
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 100,
        );
    }
    // An engine for every window and popover as it is realized.
    crate::on_every_emission("realize", |widget: gtk::Widget| {
        if widget.is::<gtk::Native>() {
            attach(&widget);
        }
    });
    for window in gtk::Window::list_toplevels() {
        if window.is_realized() {
            attach(&window);
        }
    }
}

/// Put an icon tile beside `widget`'s tooltip text; `None` removes it.
pub fn set_icon(widget: &impl IsA<gtk::Widget>, icon_name: Option<&str>) {
    let widget = widget.as_ref();
    // SAFETY: ICON_KEY holds a `String` (see its definition).
    unsafe {
        match icon_name.filter(|name| !name.is_empty()) {
            Some(name) => widget.set_data(ICON_KEY, name.to_owned()),
            None => drop(widget.steal_data::<String>(ICON_KEY)),
        }
    }
}

/// Give `widget` tooltips that change across it, as a view that draws its
/// items does: `tips` answers for a point in the widget's coordinates with the
/// text and the area it covers, or `None` where nothing is said. It receives the
/// widget, so it need not hold it. Replaces answering GTK's `query-tooltip`,
/// whose window this module does not draw.
pub fn set_area_tips(
    widget: &impl IsA<gtk::Widget>,
    tips: impl Fn(&gtk::Widget, f64, f64) -> Option<(String, gdk::Rectangle)> + 'static,
) {
    let tips: Rc<AreaTips> = Rc::new(tips);
    // SAFETY: AREA_KEY holds an `Rc<AreaTips>` (see its definition).
    unsafe { widget.as_ref().set_data(AREA_KEY, tips) };
}

/// Show cards while the window is not focused, as a panel needs: it is never
/// the active window. Off by default, matching GTK.
pub fn set_show_when_inactive(show: bool) {
    SHOW_WHEN_INACTIVE.set(show);
}

/// Anchor cards at the pointer's column instead of the whole widget, and move
/// them along as it moves: a panel's wide buttons read better that way. Off by
/// default.
pub fn set_follow_pointer(follow: bool) {
    FOLLOW_POINTER.set(follow);
}

/// Close every card now, as a press would: something no input event announced
/// (a popup, a window preview) is about to take its place.
pub fn hide_all() {
    ENGINES.with_borrow(|engines| {
        for engine in engines.iter().filter_map(Weak::upgrade) {
            engine.dismiss();
        }
    });
}

fn attach(native: &gtk::Widget) {
    // The card is a popover too; it has nothing of its own to show.
    if native.has_css_class(CARD_CLASS) || engine(native).is_some() {
        return;
    }
    let engine = Rc::new(Engine {
        native: native.downgrade(),
        card: RefCell::default(),
        owner: RefCell::default(),
        area: RefCell::default(),
        owner_handlers: RefCell::default(),
        timer: RefCell::default(),
        shown: Cell::new(false),
        hidden_at: Cell::new(None),
        pressed: Cell::new(false),
        pointer: Cell::default(),
        controllers: RefCell::default(),
        handlers: RefCell::default(),
    });

    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&engine);
    motion.connect_enter(move |_, x, y| hovered(&weak, x, y));
    let weak = Rc::downgrade(&engine);
    motion.connect_motion(move |_, x, y| hovered(&weak, x, y));
    let weak = Rc::downgrade(&engine);
    motion.connect_leave(move |_| {
        if let Some(engine) = weak.upgrade() {
            engine.hide();
        }
    });
    // A click, a key, a scroll or a touch closes the card, as GTK's does.
    let input = gtk::EventControllerLegacy::new();
    input.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&engine);
    input.connect_event(move |_, event| {
        if let Some(engine) = weak.upgrade()
            && closes_card(event.event_type())
        {
            engine.dismiss();
        }
        glib::Propagation::Proceed
    });
    for controller in [motion.upcast(), input.upcast::<gtk::EventController>()] {
        native.add_controller(controller.clone());
        engine.controllers.borrow_mut().push(controller);
    }
    let mut handlers = vec![native.connect_unrealize(detach)];
    if let Some(window) = native.downcast_ref::<gtk::Window>() {
        let weak = Rc::downgrade(&engine);
        handlers.push(window.connect_is_active_notify(move |window| {
            if !window.is_active()
                && !SHOW_WHEN_INACTIVE.get()
                && let Some(engine) = weak.upgrade()
            {
                engine.hide();
            }
        }));
    }
    engine.handlers.replace(handlers);
    ENGINES.with_borrow_mut(|engines| {
        engines.retain(|engine| engine.strong_count() > 0);
        engines.push(Rc::downgrade(&engine));
    });
    // SAFETY: ENGINE_KEY holds an `Rc<Engine>` (see its definition).
    unsafe { native.set_data(ENGINE_KEY, engine) };
}

/// Undo [`attach`]; the next realize attaches again.
fn detach(native: &gtk::Widget) {
    // SAFETY: ENGINE_KEY holds an `Rc<Engine>` (see its definition).
    let Some(engine) = (unsafe { native.steal_data::<Rc<Engine>>(ENGINE_KEY) }) else {
        return;
    };
    engine.hide();
    if let Some(card) = engine.card.take() {
        card.popover.unparent();
    }
    for controller in engine.controllers.take() {
        native.remove_controller(&controller);
    }
    for handler in engine.handlers.take() {
        native.disconnect(handler);
    }
}

fn engine(native: &gtk::Widget) -> Option<Rc<Engine>> {
    // SAFETY: ENGINE_KEY holds an `Rc<Engine>` (see its definition).
    unsafe {
        native
            .data::<Rc<Engine>>(ENGINE_KEY)
            .map(|engine| engine.as_ref().clone())
    }
}

fn closes_card(event: gdk::EventType) -> bool {
    matches!(
        event,
        gdk::EventType::ButtonPress
            | gdk::EventType::KeyPress
            | gdk::EventType::Scroll
            | gdk::EventType::TouchBegin
    )
}

fn hovered(engine: &Weak<Engine>, x: f64, y: f64) {
    if let Some(engine) = engine.upgrade() {
        engine.hovered(x, y);
    }
}

struct Engine {
    native: glib::WeakRef<gtk::Widget>,
    card: RefCell<Option<Card>>,
    owner: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    /// The owner's tip under the pointer, for a widget with [`set_area_tips`].
    area: RefCell<Area>,
    owner_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    timer: RefCell<Option<glib::SourceId>>,
    shown: Cell<bool>,
    hidden_at: Cell<Option<Instant>>,
    /// Pressed on the current owner: no card until the pointer moves to another.
    pressed: Cell<bool>,
    /// Last pointer position, in the native's coordinates.
    pointer: Cell<(f64, f64)>,
    controllers: RefCell<Vec<gtk::EventController>>,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

struct Card {
    popover: gtk::Popover,
    icon: gtk::Image,
    label: gtk::Label,
}

impl Engine {
    fn hovered(self: &Rc<Self>, x: f64, y: f64) {
        let Some(native) = self.native.upgrade() else {
            return;
        };
        self.pointer.set((x, y));
        let (owner, area) = tip_owner(&native, x, y).unzip();
        let area = area.flatten();
        if let Some(owner) = &owner
            && area.is_none()
        {
            // Before GTK's own hover timer can fire for it.
            silence(owner);
        }
        if owner == self.owner() && area == *self.area.borrow() {
            if FOLLOW_POINTER.get()
                && self.shown.get()
                && let Some(owner) = &owner
                && let Some(area) = self.anchor(&native, owner)
                && let Some(card) = self.card.borrow().as_ref()
                // GTK presents the popup again on every call, moved or not.
                && card.popover.pointing_to().1 != area
            {
                card.popover.set_pointing_to(Some(&area));
            }
            return;
        }
        let browsing = self.shown.get()
            || self
                .hidden_at
                .get()
                .is_some_and(|at| at.elapsed() < BROWSE_WINDOW);
        if owner.is_some() {
            // A card on screen moves to the next owner: popping it down and up
            // again would make the compositor a new surface for each one.
            self.release();
        } else {
            self.hide();
        }
        self.pressed.set(false);
        let Some(owner) = owner else {
            return;
        };
        self.owner.replace(Some(owner.downgrade()));
        self.area.replace(area);
        if browsing {
            self.show(true);
            return;
        }
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(SHOW_DELAY, move || {
            if let Some(engine) = weak.upgrade() {
                // Ran: nothing left to remove.
                engine.timer.take();
                engine.show(false);
            }
        });
        self.timer.replace(Some(timer));
    }

    fn owner(&self) -> Option<gtk::Widget> {
        self.owner
            .borrow()
            .as_ref()
            .and_then(glib::WeakRef::upgrade)
    }

    fn show(self: &Rc<Self>, instant: bool) {
        if !self.present(instant) {
            self.popdown();
        }
    }

    /// Fill and point the card for the owner; false when it shows nothing.
    fn present(self: &Rc<Self>, instant: bool) -> bool {
        let (Some(native), Some(owner)) = (self.native.upgrade(), self.owner()) else {
            return false;
        };
        if self.pressed.get()
            || !owner.is_mapped()
            || menu_open(&owner)
            || !(SHOW_WHEN_INACTIVE.get() || active(&native))
        {
            return false;
        }
        let Some(area) = self.anchor(&native, &owner) else {
            return false;
        };
        let card = self.card(&native);
        let tip = self.area.borrow().clone();
        if !fill(&card, &owner, tip.as_ref()) {
            return false;
        }
        card.popover.set_pointing_to(Some(&area));
        card.popover.remove_css_class("visible");
        if instant {
            card.popover.add_css_class("instant");
        } else {
            card.popover.remove_css_class("instant");
        }
        card.popover.popup();
        card.popover.add_css_class("visible");
        self.shown.set(true);
        if tip.is_some() {
            return true;
        }

        // The text may change while the card shows (a play button becoming
        // pause): follow it.
        let mut handlers = self.owner_handlers.borrow_mut();
        for property in ["tooltip-text", "tooltip-markup"] {
            let weak = Rc::downgrade(self);
            handlers.push(owner.connect_notify_local(Some(property), move |owner, _| {
                let Some(engine) = weak.upgrade() else {
                    return;
                };
                let emptied = engine
                    .card
                    .borrow()
                    .as_ref()
                    .is_some_and(|card| !fill(card, owner, None));
                if emptied {
                    engine.hide();
                }
            }));
        }
        true
    }

    /// Where the card points: the owner or its tip's area, or with
    /// [`set_follow_pointer`] the pointer's column over its full height, so the
    /// card still pops above the whole of it or flips below it.
    fn anchor(&self, native: &gtk::Widget, owner: &gtk::Widget) -> Option<gdk::Rectangle> {
        let (x, y, width, height) = match self.area.borrow().as_ref() {
            Some((_, area)) => {
                let origin = graphene::Point::new(area.x() as f32, area.y() as f32);
                let origin = owner.compute_point(native, &origin)?;
                (
                    origin.x() as i32,
                    origin.y() as i32,
                    area.width(),
                    area.height(),
                )
            }
            None => {
                let bounds = owner.compute_bounds(native)?;
                let (x, y) = (bounds.x() as i32, bounds.y() as i32);
                (x, y, bounds.width() as i32, bounds.height() as i32)
            }
        };
        if !FOLLOW_POINTER.get() {
            return Some(gdk::Rectangle::new(x, y, width, height));
        }
        let column = (self.pointer.get().0 as i32).clamp(x, x + width.max(1) - 1);
        Some(gdk::Rectangle::new(column, y, 1, height))
    }

    fn card(&self, native: &gtk::Widget) -> std::cell::Ref<'_, Card> {
        if self.card.borrow().is_none() {
            let popover = gtk::Popover::builder()
                .has_arrow(false)
                .autohide(false)
                .can_target(false)
                .can_focus(false)
                // GTK moves it below when there is no room above.
                .position(gtk::PositionType::Top)
                .css_classes([CARD_CLASS])
                .build();

            // The owner's own name and description already carry the text.
            // A role, not the hidden state: GTK resets that on every show.
            let icon = gtk::Image::builder()
                .accessible_role(gtk::AccessibleRole::None)
                .pixel_size(34)
                .valign(gtk::Align::Center)
                .css_classes(["big-tooltip-icon"])
                .build();
            // Expanding: a wrapping label in a box is otherwise given its
            // minimum width and leaves the card half empty.
            let label = gtk::Label::builder()
                .accessible_role(gtk::AccessibleRole::None)
                .max_width_chars(MAX_WIDTH_CHARS)
                .hexpand(true)
                .build();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&icon);
            row.append(&label);
            popover.set_child(Some(&row));
            popover.set_parent(native);
            self.card.replace(Some(Card {
                popover,
                icon,
                label,
            }));
        }
        std::cell::Ref::map(self.card.borrow(), |card| {
            card.as_ref().expect("made above")
        })
    }

    /// Close the card for good on its owner: a press acted on it.
    fn dismiss(&self) {
        self.cancel_timer();
        self.pressed.set(true);
        self.popdown();
        // The next owner waits its delay, as after any action.
        self.hidden_at.set(None);
    }

    fn hide(&self) {
        self.release();
        self.popdown();
    }

    /// Let go of the owner, leaving the card as it is.
    fn release(&self) {
        self.cancel_timer();
        self.area.take();
        if let Some(owner) = self.owner.take().and_then(|owner| owner.upgrade()) {
            for handler in self.owner_handlers.take() {
                owner.disconnect(handler);
            }
        } else {
            self.owner_handlers.take();
        }
    }

    fn popdown(&self) {
        if self.shown.replace(false) {
            if let Some(card) = self.card.borrow().as_ref() {
                card.popover.popdown();
            }
            self.hidden_at.set(Some(Instant::now()));
        }
    }

    fn cancel_timer(&self) {
        if let Some(timer) = self.timer.take() {
            crate::source::remove(timer);
        }
    }
}

/// The widget under `(x, y)` whose tooltip shows there: the nearest with a
/// tooltip text or a tip there, as GTK picks it (insensitive widgets explain
/// themselves too).
fn tip_owner(native: &gtk::Widget, x: f64, y: f64) -> Option<(gtk::Widget, Area)> {
    let point = graphene::Point::new(x as f32, y as f32);
    let mut widget = native.pick(x, y, gtk::PickFlags::INSENSITIVE);
    while let Some(candidate) = widget {
        if &candidate == native {
            return None;
        }
        if let Some(tips) = area_tips(&candidate)
            && let Some(at) = native.compute_point(&candidate, &point)
            && let Some(tip) = tips(&candidate, f64::from(at.x()), f64::from(at.y()))
        {
            return Some((candidate, Some(tip)));
        }
        if has_tip(&candidate) {
            return Some((candidate, None));
        }
        widget = candidate.parent();
    }
    None
}

fn area_tips(widget: &gtk::Widget) -> Option<Rc<AreaTips>> {
    // SAFETY: AREA_KEY holds an `Rc<AreaTips>` (see its definition).
    unsafe {
        widget
            .data::<Rc<AreaTips>>(AREA_KEY)
            .map(|tips| tips.as_ref().clone())
    }
}

fn has_tip(widget: &gtk::Widget) -> bool {
    // A widget its program turned `has-tooltip` off on keeps it off.
    widget.has_tooltip()
        && widget
            .tooltip_markup()
            .or_else(|| widget.tooltip_text())
            .is_some_and(|text| !text.is_empty())
}

/// Answer GTK's tooltip query for `widget` with "no tooltip", once per
/// widget: the card shows instead. Stopping the emission skips GTK's default
/// handler, which would answer with the text.
fn silence(widget: &gtk::Widget) {
    // SAFETY: SILENCED_KEY holds a `bool` (see its definition).
    if unsafe { widget.data::<bool>(SILENCED_KEY).is_some() } {
        return;
    }
    // SAFETY: as above.
    unsafe { widget.set_data(SILENCED_KEY, true) };
    widget.connect_query_tooltip(|widget, _, _, _, _| {
        if has_tip(widget) {
            widget.stop_signal_emission_by_name("query-tooltip");
        }
        false
    });
}

/// Put `owner`'s tooltip, or its tip under the pointer, into the card; false
/// when it has none any more.
fn fill(card: &Card, owner: &gtk::Widget, area: Option<&(String, gdk::Rectangle)>) -> bool {
    // Cards with structure (an icon, bold or small lines) read from the left
    // and keep their lines; a plain sentence is centred and wraps. Plain lines
    // of their own (a list of choices) wrap too, but read from the left.
    let (left, wrap, icon) = match area {
        Some((text, _)) => {
            card.label.set_text(text);
            (text.contains('\n'), true, None)
        }
        None => {
            let markup = owner.tooltip_markup();
            let Some(text) = markup
                .clone()
                .or_else(|| owner.tooltip_text())
                .filter(|text| !text.is_empty())
            else {
                return false;
            };
            if markup.is_some() {
                card.label.set_markup(&text);
            } else {
                card.label.set_text(&text);
            }
            // SAFETY: ICON_KEY holds a `String` (see its definition).
            let icon = unsafe {
                owner
                    .data::<String>(ICON_KEY)
                    .map(|icon| icon.as_ref().clone())
            };
            let structured = icon.is_some() || markup.as_deref().is_some_and(is_structured);
            (structured || text.contains('\n'), !structured, icon)
        }
    };
    card.icon.set_visible(icon.is_some());
    card.icon.set_icon_name(icon.as_deref());
    card.label.set_wrap(wrap);
    card.label.set_xalign(if left { 0.0 } else { 0.5 });
    card.label.set_justify(if left {
        gtk::Justification::Left
    } else {
        gtk::Justification::Center
    });
    true
}

/// GTK keeps an escaped copy of plain text as the markup too; only real markup
/// has a tag.
fn is_structured(markup: &str) -> bool {
    markup.contains('<')
}

fn active(native: &gtk::Widget) -> bool {
    native
        .root()
        .and_downcast::<gtk::Window>()
        .is_none_or(|window| window.is_active())
}

/// A control whose own menu or popover is open shows no card over it.
fn menu_open(owner: &gtk::Widget) -> bool {
    if let Some(button) = owner.downcast_ref::<gtk::MenuButton>() {
        return button.is_active();
    }
    let mut child = owner.first_child();
    while let Some(widget) = child {
        if widget.is::<gtk::Popover>() && widget.is_visible() {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{pump, watch};

    /// Needs a display: run under `tools/headless-gate.sh`.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn closed_windows_release_their_card_and_owner() {
        gtk::init().expect("a display");
        install();
        set_show_when_inactive(true);
        let (windows, cards, owners) = (
            Rc::new(Cell::new(0)),
            Rc::new(Cell::new(0)),
            Rc::new(Cell::new(0)),
        );
        const CYCLES: u32 = 25;
        for _ in 0..CYCLES {
            let owner = gtk::Button::with_label("Save");
            owner.set_tooltip_text(Some("Save the document"));
            let window = gtk::Window::builder().child(&owner).build();
            window.present();
            pump();
            let engine = engine(window.upcast_ref()).expect("attached at realize");
            engine
                .owner
                .replace(Some(owner.upcast_ref::<gtk::Widget>().downgrade()));
            engine.show(false);
            pump();
            let card = engine
                .card
                .borrow()
                .as_ref()
                .map(|card| card.popover.clone());
            let card = card.expect("shown");
            assert!(card.is_visible());
            watch(&window, &windows);
            watch(&card, &cards);
            watch(&owner, &owners);
            drop((engine, card, owner));
            window.destroy();
            drop(window);
            pump();
        }
        assert_eq!(
            (windows.get(), cards.get(), owners.get()),
            (CYCLES, CYCLES, CYCLES)
        );
    }

    #[test]
    fn only_real_markup_is_structured() {
        assert!(is_structured("<b>Volume</b>\n<small>50 %</small>"));
        assert!(!is_structured("Fish &amp; chips"));
        assert!(!is_structured("1 &lt; 2"));
    }
}
