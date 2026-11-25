// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! AppShell boot for BigLinux GTK/libadwaita apps.
//!
//! The boot layer from `docs/contracts/FRAMEWORK.md`: one `adw::Application`
//! per application id, every activation's open paths delivered to the app,
//! gettext and display CSS set up before the first window, and libadwaita
//! dark-mode changes. Window trees, chrome and settings belong to the apps and
//! `big-workspace`. Framework crates expose msgids; apps translate them.
//!
//! [`AppShell::run`] enters `GApplication` through a native-argument bridge
//! (argv bytes reach every activation unchanged) and never calls
//! `RelmApp::run`, so `Component::shutdown` is not guaranteed to run: roots
//! finish shutdown and durable work through typed messages while the GTK loop
//! is still active.
//!
//! `desktop_action` runs one of the BigLinux desktop's actions from another
//! process ([`activate_desktop_action`]).
//!
//! # Example
//!
//! ```no_run
//! # use big_app_kit::{AppShell, AppShellActivationError, AppShellActivationRequest,
//! #     AppShellComponent, AppShellContext, AppShellSpec};
//! # use relm4::{adw, Component, ComponentParts, ComponentSender};
//! # struct DemoRoot;
//! # impl Component for DemoRoot {
//!     type Init = ();
//!     type Input = ();
//!     type Output = ();
//!     type CommandOutput = ();
//!     type Root = adw::ApplicationWindow;
//!     type Widgets = ();
//! #   fn init_root() -> Self::Root {
//!         adw::ApplicationWindow::builder().title("Demo").build()
//! #   }
//! #   fn init(_: (), _: Self::Root, _: ComponentSender<Self>) -> ComponentParts<Self> {
//!         ComponentParts { model: Self, widgets: () }
//! #   }
//! # }
//! # impl AppShellComponent for DemoRoot {
//! #   fn init_for_activation(_: &AppShellContext, request: AppShellActivationRequest)
//! #       -> Result<Self::Init, AppShellActivationError> {
//!         request.succeed();
//!         Ok(())
//! #   }
//! #   fn input_for_activation(request: AppShellActivationRequest)
//! #       -> Result<Self::Input, AppShellActivationError> {
//!         request.succeed();
//!         Ok(())
//! #   }
//! # }
//!
//! let _exit_code = AppShell::run::<DemoRoot>(
//!     AppShellSpec::new("br.com.biglinux.AppKitDemo"),
//! );
//! ```

mod about_action;
mod activation_request;
mod allocator;
mod app_shell;
pub mod crash_marker;
mod desktop_action;
pub mod early_options;
mod gettext_init;
mod help_action;
mod logging;
pub mod memory_pressure;
mod next_paint;
pub mod notification;
pub mod own_scope;
mod product_actions;
pub mod session_runtime;
pub mod taskbar_progress;
mod window_accelerators;
mod window_set;
pub mod window_size;

pub use gettextrs::{dgettext, dngettext, gettext, ngettext};

/// Mark a literal for extraction without translating it here.
///
/// Const tables hold their strings long before any catalog is loaded, and
/// `xgettext` only ever sees literals: without this marker the row titles,
/// menu labels and shortcut names in those tables never reach a translator,
/// however many `gettext` calls read them later.
#[must_use]
pub const fn msgid(text: &'static str) -> &'static str {
    text
}

/// Plural counterpart of [`msgid`], for the singular and plural forms a
/// table holds until `ngettext` picks one by count.
#[must_use]
pub const fn msgid_plural(
    singular: &'static str,
    plural: &'static str,
) -> (&'static str, &'static str) {
    (singular, plural)
}
pub use {
    about_action::about_dialog,
    activation_request::{AppShellActivation, AppShellActivationError, AppShellActivationRequest},
    app_shell::{
        AppShell, AppShellActivationOutcome, AppShellComponent, AppShellContext, AppShellEarlyCli,
        AppShellSpec, AppShellSpecError, install_focus_ring,
    },
    desktop_action::{activate_desktop_action, activate_desktop_action_sync},
    gettext_init::GettextInit,
    help_action::open_guide,
    logging::init_logging,
    next_paint::after_next_paint,
    product_actions::{
        BigProduct, install_fullscreen_action, install_input_action, install_product_actions,
        install_window_quit,
    },
    window_accelerators::{
        connect_activated, publish_while_active as publish_accelerators_while_active,
    },
    window_set::{BigSecondaryWindow, BigWindowSet, BigWorkspaceWindow},
};

/// Bind a product's own message catalog inside a shared host process.
///
/// `setlocale` and `textdomain` are process-global, so in a multicall host the
/// last product to claim the domain would answer for every other one. Resolving
/// through `dgettext` keeps each product reading its own catalog no matter who
/// booted the process, and costs nothing in a standalone binary.
///
/// Invoke in the product's `i18n` module with the path to its domain constant,
/// and add `, plural` when the product also counts things:
///
/// ```ignore
/// big_app_kit::product_catalog!(crate::app::GETTEXT_DOMAIN, plural);
/// ```
#[macro_export]
macro_rules! product_catalog {
    ($domain:path) => {
        /// Translate one message from this product's catalog.
        pub(crate) fn gettext(msgid: &str) -> String {
            $crate::dgettext($domain, msgid)
        }

        /// Translate a message whose wording depends on what it describes,
        /// such as the gender a word takes; untranslated, it is `msgid`.
        pub(crate) fn pgettext(context: &str, msgid: &str) -> String {
            let key = format!("{context}\u{4}{msgid}");
            let translation = $crate::dgettext($domain, key.as_str());
            if translation == key {
                msgid.to_owned()
            } else {
                translation
            }
        }
    };
    ($domain:path, plural) => {
        $crate::product_catalog!($domain);

        /// Translate a counted message from this product's catalog.
        pub(crate) fn ngettext(singular: &str, plural: &str, n: u32) -> String {
            $crate::dngettext($domain, singular, plural, n)
        }
    };
}
