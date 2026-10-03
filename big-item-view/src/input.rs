// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The pointer and the keyboard.
//!
//! One set of controllers on the view, never one per item: what is under the
//! pointer is a division away ([`crate::layout::Metrics::position_at`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::layout::{Rect, Step};
use crate::view::{ItemView, Pending};
use crate::{Flow, TickMode};

/// How close to an edge a rubber band has to be for the view to scroll under it.
const AUTOSCROLL_EDGE: f64 = 24.0;
/// The most it scrolls per frame.
const AUTOSCROLL_SPEED: f64 = 18.0;
/// How long a drag has to rest on a folder before it opens (spring-loading).
const SPRING_DWELL: Duration = Duration::from_millis(900);
/// How far the pointer may drift and still count as resting.
const SPRING_DRIFT: f64 = 8.0;

pub(crate) fn install(view: &ItemView) {
    install_click(view);
    install_band(view);
    install_motion(view);
    install_keys(view);
    install_drag_source(view);
    install_tooltip(view);
    install_focus(view);
}

/// The mouse wheel scrolls a column flow sideways.
///
/// A wheel only turns one way, and a list that scrolls only sideways would
/// otherwise ignore it: Dolphin's and Nautilus's compact lists turn it into the
/// direction they scroll. By the same step a scrolled window takes for a
/// notch; a touchpad's own pixels as they come.
///
/// On the scrolled window the view sits in, and before it: that window takes
/// every scroll in its capture phase, so a controller on the view itself never
/// hears the wheel. Called when the view is mapped; once per scrolled window.
pub(crate) fn install_wheel(view: &ItemView) {
    let Some(scroller) = view
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    let imp = view.imp();
    if imp.wheel_on.upgrade().as_ref() == Some(&scroller) {
        return;
    }
    imp.wheel_on.set(Some(&scroller));
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = view.downgrade();
    scroll.connect_scroll(move |controller, _, delta_y| {
        let Some(view) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let imp = view.imp();
        if imp.geometry.borrow().flow != Flow::ColumnMajor || delta_y == 0.0 {
            return glib::Propagation::Proceed;
        }
        let Some(adjustment) = imp.adjustment_along() else {
            return glib::Propagation::Proceed;
        };
        let step = match controller.unit() {
            gdk::ScrollUnit::Wheel => adjustment.page_size().powf(2.0 / 3.0),
            _ => 1.0,
        };
        let top = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        // Ctrl with the wheel is the consumer's (a zoom), not a scroll.
        if controller
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
            || !view.is_mapped()
        {
            return glib::Propagation::Proceed;
        }
        adjustment.set_value((adjustment.value() + delta_y * step).clamp(adjustment.lower(), top));
        glib::Propagation::Stop
    });
    scroller.add_controller(scroll);
}

fn modifiers(controller: &impl IsA<gtk::EventController>) -> (bool, bool) {
    let state = controller.current_event_state();
    (
        state.contains(gdk::ModifierType::CONTROL_MASK),
        state.contains(gdk::ModifierType::SHIFT_MASK),
    )
}

fn select_range(view: &ItemView, from: u32, to: u32, unselect_rest: bool) {
    if let Some(model) = view.model() {
        let (start, end) = (from.min(to), from.max(to));
        model.select_range(start, end - start + 1, unselect_rest);
    }
}

fn toggle(view: &ItemView, position: u32) {
    if let Some(model) = view.model() {
        if model.is_selected(position) {
            model.unselect_item(position);
        } else {
            model.select_item(position, false);
        }
    }
}

/// Whether `(x, y)` in view coordinates is on the tick of the item at `position`.
fn on_tick(view: &ItemView, position: u32, x: f64, y: f64) -> bool {
    let imp = view.imp();
    let geometry = imp.geometry.borrow();
    let shown = match geometry.tick {
        TickMode::None => false,
        TickMode::Selection => imp.hover.get() == Some(position),
        TickMode::Check => true,
    };
    let Some(rect) = view.item_rect(position) else {
        return false;
    };
    let rtl = view.direction() == gtk::TextDirection::Rtl;
    let (offset, primary) =
        crate::draw::primary_span(&geometry, (rect.width(), rect.height()), rtl);
    let tick = crate::draw::tick_box(&geometry, primary, rtl);
    shown
        && tick.contains_point(&graphene::Point::new(
            x as f32 - rect.x() - offset,
            y as f32 - rect.y(),
        ))
}

