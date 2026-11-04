// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Window shadows that stop growing GTK's Cairo shadow cache.

use std::cell::Cell;

use gtk::prelude::*;
use gtk::{gio, gsk};

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    static CHECKED: Cell<bool> = const { Cell::new(false) };
}

/// The rules of libadwaita's stylesheet that `shadow.css` restates, verbatim.
const ADWAITA_RULES: [&str; 9] = [
    "window.csd { box-shadow: 0 0 14px 5px RGB(0 0 0/15%), 0 0 5px 2px RGB(0 0 0/10%), 0 0 0 1px RGB(0 0 0/5%); margin: 0px; border-radius: var(--window-radius); outline: 1px solid RGB(255 255 255/7%); outline-offset: -1px; }",
    "@media (prefers-contrast: more) { window.csd { outline-color: RGB(255 255 255/30%); box-shadow: 0 0 14px 5px RGB(0 0 0/15%), 0 0 5px 2px RGB(0 0 0/10%), 0 0 0 1px RGB(0 0 0/80%); } }",
    "window.csd:backdrop { box-shadow: 0 0 14px 5px transparent, 0 0 10px 5px RGB(0 0 0/8%), 0 0 0 1px RGB(0 0 0/5%); transition: box-shadow 200ms ease-out; }",
    "@media (prefers-contrast: more) { window.csd:backdrop { box-shadow: 0 0 14px 5px transparent, 0 0 10px 5px RGB(0 0 0/8%), 0 0 0 1px RGB(0 0 0/80%); } }",
    "window.csd.dialog.message, window.csd.messagedialog { box-shadow: 0 0 14px 2px RGB(0 0 6/3%), 0 0 5px 2px RGB(0 0 6/10%), 0 0 0 1px RGB(0 0 0/5%); }",
    "@media (prefers-contrast: more) { window.csd.dialog.message, window.csd.messagedialog { box-shadow: 0 0 14px 2px RGB(0 0 6/3%), 0 0 5px 2px RGB(0 0 6/10%), 0 0 0 1px RGB(0 0 0/80%); } }",
    "window.csd.tiled, window.csd.tiled-top, window.csd.tiled-left, window.csd.tiled-right, window.csd.tiled-bottom { --window-radius: 0px; outline: none; box-shadow: 0 0 0 1px color-mix(in srgb, currentColor var(--border-opacity), transparent), 0 0 0 20px transparent; }",
    "window.csd.tiled:backdrop, window.csd.tiled-top:backdrop, window.csd.tiled-left:backdrop, window.csd.tiled-right:backdrop, window.csd.tiled-bottom:backdrop { box-shadow: 0 0 0 1px color-mix(in srgb, currentColor var(--border-opacity), transparent), 0 0 0 20px transparent; }",
    "window.csd.maximized, window.csd.fullscreen { --window-radius: 0px; outline: none; box-shadow: none; transition: none; }",
];

/// Give libadwaita's window shadows a form whose fade reuses GTK's cached
/// corner masks, in a program that draws with Cairo. Called by
/// [`crate::install`].
///
/// GTK's Cairo renderer caches every blurred shadow corner it draws, keyed by
/// blur radius and corner size, in a table it never empties (`corner_mask_cache`
/// in `gskcairoshadow.c`, GTK 4.22 to 4.24, still so on `main`). libadwaita
/// 1.10 fades a window's shadow to a wider, softer one as the window loses the
/// focus, and every frame of that fade has a new blur and spread, so each
/// focus change adds about a dozen masks. `shadow.css` reaches the same
/// shadows by fading colours only, high contrast included. It is loaded once
/// the first window maps with a Cairo renderer, and only while libadwaita's
/// stylesheet is the one in use and still has the rules it restates.
pub(crate) fn install() {
    if INSTALLED.replace(true) {
        return;
    }
    crate::on_every_emission("map", |window: gtk::Window| {
        if CHECKED.get()
            || !window
                .renderer()
                .is_some_and(|renderer| renderer.is::<gsk::CairoRenderer>())
        {
            return;
        }
        CHECKED.set(true);
        let display = WidgetExt::display(&window);
        if !adwaita_sheet_in_use(&display) || !adwaita_has_restated_rules() {
            return;
        }
        // Its high-contrast rules answer as libadwaita's do, which writes
        // the contrast it follows into the settings.
        let provider = crate::css::following_settings(&display);
        provider.load_from_string(include_str!("shadow.css"));
        // Above libadwaita's sheet (THEME), below the program's own CSS.
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_THEME + 1,
        );
    });
}

/// Whether libadwaita draws `display` with its own stylesheet: it names the
/// theme `Adwaita-empty` when it does, and leaves `GTK_THEME` or granite's
/// theme alone otherwise (`adw-style-manager.c`, libadwaita 1.10).
fn adwaita_sheet_in_use(display: &gtk::gdk::Display) -> bool {
    gtk::Settings::for_display(display)
        .gtk_theme_name()
        .is_some_and(|name| name == "Adwaita-empty")
}

fn adwaita_has_restated_rules() -> bool {
    let Ok(sheet) = gio::resources_lookup_data(
        "/org/gnome/Adwaita/styles/gtk.css",
        gio::ResourceLookupFlags::NONE,
    ) else {
        return false;
    };
    let sheet = String::from_utf8_lossy(&sheet);
    ADWAITA_RULES.iter().all(|rule| sheet.contains(rule))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::init;

    #[gtk::test]
    #[ignore = "needs a display"]
    fn adwaita_still_has_restated_rules() {
        init();
        assert!(adwaita_has_restated_rules());
    }

    #[gtk::test]
    #[ignore = "needs a display"]
    fn adwaita_sheet_is_found_in_use() {
        init();
        let display = gtk::gdk::Display::default().expect("a display");
        // libadwaita leaves a theme named in GTK_THEME in place.
        assert_eq!(
            adwaita_sheet_in_use(&display),
            std::env::var_os("GTK_THEME").is_none()
        );
    }

    #[gtk::test]
    #[ignore = "needs a display"]
    fn sheet_parses_cleanly() {
        init();
        let provider = gtk::CssProvider::new();
        let errors = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = errors.clone();
        provider.connect_parsing_error(move |_, _, _| counted.set(counted.get() + 1));
        provider.load_from_string(include_str!("shadow.css"));
        assert_eq!(errors.get(), 0);
    }
}
