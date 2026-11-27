// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A statement the person acknowledges: what happened, why, and one answer
//! that closes it.

use adw::prelude::*;
use big_gtk_kit::alert::Alert;

/// An alert over `parent` saying `heading`, then `body` when it is not empty,
/// with `close` as its only answer, which Enter and Escape give too. Present
/// it, or choose on it to learn when it closed.
pub fn tell(
    parent: Option<&impl IsA<gtk::Widget>>,
    heading: &str,
    body: &str,
    close: &str,
) -> Alert {
    let alert = Alert::new(parent, Some(heading), Some(body));
    alert.add_response("close", close);
    alert.set_default_response(Some("close"));
    alert.set_close_response("close");
    alert
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_escape_close() {
        big_testkit::harness::require_gtk_test(|| {
            let alert = tell(None::<&gtk::Widget>, "Not sent", "", "Close");
            let dialog = alert.dialog();
            assert_eq!(dialog.default_response().as_deref(), Some("close"));
            assert_eq!(dialog.close_response().as_str(), "close");
            assert_eq!(dialog.response_label("close").as_str(), "Close");
        });
    }
}