fn install_click(view: &ItemView) {
    let click = gtk::GestureClick::builder().button(0).build();
    let weak = view.downgrade();
    click.connect_pressed(move |gesture, n_press, x, y| {
        let Some(view) = weak.upgrade() else {
            return;
        };
        let imp = view.imp();
        imp.keyboard_cursor.set(false);
        let position = view.position_at(x, y);
        match gesture.current_button() {
            gdk::BUTTON_PRIMARY => {}
            gdk::BUTTON_MIDDLE => {
                if let Some(position) = position {
                    view.emit_by_name::<()>("middle-click", &[&position]);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
                return;
            }
            gdk::BUTTON_SECONDARY => {
                view.grab_focus();
                let position = position.unwrap_or(gtk::INVALID_LIST_POSITION);
                view.emit_by_name::<()>("context-menu", &[&position, &x, &y]);
                gesture.set_state(gtk::EventSequenceState::Claimed);
                return;
            }
            _ => return,
        }
        view.grab_focus();
        let (ctrl, shift) = modifiers(gesture);
        imp.pending.set(None);
        let Some(position) = position else {
            if !ctrl
                && !shift
                && let Some(model) = view.model()
            {
                model.unselect_all();
            }
            return;
        };
        if on_tick(&view, position, x, y) {
            if imp.geometry.borrow().tick == TickMode::Check {
                view.emit_by_name::<()>("check-toggled", &[&position]);
            } else {
                toggle(&view, position);
            }
            imp.set_cursor(Some(position));
            imp.anchor.set(Some(position));
            gesture.set_state(gtk::EventSequenceState::Claimed);
            return;
        }
        let selected = view
            .model()
            .is_some_and(|model| model.is_selected(position));
        if shift {
            select_range(&view, imp.anchor.get().unwrap_or(position), position, !ctrl);
        } else if ctrl {
            imp.pending.set(Some(Pending::Toggle(position)));
            imp.anchor.set(Some(position));
        } else if selected {
            // Kept until the release: dragging a selection must carry all of it.
            imp.pending.set(Some(Pending::SelectOnly(position)));
            imp.anchor.set(Some(position));
        } else {
            if let Some(model) = view.model() {
                model.select_item(position, true);
            }
            imp.anchor.set(Some(position));
        }
        imp.set_cursor(Some(position));
        if n_press == 2 {
            imp.pending.set(None);
            view.emit_by_name::<()>("activate", &[&position]);
        }
    });
    let weak = view.downgrade();
    click.connect_released(move |gesture, n_press, x, y| {
        let Some(view) = weak.upgrade() else {
            return;
        };
        if gesture.current_button() != gdk::BUTTON_PRIMARY {
            return;
        }
        let imp = view.imp();
        let pending = imp.pending.take();
        match pending {
            Some(Pending::Toggle(position)) => toggle(&view, position),
            Some(Pending::SelectOnly(position)) => {
                if let Some(model) = view.model() {
                    model.select_item(position, true);
                }
            }
            None => {}
        }
        let (ctrl, shift) = modifiers(gesture);
        if imp.single_click.get()
            && n_press == 1
            && !ctrl
            && !shift
            && let Some(position) = view.position_at(x, y)
            && !on_tick(&view, position, x, y)
        {
            view.emit_by_name::<()>("activate", &[&position]);
        }
    });
    view.add_controller(click);
}

/// Drag a rectangle from empty space to select what it meets.
fn install_band(view: &ItemView) {
    struct Band {
        start: (f32, f32),
        pointer: Cell<(f64, f64)>,
        /// Past the drag threshold: until then the press is a click on the
        /// background, and a hand's tremor selects nothing.
        moved: Cell<bool>,
        initial: gtk::Bitset,
        toggle: bool,
        scroller: RefCell<Option<gtk::TickCallbackId>>,
    }
    let state: Rc<RefCell<Option<Rc<Band>>>> = Rc::new(RefCell::new(None));
    let drag = gtk::GestureDrag::builder()
        .button(gdk::BUTTON_PRIMARY)
        .build();

    fn update(view: &ItemView, band: &Band) {
        let imp = view.imp();
        let (x, y) = band.pointer.get();
        let scroll = imp.scroll();
        let now = (x as f32 + scroll.0, y as f32 + scroll.1);
        let rect = Rect {
            x: band.start.0.min(now.0),
            y: band.start.1.min(now.1),
            width: (band.start.0 - now.0).abs(),
            height: (band.start.1 - now.1).abs(),
        };
        imp.band.set(Some(rect));
        // The cells the band crosses; where only what an item draws is the
        // item, the band has to reach that, not the room around it.
        let on_screen = graphene::Rect::new(
            rect.x - scroll.0,
            rect.y - scroll.1,
            rect.width,
            rect.height,
        );
        let drawn_only = view.hits_drawn_only();
        let met: Vec<u32> = imp
            .sections
            .borrow()
            .positions_in(&imp.metrics.borrow().clone(), &rect)
            .into_iter()
            .filter(|position| {
                !drawn_only
                    || view.drawn_areas(*position).is_some_and(|areas| {
                        areas
                            .iter()
                            .any(|area| area.intersection(&on_screen).is_some())
                    })
            })
            .collect();
        if let Some(model) = view.model() {
            let selected = band.initial.copy();
            for position in met {
                if band.toggle && selected.contains(position) {
                    selected.remove(position);
                } else {
                    selected.add(position);
                }
            }
            let mask = gtk::Bitset::new_range(0, model.n_items());
            model.set_selection(&selected, &mask);
        }
        view.queue_draw();
    }

    let weak = view.downgrade();
    let slot = Rc::clone(&state);
    drag.connect_drag_begin(move |gesture, x, y| {
        let Some(view) = weak.upgrade() else {
            return;
        };
        if !view.imp().rubberband.get() || view.position_at(x, y).is_some() {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }
        let (ctrl, _) = modifiers(gesture);
        let scroll = view.imp().scroll();
        let initial = if ctrl {
            view.model()
                .map_or_else(gtk::Bitset::new_empty, |model| model.selection())
        } else {
            gtk::Bitset::new_empty()
        };
        slot.replace(Some(Rc::new(Band {
            start: (x as f32 + scroll.0, y as f32 + scroll.1),
            pointer: Cell::new((x, y)),
            moved: Cell::new(false),
            initial,
            toggle: ctrl,
            scroller: RefCell::new(None),
        })));
    });
    let weak = view.downgrade();
    let slot = Rc::clone(&state);
    drag.connect_drag_update(move |gesture, dx, dy| {
        let (Some(view), Some(band)) = (weak.upgrade(), slot.borrow().clone()) else {
            return;
        };
        let Some((x0, y0)) = gesture.start_point() else {
            return;
        };
        if !band.moved.get() {
            let threshold = view.settings().gtk_dnd_drag_threshold().max(1);
            if dx.hypot(dy) < f64::from(threshold) {
                return;
            }
            band.moved.set(true);
        }
        band.pointer.set((x0 + dx, y0 + dy));
        update(&view, &band);
        // Past an edge, keep scrolling while the pointer stays there.
        if band.scroller.borrow().is_none() {
            let weak_band = Rc::downgrade(&band);
            let id = view.add_tick_callback(move |view, _| {
                let Some(band) = weak_band.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let Some(adjustment) = view.imp().adjustment_along() else {
                    return glib::ControlFlow::Continue;
                };
                let (x, y) = band.pointer.get();
                let (along, extent) = match view.imp().geometry.borrow().flow {
                    crate::Flow::RowMajor { .. } => (y, f64::from(view.height())),
                    crate::Flow::ColumnMajor => (x, f64::from(view.width())),
                };
                let speed = if along < AUTOSCROLL_EDGE {
                    -AUTOSCROLL_SPEED * (1.0 - along.max(0.0) / AUTOSCROLL_EDGE)
                } else if along > extent - AUTOSCROLL_EDGE {
                    AUTOSCROLL_SPEED * (1.0 - (extent - along).max(0.0) / AUTOSCROLL_EDGE)
                } else {
                    0.0
                };
                if speed != 0.0 {
                    adjustment.set_value(adjustment.value() + speed);
                    update(view, &band);
                }
                glib::ControlFlow::Continue
            });
            band.scroller.replace(Some(id));
        }
    });
    let weak = view.downgrade();
    let slot = Rc::clone(&state);
    drag.connect_drag_end(move |_, _, _| {
        let Some(band) = slot.take() else {
            return;
        };
        if let Some(id) = band.scroller.take() {
            id.remove();
        }
        if let Some(view) = weak.upgrade() {
            view.imp().band.set(None);
            view.queue_draw();
        }
    });
    view.add_controller(drag);
}

fn install_motion(view: &ItemView) {
    let motion = gtk::EventControllerMotion::new();
    let weak = view.downgrade();
    let hovered = move |x: Option<(f64, f64)>| {
        let Some(view) = weak.upgrade() else {
            return;
        };
        let position = x.and_then(|(x, y)| view.position_at(x, y));
        let imp = view.imp();
        if imp.hover.replace(position) == position {
            return;
        }
        view.queue_draw();
        let (position, rect) = match position.and_then(|p| Some((p, view.item_rect(p)?))) {
            Some((p, rect)) => (p, rect),
            None => (gtk::INVALID_LIST_POSITION, graphene::Rect::zero()),
        };
        view.emit_by_name::<()>(
            "hover",
            &[
                &position,
                &f64::from(rect.x()),
                &f64::from(rect.y()),
                &f64::from(rect.width()),
                &f64::from(rect.height()),
            ],
        );
    };
    let hovered = Rc::new(hovered);
    let on_motion = Rc::clone(&hovered);
    motion.connect_motion(move |_, x, y| on_motion(Some((x, y))));
    let on_enter = Rc::clone(&hovered);
    motion.connect_enter(move |_, x, y| on_enter(Some((x, y))));
    motion.connect_leave(move |_| hovered(None));
    view.add_controller(motion);
}

/// Scroll just enough for the item at `position` to be wholly on screen.
pub(crate) fn bring_into_view(view: &ItemView, position: u32) {
    let imp = view.imp();
    let Some(rect) = imp
        .sections
        .borrow()
        .rect(&imp.metrics.borrow().clone(), position)
    else {
        return;
    };
    let Some(adjustment) = imp.adjustment_along() else {
        bring_into_outer_view(view, &rect);
        return;
    };
    let (start, extent) = match imp.geometry.borrow().flow {
        crate::Flow::RowMajor { .. } => (rect.y, rect.height),
        crate::Flow::ColumnMajor => (rect.x, rect.width),
    };
    let (start, end) = (f64::from(start), f64::from(start + extent));
    let (value, page) = (adjustment.value(), adjustment.page_size());
    if start < value {
        adjustment.set_value(start);
    } else if end > value + page {
        adjustment.set_value(end - page);
    }
}

/// The same for a view that does not scroll itself (a band on a page): the
/// page's scroller is moved instead, as it would be for a focused widget.
fn bring_into_outer_view(view: &ItemView, rect: &Rect) {
    let Some(scroller) = view
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    // What the adjustment scrolls: the viewport's child, or a scrollable child.
    let Some(content) = scroller.child().map(|child| {
        child
            .downcast_ref::<gtk::Viewport>()
            .and_then(gtk::Viewport::child)
            .unwrap_or(child)
    }) else {
        return;
    };
    let scroll = view.imp().scroll();
    let Some(top) = view.compute_point(
        &content,
        &graphene::Point::new(rect.x - scroll.0, rect.y - scroll.1),
    ) else {
        return;
    };
    let adjustment = scroller.vadjustment();
    let (start, end) = (f64::from(top.y()), f64::from(top.y() + rect.height));
    let (value, page) = (adjustment.value(), adjustment.page_size());
    if start < value {
        adjustment.set_value(start);
    } else if end > value + page {
        adjustment.set_value(end - page);
    }
}

fn move_cursor(view: &ItemView, step: Step, ctrl: bool, shift: bool) -> bool {
    let imp = view.imp();
    let n = imp.n_items();
    if n == 0 {
        return false;
    }
    let from = imp.cursor.get().unwrap_or(0);
    let Some(target) = imp.metrics.borrow().clone().step(from, step) else {
        return false;
    };
    if shift {
        select_range(view, imp.anchor.get().unwrap_or(from), target, !ctrl);
    } else if !ctrl {
        if let Some(model) = view.model() {
            model.select_item(target, true);
        }
        imp.anchor.set(Some(target));
    }
    imp.set_cursor(Some(target));
    imp.keyboard_cursor.set(true);
    bring_into_view(view, target);
    true
}

/// Move the cursor one step the way `direction` points, as a plain arrow key
/// does. `false` at the edge.
pub(crate) fn step_cursor(view: &ItemView, direction: gtk::DirectionType) -> bool {
    let rtl = view.direction() == gtk::TextDirection::Rtl;
    let step = match direction {
        gtk::DirectionType::Left if rtl => Step::Next,
        gtk::DirectionType::Left => Step::Previous,
        gtk::DirectionType::Right if rtl => Step::Previous,
        gtk::DirectionType::Right => Step::Next,
        gtk::DirectionType::Up => Step::Up,
        gtk::DirectionType::Down => Step::Down,
        gtk::DirectionType::TabForward => Step::Next,
        gtk::DirectionType::TabBackward => Step::Previous,
        _ => return false,
    };
    move_cursor(view, step, false, false)
}

fn install_keys(view: &ItemView) {
    let keys = gtk::EventControllerKey::new();
    let weak = view.downgrade();
    keys.connect_key_pressed(move |controller, key, _, _| {
        let Some(view) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let (ctrl, shift) = modifiers(controller);
        let rtl = view.direction() == gtk::TextDirection::Rtl;
        let (back, forward) = if rtl {
            (Step::Next, Step::Previous)
        } else {
            (Step::Previous, Step::Next)
        };
        let step = match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some(back),
            gdk::Key::Right | gdk::Key::KP_Right => Some(forward),
            gdk::Key::Up | gdk::Key::KP_Up => Some(Step::Up),
            gdk::Key::Down | gdk::Key::KP_Down => Some(Step::Down),
            gdk::Key::Home | gdk::Key::KP_Home => Some(Step::First),
            gdk::Key::End | gdk::Key::KP_End => Some(Step::Last),
            gdk::Key::Page_Up | gdk::Key::KP_Page_Up => Some(Step::PageBack),
            gdk::Key::Page_Down | gdk::Key::KP_Page_Down => Some(Step::PageForward),
            _ => None,
        };
        if let Some(step) = step {
            if move_cursor(&view, step, ctrl, shift) {
                return glib::Propagation::Stop;
            }
            // At the edge: GTK's own question, so a consumer can take the
            // keyboard somewhere else (`keynav-failed`) and the default keeps it.
            let direction = match key {
                gdk::Key::Left | gdk::Key::KP_Left => gtk::DirectionType::Left,
                gdk::Key::Right | gdk::Key::KP_Right => gtk::DirectionType::Right,
                gdk::Key::Up | gdk::Key::KP_Up => gtk::DirectionType::Up,
                gdk::Key::Down | gdk::Key::KP_Down => gtk::DirectionType::Down,
                _ => return glib::Propagation::Stop,
            };
            return if view.keynav_failed(direction) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            };
        }
        let cursor = view.cursor();
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                if let Some(position) = cursor {
                    view.emit_by_name::<()>("activate", &[&position]);
                    return glib::Propagation::Stop;
                }
            }
            gdk::Key::Menu => {
                emit_menu_at_cursor(&view);
                return glib::Propagation::Stop;
            }
            gdk::Key::F10 if shift => {
                emit_menu_at_cursor(&view);
                return glib::Propagation::Stop;
            }
            // Space ticks the item the keyboard is on, where items carry a tick
            // of their own; elsewhere it is left to the consumer.
            gdk::Key::space if !ctrl && view.imp().geometry.borrow().tick == TickMode::Check => {
                if let Some(position) = cursor {
                    view.emit_by_name::<()>("check-toggled", &[&position]);
                    return glib::Propagation::Stop;
                }
            }
            gdk::Key::space if ctrl => {
                if let Some(position) = cursor {
                    toggle(&view, position);
                    return glib::Propagation::Stop;
                }
            }
            gdk::Key::a | gdk::Key::A if ctrl => {
                if let Some(model) = view.model() {
                    if shift {
                        model.unselect_all();
                    } else {
                        model.select_all();
                    }
                }
                return glib::Propagation::Stop;
            }
            _ => {}
        }
        glib::Propagation::Proceed
    });
    view.add_controller(keys);
}

