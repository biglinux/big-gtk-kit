// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! wayland-client objects on GTK's own Wayland display, with a connection
//! that belongs to the module asking.
//!
//! gdk4-wayland keeps the `Connection` it makes in the `GdkDisplay`, under a
//! fixed name (`gtk-rs-wayland-display-connection-quark`). In a process that
//! loads modules as shared libraries, each module carries its own copy of
//! wayland-client, so a module handed another's
//! `Connection` dispatches its objects with the wrong dispatcher and panics
//! where unwinding is impossible. Here each module keeps its own connection,
//! in a static of its own copy of this crate.

use std::cell::RefCell;

use gdk4_wayland::prelude::WaylandSurfaceExtManual;
use gdk4_wayland::{WaylandDisplay, WaylandSurface};
use gtk::prelude::*;
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

thread_local! {
    /// The display this module's connection was made on, and the connection.
    static CONNECTION: RefCell<Option<(usize, Connection)>> = const { RefCell::new(None) };
}

/// This module's connection on `display`, made the first time it is asked.
///
/// # Safety
///
/// The connection and every object made through it are used only while
/// `display` is open: closing it disconnects the `wl_display` the connection
/// wraps, and a clone kept past that reaches freed memory.
#[must_use]
pub unsafe fn connection(display: &WaylandDisplay) -> Option<Connection> {
    let raw = display.wl_display_raw()?.as_ptr();
    CONNECTION.with_borrow_mut(|kept| {
        if let Some((on, connection)) = kept.as_ref()
            && *on == raw as usize
        {
            return Some(connection.clone());
        }
        // SAFETY: `raw` is GTK's live `wl_display`; the caller keeps what it
        // makes from outliving the display.
        let backend =
            unsafe { wayland_backend::sys::client::Backend::from_foreign_display(raw.cast()) };
        let connection = Connection::from_backend(backend);
        *kept = Some((raw as usize, connection.clone()));
        Some(connection)
    })
}

/// `surface`'s `wl_surface`, on this module's connection.
///
/// # Safety
///
/// The proxy is used only while GTK's `surface` lives (its window mapped):
/// it names GTK's own `wl_surface`, which nothing marks dead once GTK
/// destroys it. [`connection`]'s rule holds for the display as well.
#[must_use]
pub unsafe fn surface(surface: &WaylandSurface) -> Option<WlSurface> {
    let display = surface.display().downcast::<WaylandDisplay>().ok()?;
    // SAFETY: the caller keeps the display open while it uses the proxy.
    let connection = unsafe { connection(&display) }?;
    let raw = surface.wl_surface_raw()?;
    // SAFETY: GTK's live `wl_surface` of `surface`, of that interface; the
    // caller uses the proxy only while it lives.
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), raw.as_ptr().cast()) }.ok()?;
    WlSurface::from_id(&connection, id).ok()
}
