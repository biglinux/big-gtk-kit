// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Cards that open beside their row as the pointer reaches it.
//!
//! Each card is a child of the window's overlay, placed beside its row and
//! joined to it by a bridge, so the row and its card read as one shape. The
//! pointer opens a card and may travel into it: leaving the row or the card
//! starts a short grace before it closes, and a row crossed on the way takes
//! over only if the pointer rests on it. A click or Enter opens a card for the
//! keyboard: the owner focuses it, and Escape, a click outside or the focus
//! leaving closes it; the pointer never replaces it.
//!
//! The widgets' handlers only report [`CardEvent`]s; the owner passes them back
//! to [`HoverCards::handle`], so no handler holds a widget.
//!
//! The row of the open card carries `card-open` and the overlay `cards-open`;
//! how a row under the pointer looks, and that none lights up while a card is
//! open, is the program's stylesheet.

use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::side_card::place_beside;
use crate::timer::OnceTimer;

/// How long the pointer rests on another row before its card replaces the
/// open one.
const SWITCH_DELAY: Duration = Duration::from_millis(150);
/// How long a card stays after the pointer leaves it and its row.
const CLOSE_DELAY: Duration = Duration::from_millis(300);

/// What the rows and cards report to their owner, which passes it back to
/// [`HoverCards::handle`].
#[derive(Debug, Clone, Copy)]
pub enum CardEvent {
    /// The pointer reached row `n`.
    Entered(usize),
    /// The pointer left row `n`.
    Left(usize),
    /// The pointer reached the open card.
    CardEntered,
    /// The pointer left the open card.
    CardLeft,
    /// Row `n` was clicked or activated with the keyboard.
    Activated(usize),
    /// Escape was pressed in the open card.
    Escape,
    /// The keyboard focus left the open card.
    FocusLeft,
    /// A press anywhere in the window, at these overlay coordinates.
    Pressed(f64, f64),
    /// The timer set by the last pointer move ran out.
    Settled,
    /// The rows moved under the card (the sidebar scrolled).
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Opener {
    Pointer,
    Keyboard,
}

/// The rows, their cards, which card is open and what opened it.
pub struct HoverCards {
    layer: gtk::Overlay,
    rows: Vec<gtk::Widget>,
    cards: Vec<gtk::Widget>,
    bridge: gtk::Box,
    open: Option<(usize, Opener)>,
    /// The row under the pointer, if any.
    hovered: Option<usize>,
    /// What the running timer does: open this row's card, or close.
    settle_to: Option<usize>,
    /// True from arming until the timer's [`CardEvent::Settled`] is acted on or
    /// the timer is cancelled. An owner that queues events may hand one over
    /// late, and [`OnceTimer::is_pending`] alone cannot tell a fired timer whose
    /// event waits from a cancelled one.
    settling: bool,
    timer: OnceTimer,
    emit: Rc<dyn Fn(CardEvent)>,
}

impl HoverCards {
    /// Puts `cards` on `layer`, card `n` belonging to `rows[n]`; a row or an
    /// index without a card opens nothing. The cards and controllers stay on
    /// `layer` and the rows for their lifetime: make one for each layer.
    pub fn new(
        layer: &gtk::Overlay,
        rows: Vec<gtk::Widget>,
        cards: Vec<gtk::Widget>,
        emit: impl Fn(CardEvent) + 'static,
    ) -> Self {
        crate::side_card::install_css();
        let emit: Rc<dyn Fn(CardEvent)> = Rc::new(emit);
        for (index, row) in rows.iter().enumerate() {
            let motion = gtk::EventControllerMotion::new();
            let enter = emit.clone();
            motion.connect_enter(move |_, _, _| enter(CardEvent::Entered(index)));
            let leave = emit.clone();
            motion.connect_leave(move |_| leave(CardEvent::Left(index)));
            row.add_controller(motion);
            row.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
        }
        for card in &cards {
            card.set_visible(false);
            card.set_halign(gtk::Align::Start);
            card.set_valign(gtk::Align::Start);
            card.add_css_class("side-card");
            let motion = gtk::EventControllerMotion::new();
            let enter = emit.clone();
            motion.connect_enter(move |_, _, _| enter(CardEvent::CardEntered));
            let leave = emit.clone();
            motion.connect_leave(move |_| leave(CardEvent::CardLeft));
            card.add_controller(motion);
            let keys = gtk::EventControllerKey::new();
            let escape = emit.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    escape(CardEvent::Escape);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            card.add_controller(keys);
            let focus = gtk::EventControllerFocus::new();
            let left = emit.clone();
            focus.connect_leave(move |_| left(CardEvent::FocusLeft));
            card.add_controller(focus);
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
            hovered: None,
            settle_to: None,
            settling: false,
            timer: OnceTimer::new(),
            emit,
        }
    }

