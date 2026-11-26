// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Stylesheets the program adds to GTK's own.

use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash, Hasher};

use gtk::gdk;
use gtk::prelude::*;

/// A provider whose `@media (prefers-color-scheme)`, `(prefers-contrast)` and
/// `(prefers-reduced-motion)` rules answer as the system does.
///
/// A provider the program makes resolves them from its own properties, light
/// and no preference until set, so its dark, high-contrast and reduced-motion
/// rules never apply. GTK's settings hold what the program renders with:
/// libadwaita writes its style manager's dark and high contrast there, above
/// what the portal says, and the portal's reduced motion arrives there too.
/// GTK binds its own theme provider to them the same way.
#[must_use]
pub fn provider() -> gtk::CssProvider {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a provider is made"
    )]
    let provider = gtk::CssProvider::new();
    // Before GTK opens its display there is nothing to follow, or to draw.
    if let Some(settings) = gtk::Settings::default() {
        for (setting, preference) in [
            ("gtk-interface-color-scheme", "prefers-color-scheme"),
            ("gtk-interface-contrast", "prefers-contrast"),
            ("gtk-interface-reduced-motion", "prefers-reduced-motion"),
        ] {
            settings
                .bind_property(setting, &provider, preference)
                .sync_create()
                .build();
        }
    }
    provider
}

thread_local! {
    /// What [`install`] added: each display with the hash of a sheet's text.
    static INSTALLED: RefCell<Vec<(gdk::Display, u64)>> = const { RefCell::new(Vec::new()) };
}

/// Add `sheet` to the default display at `priority`, once per display.
///
/// A later call with the same text and priority adds nothing: the display
/// keeps every provider it is given, and a second copy restyles every window
/// for nothing.
/// Before GTK opens a display there is nothing to add it to, and the next call
/// adds it. A sheet that changes while the program runs is a [`DisplaySheet`].
pub fn install(sheet: &str, priority: u32) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let mut hasher = DefaultHasher::new();
    (sheet, priority).hash(&mut hasher);
    let key = (display, hasher.finish());
    if INSTALLED.with_borrow(|installed| installed.contains(&key)) {
        return;
    }
    let provider = provider();
    provider.load_from_string(sheet);
    add_provider(&key.0, &provider, priority);
    INSTALLED.with_borrow_mut(|installed| installed.push(key));
}

/// A sheet on a display that its owner replaces as settings change.
///
/// The provider is made the first time [`DisplaySheet::load`] has text and is
/// reloaded in place after that, so its rules keep their place in the cascade.
/// Unchanged text is not loaded again: loading restyles every window on the
/// display.
pub struct DisplaySheet {
    priority: u32,
    loaded: RefCell<Option<LoadedSheet>>,
}

struct LoadedSheet {
    display: gdk::Display,
    provider: gtk::CssProvider,
    text: String,
}

impl DisplaySheet {
    /// A sheet that will sit at `priority` on its display.
    #[must_use]
    pub const fn new(priority: u32) -> Self {
        Self {
            priority,
            loaded: RefCell::new(None),
        }
    }

    /// Show `text` on `display`. Empty text takes the sheet off.
    pub fn load(&self, display: &gdk::Display, text: &str) {
        let current = self.loaded.borrow().as_ref().map(|loaded| {
            (
                loaded.display == *display,
                loaded.text == text,
                loaded.provider.clone(),
            )
        });
        match current {
            Some((true, true, _)) => {}
            Some((true, false, provider)) if !text.is_empty() => {
                provider.load_from_string(text);
                if let Some(loaded) = self.loaded.borrow_mut().as_mut() {
                    text.clone_into(&mut loaded.text);
                }
            }
            _ => {
                self.release();
                if text.is_empty() {
                    return;
                }
                let provider = provider();
                provider.load_from_string(text);
                add_provider(display, &provider, self.priority);
                *self.loaded.borrow_mut() = Some(LoadedSheet {
                    display: display.clone(),
                    provider,
                    text: text.to_owned(),
                });
            }
        }
    }

    /// Whether the sheet is on a display.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.loaded.borrow().is_some()
    }

    /// Take the sheet off its display, for its owner going away.
    pub fn release(&self) {
        let loaded = self.loaded.borrow_mut().take();
        if let Some(loaded) = loaded {
            gtk::style_context_remove_provider_for_display(&loaded.display, &loaded.provider);
        }
    }
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
    add_provider(&display, provider, priority);
    let provider = provider.clone();
    owner.connect_destroy(move |_| {
        gtk::style_context_remove_provider_for_display(&display, &provider);
    });
}

fn add_provider(display: &gdk::Display, provider: &gtk::CssProvider, priority: u32) {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a provider joins a display"
    )]
    gtk::style_context_add_provider_for_display(display, provider, priority);
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::testing::{pump, watch};

    #[gtk::test]
    #[ignore = "needs a display"]
    fn media_follow_the_settings() {
        gtk::init().expect("a display");
        let provider = provider();
        let settings = gtk::Settings::default().expect("a display");
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
            let provider = provider();
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

    /// The same text twice adds one provider, at each priority.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn installs_once_per_display() {
        gtk::init().expect("a display");
        let sheet = "label.install-probe { color: rgb(255, 0, 0); }";
        let count = || INSTALLED.with_borrow(Vec::len);
        let before = count();
        install(sheet, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        install(sheet, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        assert_eq!(count(), before + 1);
        install(sheet, gtk::STYLE_PROVIDER_PRIORITY_USER);
        assert_eq!(count(), before + 2, "another priority is another sheet");
        let label = gtk::Label::builder()
            .label("probe")
            .css_classes(["install-probe"])
            .build();
        let window = gtk::Window::builder().child(&label).build();
        window.present();
        pump();
        assert!(label.color().red() > 0.99);
        window.destroy();
    }

    /// The same text keeps the provider it has; other text reloads it in
    /// place, and empty text takes it off.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn display_sheet_reloads_in_place() {
        gtk::init().expect("a display");
        let display = gdk::Display::default().expect("a display");
        let sheet = DisplaySheet::new(gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let provider = || {
            sheet
                .loaded
                .borrow()
                .as_ref()
                .map(|loaded| loaded.provider.clone())
        };
        sheet.load(&display, "label { color: red; }");
        let first = provider();
        assert!(first.is_some());
        sheet.load(&display, "label { color: red; }");
        assert_eq!(provider(), first);
        sheet.load(&display, "label { color: blue; }");
        assert_eq!(provider(), first);
        assert_eq!(
            sheet
                .loaded
                .borrow()
                .as_ref()
                .map(|loaded| loaded.text.clone()),
            Some("label { color: blue; }".to_owned())
        );
        sheet.load(&display, "");
        assert!(provider().is_none());
        sheet.release();
    }
}
