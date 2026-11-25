// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Title-bar handles that leave button presses to the buttons.
//!
//! A `gtk::WindowHandle` turns a double-click into maximize and a drag into a
//! window move. An insensitive button cannot be picked, so a press on it lands
//! on the handle: clicking Back more often than needed, once it has turned
//! insensitive at the top folder, maximizes the window. `clippy.toml` sends
//! every handle of the suite through [`build`], and every `adw::HeaderBar`,
//! which carries a handle of its own, through [`header_bar`].

use crate::layout::widget_container::descendants;
use std::cell::Cell;

use adw::prelude::*;
use gtk::{gdk, glib, graphene};

/// The handle `builder` describes, leaving presses on its buttons to them.
#[must_use]
pub fn build(builder: gtk::builders::WindowHandleBuilder) -> gtk::WindowHandle {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let handle = builder.build();
    keep_button_presses(&handle);
    handle
}

/// The header bar `builder` describes, its own handle leaving presses on its
/// buttons to them.
#[must_use]
pub fn header_bar(builder: adw::builders::HeaderBarBuilder) -> adw::HeaderBar {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place allowed to build it"
    )]
    let bar = builder.build();
    // libadwaita builds the bar from a template whose handle holds everything
    // else; a bar without one has no title-bar actions to keep from its buttons.
    if let Some(handle) = inner_handle(bar.upcast_ref()) {
        keep_button_presses(&handle);
    }
    bar
}

fn inner_handle(widget: &gtk::Widget) -> Option<gtk::WindowHandle> {
    descendants(widget).find_map(|widget| widget.downcast::<gtk::WindowHandle>().ok())
}

/// Keep presses on a button inside `handle`, sensitive or not, from turning
/// into title-bar actions. A first primary press still reaches the handle, so
/// a drag that starts on a button moves the window; presses between the
/// buttons reach it as before.
fn keep_button_presses(handle: &gtk::WindowHandle) {
    // The handle counts a press as the second of a double-click when it comes
    // within the double-click time of the last press the handle saw.
    let last_seen = Cell::new(None::<u32>);
    let controller = gtk::EventControllerLegacy::new();
    controller.connect_event(move |controller, event| {
        let primary = match event.event_type() {
            gdk::EventType::TouchBegin => true,
            gdk::EventType::ButtonPress => event
                .downcast_ref::<gdk::ButtonEvent>()
                .is_some_and(|press| press.button() == gdk::BUTTON_PRIMARY),
            _ => return glib::Propagation::Proceed,
        };
        let Some(handle) = controller.widget().and_downcast::<gtk::WindowHandle>() else {
            return glib::Propagation::Proceed;
        };
        let time = event.time();
        if press_is_on_button(&handle, event) {
            let double_click = handle.settings().gtk_double_click_time().unsigned_abs();
            let repeat = last_seen
                .get()
                .is_some_and(|seen| time.wrapping_sub(seen) <= double_click);
            // Secondary and middle presses open the window menu or lower the
            // window on their first press.
            if repeat || !primary {
                return glib::Propagation::Stop;
            }
        }
        last_seen.set(Some(time));
        glib::Propagation::Proceed
    });
    // A widget runs its last-added controller first, and a handled event stops
    // there, so the handle's own click and drag gestures never see the press.
    handle.add_controller(controller);
}

fn press_is_on_button(handle: &gtk::WindowHandle, event: &gdk::Event) -> bool {
    let Some(native) = handle.native() else {
        return false;
    };
    // A press in a popover bubbles up through its button to the handle, in
    // the popover's own surface coordinates.
    if event.surface() != native.surface() {
        return false;
    }
    let Some((x, y)) = event.position() else {
        return false;
    };
    let (dx, dy) = native.surface_transform();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "surface coordinates fit f32, as GTK's own graphene points"
    )]
    let in_native = graphene::Point::new((x - dx) as f32, (y - dy) as f32);
    native
        .compute_point(handle, &in_native)
        .is_some_and(|point| button_at(handle, f64::from(point.x()), f64::from(point.y())))
}

/// Whether `(x, y)`, in `handle`'s coordinates, falls on a button, counting
/// insensitive ones.
fn button_at(handle: &gtk::WindowHandle, x: f64, y: f64) -> bool {
    let handle = handle.upcast_ref::<gtk::Widget>();
    let mut widget = handle.pick(x, y, gtk::PickFlags::INSENSITIVE);
    while let Some(current) = widget {
        if &current == handle {
            return false;
        }
        if current.is::<gtk::Button>() {
            return true;
        }
        widget = current.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn center_of(widget: &impl IsA<gtk::Widget>, handle: &gtk::WindowHandle) -> (f64, f64) {
        let bounds = widget
            .compute_bounds(handle)
            .expect("allocated in the handle");
        let center = bounds.center();
        (f64::from(center.x()), f64::from(center.y()))
    }

    #[test]
    fn buttons_keep_presses_gaps_do_not() {
        big_testkit::harness::require_gtk_test(|| {
            let back = gtk::Button::from_icon_name("go-previous-symbolic");
            back.set_sensitive(false);
            let forward = gtk::Button::from_icon_name("go-next-symbolic");
            let gap = gtk::Box::builder().width_request(80).build();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.append(&back);
            row.append(&gap);
            row.append(&forward);
            let handle = build(gtk::WindowHandle::builder().child(&row));
            let window = gtk::Window::builder().child(&handle).build();
            window.present();
            big_testkit::harness::pump();

            let (x, y) = center_of(&back, &handle);
            assert!(
                button_at(&handle, x, y),
                "an insensitive button keeps its press"
            );
            let (x, y) = center_of(&forward, &handle);
            assert!(
                button_at(&handle, x, y),
                "a sensitive button keeps its press"
            );
            let (x, y) = center_of(&gap, &handle);
            assert!(!button_at(&handle, x, y), "a gap goes to the handle");
            window.destroy();
        });
    }

    /// The header bar's own handle is found and keeps presses on its buttons.
    #[test]
    fn header_bar_keeps_button_presses() {
        big_testkit::harness::require_gtk_test(|| {
            let back = gtk::Button::from_icon_name("go-previous-symbolic");
            back.set_sensitive(false);
            let bar = header_bar(adw::HeaderBar::builder());
            bar.pack_start(&back);
            let window = gtk::Window::builder().titlebar(&bar).build();
            window.present();
            big_testkit::harness::pump();

            let handle = inner_handle(bar.upcast_ref()).expect("the header bar's handle");
            let controllers = handle.observe_controllers();
            assert!(
                (0..controllers.n_items()).any(|at| controllers
                    .item(at)
                    .is_some_and(|controller| controller.is::<gtk::EventControllerLegacy>())),
                "the handle watches presses"
            );
            let (x, y) = center_of(&back, &handle);
            assert!(
                button_at(&handle, x, y),
                "an insensitive button keeps its press"
            );
            window.destroy();
        });
    }
}