    /// The row whose card is open.
    pub fn open(&self) -> Option<usize> {
        self.open.map(|(index, _)| index)
    }

    /// Acts on an event; `true` when a card was opened for the keyboard, for
    /// the owner to move the focus into it.
    pub fn handle(&mut self, event: CardEvent) -> bool {
        let by_pointer = matches!(self.open, Some((_, Opener::Pointer)));
        match event {
            CardEvent::Entered(index) => {
                self.hovered = Some(index);
                match self.open {
                    None => {
                        self.show(index, Opener::Pointer);
                    }
                    Some((open, Opener::Pointer)) if open == index => self.cancel(),
                    Some((_, Opener::Pointer)) => self.schedule(SWITCH_DELAY, Some(index)),
                    Some((_, Opener::Keyboard)) => {}
                }
            }
            CardEvent::Left(index) => {
                if self.hovered == Some(index) {
                    self.hovered = None;
                }
                if by_pointer {
                    self.schedule(CLOSE_DELAY, None);
                }
            }
            CardEvent::CardEntered => {
                if by_pointer {
                    self.cancel();
                }
            }
            CardEvent::CardLeft => {
                if by_pointer && self.hovered.is_none() {
                    self.schedule(CLOSE_DELAY, None);
                }
            }
            CardEvent::Settled => {
                if by_pointer && !self.timer.is_pending() && std::mem::take(&mut self.settling) {
                    match self.settle_to.or(self.hovered) {
                        Some(index) => {
                            self.show(index, Opener::Pointer);
                        }
                        None => self.close(),
                    }
                }
            }
            CardEvent::Activated(index) => return self.show(index, Opener::Keyboard),
            CardEvent::Escape => {
                if let Some(index) = self.open() {
                    self.close();
                    self.rows[index].grab_focus();
                }
            }
            CardEvent::FocusLeft => {
                // A card closed for the next one reports leaving after that one
                // opened: only a focus outside the open card closes it.
                if let Some((index, Opener::Keyboard)) = self.open
                    && !crate::focus::holds_focus(&self.cards[index])
                {
                    self.close();
                }
            }
            CardEvent::Pressed(x, y) => {
                if let Some(index) = self.open()
                    && !self.contains(&self.cards[index], x, y)
                    && !self.contains(&self.rows[index], x, y)
                {
                    self.close();
                }
            }
            CardEvent::Close => self.close(),
        }
        false
    }

    /// Close the open card.
    pub fn close(&mut self) {
        self.cancel();
        if let Some((index, _)) = self.open.take() {
            self.cards[index].set_visible(false);
            self.bridge.set_visible(false);
            let row = &self.rows[index];
            row.remove_css_class("card-open");
            row.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
            self.layer.remove_css_class("cards-open");
        }
    }

    /// Opens row `index`'s card beside it, held inside the window; whether
    /// it opened.
    fn show(&mut self, index: usize, opener: Opener) -> bool {
        self.cancel();
        if self.open.map(|(open, _)| open) != Some(index) {
            self.close();
        }
        let (Some(row), Some(card)) = (self.rows.get(index), self.cards.get(index)) else {
            return false;
        };
        if !place_beside(&self.layer, row, card, &self.bridge) {
            return false;
        }
        row.add_css_class("card-open");
        row.update_state(&[gtk::accessible::State::Expanded(Some(true))]);
        self.layer.add_css_class("cards-open");
        self.open = Some((index, opener));
        true
    }

