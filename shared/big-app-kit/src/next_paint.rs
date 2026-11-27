// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Work that waits for a widget's next frame on screen.

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// Run `f` once, after the next frame `widget` paints, and disconnect.
///
/// The frame is asked for: the change `f` waits on may already be painted
/// when this runs, and a clock with nothing left to do never paints again.
/// Returns false, dropping `f`, when the widget has no frame clock: it is not
/// realized.
pub fn after_next_paint(widget: &impl IsA<gtk::Widget>, f: impl FnOnce() + 'static) -> bool {
    let Some(clock) = widget.frame_clock() else {
        return false;
    };
    let f = Cell::new(Some(f));
    let handler: Rc<Cell<Option<glib::SignalHandlerId>>> = Rc::default();
    let id = clock.connect_after_paint({
        let handler = Rc::clone(&handler);
        move |clock| {
            if let Some(id) = handler.take() {
                clock.disconnect(id);
            }
            if let Some(f) = f.take() {
                f();
            }
        }
    });
    handler.set(Some(id));
    clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paints_with_nothing_changed() {
        big_testkit::harness::require_gtk_test(|| {
            let window = gtk::Window::new();
            window.present();
            assert!(big_testkit::harness::pump_frame(&window));
            big_testkit::harness::pump();
            let ran = Rc::new(Cell::new(false));
            let painting = after_next_paint(&window, {
                let ran = Rc::clone(&ran);
                move || ran.set(true)
            });
            let painted = big_testkit::harness::pump_until(|| ran.get());
            window.destroy();

            assert!(painting);
            assert!(painted, "no frame came after the window went quiet");
        });
    }
}
