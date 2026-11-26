// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Option rows that show their value and open a card of choices beside them.
//!
//! A click or Enter on a row opens its card on the window's overlay, beside
//! the row and joined to it by a bridge, with the focus on the current choice.
//! A choice taken with a click or Enter closes the card and gives the focus
//! back to the row; so does Escape. A press outside the card and its row, or
//! the focus leaving the card, closes it. Nothing opens on hover: the pointer
//! crosses rows on its way elsewhere.
//!
//! The widgets' handlers only report [`CardEvent`]s; the owner passes them
//! back to [`SideCards::handle`], so no handler holds a widget. Callers pass
//! every string already translated.

use crate::widget_tree::descendants;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

/// The card's width; [`card`] holds its content to it.
pub const CARD_WIDTH: i32 = 340;
/// The space between the row and its card, crossed by the bridge, and the
/// card's least distance from the overlay's edges.
const GAP: i32 = 12;
/// The ring's distance from the edge and its width, as the CSS draws it.
const RING_INSET: i32 = 3;
const RING: i32 = 2;

const CSS: &str = "
/* Quieter than the title, as `dimmed` would make it, but still 4.5:1. */
.side-card-value {
    color: alpha(currentColor, 0.75);
}

/* libadwaita's ring on a row is half its accent, too faint to find on a
   light card. */
.side-card-row:focus-visible {
    outline: 2px solid var(--accent-color);
    outline-offset: -2px;
}

.side-card-row.card-open {
    background-color: var(--popover-bg-color);
}

.side-card-row.card-open,
.side-card {
    outline: 2px solid alpha(var(--accent-bg-color), 0.6);
    outline-offset: -5px;
}

.side-card-row.card-open .side-card-value {
    opacity: 0;
}

.side-card {
    background-color: var(--popover-bg-color);
    color: var(--popover-fg-color);
    border-radius: 12px;
    box-shadow:
        0 0 0 1px alpha(black, 0.10),
        8px 10px 24px -6px alpha(black, 0.28);
}

.side-card.joined-left {
    border-top-left-radius: 0;
}

.side-card.joined-right {
    border-top-right-radius: 0;
}

.side-card-content {
    padding: 12px;
}

.side-card-bridge {
    background-color: var(--popover-bg-color);
    box-shadow:
        inset 0 2px alpha(var(--accent-bg-color), 0.6),
        inset 0 -2px alpha(var(--accent-bg-color), 0.6);
}

/* A choice is a tile; the chosen one is tinted and outlined. The radio stays
   for screen readers and the keyboard, out of sight. */
checkbutton.side-card-tile {
    min-height: 24px;
    padding: 8px 12px;
    border-radius: 12px;
    box-shadow: inset 0 0 0 1px alpha(currentColor, 0.18);
}

checkbutton.side-card-tile > radio {
    opacity: 0;
    min-width: 0;
    min-height: 0;
    margin: 0;
    padding: 0;
    border: none;
    -gtk-icon-size: 0;
}

checkbutton.side-card-tile:hover {
    background-color: alpha(currentColor, 0.06);
}

checkbutton.side-card-tile:checked {
    background-color: alpha(var(--accent-bg-color), 0.12);
    box-shadow: inset 0 0 0 2px alpha(var(--accent-bg-color), 0.7);
}

checkbutton.side-card-tile:checked .side-card-tile-name {
    font-weight: bold;
}

checkbutton.side-card-tile:focus-visible {
    outline: 2px solid var(--accent-color);
    outline-offset: 2px;
}

checkbutton.side-card-tile:disabled .side-card-tile-name {
    opacity: 0.5;
}
";

/// What the rows and cards report to their owner, which passes it back to
/// [`SideCards::handle`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CardEvent {
    /// Row `n` was clicked or activated with the keyboard.
    Activated(usize),
    /// A choice in the open card was taken with a click or Enter.
    Chosen,
    /// Escape was pressed in the open card.
    Escape,
    /// The keyboard focus left the open card.
    FocusLeft,
    /// A press anywhere on the overlay, at these coordinates.
    Pressed(f64, f64),
    /// The rows moved under the card (their list scrolled).
    Close,
}

