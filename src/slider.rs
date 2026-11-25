// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Sliders that leave the wheel to the page around them.
//!
//! A `GtkScale` takes every wheel turn over it, so scrolling a page moves
//! whichever slider passes under the pointer. These take the wheel only with
//! the keyboard focus, after a click or Tab, or when nothing around them
//! scrolls; otherwise the turn scrolls the page. `clippy.toml` sends every
//! slider of the suite through [`with_range`] and [`build`].

use gtk::prelude::*;

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
    let follow = move |scale: &gtk::Scale| {
        let takes = scale.has_focus() || !scrolled(scale.upcast_ref());
        for (wheel, phase) in &wheels {
            wheel.set_propagation_phase(if takes {
                *phase
            } else {
                gtk::PropagationPhase::None
            });
        }
    };
    let follow = std::rc::Rc::new(follow);
    scale.connect_map({
        let follow = follow.clone();
        move |scale| follow(scale)
    });
    scale.connect_has_focus_notify(move |scale| follow(scale));
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
    /// has the focus; a slider nothing scrolls around keeps the wheel.
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
            assert!(pump_until(|| scale.is_mapped()));
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
