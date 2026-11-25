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
    let body = format!(
        "{}\n\n{}",
        explanation.replace("{server}", server),
        fingerprints.join("\n")
    );
    let heading = heading.replace("{server}", server);
    let cancel = translate("Cancel");
    super::confirm::ask(
        Some(parent),
        &heading,
        &body,
        &answer,
        &cancel,
        appearance,
        |trust| {
            if trust {
                accept();
            }
        },
    );
}