/// The rows, their cards and which card is open.
pub struct SideCards {
    layer: gtk::Overlay,
    rows: Vec<gtk::Widget>,
    cards: Vec<gtk::ScrolledWindow>,
    bridge: gtk::Box,
    open: Option<usize>,
}

impl SideCards {
    /// Puts `cards` on `layer`, card `n` belonging to `rows[n]`. A row reports
    /// [`CardEvent::Activated`] itself: a [`ValueRow`] in a `gtk::ListBox`
    /// does through `activated`.
    pub fn new(
        layer: &gtk::Overlay,
        rows: Vec<gtk::Widget>,
        cards: Vec<gtk::ScrolledWindow>,
        emit: impl Fn(CardEvent) + 'static,
    ) -> Self {
        install_css();
        let emit: Rc<dyn Fn(CardEvent)> = Rc::new(emit);
        for row in &rows {
            row.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
        }
        for card in &cards {
            card.set_visible(false);
            card.set_halign(gtk::Align::Start);
            card.set_valign(gtk::Align::Start);
            // Ahead of the focused tile, which takes Enter for itself.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let key = emit.clone();
            keys.connect_key_pressed(move |controller, pressed, _, _| {
                match pressed {
                    gdk::Key::Escape => key(CardEvent::Escape),
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                        let Some(tile) = controller
                            .widget()
                            .and_then(|card| card.root())
                            .and_then(|root| root.focus())
                            .and_downcast::<gtk::CheckButton>()
                            .filter(|tile| tile.has_css_class("side-card-tile"))
                        else {
                            return glib::Propagation::Proceed;
                        };
                        tile.set_active(true);
                        key(CardEvent::Chosen);
                    }
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            card.add_controller(keys);
            let focus = gtk::EventControllerFocus::new();
            let left = emit.clone();
            focus.connect_leave(move |_| left(CardEvent::FocusLeft));
            card.add_controller(focus);
            for tile in tiles(card.upcast_ref()) {
                // Ahead of the tile's own click, which then picks it; the event
                // comes back after it, through the owner.
                let click = gtk::GestureClick::new();
                click.set_propagation_phase(gtk::PropagationPhase::Capture);
                let chosen = emit.clone();
                click.connect_released(move |click, _, x, y| {
                    if click
                        .widget()
                        .is_some_and(|tile| tile.is_sensitive() && tile.contains(x, y))
                    {
                        chosen(CardEvent::Chosen);
                    }
                });
                tile.add_controller(click);
            }
            layer.add_overlay(card);
        }
        let bridge = gtk::Box::builder()
            .visible(false)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .can_target(false)
            .css_classes(["side-card-bridge"])
            .build();
        layer.add_overlay(&bridge);

        // Observes presses without claiming them, so the press still reaches
        // what is under it.
        let presses = gtk::GestureClick::new();
        presses.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pressed = emit.clone();
        presses.connect_pressed(move |_, _, x, y| pressed(CardEvent::Pressed(x, y)));
        layer.add_controller(presses);

        Self {
            layer: layer.clone(),
            rows,
            cards,
            bridge,
            open: None,
        }
    }

    /// The row whose card is open.
    pub fn open(&self) -> Option<usize> {
        self.open
    }

    /// Opens, closes or keeps the card that `event` concerns.
    pub fn handle(&mut self, event: CardEvent) {
        match event {
            CardEvent::Activated(index) if self.open == Some(index) => self.close_to_row(),
            CardEvent::Activated(index) => self.show(index),
            CardEvent::Chosen | CardEvent::Escape => self.close_to_row(),
            CardEvent::FocusLeft => {
                // A card closed by a press reports leaving after the next one
                // opened: only a focus outside the open card closes it.
                if let Some(index) = self.open
                    && !crate::focus::holds_focus(&self.cards[index])
                {
                    self.close();
                }
            }
            CardEvent::Pressed(x, y) => {
                if let Some(index) = self.open
                    && !self.contains(self.cards[index].upcast_ref(), x, y)
                    && !self.contains(&self.rows[index], x, y)
                {
                    self.close();
                }
            }
            CardEvent::Close => self.close(),
        }
    }

    /// Escape for an owner whose own Escape does more: close the open card,
    /// with the focus back on its row. `false` when none was open.
    pub fn escape(&mut self) -> bool {
        let was_open = self.open.is_some();
        self.close_to_row();
        was_open
    }

    /// Closes the open card, leaving the focus where it is.
    pub fn close(&mut self) {
        if let Some(index) = self.open.take() {
            self.cards[index].set_visible(false);
            self.bridge.set_visible(false);
            let row = &self.rows[index];
            row.remove_css_class("card-open");
            row.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
        }
    }

    fn close_to_row(&mut self) {
        if let Some(index) = self.open {
            self.close();
            self.rows[index].grab_focus();
        }
    }

    /// Opens row `index`'s card beside it, inside the overlay, with the focus
    /// on its current choice.
    fn show(&mut self, index: usize) {
        self.close();
        let (row, card) = (&self.rows[index], &self.cards[index]);
        // A card taller than the overlay scrolls.
        card.set_max_content_height((self.layer.height() - 2 * GAP).max(0));
        if !place_beside(&self.layer, row, card.upcast_ref(), &self.bridge) {
            return;
        }
        row.add_css_class("card-open");
        row.update_state(&[gtk::accessible::State::Expanded(Some(true))]);
        self.open = Some(index);
        match tiles(card.upcast_ref())
            .into_iter()
            .find(|tile| tile.is_active())
        {
            Some(chosen) => {
                chosen.grab_focus();
            }
            None => {
                card.child_focus(gtk::DirectionType::TabForward);
            }
        }
    }

    fn contains(&self, widget: &gtk::Widget, x: f64, y: f64) -> bool {
        widget
            .compute_bounds(&self.layer)
            .is_some_and(|b| b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)))
    }
}

