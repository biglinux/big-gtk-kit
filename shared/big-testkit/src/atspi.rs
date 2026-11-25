// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! This process seen from the AT-SPI bus, the way a screen reader sees it.
//!
//! Only under the real bridge (`headless-gate.sh --accessible`); under
//! the test AT context [`accessibility_bus`] finds no bus.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

/// Run this thread's main loop for about a quarter second, so the bridge's
/// D-Bus traffic for what just changed is sent and answered.
pub fn settle() {
    let context = glib::MainContext::default();
    for _ in 0..50 {
        context.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// `GetItems` of this process's AT-SPI cache, asked over a connection of the
/// test's own so GTK answers it from this thread's main loop.
///
/// # Errors
///
/// The D-Bus error when the cache does not answer.
pub fn cached_items(bus: &gio::DBusConnection, application: &str) -> Result<(), glib::Error> {
    glib::MainContext::default().block_on(bus.call_future(
        Some(application),
        "/org/a11y/atspi/cache",
        "org.a11y.atspi.Cache",
        "GetItems",
        None,
        None,
        gio::DBusCallFlags::NONE,
        5000,
    ))?;
    Ok(())
}

/// The accessibility bus and this process's name on it, when the real AT-SPI
/// bridge is running (`headless-gate.sh --accessible`).
pub fn accessibility_bus() -> Option<(gio::DBusConnection, String)> {
    let session = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let address = session
        .call_sync(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            "org.a11y.Bus",
            "GetAddress",
            None,
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .ok()?
        .child_value(0)
        .get::<String>()?;
    let bus = glib::MainContext::default()
        .block_on(gio::DBusConnection::for_address_future(
            &address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
        ))
        .ok()?;
    // Every application the registry knows is a child of its desktop; this
    // process is the one whose connection carries its PID.
    let call = |destination, path, interface, method, arguments: Option<glib::Variant>| {
        glib::MainContext::default()
            .block_on(bus.call_future(
                Some(destination),
                path,
                interface,
                method,
                arguments.as_ref(),
                None,
                gio::DBusCallFlags::NONE,
                5000,
            ))
            .ok()
    };
    let children = call(
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        "org.a11y.atspi.Accessible",
        "GetChildren",
        None,
    )?
    .child_value(0);
    let application = children
        .iter()
        .filter_map(|child| child.child_value(0).get::<String>())
        .find(|name| {
            call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetConnectionUnixProcessID",
                Some((name.as_str(),).to_variant()),
            )
            .and_then(|pid| pid.child_value(0).get::<u32>())
                == Some(std::process::id())
        })?;
    Some((bus, application))
}
