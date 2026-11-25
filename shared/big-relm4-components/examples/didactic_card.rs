// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `BigDidacticCard`: a titled card that explains one setting.
//!
//! Run:   cargo run -p big-relm4-components --example didactic_card
//! Test:  cargo test -p big-relm4-components didactic_card

use adw::prelude::*;
use big_relm4_components::layout::didactic_card::{BigDidacticCard, BigDidacticCardSpec};

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("br.com.biglinux.example.didactic-card")
        .build();
    app.connect_activate(|app| {
        // A spec carries the resource prefix for an optional illustration,
        // plus the title and the consequence-oriented description.
        let mut spec = BigDidacticCardSpec::new(
            "",
            "",
            "Didactic card",
            "Pairs a short title with the sentence that explains what the \
             setting changes. The card chrome ships with the component.",
        );
        spec.illustration_name = None;
        // The second argument is an optional trailing control (switch,
        // dropdown, ...). `None` renders a text-only card.
        let card = BigDidacticCard::new(spec, None::<&gtk::Widget>);

        let group = adw::PreferencesGroup::builder()
            .title("Didactic cards")
            .build();
        group.add(card.root());
        let page = adw::PreferencesPage::new();
        page.add(&group);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&big_relm4_components::layout::window_handle::header_bar(
            adw::HeaderBar::builder(),
        ));
        toolbar.set_content(Some(&page));
        adw::ApplicationWindow::builder()
            .application(app)
            .title("BigDidacticCard example")
            .default_width(640)
            .default_height(360)
            .content(&toolbar)
            .build()
            .present();
    });
    app.run()
}
