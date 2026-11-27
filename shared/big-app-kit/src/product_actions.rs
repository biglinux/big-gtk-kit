// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The window actions every product spells the same. They live on the window:
//! in a multicall host several products share one `GtkApplication`, where the
//! first product to boot would answer for the others.

use adw::prelude::*;
use gtk::gio;

/// What Help and About say about a product.
#[derive(Debug, Clone)]
pub struct BigProduct {
    /// Its name as About shows it.
    pub name: String,
    /// Its application id, which also names its icon.
    pub application_id: &'static str,
    /// Its version, `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
    /// Its page of the user guide, a file name without extension such as `files`.
    pub guide_page: &'static str,
}

impl BigProduct {
    /// Whether this system has the product's page of the guide in the
    /// person's language: the main menu offers Help only then.
    #[must_use]
    pub fn has_guide(&self) -> bool {
        crate::help_action::installed_guide_page(self.guide_page).is_file()
    }
}

/// Install `win.help` and `win.about` for `product`; `gettext` is the
/// product's, so its catalog names the missing guide and the translators.
pub fn install_product_actions(
    window: &adw::ApplicationWindow,
    product: &BigProduct,
    gettext: impl Fn(&str) -> String,
) {
    crate::help_action::install_help_action(
        window,
        product.guide_page,
        &gettext("The guide is not installed"),
        &gettext("Close"),
    );
    crate::about_action::install_about_action(
        window,
        &product.name,
        product.application_id,
        product.version,
        &gettext("translator-credits"),
    );
}

/// Install `win.{name}`, which sends `input()` to `owner`, and return it.
pub fn install_input_action<M: 'static>(
    window: &impl IsA<gio::ActionMap>,
    name: &str,
    owner: &relm4::Sender<M>,
    input: impl Fn() -> M + 'static,
) -> gio::SimpleAction {
    let action = gio::SimpleAction::new(name, None);
    let owner = owner.clone();
    action.connect_activate(move |_, _| {
        let _ = owner.send(input());
    });
    window.add_action(&action);
    action
}

/// Install `win.quit` closing this window only, for a product whose windows
/// have nothing to stop or save first.
///
/// The action holds the window weakly: it is added to the window, so a strong
/// reference would make a cycle that keeps every closed window alive.
pub fn install_window_quit(window: &adw::ApplicationWindow) {
    let action = gio::SimpleAction::new("quit", None);
    let window_for_close = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = window_for_close.upgrade() {
            window.close();
        }
    });
    window.add_action(&action);
}

/// Install `win.fullscreen`, which fills the screen with the window and gives
/// it back.
///
/// Stateful so a menu shows whether the window fills the screen, and one item
/// does both directions. The window can leave full screen without the action
/// (the compositor, or the Escape the shell offers), and the state follows it.
pub fn install_fullscreen_action(window: &adw::ApplicationWindow) {
    let action =
        gio::SimpleAction::new_stateful("fullscreen", None, &window.is_fullscreen().to_variant());
    let window_weak = window.downgrade();
    action.connect_activate(move |action, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let wanted = !window.is_fullscreen();
        window.set_fullscreened(wanted);
        action.set_state(&wanted.to_variant());
    });
    window.add_action(&action);
    let action_for_window = action;
    window.connect_fullscreened_notify(move |window| {
        action_for_window.set_state(&window.is_fullscreen().to_variant());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window that already fills the screen, as a restored one may, shows
    /// so in the menu before it changes.
    #[test]
    fn fullscreen_state_starts_from_window() {
        big_testkit::harness::require_gtk_test(|| {
            let window = adw::ApplicationWindow::builder().build();
            window.set_fullscreened(true);
            window.present();
            let filled = big_testkit::harness::pump_until(|| window.is_fullscreen());
            install_fullscreen_action(&window);
            let state = window
                .lookup_action("fullscreen")
                .and_then(|action| action.state())
                .and_then(|state| state.get::<bool>());
            window.destroy();

            assert!(filled, "the compositor did not fill the screen");
            assert_eq!(state, Some(true));
        });
    }
}
