// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The question before a server's host key is trusted: the server's own
//! fingerprints, to compare with what its administrator gives.

use adw::prelude::*;

/// Why the key is being looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyQuestion {
    /// This computer has never connected to the server.
    New,
    /// The server shows another key than the one this computer knows.
    Changed,
}

/// Ask whether `fingerprints` are `server`'s keys; `accept` runs on yes.
///
/// Cancel is the default: Enter trusts no key. For a changed key yes is the
/// destructive answer, since it replaces what this computer knew.
pub fn ask_to_trust_host_key(
    parent: &impl IsA<gtk::Widget>,
    server: &str,
    fingerprints: &[String],
    question: HostKeyQuestion,
    translate: impl Fn(&'static str) -> String,
    accept: impl FnOnce() + 'static,
) {
    let (heading, explanation, answer, appearance) = match question {
        HostKeyQuestion::New => (
            translate("Trust {server}?"),
            translate(
                "This computer has not connected to {server} before. Compare these fingerprints with the ones its administrator gives you, and connect only if they match.",
            ),
            translate("Trust and Connect"),
            adw::ResponseAppearance::Default,
        ),
        HostKeyQuestion::Changed => (
            translate("The identity of {server} changed"),
            translate(
                "{server} shows a key this computer does not know for it. A reinstalled server does this, and so does someone pretending to be it. Connect only if its administrator confirms these fingerprints.",
            ),
            translate("Remove Old Key and Connect"),
            adw::ResponseAppearance::Destructive,
        ),
    };
    // One key a line, as written: left aligned, in a fixed width, selectable
    // to copy, and broken only between characters, never with a hyphen that
    // is not part of the key.
    let keys = gtk::Label::builder()
        .label(fingerprints.join("\n"))
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::Char)
        .xalign(0.0)
        .css_classes(["monospace"])
        .build();
    let no_hyphens = gtk::pango::AttrList::new();
    no_hyphens.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
    keys.set_attributes(Some(&no_hyphens));
    let heading = heading.replace("{server}", server);
    let cancel = translate("Cancel");
    super::confirm::ask(
        Some(parent),
        &heading,
        &explanation.replace("{server}", server),
        &answer,
        &cancel,
        appearance,
        Some(keys.upcast_ref()),
        |trust| {
            if trust {
                accept();
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use big_testkit::harness::pump_until;

    /// The keys are text to copy, one a line and never hyphenated, and the
    /// keyboard starts on Cancel.
    #[test]
    fn fingerprints_are_text_to_copy() {
        big_testkit::harness::require_gtk_test(|| {
            let window = adw::Window::new();
            window.present();
            let fingerprints = [
                "ED25519 SHA256:uNiVztksCQmyhFl7l7YAqCUpXy6ExuyLvqBujQXVUpK".to_owned(),
                "RSA SHA256:ZIDTMczCvj3tD2s".to_owned(),
            ];
            ask_to_trust_host_key(
                &window,
                "files.example.org",
                &fingerprints,
                HostKeyQuestion::New,
                str::to_owned,
                || {},
            );
            let dialog = big_gtk_kit::widget_tree::descendants(window.upcast_ref::<gtk::Widget>())
                .find_map(|widget| widget.downcast::<adw::AlertDialog>().ok())
                .expect("the question");
            assert!(
                !dialog.body().contains("SHA256"),
                "the keys are not body text"
            );
            let keys = dialog
                .extra_child()
                .and_downcast::<gtk::Label>()
                .expect("the keys");
            assert_eq!(keys.label(), fingerprints.join("\n"));
            assert!(keys.is_selectable());
            assert!(keys.has_css_class("monospace"));
            let attributes = keys.attributes().expect("no hyphens").to_string();
            assert!(attributes.contains("insert-hyphens false"), "{attributes}");
            assert!(
                pump_until(|| GtkWindowExt::focus(&window)
                    .and_downcast::<gtk::Button>()
                    .is_some_and(|button| button.label().as_deref() == Some("Cancel"))),
                "Cancel lacks the keyboard: {:?}",
                GtkWindowExt::focus(&window)
            );
            dialog.close();
            window.destroy();
        });
    }
}