    fn contains(&self, widget: &gtk::Widget, x: f64, y: f64) -> bool {
        widget
            .compute_bounds(&self.layer)
            .is_some_and(|b| b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)))
    }

    fn schedule(&mut self, delay: Duration, settle_to: Option<usize>) {
        self.cancel();
        self.settle_to = settle_to;
        self.settling = true;
        let emit = self.emit.clone();
        self.timer.arm(delay, move || emit(CardEvent::Settled));
    }

    fn cancel(&mut self) {
        self.timer.cancel();
        self.settling = false;
    }
}

impl Drop for HoverCards {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The pointer opens a card and its leaving closes it once the grace has
    /// run; a card opened for the keyboard stays when the pointer passes.
    #[test]
    fn pointer_opens_and_grace_closes() {
        big_testkit::harness::require_gtk_test(|| {
            let layer = gtk::Overlay::new();
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let rows: Vec<gtk::Widget> = (0..2)
                .map(|index| gtk::Label::new(Some(&format!("Row {index}"))).upcast())
                .collect();
            for row in &rows {
                column.append(row);
            }
            layer.set_child(Some(&column));
            let cards: Vec<gtk::Widget> = (0..2)
                .map(|index| gtk::Button::with_label(&format!("Card {index}")).upcast())
                .collect();
            let heard = Rc::new(RefCell::new(Vec::new()));
            let cards_heard = Rc::clone(&heard);
            let mut hover = HoverCards::new(&layer, rows, cards.clone(), move |event| {
                cards_heard.borrow_mut().push(event);
            });
            let window = gtk::Window::builder().child(&layer).build();
            window.present();
            assert!(big_testkit::harness::pump_until(|| layer.width() > 0));

            hover.handle(CardEvent::Entered(0));
            assert_eq!(hover.open(), Some(0));
            assert!(cards[0].is_visible());
            let settled = || {
                heard
                    .borrow()
                    .iter()
                    .filter(|event| matches!(event, CardEvent::Settled))
                    .count()
            };
            hover.handle(CardEvent::Left(0));
            assert!(big_testkit::harness::pump_until(|| settled() == 1));
            // Queued behind the timer's event, the pointer reached the card.
            hover.handle(CardEvent::CardEntered);
            hover.handle(CardEvent::Settled);
            assert_eq!(hover.open(), Some(0), "a cancelled grace closes nothing");
            // Queued behind it, a leave armed the grace again.
            hover.handle(CardEvent::CardLeft);
            assert!(big_testkit::harness::pump_until(|| settled() == 2));
            hover.handle(CardEvent::CardEntered);
            hover.handle(CardEvent::CardLeft);
            hover.handle(CardEvent::Settled);
            assert_eq!(hover.open(), Some(0), "a grace armed again runs whole");
            assert!(big_testkit::harness::pump_until(|| settled() == 3));
            hover.handle(CardEvent::Settled);
            assert_eq!(hover.open(), None);
            assert!(!cards[0].is_visible());

            assert!(
                hover.handle(CardEvent::Activated(1)),
                "the owner focuses it"
            );
            hover.handle(CardEvent::Entered(0));
            assert_eq!(hover.open(), Some(1), "the pointer never replaces it");
            cards[1].grab_focus();
            hover.handle(CardEvent::FocusLeft);
            assert_eq!(
                hover.open(),
                Some(1),
                "a stale leave keeps the focused card"
            );
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            hover.handle(CardEvent::FocusLeft);
            assert_eq!(hover.open(), None, "a focus outside it closes it");
            assert!(hover.handle(CardEvent::Activated(1)));
            hover.handle(CardEvent::Escape);
            assert_eq!(hover.open(), None);
            window.destroy();
        });
    }
}
