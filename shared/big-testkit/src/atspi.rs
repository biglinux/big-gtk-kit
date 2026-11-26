// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! This process seen from the AT-SPI bus, the way a screen reader sees it.
//!
//! Only under the real bridge (`headless-gate.sh --accessible`); under
//! the test AT context [`accessibility_bus`] finds no bus.

use std::cell::RefCell;
use std::rc::Rc;

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

/// What this process says to a screen reader while it is kept: the text of each
/// `Announcement` event the bridge sends.
pub struct Announcements {
    _subscription: gio::SignalSubscription,
    heard: Rc<RefCell<Vec<String>>>,
}

impl Announcements {
    /// Listen on `bus` (from [`accessibility_bus`]) to `application`; the
    /// events arrive while this thread's main loop runs ([`settle`]).
    #[must_use]
    pub fn listen(bus: &gio::DBusConnection, application: &str) -> Self {
        let heard = Rc::<RefCell<Vec<String>>>::default();
        let sink = Rc::clone(&heard);
        let subscription = bus.subscribe_to_signal(
            Some(application),
            Some("org.a11y.atspi.Event.Object"),
            Some("Announcement"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                // (siiva{sv}): the text is the variant after the two numbers.
                let text = signal
                    .parameters
                    .try_child_value(3)
                    .and_then(|value| value.as_variant())
                    .and_then(|text| text.get::<String>());
                if let Some(text) = text {
                    sink.borrow_mut().push(text);
                }
            },
        );
        Self {
            _subscription: subscription,
            heard,
        }
    }

    /// What was said since the last call, oldest first.
    #[must_use]
    pub fn take(&self) -> Vec<String> {
        self.heard.take()
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