/// A row with `icon`, `title` and its value, and an arrow saying it opens.
pub struct ValueRow {
    /// The row to put in a `gtk::ListBox`.
    pub row: adw::ActionRow,
    value: gtk::Label,
}

impl ValueRow {
    /// A row with no value shown yet.
    pub fn new(icon: &str, title: &str) -> Self {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .css_classes(["side-card-row"])
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        let value = gtk::Label::builder()
            .xalign(1.0)
            .wrap(true)
            // One line while the row has room; a row asks its suffixes for
            // their width at a given height, which would wrap every value.
            .natural_wrap_mode(gtk::NaturalWrapMode::None)
            .justify(gtk::Justification::Right)
            .css_classes(["side-card-value"])
            .build();
        row.add_suffix(&value);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        crate::list::offer_activation(&row);
        Self { row, value }
    }

    /// Show `value`, which a screen reader reads as the row's description.
    pub fn set_value(&self, value: &str) {
        if self.value.label() != value {
            self.value.set_label(value);
            self.row
                .update_property(&[gtk::accessible::Property::Description(value)]);
        }
    }
}

/// A card: `description`, then `choices`, held to [`CARD_WIDTH`].
pub fn card(description: &str, choices: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let about = gtk::Label::builder()
        .label(description)
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(36)
        .css_classes(["dimmed"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .css_classes(["side-card-content"])
        .build();
    content.append(&about);
    content.append(choices);
    // A long line wraps instead of widening the card.
    let clamp = adw::Clamp::builder()
        .child(&content)
        .maximum_size(CARD_WIDTH)
        .tightening_threshold(CARD_WIDTH)
        .build();
    gtk::ScrolledWindow::builder()
        .child(&clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .css_classes(["side-card"])
        .build()
}

/// A choice in a card: `name`, and `detail` under it when there is one.
/// Choices of one setting share a group (`gtk::CheckButton::set_group`).
pub fn tile(name: &str, detail: &str) -> gtk::CheckButton {
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["side-card-tile-name"])
            .build(),
    );
    text.append(
        &gtk::Label::builder()
            .label(detail)
            .xalign(0.0)
            .wrap(true)
            .visible(!detail.is_empty())
            .css_classes(["caption", "dimmed"])
            .build(),
    );
    let tile = gtk::CheckButton::builder()
        .child(&text)
        .css_classes(["side-card-tile"])
        .build();
    // Named by its name alone: GTK would run both labels together.
    tile.update_property(&[gtk::accessible::Property::Label(name)]);
    set_detail(&tile, detail);
    tile
}

/// Turn `tile` off where it stands, saying `reason` under its name.
pub fn set_tile_unavailable(tile: &gtk::CheckButton, reason: &str) {
    tile.set_sensitive(false);
    set_detail(tile, reason);
}

fn set_detail(tile: &gtk::CheckButton, detail: &str) {
    let label = tile
        .child()
        .and_then(|text| text.last_child())
        .and_downcast::<gtk::Label>();
    if let Some(label) = label {
        label.set_label(detail);
        label.set_visible(!detail.is_empty());
    }
    if detail.is_empty() {
        tile.reset_property(gtk::AccessibleProperty::Description);
    } else {
        tile.update_property(&[gtk::accessible::Property::Description(detail)]);
    }
}

/// The tiles under `widget`, in order.
fn tiles(widget: &gtk::Widget) -> Vec<gtk::CheckButton> {
    descendants(widget)
        .filter_map(|widget| widget.downcast::<gtk::CheckButton>().ok())
        .filter(|tile| tile.has_css_class("side-card-tile"))
        .collect()
}

pub(crate) fn install_css() {
    // Below application priority, so a product's own sheet wins.
    crate::css::install(CSS, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION - 1);
}

/// Show `card` beside `row` on `layer`, joined to it by `bridge`: on the
/// row's end side, or its other side when only that one has room, else below
/// the row without the bridge, narrowed to the overlay; held inside the
/// overlay either way. `false`, and nothing shown, when the row is not on the
/// overlay.
pub(crate) fn place_beside(
    layer: &gtk::Overlay,
    row: &gtk::Widget,
    card: &gtk::Widget,
    bridge: &gtk::Box,
) -> bool {
    let Some(bounds) = row.compute_bounds(layer) else {
        return false;
    };
    let (x, y) = (bounds.x() as i32, bounds.y() as i32);
    let (width, height) = (bounds.width() as i32, bounds.height() as i32);
    // Hidden, a card would measure 0.
    card.set_visible(true);
    // Margins count from the start edge, which is the right one in a
    // right-to-left layout.
    let rtl = layer.direction() == gtk::TextDirection::Rtl;
    let from_start = |left: i32, span: i32| {
        if rtl {
            layer.width() - left - span
        } else {
            left
        }
    };
    let (_, natural_width, _, _) = card.measure(gtk::Orientation::Horizontal, -1);
    let right = x + width + GAP;
    let left = x - GAP - natural_width;
    let fits =
        |card_left: i32| card_left >= GAP && card_left + natural_width <= layer.width() - GAP;
    card.remove_css_class("joined-left");
    card.remove_css_class("joined-right");
    if !fits(right) && !fits(left) {
        // The end margin keeps the overlay from giving the card more than
        // the room left of it.
        let card_width = natural_width.min(layer.width() - 2 * GAP);
        let (_, card_height, _, _) = card.measure(gtk::Orientation::Vertical, card_width);
        let card_left = x.min(layer.width() - GAP - card_width).max(GAP);
        card.set_margin_start(from_start(card_left, card_width));
        card.set_margin_end(GAP);
        card.set_margin_top(
            (y + height + GAP)
                .min(layer.height() - card_height - GAP)
                .max(0),
        );
        bridge.set_visible(false);
        return true;
    }
    let card_width = natural_width;
    let (_, card_height, _, _) = card.measure(gtk::Orientation::Vertical, card_width);
    let top = y.min(layer.height() - card_height - GAP).max(0);
    let on_right = if rtl {
        !fits(left) && fits(right)
    } else {
        fits(right) || !fits(left)
    };
    card.set_margin_start(from_start(if on_right { right } else { left }, card_width));
    card.set_margin_end(0);
    card.set_margin_top(top);
    if top == y {
        card.add_css_class(if on_right {
            "joined-left"
        } else {
            "joined-right"
        });
    }

    // Over both rings' facing sides, so they run on into each other.
    let reach = RING_INSET + RING;
    let bridge_left = if on_right {
        x + width - reach
    } else {
        x - GAP - reach
    };
    let bridge_width = GAP + 2 * reach;
    bridge.set_margin_start(from_start(bridge_left, bridge_width));
    bridge.set_margin_top(y + RING_INSET);
    bridge.set_size_request(bridge_width, height - 2 * RING_INSET);
    bridge.set_visible(true);
    true
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gtk::glib::translate::IntoGlib;

    use super::*;

    struct Fixture {
        window: gtk::Window,
        row: ValueRow,
        tiles: [gtk::CheckButton; 2],
        cards: SideCards,
        events: Rc<RefCell<Vec<CardEvent>>>,
    }

    impl Fixture {
        /// A row whose card offers two choices, the second chosen.
        fn new() -> Self {
            adw::init().expect("libadwaita");
            let row = ValueRow::new("preferences-system-time-symbolic", "Delay");
            let list = gtk::ListBox::new();
            list.append(&row.row);
            let tiles = [tile("Now", ""), tile("Later", "In a while")];
            tiles[1].set_group(Some(&tiles[0]));
            tiles[1].set_active(true);
            let choices = gtk::Box::new(gtk::Orientation::Vertical, 4);
            for tile in &tiles {
                choices.append(tile);
            }
            let layer = gtk::Overlay::builder().child(&list).build();
            let events = Rc::new(RefCell::new(Vec::new()));
            let sink = events.clone();
            let cards = SideCards::new(
                &layer,
                vec![row.row.clone().upcast()],
                vec![card("When to start.", &choices)],
                move |event| sink.borrow_mut().push(event),
            );
            let window = gtk::Window::builder()
                .default_width(800)
                .default_height(600)
                .child(&layer)
                .build();
            window.present();
            while glib::MainContext::default().iteration(false) {}
            Self {
                window,
                row,
                tiles,
                cards,
                events,
            }
        }

        fn focus(&self) -> gtk::Widget {
            GtkWindowExt::focus(&self.window).expect("a focus")
        }

        /// Press `key` on the open card.
        fn press(&mut self, key: gdk::Key) {
            let card = &self.cards.cards[0];
            let keys = card
                .observe_controllers()
                .iter::<glib::Object>()
                .filter_map(Result::ok)
                .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
                .expect("the card's keys");
            keys.emit_by_name::<bool>(
                "key-pressed",
                &[&key.into_glib(), &0_u32, &gdk::ModifierType::empty()],
            );
            let events: Vec<_> = self.events.borrow_mut().drain(..).collect();
            for event in events {
                self.cards.handle(event);
            }
        }
    }

    /// A short value beside a short title stays on one line in a narrow
    /// panel.
    #[test]
    fn value_fits_on_one_line() {
        big_testkit::harness::require_gtk_test(|| {
            adw::init().expect("libadwaita");
            let row = ValueRow::new("preferences-system-time-symbolic", "Delay");
            row.set_value("No delay");
            let list = gtk::ListBox::new();
            list.append(&row.row);
            let window = gtk::Window::builder()
                .default_width(296)
                .child(&list)
                .build();
            window.present();
            // The window has no size until the compositor configures it.
            assert!(big_testkit::harness::pump_until(|| row.value.width() > 0));
            let (_, unwrapped, _, _) = row.value.measure(gtk::Orientation::Horizontal, -1);
            assert!(
                row.value.width() >= unwrapped,
                "{} px for a value {unwrapped} px wide",
                row.value.width()
            );
            window.destroy();
        });
    }

    #[test]
    fn escape_returns_focus_to_row() {
        big_testkit::harness::require_gtk_test(|| {
            let mut fixture = Fixture::new();
            fixture.cards.handle(CardEvent::Activated(0));
            assert_eq!(fixture.cards.open(), Some(0));
            assert_eq!(
                fixture.focus(),
                fixture.tiles[1].clone().upcast::<gtk::Widget>(),
                "the current choice takes the focus"
            );
            fixture.press(gdk::Key::Escape);
            assert_eq!(fixture.cards.open(), None);
            assert_eq!(
                fixture.focus(),
                fixture.row.row.clone().upcast::<gtk::Widget>()
            );
            assert!(!fixture.cards.escape(), "nothing left to close");
            fixture.window.destroy();
        });
    }

    /// A row at the overlay's end edge opens its card on its other side.
    #[test]
    fn card_stays_inside_layer() {
        big_testkit::harness::require_gtk_test(|| {
            let mut fixture = Fixture::new();
            let list = fixture.row.row.parent().expect("the list");
            list.set_halign(gtk::Align::End);
            list.set_size_request(300, -1);
            assert!(big_testkit::harness::pump_frame(&fixture.window));
            fixture.cards.handle(CardEvent::Activated(0));
            assert!(big_testkit::harness::pump_frame(&fixture.window));
            let card = fixture.cards.cards[0]
                .compute_bounds(&fixture.window)
                .expect("the card's bounds");
            let row = fixture
                .row
                .row
                .compute_bounds(&fixture.window)
                .expect("the row's bounds");
            assert!(card.x() >= 0.0, "{card:?}");
            assert!(card.x() + card.width() <= row.x(), "{card:?} over {row:?}");
            fixture.window.destroy();
        });
    }

    /// A row as wide as a narrow overlay has no room on either side: the
    /// card opens below it, inside the overlay.
    #[test]
    fn card_below_a_row_with_no_room_beside() {
        big_testkit::harness::require_gtk_test(|| {
            let mut fixture = Fixture::new();
            let layer = fixture
                .row
                .row
                .ancestor(gtk::Overlay::static_type())
                .expect("the layer");
            layer.set_halign(gtk::Align::Start);
            layer.set_size_request(360, -1);
            assert!(big_testkit::harness::pump_frame(&fixture.window));
            fixture.cards.handle(CardEvent::Activated(0));
            assert!(big_testkit::harness::pump_frame(&fixture.window));
            let card = fixture.cards.cards[0]
                .compute_bounds(&layer)
                .expect("the card's bounds");
            let row = fixture
                .row
                .row
                .compute_bounds(&layer)
                .expect("the row's bounds");
            assert!(card.x() >= 0.0, "{card:?}");
            assert!(
                card.x() + card.width() <= layer.width() as f32,
                "{card:?} past {} px",
                layer.width()
            );
            assert!(card.y() >= row.y() + row.height(), "{card:?} over {row:?}");
            assert!(
                fixture.tiles[1].is_drawable()
                    && fixture.focus() == fixture.tiles[1].clone().upcast::<gtk::Widget>(),
                "the focus on a choice in sight"
            );
            fixture.window.destroy();
        });
    }

    #[test]
    fn enter_chooses_and_closes() {
        big_testkit::harness::require_gtk_test(|| {
            let mut fixture = Fixture::new();
            fixture.cards.handle(CardEvent::Activated(0));
            fixture.tiles[0].grab_focus();
            fixture.press(gdk::Key::Return);
            assert!(fixture.tiles[0].is_active());
            assert_eq!(fixture.cards.open(), None);
            assert_eq!(
                fixture.focus(),
                fixture.row.row.clone().upcast::<gtk::Widget>()
            );
            fixture.window.destroy();
        });
    }
}