fn emit_menu_at_cursor(view: &ItemView) {
    let (position, x, y) = match view.cursor().and_then(|p| Some((p, view.item_rect(p)?))) {
        Some((position, rect)) => (
            position,
            f64::from(rect.x() + rect.width() / 2.0),
            f64::from(rect.y() + rect.height() / 2.0),
        ),
        None => (
            gtk::INVALID_LIST_POSITION,
            f64::from(view.width()) / 2.0,
            f64::from(view.height()) / 2.0,
        ),
    };
    view.emit_by_name::<()>("context-menu", &[&position, &x, &y]);
}

fn install_drag_source(view: &ItemView) {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
    let weak = view.downgrade();
    source.connect_prepare(move |_, x, y| {
        let view = weak.upgrade()?;
        let position = view.position_at(x, y)?;
        let model = view.model()?;
        let positions: Vec<u32> = if model.is_selected(position) {
            let selection = model.selection();
            let mut all = Vec::new();
            if let Some((mut iter, first)) = gtk::BitsetIter::init_first(&selection) {
                all.push(first);
                for next in &mut iter {
                    all.push(next);
                }
            }
            all
        } else {
            vec![position]
        };
        let presenter = view.imp().presenter.borrow().clone()?;
        presenter.drag_content(&positions)
    });
    let weak = view.downgrade();
    source.connect_drag_begin(move |_, _| {
        if let Some(view) = weak.upgrade() {
            // A drag, not a click: the selection it carries stays as it is.
            view.imp().pending.set(None);
        }
    });
    view.add_controller(source);
}

