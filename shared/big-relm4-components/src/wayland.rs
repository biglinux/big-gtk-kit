// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! wayland-client objects on GTK's own Wayland display, with a connection
//! that belongs to the module asking.
//!
//! gdk4-wayland keeps the `Connection` it makes in the `GdkDisplay`, under a
//! fixed name (`gtk-rs-wayland-display-connection-quark`). In `big-host` each
//! module carries its own copy of wayland-client, so a module handed another's
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
#[must_use]
pub fn connection(display: &WaylandDisplay) -> Option<Connection> {
    let raw = display.wl_display_raw()?.as_ptr();
    CONNECTION.with_borrow_mut(|kept| {
        if let Some((on, connection)) = kept.as_ref()
            && *on == raw as usize
        {
            return Some(connection.clone());
        }
        // SAFETY: `raw` is GTK's live `wl_display`, which outlives every
        // window of the desktop and so every object made through this.
        let backend =
            unsafe { wayland_backend::sys::client::Backend::from_foreign_display(raw.cast()) };
        let connection = Connection::from_backend(backend);
        *kept = Some((raw as usize, connection.clone()));
        Some(connection)
    })
}

/// `surface`'s `wl_surface`, on this module's connection.
#[must_use]
pub fn surface(surface: &WaylandSurface) -> Option<WlSurface> {
    let display = surface.display().downcast::<WaylandDisplay>().ok()?;
    let connection = connection(&display)?;
    let raw = surface.wl_surface_raw()?;
    // SAFETY: GTK's live `wl_surface` of `surface`, of that interface.
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), raw.as_ptr().cast()) }.ok()?;
    WlSurface::from_id(&connection, id).ok()
}
