// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `BigShortcutEditor`: categorized accelerator editor with capture,
//! conflict rejection, and reset.
//!
//! Run:   cargo run -p big-relm4-components --example shortcut_editor
//! Test:  cargo test -p big-relm4-components shortcut_editor
//!
//! Click "Change shortcut", press a combination, and watch the typed
//! `BigShortcutEditorOutput` land in the status label below.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use big_relm4_components::input::shortcut_editor::{
    BigShortcutEditor, BigShortcutEditorInit, BigShortcutEditorLabels, BigShortcutEditorOutput,
    BigShortcutEntry,
};
use relm4::{Component, ComponentController, Controller};

fn editor_init() -> BigShortcutEditorInit {
    BigShortcutEditorInit::new(
        "Playback shortcuts",
        vec![
            BigShortcutEntry::new("Playback", "Play / Pause", "play-pause", "space", "space"),
            BigShortcutEntry::new("Playback", "Next item", "next", "n", "n"),
            BigShortcutEntry::new("Playback", "Previous item", "previous", "p", "p"),
        ],
        BigShortcutEditorLabels::translated(str::to_owned),
    )
}

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("br.com.biglinux.example.shortcut-editor")
        .build();
    let editors = Rc::new(RefCell::new(Vec::<Controller<BigShortcutEditor>>::new()));
    let active_editors = Rc::clone(&editors);
    app.connect_activate(move |app| {
        let status = gtk::Label::builder()
            .label("No shortcut changed yet")
            .margin_bottom(12)
            .build();

        // Typed outputs arrive on a plain relm4 channel; a real app persists
        // the change and re-registers its accelerators here.
        let (sender, receiver) = relm4::channel::<BigShortcutEditorOutput>();
        let editor = BigShortcutEditor::builder()
            .launch(editor_init())
            .forward(&sender, |output| output);
        {
            let status = status.clone();
            gtk::glib::MainContext::default().spawn_local(async move {
                let receiver = receiver;
                while let Some(output) = receiver.recv().await {
                    match output {
                        BigShortcutEditorOutput::Changed(change) => {
                            status.set_label(&format!(
                                "Changed {} to {}",
                                change.action, change.accel
                            ));
                        }
                        BigShortcutEditorOutput::ConflictRejected { owner, .. } => {
                            status.set_label(&format!("Conflict: already used by {owner}"));
                        }
                        BigShortcutEditorOutput::CaptureCancelled { .. } => {
                            status.set_label("Capture cancelled");
                        }
                    }
                }
            });
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(editor.widget());
        content.append(&status);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&big_relm4_components::layout::window_handle::header_bar(
            adw::HeaderBar::builder(),
        ));
        toolbar.set_content(Some(&content));
        adw::ApplicationWindow::builder()
            .application(app)
            .title("Shortcut editor example")
            .default_width(560)
            .default_height(420)
            .content(&toolbar)
            .build()
            .present();
        active_editors.borrow_mut().push(editor);
    });
    app.run()
}