/// Each item's tooltip as the shared card, pointing at the item.
fn install_tooltip(view: &ItemView) {
    big_gtk_kit::tooltip::set_area_tips(view, |view, x, y| {
        let view = view.downcast_ref::<ItemView>()?;
        let position = view.position_at(x, y)?;
        let presenter = view.imp().presenter.borrow().clone()?;
        let item = view.model()?.item(position)?;
        let text = presenter.tooltip(position, &item)?;
        let rect = view.item_rect(position)?;
        Some((
            text,
            gdk::Rectangle::new(
                rect.x() as i32,
                rect.y() as i32,
                rect.width() as i32,
                rect.height() as i32,
            ),
        ))
    });
}

fn install_focus(view: &ItemView) {
    let focus = gtk::EventControllerFocus::new();
    let weak = view.downgrade();
    focus.connect_enter(move |_| {
        let Some(view) = weak.upgrade() else {
            return;
        };
        let imp = view.imp();
        if imp.cursor.get().is_none() && imp.n_items() > 0 {
            // The first selected item, or the first item: never selecting it.
            let first = view
                .model()
                .and_then(|model| {
                    gtk::BitsetIter::init_first(&model.selection()).map(|(_, first)| first)
                })
                .unwrap_or(0);
            imp.set_cursor(Some(first));
        }
        imp.sync_focus();
        view.queue_draw();
    });
    let weak = view.downgrade();
    focus.connect_leave(move |_| {
        if let Some(view) = weak.upgrade() {
            view.imp().sync_focus();
            view.queue_draw();
        }
    });
    view.add_controller(focus);
}

