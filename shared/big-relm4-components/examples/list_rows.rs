// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Compact list rows: `BigButtonStripRow`, beside the libadwaita row it sits
//! next to.
//!
//! Run:   cargo run -p big-relm4-components --example list_rows
//! Test:  cargo test -p big-relm4-components button_strip_row

use adw::prelude::*;
use big_relm4_components::list::button_strip_row::{BigButtonStripRow, BigButtonStripRowSpec};

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("br.com.biglinux.example.list-rows")
        .build();
    app.connect_activate(|app| {
        let group = adw::PreferencesGroup::builder()
            .title("List rows")
            .description("Dense rows for preference lists and inline choices.")
            .build();

        // An inline choice is an `AdwComboRow` that shows its choices whole.
        let scheme = big_relm4_components::input::dropdown::build(
            adw::ComboRow::builder()
                .title("Color scheme")
                .subtitle("Applies to new windows only.")
                .model(&gtk::StringList::new(&["Follow system", "Light", "Dark"]))
                .selected(1),
        );
        group.add(&scheme);

        let mut strip_spec = BigButtonStripRowSpec::new("Profile actions");
        strip_spec.subtitle = Some("Import, export, or reset the profile.".into());
        strip_spec.prefix_icon_name = Some("document-save-symbolic".into());
        let strip = BigButtonStripRow::new(strip_spec);
        for label in ["Import…", "Export…", "Reset"] {
            let button = gtk::Button::builder()
                .label(label)
                .valign(gtk::Align::Center)
                .build();
            button.add_css_class("flat");
            strip.root().add_suffix(&button);
        }
        group.add(strip.root());

        let page = adw::PreferencesPage::new();
        page.add(&group);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&big_relm4_components::layout::window_handle::header_bar(
            adw::HeaderBar::builder(),
        ));
        toolbar.set_content(Some(&page));
        adw::ApplicationWindow::builder()
            .application(app)
            .title("List rows example")
            .default_width(640)
            .default_height(400)
            .content(&toolbar)
            .build()
            .present();
    });
    app.run()
}
