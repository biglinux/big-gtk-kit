// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Sliders that leave the wheel to the page around them.
//!
//! A `GtkScale` takes every wheel turn over it, so scrolling a page moves
//! whichever slider passes under the pointer. These take the wheel only with
//! the keyboard focus, after a click or Tab (not the focus a window gives its
//! first widget as it opens), or when nothing around them scrolls; otherwise
//! the turn scrolls the page. `clippy.toml` sends every slider of the suite
//! through [`with_range`] and [`build`].

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

/// A slider from `min` to `max` in steps of `step`.
#[must_use]
pub fn with_range(orientation: gtk::Orientation, min: f64, max: f64, step: f64) -> gtk::Scale {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let scale = gtk::Scale::with_range(orientation, min, max, step);
    wheel_with_focus(&scale);
    scale
}

/// The slider `builder` describes.
#[must_use]
pub fn build(builder: gtk::builders::ScaleBuilder) -> gtk::Scale {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let scale = builder.build();
    wheel_with_focus(&scale);
    scale
}

/// Let `scale` take the wheel only while it has the focus or while no scrolled
/// window holds it in its own window or popover.
fn wheel_with_focus(scale: &gtk::Scale) {
    // The scale's own wheel controllers: GtkRange adds one.
    let controllers = scale.observe_controllers();
    let wheels: Vec<(gtk::EventController, gtk::PropagationPhase)> = (0..controllers.n_items())
        .filter_map(|at| controllers.item(at))
        .filter_map(|item| item.downcast::<gtk::EventControllerScroll>().ok())
        .map(|wheel| {
            let phase = wheel.propagation_phase();
            (wheel.upcast(), phase)
        })
        .collect();
    drop(controllers);
    // The focus the scale had as it mapped (GTK gives it to a window's first
    // focusable widget) is no click or Tab: it counts once the person presses
    // on the scale, or the focus has left it and come back.
    let given = Rc::new(Cell::new(false));
    let follow = {
        let given = Rc::clone(&given);
        Rc::new(move |scale: &gtk::Scale| {
            let takes = (scale.has_focus() && !given.get()) || !scrolled(scale.upcast_ref());
            for (wheel, phase) in &wheels {
                wheel.set_propagation_phase(if takes {
                    *phase
                } else {
                    gtk::PropagationPhase::None
                });
            }
        })
    };
    scale.connect_map({
        let (follow, given) = (Rc::clone(&follow), Rc::clone(&given));
        move |scale| {
            follow(scale);
            // In an idle: GTK focuses the window's first widget after mapping it.
            let (scale, follow, given) = (scale.downgrade(), Rc::clone(&follow), Rc::clone(&given));
            glib::idle_add_local_once(move || {
                if let Some(scale) = scale.upgrade()
                    && crate::focus::holds_focus(&scale)
                {
                    given.set(true);
                    follow(&scale);
                }
            });
        }
    });
    scale.connect_has_focus_notify({
        let (follow, given) = (Rc::clone(&follow), Rc::clone(&given));
        move |scale| {
            // `has-focus` also drops while the window is inactive, its focus
            // still on the scale.
            if !crate::focus::holds_focus(scale) {
                given.set(false);
            }
            follow(scale);
        }
    });
    let input = gtk::EventControllerLegacy::new();
    input.set_propagation_phase(gtk::PropagationPhase::Capture);
    input.connect_event(move |input, event| {
        let pressed = matches!(
            event.event_type(),
            gdk::EventType::ButtonPress | gdk::EventType::TouchBegin | gdk::EventType::KeyPress
        );
        if pressed
            && given.replace(false)
            && let Some(scale) = input.widget().and_downcast::<gtk::Scale>()
        {
            follow(&scale);
        }
        glib::Propagation::Proceed
    });
    scale.add_controller(input);
}

/// Whether a scrolled window holds `widget` within its window or popover.
fn scrolled(widget: &gtk::Widget) -> bool {
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if widget.is::<gtk::ScrolledWindow>() {
            return true;
        }
        if widget.is::<gtk::Native>() {
            return false;
        }
        parent = widget.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use big_testkit::harness::{pump_until, require_gtk_test};

    /// Whether `scale`'s own wheel controllers take a wheel turn over it.
    fn takes_wheel(scale: &gtk::Scale) -> bool {
        let controllers = scale.observe_controllers();
        let phases: Vec<gtk::PropagationPhase> = (0..controllers.n_items())
            .filter_map(|at| controllers.item(at))
            .filter_map(|item| item.downcast::<gtk::EventControllerScroll>().ok())
            .map(|wheel| wheel.propagation_phase())
            .collect();
        assert!(!phases.is_empty(), "a GtkScale has a wheel controller");
        phases
            .iter()
            .all(|phase| *phase != gtk::PropagationPhase::None)
    }

    /// In a page the wheel scrolls the page past a slider until the slider
    /// has the focus from the person, not from its window opening; a slider
    /// nothing scrolls around keeps the wheel.
    #[test]
    #[ignore = "display: the window must be active for the focus to count"]
    fn wheel_taken_only_with_focus() {
        require_gtk_test(|| {
            let scale = with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
            let button = gtk::Button::with_label("Other");
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&scale);
            content.append(&button);
            let page = gtk::ScrolledWindow::builder().child(&content).build();
            let window = gtk::Window::builder().child(&page).build();
            window.present();
            assert!(
                pump_until(|| scale.has_focus()),
                "the window's first widget"
            );
            big_testkit::harness::pump();
            assert!(!takes_wheel(&scale), "focused as the window opened");
            assert!(button.grab_focus());
            assert!(pump_until(|| button.has_focus()), "the window is active");
            assert!(!takes_wheel(&scale), "unfocused in a page");
            assert!(scale.grab_focus());
            assert!(pump_until(|| takes_wheel(&scale)), "focused");
            assert!(button.grab_focus());
            assert!(pump_until(|| !takes_wheel(&scale)), "focus moved on");
            window.destroy();

            let alone = build(gtk::Scale::builder().orientation(gtk::Orientation::Vertical));
            let window = gtk::Window::builder().child(&alone).build();
            window.present();
            assert!(pump_until(|| alone.is_mapped()));
            assert!(takes_wheel(&alone), "nothing scrolls around it");
            window.destroy();
        });
    }
}
