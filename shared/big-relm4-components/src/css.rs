// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Stylesheets the program adds to GTK's own.

use gtk::gdk;

pub use big_gtk_kit::css::{DisplaySheet, install, provider};

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
