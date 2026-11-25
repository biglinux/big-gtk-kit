// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Run one of the BigLinux desktop's `GApplication` actions from another
//! process, as its menus do (`open-widget-settings`, `set-wallpaper`).

use gio::prelude::*;

/// The names the desktop's actions answer on, in the order they are asked.
/// A BigLinux session runs the shell as a `big-host` module, so its actions are
/// the host's and the bus holds no `br.com.biglinux.BigShell` at all; the shell
/// started on its own holds only that one.
const DESKTOP_NAMES: [&str; 2] = ["br.com.biglinux.BigShell", "br.com.biglinux.bighost"];

/// [`activate_desktop_action`] waiting for the answer on this thread, for a
/// command-line tool without a main loop.
///
/// # Errors
///
/// As [`activate_desktop_action`].
pub fn activate_desktop_action_sync(action: &str, parameter: &str) -> Result<(), glib::Error> {
    glib::MainContext::new().block_on(activate_desktop_action(action, parameter))
}

/// Run `action` with the string `parameter` on the running desktop.
///
/// # Errors
///
/// The session bus is missing, or the last name asked refused; an error
/// matching [`gio::DBusError::ServiceUnknown`] means no desktop is running.
pub async fn activate_desktop_action(action: &str, parameter: &str) -> Result<(), glib::Error> {
    let connection = gio::bus_get_future(gio::BusType::Session).await?;
    let mut refused = None;
    for name in DESKTOP_NAMES {
        let answer = connection
            .call_future(
                Some(name),
                &object_path(name),
                "org.gtk.Actions",
                "Activate",
                Some(&payload(action, parameter)),
                None,
                gio::DBusCallFlags::NONE,
                ANSWER_TIMEOUT_MS,
            )
            .await;
        match answer {
            Ok(_) => return Ok(()),
            Err(error) => refused = Some(error),
        }
    }
    refused.map_or(Ok(()), Err)
}

const ANSWER_TIMEOUT_MS: i32 = 5_000;

/// `GApplication` exports its actions at the path its id spells.
fn object_path(name: &str) -> String {
    format!("/{}", name.replace('.', "/"))
}

/// `Activate(sava{sv})`, the parameter built as a variant so a file or page
/// name is never read as anything else.
fn payload(action: &str, parameter: &str) -> glib::Variant {
    let arguments = glib::Variant::array_from_iter_with_type(
        glib::VariantTy::VARIANT,
        [glib::Variant::from_variant(&parameter.to_variant())],
    );
    glib::Variant::tuple_from_iter([
        action.to_variant(),
        arguments,
        glib::VariantDict::new(None).end(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_has_the_actions_signature() {
        let call = payload("set-wallpaper", "/home/a 'b'.png");
        assert_eq!(call.type_().as_str(), "(sava{sv})");
        assert_eq!(call.child_value(0).str(), Some("set-wallpaper"));
        let parameter = call.child_value(1).child_value(0).as_variant();
        assert_eq!(
            parameter.as_ref().and_then(glib::Variant::str),
            Some("/home/a 'b'.png")
        );
        assert_eq!(
            object_path("br.com.biglinux.bighost"),
            "/br/com/biglinux/bighost"
        );
    }
}
