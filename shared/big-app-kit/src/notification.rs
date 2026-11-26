// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A product's notifications, signed with its own name. An application that
//! hosts several products sends for all of them under its own application id,
//! so a hosted product names itself in the notification id, `<desktop id>/<id>`,
//! which [`hosted_product`] reads back for the shell that shows it. Actions
//! still go to the application that sent the notification.

use gio::prelude::*;

/// Send `notification` as `product`, its desktop id, under `id` (a new one
/// when `None`).
pub fn send(
    app: &impl IsA<gio::Application>,
    product: &str,
    id: Option<&str>,
    notification: &gio::Notification,
) {
    if app.application_id().is_some_and(|own| own == product) {
        app.send_notification(id, notification);
        return;
    }
    let id = id.map_or_else(|| glib::uuid_string_random().to_string(), str::to_owned);
    app.send_notification(Some(&hosted_id(product, &id)), notification);
}

/// The id [`send`] gives notification `id` of `product` when another
/// application sends it.
fn hosted_id(product: &str, id: &str) -> String {
    format!("{product}/{id}")
}

/// The product a hosted notification's id names, as [`send`] wrote it;
/// `None` when it names none.
pub fn hosted_product(id: &str) -> Option<&str> {
    let (product, _) = id.split_once('/')?;
    gio::Application::id_is_valid(product).then_some(product)
}

#[cfg(test)]
mod tests {
    use super::{hosted_id, hosted_product};

    #[test]
    fn hosted_id_names_its_product() {
        let id = hosted_id("br.com.biglinux.bigterminal", "remote-edit-host:/a/b");
        assert_eq!(hosted_product(&id), Some("br.com.biglinux.bigterminal"));
        assert_eq!(hosted_product("remote-edit-host:/a/b"), None);
    }
}
