// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! GTK4 building blocks shared by the BigLinux programs. Each one stands in for
//! a toolkit behaviour that was measured to leak, stall or look different from
//! program to program, without patching GTK or libadwaita: a fixed library
//! version leaves every workaround here idle.

pub mod accessible;
pub mod alert;
pub mod breakpoint;
pub mod css;
pub mod dialog;
pub mod menu;
pub mod password;
pub mod popover;
pub mod source;
mod surface;
pub mod switch;
pub mod tooltip;
pub mod view_switcher;

use gtk::glib;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;

/// Turn on what every program gets without changing a call site: the
/// tooltip card, the closing-dialog focus fix and the popup-parent release.
///
/// Call once, after GTK and libadwaita are initialised (an application's
/// `startup`). Idempotent.
pub fn install() {
    tooltip::install();
    dialog::install();
    surface::install();
}

/// Run `callback` with the object of every `signal` emission on `T` and its
/// subclasses, for the rest of the process.
fn on_every_emission<T: IsA<glib::Object>>(signal: &str, callback: fn(T)) {
    let object_type = T::static_type();
    // A class's signals exist once the class is initialised.
    let _class = glib::Class::<glib::Object>::from_type(object_type);
    let Some(signal) = glib::subclass::SignalId::lookup(signal, object_type) else {
        return;
    };
    unsafe extern "C" fn trampoline<T: IsA<glib::Object>>(
        _hint: *mut glib::gobject_ffi::GSignalInvocationHint,
        n_values: std::ffi::c_uint,
        values: *const glib::gobject_ffi::GValue,
        data: glib::ffi::gpointer,
    ) -> glib::ffi::gboolean {
        // SAFETY: `data` is the `fn(T)` passed below; function and data
        // pointers have the same size on every platform GTK runs on.
        let callback = unsafe { std::mem::transmute::<glib::ffi::gpointer, fn(T)>(data) };
        if n_values > 0 {
            // SAFETY: GLib hands the emission's parameters; the first is the
            // emitting instance. `glib::Value` is a transparent `GValue`.
            let value = unsafe { &*values.cast::<glib::Value>() };
            if let Ok(object) = value.get::<glib::Object>()
                && let Ok(object) = object.downcast::<T>()
            {
                callback(object);
            }
        }
        glib::ffi::GTRUE
    }
    // SAFETY: `trampoline::<T>` matches GSignalEmissionHook and reads `data`
    // back as the same `fn(T)`, which lives for the whole program.
    unsafe {
        glib::gobject_ffi::g_signal_add_emission_hook(
            signal.into_glib(),
            0,
            Some(trampoline::<T>),
            callback as *const () as glib::ffi::gpointer,
            None,
        );
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use gtk::glib;
    use gtk::prelude::*;

    /// Counts its own drops: set as qdata, it drops when the object finalizes,
    /// which a weak reference alone does not prove.
    struct Finalized(Rc<Cell<u32>>);

    impl Drop for Finalized {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    pub fn watch(object: &impl IsA<glib::Object>, count: &Rc<Cell<u32>>) {
        // SAFETY: this key is only ever written with a `Finalized`.
        unsafe { object.set_data("big-gtk-kit-test-finalized", Finalized(count.clone())) };
    }

    pub fn pump() {
        let context = glib::MainContext::default();
        for _ in 0..50 {
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn init() {
        gtk::init().expect("a display");
        adw::init().expect("libadwaita");
    }
}
