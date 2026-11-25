// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The window's `help` action, identical in every product that has one.

use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::{gio, glib};

/// Where the packages install the guide, one directory per language.
const GUIDE_DIR: &str = "/usr/share/doc/big/user";
/// Names another guide directory, for a checkout that is not installed.
const GUIDE_DIR_VARIABLE: &str = "BIG_GUIDE_DIR";

/// Install the `win.help` action, which opens the user guide at `page` (a file
/// name without extension, such as `files`).
///
/// `not_installed` and `close` are the product's translations of the alert
/// shown when the page is missing from this system.
///
/// The action holds a weak reference: it is added to the window, so a strong
/// one would keep the window alive.
pub(crate) fn install_help_action(
    window: &adw::ApplicationWindow,
    page: &'static str,
    not_installed: &str,
    close: &str,
) {
    let action = gio::SimpleAction::new("help", None);
    let window_for_alert = window.downgrade();
    let (not_installed, close) = (not_installed.to_owned(), close.to_owned());
    action.connect_activate(move |_, _| {
        if let Some(window) = window_for_alert.upgrade() {
            open_guide(&window, page, &not_installed, &close);
        }
    });
    window.add_action(&action);
}

/// Open the user guide at `page` for the person's language, or tell them over
/// `parent` that the guide is not installed, with `close` as the answer. Call
/// it from the handler of the click or key press that asked for it (see
/// [`crate::own_scope::open_uri`]).
/// The application is held until the guide is handed over, so a window that
/// closes as it asks still opens the guide.
pub fn open_guide(parent: &impl IsA<gtk::Widget>, page: &str, not_installed: &str, close: &str) {
    let path = installed_guide_page(page);
    if !path.is_file() {
        big_relm4_components::feedback::statement::tell(Some(parent), not_installed, "", close)
            .present();
        return;
    }
    let uri = gio::File::for_path(&path).uri();
    let hold = gio::Application::default().map(|application| application.hold());
    glib::spawn_future_local(async move {
        if let Err(error) = crate::own_scope::open_uri(&uri, None).await {
            log::warn!("could not open the guide at {uri}: {error}");
        }
        drop(hold);
    });
}

/// Where this system keeps the guide's `page` for the person's language.
pub(crate) fn installed_guide_page(page: &str) -> PathBuf {
    let root = std::env::var_os(GUIDE_DIR_VARIABLE)
        .map_or_else(|| PathBuf::from(GUIDE_DIR), PathBuf::from);
    guide_page(&root, &glib::language_names(), page)
}

/// The page in the guide's language for the person's: Brazilian Portuguese for
/// any `pt`, English for the rest.
fn guide_page(root: &Path, languages: &[glib::GString], page: &str) -> PathBuf {
    let language = if languages.iter().any(|language| language.starts_with("pt")) {
        "pt-BR"
    } else {
        "en"
    };
    root.join(language).join(format!("{page}.html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn languages(names: &[&str]) -> Vec<glib::GString> {
        names.iter().map(|name| (*name).into()).collect()
    }

    #[test]
    fn portuguese_gets_its_page() {
        let path = guide_page(
            Path::new("/guide"),
            &languages(&["pt_BR.UTF-8", "pt", "C"]),
            "files",
        );
        assert_eq!(path, Path::new("/guide/pt-BR/files.html"));
    }

    #[test]
    fn other_languages_get_english() {
        let path = guide_page(
            Path::new("/guide"),
            &languages(&["de_DE.UTF-8", "de", "C"]),
            "terminal",
        );
        assert_eq!(path, Path::new("/guide/en/terminal.html"));
    }
}
