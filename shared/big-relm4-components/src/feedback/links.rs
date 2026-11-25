// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Labels with links that a screen reader can walk without crashing the process.

use gtk::prelude::*;

/// Keep the link objects of `label` ahead of its accessible context.
///
/// GTK 4.22 makes a label's links accessible the first time something asks for
/// them. When that first ask comes from inside the label's own AT-SPI
/// registration, each link registers the label a second time under a new path,
/// and the extra entry outlives the label in GTK's AT-SPI cache: the next
/// screen reader that fetches the cache reads a freed object and the process
/// dies. Making the links when the label joins a window, and again whenever its
/// markup changes, means the registration finds them already made.
///
/// Each link is also named by its visible text, which GTK leaves empty.
///
/// Call it once on every label whose markup holds `<a href>`.
pub fn expose_links(label: &gtk::Label) {
    let make_links = |label: &gtk::Label| {
        // The links are made as a side effect of asking for the first one; a
        // label that is not in a window yet would register outside it.
        if label.root().is_none() {
            return;
        }
        let mut link = label.first_accessible_child();
        for text in link_texts(&label.label()) {
            let Some(accessible) = link else {
                break;
            };
            accessible.update_property(&[gtk::accessible::Property::Label(&text)]);
            link = accessible.next_accessible_sibling();
        }
    };
    make_links(label);
    label.connect_root_notify(make_links);
    label.connect_label_notify(make_links);
}

/// The visible text of each `<a>` element of `markup`, in order: the order
/// GTK keeps the label's link accessibles in.
fn link_texts(markup: &str) -> Vec<String> {
    let mut texts = Vec::new();
    let mut rest = markup;
    while let Some(open) = rest.find("<a ") {
        // The start tag ends at the first `>` outside a quoted attribute.
        let mut quote = None;
        let Some(tag_end) = rest[open..].char_indices().find_map(|(index, ch)| {
            match (quote, ch) {
                (None, '"' | '\'') => quote = Some(ch),
                (Some(open_quote), _) if open_quote == ch => quote = None,
                (None, '>') => return Some(open + index + 1),
                _ => {}
            }
            None
        }) else {
            break;
        };
        let Some(close) = rest[tag_end..].find("</a>") else {
            break;
        };
        let inner = &rest[tag_end..tag_end + close];
        texts.push(
            gtk::pango::parse_markup(inner, '\0')
                .map_or_else(|_| inner.to_owned(), |(_, text, _)| text.to_string()),
        );
        rest = &rest[tag_end + close + "</a>".len()..];
    }
    texts
}

#[cfg(test)]
mod tests {
    use big_testkit::atspi::{accessibility_bus, cached_items, settle};
    use gtk::prelude::*;

    use super::{expose_links, link_texts};

    const LINKS: &str = "Read <a href=\"https://example.com/a\">the docs</a> \
                         or <a href=\"https://example.com/b\">the notes</a>.";

    #[test]
    fn links_take_their_text() {
        assert_eq!(
            link_texts(
                "<a href=\"a>b\" title='x'>the <b>docs</b> &amp; more</a> and <a href=\"c\">notes</a>"
            ),
            ["the docs & more", "notes"]
        );
        big_testkit::harness::require_gtk_test(|| {
            let label = gtk::Label::new(None);
            label.set_markup(LINKS);
            expose_links(&label);
            let window = gtk::Window::builder().child(&label).build();
            window.present();
            settle();
            let mut named = 0;
            let mut link = label.first_accessible_child();
            while let Some(accessible) = link {
                assert!(gtk::test_accessible_has_property(
                    &accessible,
                    gtk::AccessibleProperty::Label
                ));
                named += 1;
                link = accessible.next_accessible_sibling();
            }
            window.destroy();
            assert_eq!(named, 2);
        });
    }

    #[test]
    #[ignore = "other-display: needs the AT-SPI bridge: tools/headless-gate.sh --accessible cargo test -p big-relm4-components -- --ignored"]
    fn removed_link_label_keeps_cache() {
        big_testkit::harness::require_gtk_test(|| {
            let window = gtk::Window::new();
            window.present();
            settle();
            let (bus, application) =
                accessibility_bus().expect("the AT-SPI bridge (headless-gate.sh --accessible)");
            cached_items(&bus, &application).expect("cache answers");
            // Added after the window is shown, so its context is realized and
            // registers each label as it arrives, as a preview being rendered.
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            window.set_child(Some(&column));
            for _ in 0..3 {
                let label = gtk::Label::new(None);
                label.set_markup(LINKS);
                expose_links(&label);
                column.append(&label);
                settle();
                column.remove(&label);
                drop(label);
                settle();
                cached_items(&bus, &application).expect("cache answers after the label is gone");
            }
            window.destroy();
        });
    }
}
