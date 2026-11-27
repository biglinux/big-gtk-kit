// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The window's `about` action and its dialog, identical in every product
//! that has one.

use adw::prelude::*;
use big_relm4_components::feedback::links::expose_links;
use big_relm4_components::layout::widget_container::children;
use gtk::gio;

/// The msgid a product's catalog translates into its translators' names.
const TRANSLATOR_CREDITS: &str = "translator-credits";

/// Install the `win.about` action, which opens this product's About dialog.
///
/// `application_name` is the `Name` of the product's `.desktop` entry.
/// `version` is the calling crate's `env!("CARGO_PKG_VERSION")`: read here it
/// would report this kit's version instead of the product's. The release
/// channel, when the build sets one, follows it: "0.1.0 (alpha)".
/// `translator_credits` is the product's own `gettext("translator-credits")`;
/// a catalog that leaves it untranslated shows no credits.
///
/// The action holds a weak reference: it is added to the window, so a strong
/// one would keep the window alive.
pub(crate) fn install_about_action(
    window: &adw::ApplicationWindow,
    application_name: &str,
    application_icon: &str,
    version: &str,
    translator_credits: &str,
) {
    let action = gio::SimpleAction::new("about", None);
    let window_for_dialog = window.downgrade();
    let application_name = application_name.to_owned();
    let application_icon = application_icon.to_owned();
    let version = version.to_owned();
    let translator_credits = translator_credits.to_owned();
    // Made on the first opening and presented again after: each dialog made
    // leaves its breakpoints, and with them its whole widget tree, behind for
    // the life of the process. Under a screen reader each reopening logs one
    // `gtk_at_context_set_accessible_role` critical: the AT-SPI cache read
    // realizes the closed dialog's contexts, and `adw_dialog_present`
    // re-roots them (GTK 4.22, libadwaita 1.9), whose root handler sets a role
    // on a realized context. The log line is harmless and cheaper than the leak.
    let dialog = std::cell::OnceCell::new();
    action.connect_activate(move |_, _| {
        let Some(window) = window_for_dialog.upgrade() else {
            return;
        };
        dialog
            .get_or_init(|| {
                about_dialog(
                    &application_name,
                    &application_icon,
                    &version,
                    &translator_credits,
                )
            })
            .present(Some(&window));
    });
    window.add_action(&action);
}

/// A product's About dialog, as `win.about` opens it, for a window without
/// that action. `application_name` is the `Name` of its `.desktop` entry,
/// `version` the caller's `env!("CARGO_PKG_VERSION")` and
/// `translator_credits` its own `gettext("translator-credits")`. Keep the
/// dialog and present it again: each one made stays alive with the process.
#[must_use]
pub fn about_dialog(
    application_name: &str,
    application_icon: &str,
    version: &str,
    translator_credits: &str,
) -> adw::AboutDialog {
    let version = crate::early_options::release(version);
    // Every product that installs this action is GPL-3.0-or-later and shares
    // the suite's site and issue tracker (its metainfo).
    let dialog = adw::AboutDialog::builder()
        .application_name(application_name)
        .application_icon(application_icon)
        .developer_name("BigLinux")
        .version(&version)
        .website("https://www.biglinux.com.br/")
        .issue_url("https://github.com/biglinux/big/issues")
        .license_type(gtk::License::Gpl30)
        .build();
    if let Some(credits) = credits(translator_credits) {
        dialog.set_translator_credits(credits);
    }
    // The Legal page links the license from a label. Without `expose_links` a
    // screen reader reading that page leaves the label in GTK's AT-SPI cache
    // after the dialog closes, and its next read of the cache aborts the program.
    // The walk sees only the labels that exist now, so it runs after every
    // setter that adds to the Legal page. Until presented, the content hangs
    // from `child` only.
    let mut pending: Vec<gtk::Widget> = dialog.child().into_iter().collect();
    while let Some(widget) = pending.pop() {
        if let Some(label) = widget.downcast_ref::<gtk::Label>()
            && label.uses_markup()
            && label.label().contains("<a ")
        {
            expose_links(label);
        }
        for next in children(&widget) {
            pending.push(next);
        }
    }
    dialog
}

/// The credits to show, or `None` when the catalog handed the msgid back.
fn credits(translated: &str) -> Option<&str> {
    let translated = translated.trim();
    (!translated.is_empty() && translated != TRANSLATOR_CREDITS).then_some(translated)
}

#[cfg(test)]
mod tests {
    use adw::prelude::*;
    use big_relm4_components::layout::widget_container::children;
    use big_testkit::atspi::{accessibility_bus, cached_items, settle};
    use big_testkit::harness::pump;
    use gtk::{gio, glib};

    use super::{about_dialog, credits};

    /// Ask for the children of every node under `path`, as a screen reader
    /// walking the window does: what makes GTK register each one as it goes.
    fn read_tree(bus: &gio::DBusConnection, application: &str, path: &str) {
        let Ok(reply) = glib::MainContext::default().block_on(bus.call_future(
            Some(application),
            path,
            "org.a11y.atspi.Accessible",
            "GetChildren",
            None,
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )) else {
            return;
        };
        for child in reply.child_value(0).iter() {
            if let (Some(name), Some(child_path)) = (
                child.child_value(0).get::<String>(),
                child.child_value(1).get::<glib::variant::ObjectPath>(),
            ) && name == application
            {
                read_tree(bus, application, child_path.as_str());
            }
        }
    }

    #[test]
    #[ignore = "other-display: needs the AT-SPI bridge: tools/headless-gate.sh --accessible cargo test -p big-app-kit -- --ignored"]
    fn closed_about_keeps_cache() {
        big_testkit::harness::require_gtk_test(|| {
            let _ = adw::init();
            big_testkit::harness::disable_animations();
            // An adw window, so the dialog opens as a sheet inside it as in a product.
            let window = adw::Window::new();
            window.present();
            settle();
            let (bus, application) =
                accessibility_bus().expect("the AT-SPI bridge (headless-gate.sh --accessible)");
            // Kept across openings, as the action keeps it.
            let dialog = about_dialog("BigTest", "big-test", "1.0", "");
            for _ in 0..50 {
                dialog.present(Some(&window));
                settle();
                let mut pending = vec![dialog.clone().upcast::<gtk::Widget>()];
                while let Some(widget) = pending.pop() {
                    if let Some(view) = widget.downcast_ref::<adw::NavigationView>()
                        && view.visible_page_tag().as_deref() != Some("legal")
                    {
                        view.push_by_tag("legal");
                    }
                    for next in children(&widget) {
                        pending.push(next);
                    }
                }
                pump();
                read_tree(&bus, &application, "/org/a11y/atspi/accessible/root");
                dialog.close();
                pump();
                cached_items(&bus, &application).expect("cache answers after the About closed");
            }
            window.destroy();
        });
    }

    #[test]
    fn untranslated_credits_are_hidden() {
        assert_eq!(credits("translator-credits"), None);
        assert_eq!(credits(""), None);
        assert_eq!(
            credits("Ana <ana@example.org>"),
            Some("Ana <ana@example.org>")
        );
    }
}
