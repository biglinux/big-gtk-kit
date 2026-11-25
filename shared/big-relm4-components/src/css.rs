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
/// A later call with the same text adds nothing: the display keeps every
/// provider it is given, and a second copy restyles every window for nothing.
/// Before GTK opens a display there is nothing to add it to, and the next call
/// adds it. A sheet that changes while the program runs is a [`DisplaySheet`].
pub fn install(sheet: &str, priority: u32) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let mut hasher = DefaultHasher::new();
    sheet.hash(&mut hasher);
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

fn add_provider(display: &gdk::Display, provider: &gtk::CssProvider, priority: u32) {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a provider joins a display"
    )]
    gtk::style_context_add_provider_for_display(display, provider, priority);
}

/// `color` as the `#rrggbb` a stylesheet, a Pango attribute or a saved setting
/// takes: each channel clamped to 0..=1 and rounded, the alpha left out.
#[must_use]
pub fn rgb_hex(color: &gdk::RGBA) -> String {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255 before the cast"
    )]
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.red()),
        channel(color.green()),
        channel(color.blue())
    )
}

#[cfg(test)]
mod tests {
    use adw::prelude::*;
    use big_testkit::harness::{pump_until, require_gtk_test};
    use gtk::gio;

    const SHEET: &str = "
@media (prefers-color-scheme: dark) { label.media-probe-dark { color: rgb(255, 0, 0); } }
@media (prefers-contrast: more) { label.media-probe-contrast { color: rgb(255, 0, 0); } }
@media (prefers-reduced-motion: reduce) { label.media-probe-motion { color: rgb(255, 0, 0); } }
";

    /// A label under [`SHEET`], and whether its rule is the one drawing it.
    fn probe(class: &str) -> (gtk::Window, impl Fn() -> bool) {
        let provider = super::provider();
        provider.load_from_string(SHEET);
        let label = gtk::Label::builder()
            .label("probe")
            .css_classes([class])
            .build();
        let window = gtk::Window::builder().child(&label).build();
        big_gtk_kit::css::add_provider_for_widget_lifetime(
            &window,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        window.present();
        let red = move || {
            let color = label.color();
            color.red() > 0.99 && color.green() < 0.01
        };
        (window, red)
    }

    #[test]
    fn dark_rule_follows_scheme() {
        require_gtk_test(|| {
            adw::init().expect("libadwaita");
            let style = adw::StyleManager::default();
            let (window, red) = probe("media-probe-dark");
            style.set_color_scheme(adw::ColorScheme::ForceDark);
            let dark = pump_until(&red);
            style.set_color_scheme(adw::ColorScheme::ForceLight);
            let light = !pump_until(|| !red());
            style.set_color_scheme(adw::ColorScheme::Default);
            window.destroy();
            assert!(dark, "the dark rule did not apply under dark");
            assert!(!light, "the dark rule applied under light");
        });
    }

    /// The key the Accessibility settings' "High contrast" switch writes.
    #[test]
    fn contrast_rule_follows_high_contrast() {
        require_gtk_test(|| {
            assert_eq!(
                std::env::var("GSETTINGS_BACKEND").as_deref(),
                Ok("memory"),
                "this test writes the system's high contrast; run it where GSettings live in memory"
            );
            adw::init().expect("libadwaita");
            let style = adw::StyleManager::default();
            let a11y = gio::Settings::new("org.gnome.desktop.a11y.interface");
            let (window, red) = probe("media-probe-contrast");
            let plain = !pump_until(|| !red());
            a11y.set_boolean("high-contrast", true)
                .expect("a writable high-contrast key");
            assert!(
                pump_until(|| style.is_high_contrast()),
                "libadwaita did not read high contrast from GSettings (a Settings portal answered?)"
            );
            let more = pump_until(&red);
            a11y.reset("high-contrast");
            window.destroy();
            assert!(
                !plain,
                "the high-contrast rule applied without high contrast"
            );
            assert!(
                more,
                "the high-contrast rule did not apply under high contrast"
            );
        });
    }

    /// The reduced motion the session's Settings portal gives GTK, which
    /// "Fewer animations" turns on.
    #[test]
    fn motion_rule_follows_reduced_motion() {
        require_gtk_test(|| {
            let settings = gtk::Settings::default().expect("GTK's settings");
            let before = settings.gtk_interface_reduced_motion();
            let (window, red) = probe("media-probe-motion");
            settings.set_gtk_interface_reduced_motion(gtk::ReducedMotion::NoPreference);
            let plain = !pump_until(|| !red());
            settings.set_gtk_interface_reduced_motion(gtk::ReducedMotion::Reduce);
            let reduced = pump_until(&red);
            settings.set_gtk_interface_reduced_motion(before);
            window.destroy();
            assert!(
                !plain,
                "the reduced-motion rule applied without reduced motion"
            );
            assert!(
                reduced,
                "the reduced-motion rule did not apply under reduced motion"
            );
        });
    }

    /// The same text twice adds one provider; a display that is not open yet
    /// has nothing added.
    #[test]
    fn installs_once_per_display() {
        require_gtk_test(|| {
            let sheet = "label.install-probe { color: rgb(255, 0, 0); }";
            let count = || super::INSTALLED.with_borrow(Vec::len);
            let before = count();
            super::install(sheet, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            super::install(sheet, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            assert_eq!(count(), before + 1);
            let label = gtk::Label::builder()
                .label("probe")
                .css_classes(["install-probe"])
                .build();
            let window = gtk::Window::builder().child(&label).build();
            window.present();
            assert!(pump_until(|| label.color().red() > 0.99));
            window.destroy();
        });
    }

    /// The same text keeps the provider it has; other text reloads it in
    /// place, and empty text takes it off.
    #[test]
    fn display_sheet_reloads_in_place() {
        require_gtk_test(|| {
            let display = gtk::gdk::Display::default().expect("a display");
            let sheet = super::DisplaySheet::new(gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
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
        });
    }

    /// The alpha is left out, channels outside 0..=1 are clamped, and a value
    /// read back from `#444444` (0x44 / 255) gives `#444444` again.
    #[test]
    fn rgb_hex_drops_alpha_and_clamps() {
        let hex =
            |red, green, blue, alpha| super::rgb_hex(&gtk::gdk::RGBA::new(red, green, blue, alpha));
        assert_eq!(hex(0.0, 0.0, 0.0, 1.0), "#000000");
        assert_eq!(hex(1.0, 1.0, 1.0, 0.5), "#ffffff");
        assert_eq!(hex(0.2667, 0.2667, 0.2667, 1.0), "#444444");
        assert_eq!(hex(1.5, -0.5, 0.5, 1.0), "#ff0080");
    }
}