/// A spring-load timer: its source, the item it waits on, where the pointer was.
type Dwell = (glib::SourceId, u32, (f64, f64));

/// Accept drops on the view; the presenter decides which items take them.
pub(crate) fn install_drop(view: &ItemView, types: &[glib::Type], actions: gdk::DragAction) {
    let imp = view.imp();
    if imp.drop_installed.replace(true) {
        return;
    }
    let target = gtk::DropTarget::new(glib::Type::INVALID, actions);
    target.set_types(types);
    let dwell: Rc<RefCell<Option<Dwell>>> = Rc::new(RefCell::new(None));
    let weak = view.downgrade();
    let spring = Rc::clone(&dwell);
    target.connect_motion(move |_, x, y| {
        let Some(view) = weak.upgrade() else {
            return gdk::DragAction::empty();
        };
        let presenter = view.imp().presenter.borrow().clone();
        let Some(presenter) = presenter else {
            return gdk::DragAction::empty();
        };
        let under = view
            .position_at(x, y)
            .filter(|p| presenter.accepts_drop(Some(*p)));
        if view.imp().drop_target.replace(under) != under {
            view.queue_draw();
        }
        // Resting on a folder opens it; crossing folders does not.
        let restart = match (spring.borrow().as_ref(), under) {
            (Some((_, at, from)), Some(p)) => {
                *at != p || (x - from.0).hypot(y - from.1) > SPRING_DRIFT
            }
            (None, Some(_)) => true,
            (_, None) => false,
        };
        if (restart || under.is_none())
            && let Some((source, _, _)) = spring.take()
        {
            big_gtk_kit::source::remove(source);
        }
        if restart && let Some(position) = under {
            let view_weak = view.downgrade();
            let slot = Rc::clone(&spring);
            let source = glib::timeout_add_local_once(SPRING_DWELL, move || {
                slot.take();
                if let Some(view) = view_weak.upgrade()
                    && view.imp().drop_target.get() == Some(position)
                {
                    view.emit_by_name::<()>("activate", &[&position]);
                }
            });
            spring.replace(Some((source, position, (x, y))));
        }
        if under.is_some() || presenter.accepts_drop(None) {
            gdk::DragAction::COPY | gdk::DragAction::MOVE
        } else {
            gdk::DragAction::empty()
        }
    });
    let weak = view.downgrade();
    let spring = Rc::clone(&dwell);
    target.connect_leave(move |_| {
        if let Some((source, _, _)) = spring.take() {
            big_gtk_kit::source::remove(source);
        }
        if let Some(view) = weak.upgrade() {
            view.imp().drop_target.set(None);
            view.queue_draw();
        }
    });
    let weak = view.downgrade();
    let spring = dwell;
    target.connect_drop(move |target, value, x, y| {
        if let Some((source, _, _)) = spring.take() {
            big_gtk_kit::source::remove(source);
        }
        let Some(view) = weak.upgrade() else {
            return false;
        };
        view.imp().drop_target.set(None);
        view.queue_draw();
        let presenter = view.imp().presenter.borrow().clone();
        let Some(presenter) = presenter else {
            return false;
        };
        let want_move = target.current_event().is_some_and(|event| {
            event
                .modifier_state()
                .contains(gdk::ModifierType::SHIFT_MASK)
        });
        let under = view
            .position_at(x, y)
            .filter(|p| presenter.accepts_drop(Some(*p)));
        presenter.receive_drop(under, value, want_move)
    });
    view.add_controller(target);
}
