// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Every kind of tooltip a program sets, shown through `big_gtk_kit::tooltip`.
//! `cargo run --example tooltip_demo`

use gtk::glib;
use gtk::prelude::*;

fn main() -> glib::ExitCode {
    let app = gtk::Application::new(
        Some("br.com.biglinux.GtkKitTooltipDemo"),
        Default::default(),
    );
    app.connect_startup(|_| {
        // `BIG_DEMO_DARK=1` shows the dark card without a desktop portal.
        if std::env::var_os("BIG_DEMO_DARK").is_some()
            && let Some(settings) = gtk::Settings::default()
        {
            settings.set_gtk_interface_color_scheme(gtk::InterfaceColorScheme::Dark);
        }
        big_gtk_kit::tooltip::install();
        // `BIG_DEMO_FOLLOW=1` anchors cards at the pointer, as a panel does.
        big_gtk_kit::tooltip::set_follow_pointer(std::env::var_os("BIG_DEMO_FOLLOW").is_some());
    });
    app.connect_activate(|app| {
        let row = gtk::Box::builder()
            .spacing(24)
            .margin_top(160)
            .margin_start(32)
            .margin_end(32)
            .valign(gtk::Align::Start)
            .build();

        let plain = gtk::Button::with_label("Plain");
        plain.set_tooltip_text(Some("Save the document"));
        row.append(&plain);

        let icon_only = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Open a file")
            .build();
        row.append(&icon_only);

        let card = gtk::Button::with_label("Card");
        card.set_tooltip_markup(Some("<b>Volume</b>\n50 % · Speakers"));
        big_gtk_kit::tooltip::set_icon(&card, Some("audio-volume-medium-symbolic"));
        row.append(&card);

        let counter = gtk::Button::with_label("Click me");
        counter.set_tooltip_text(Some("Clicked 0 times"));
        let clicks = std::cell::Cell::new(0);
        counter.connect_clicked(move |button| {
            clicks.set(clicks.get() + 1);
            button.set_tooltip_text(Some(&format!("Clicked {} times", clicks.get())));
        });
        row.append(&counter);

        let insensitive = gtk::Button::with_label("Off");
        insensitive.set_sensitive(false);
        insensitive.set_tooltip_text(Some("Needs a file first"));
        row.append(&insensitive);

        let inner = gtk::Button::with_label("Inside");
        inner.set_tooltip_text(Some("A tooltip inside a popover"));
        let popover = gtk::Popover::builder().child(&inner).build();
        let menu = gtk::MenuButton::builder()
            .label("Menu")
            .popover(&popover)
            .tooltip_text("More actions")
            .build();
        row.append(&menu);

        // As a seek-preview button does: its own popup replaces the tooltip.
        let preview = gtk::Button::with_label("Preview");
        preview.set_tooltip_text(Some("Hidden while the preview shows"));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter(|motion, _, _| {
            if let Some(button) = motion.widget() {
                button.set_has_tooltip(false);
            }
        });
        preview.add_controller(motion);
        row.append(&preview);

        let long = gtk::Label::new(Some("Long"));
        long.set_tooltip_text(Some(
            "A long explanation that has to wrap onto more than one line, because \
             a tooltip wider than eighty characters is hard to read at a glance.",
        ));
        row.append(&long);

        // Drawn items, as a grid view of files: one tip per half.
        let halves = gtk::DrawingArea::builder()
            .content_width(96)
            .content_height(34)
            .build();
        halves.set_draw_func(|_, cairo, width, height| {
            cairo.set_source_rgb(0.55, 0.65, 0.85);
            cairo.rectangle(0.0, 0.0, f64::from(width / 2), f64::from(height));
            cairo.fill().ok();
        });
        big_gtk_kit::tooltip::set_area_tips(&halves, |halves, x, _| {
            let half = halves.width() / 2;
            let (name, left) = if x < f64::from(half) {
                ("Left", 0)
            } else {
                ("Right", half)
            };
            let area = gtk::gdk::Rectangle::new(left, 0, half, halves.height());
            Some((format!("{name} half"), area))
        });
        row.append(&halves);

        gtk::ApplicationWindow::builder()
            .application(app)
            .title("big-gtk-kit tooltips")
            .default_width(880)
            .default_height(320)
            .child(&row)
            .build()
            .present();
    });
    app.run()
}
