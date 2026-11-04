// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Display-wide CSS that lives as long as a widget.

use gtk::prelude::*;

/// A provider whose `@media` queries follow the user's colour scheme,
/// contrast and motion settings on `display`, as the theme's do: an
/// application's provider answers them from its own properties.
pub(crate) fn following_settings(display: &gtk::gdk::Display) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    let settings = gtk::Settings::for_display(display);
    for (setting, preference) in [
        ("gtk-interface-color-scheme", "prefers-color-scheme"),
        ("gtk-interface-contrast", "prefers-contrast"),
        ("gtk-interface-reduced-motion", "prefers-reduced-motion"),
    ] {
        // GTK 4.20 added them; an older one keeps the defaults.
        if provider.find_property(preference).is_none() {
            continue;
        }
        settings
            .bind_property(setting, &provider, preference)
            .sync_create()
            .build();
    }
    provider
}

/// Register `provider` on `owner`'s display until `owner` is destroyed.
///
/// This scopes the registration's lifetime, not its selectors. The display
/// holds its own reference, so dropping the provider alone neither releases it
/// nor stops its rules: a window that registered one per open kept them all.
pub fn add_provider_for_widget_lifetime(
    owner: &impl IsA<gtk::Widget>,
    provider: &gtk::CssProvider,
    priority: u32,
) {
    let display = owner.display();
    gtk::style_context_add_provider_for_display(&display, provider, priority);
    let provider = provider.clone();
    owner.connect_destroy(move |_| {
        gtk::style_context_remove_provider_for_display(&display, &provider);
    });
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::watch;

    #[gtk::test]
    #[ignore = "needs a display"]
    fn media_follow_the_settings() {
        gtk::init().expect("a display");
        let display = gtk::gdk::Display::default().expect("a display");
        let provider = following_settings(&display);
        let settings = gtk::Settings::for_display(&display);
        for contrast in [
            gtk::InterfaceContrast::More,
            gtk::InterfaceContrast::NoPreference,
        ] {
            settings.set_gtk_interface_contrast(contrast);
            assert_eq!(provider.prefers_contrast(), contrast);
        }
    }

    #[gtk::test]
    #[ignore = "needs a display"]
    fn providers_finalize_with_their_widgets() {
        gtk::init().expect("a display");
        let finalized = Rc::new(Cell::new(0));
        for cycle in 1..=25 {
            let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let provider = gtk::CssProvider::new();
            provider.load_from_string(".kit-test-provider { color: red; }");
            watch(&provider, &finalized);
            add_provider_for_widget_lifetime(
                &widget,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            drop((provider, widget));
            assert_eq!(finalized.get(), cycle);
        }
    }
}
