// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The user guide's shortcut reference, kept equal to the tables the shortcut
//! dialogs read.
//!
//! Each product builds its [`Group`]s from the same constants and registries its
//! dialog uses and calls [`check`] from a test. The page `docs/user/<lang>/shortcuts.md`
//! holds one block per product between `<!-- shortcuts:NAME -->` markers; the
//! check fails when a block differs from the tables, and `BIG_BLESS_GUIDE=1`
//! rewrites the blocks instead.
#![expect(clippy::panic, reason = "a failed fixture fails the test")]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use big_relm4_components::feedback::shortcuts_dialog::{BigShortcutRow, BigShortcutsSection};
use gtk::prelude::*;

/// One titled table of the reference.
pub struct Group {
    /// Section title as a msgid of the product's catalog.
    pub title: String,
    /// Rows in the order the dialog shows them.
    pub rows: Vec<Row>,
}

/// One action and the keys that trigger it.
pub struct Row {
    /// Action label as a msgid of the product's catalog.
    pub label: String,
    /// Accelerators in GTK's spelling (`<Primary>P`) or the desktop's (`Super+v`).
    pub keys: Vec<String>,
}

impl Row {
    /// A row from borrowed text.
    pub fn new(label: &str, keys: &[&str]) -> Self {
        Self {
            label: label.to_owned(),
            keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        }
    }
}

/// The groups of the shortcuts dialog's sections. Sections of one title form
/// one group, as in the dialog. A row bound to an action takes the
/// accelerators `app` holds for it, as the dialog does.
///
/// # Panics
///
/// When a row names an action and no `app` is given.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "a test harness: its panics are the failures it reports"
)]
pub fn from_sections(
    sections: &[BigShortcutsSection],
    app: Option<&gtk::Application>,
) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for section in sections {
        let rows = section.rows.iter().map(|row| match row {
            BigShortcutRow::Accels { title, accels } => Row {
                label: title.clone(),
                keys: accels.clone(),
            },
            BigShortcutRow::Action { title, action_name } => Row {
                label: title.clone(),
                keys: app
                    .expect("an action row needs the application that holds its keys")
                    .accels_for_action(action_name)
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            },
        });
        match groups.iter_mut().find(|group| group.title == section.title) {
            Some(group) => group.rows.extend(rows),
            None => groups.push(Group {
                title: section.title.clone(),
                rows: rows.collect(),
            }),
        }
    }
    groups
}

/// Every `(label, key)` pair of `groups`, in order.
#[must_use]
pub fn bindings(groups: &[Group]) -> Vec<(&str, &str)> {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .flat_map(|row| {
            row.keys
                .iter()
                .map(|key| (row.label.as_str(), key.as_str()))
        })
        .collect()
}

/// The key `accelerator` names, as GTK reads it: `<Primary>c` and
/// `<Control>C` are one key.
fn parsed(accelerator: &str) -> (gtk::gdk::Key, gtk::gdk::ModifierType) {
    let (key, modifiers) = gtk::accelerator_parse(accelerator)
        .unwrap_or_else(|| panic!("GTK cannot read the accelerator `{accelerator}`"));
    (key.to_lower(), modifiers)
}

/// Fail when one key of `bindings` triggers two differently labelled actions.
///
/// # Panics
///
/// On such a key, or on an accelerator GTK cannot read.
pub fn assert_unique_keys<'a>(bindings: impl IntoIterator<Item = (&'a str, &'a str)>) {
    let mut seen: Vec<((gtk::gdk::Key, gtk::gdk::ModifierType), &str)> = Vec::new();
    for (label, accelerator) in bindings {
        let key = parsed(accelerator);
        if let Some((_, other)) = seen.iter().find(|(seen_key, _)| *seen_key == key) {
            assert!(
                *other == label,
                "`{accelerator}` is bound to both `{other}` and `{label}`"
            );
        } else {
            seen.push((key, label));
        }
    }
}

/// Fail when a `(label, key)` of `expected` is missing from `bindings`.
///
/// # Panics
///
/// On a missing pair, or on an accelerator GTK cannot read.
pub fn assert_bound<'a>(
    bindings: impl IntoIterator<Item = (&'a str, &'a str)>,
    expected: &[(&str, &str)],
) {
    let bound: Vec<(&str, (gtk::gdk::Key, gtk::gdk::ModifierType))> = bindings
        .into_iter()
        .map(|(label, accelerator)| (label, parsed(accelerator)))
        .collect();
    for (label, accelerator) in expected {
        let key = parsed(accelerator);
        assert!(
            bound.contains(&(*label, key)),
            "`{label}` is not bound to `{accelerator}`"
        );
    }
}

struct Language {
    dir: &'static str,
    catalog: Option<&'static str>,
    action: &'static str,
    keys: &'static str,
    or: &'static str,
}

const LANGUAGES: [Language; 2] = [
    Language {
        dir: "en",
        catalog: None,
        action: "Action",
        keys: "Keys",
        or: "or",
    },
    Language {
        dir: "pt-BR",
        catalog: Some("pt_BR"),
        action: "Ação",
        keys: "Teclas",
        or: "ou",
    },
];

/// Compare (or, with `BIG_BLESS_GUIDE=1`, rewrite) `product`'s block in both
/// languages' shortcut pages. `catalog` is the product's `po` directory,
/// relative to the repository root, holding the `pt_BR.po` the labels come from.
///
/// # Panics
///
/// When a block is missing or differs, or a label has no Portuguese translation.
pub fn check(product: &str, catalog_dir: &str, groups: &[Group]) {
    let root = repository_root();
    let bless = std::env::var_os("BIG_BLESS_GUIDE").is_some();
    for language in &LANGUAGES {
        let translations = language
            .catalog
            .map(|name| load_catalog(&root.join(catalog_dir).join(format!("{name}.po"))));
        let expected = render(language, translations.as_ref(), groups);
        let page = root
            .join("docs/user")
            .join(language.dir)
            .join("shortcuts.md");
        let text = std::fs::read_to_string(&page)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", page.display()));
        let open = format!("<!-- shortcuts:{product} -->\n");
        let close = format!("<!-- /shortcuts:{product} -->");
        let (Some(start), Some(end)) = (text.find(&open), text.find(&close)) else {
            panic!("{} has no `{}` block", page.display(), open.trim_end());
        };
        let start = start + open.len();
        if text[start..end] == expected {
            continue;
        }
        assert!(
            bless,
            "{} differs from the {product} shortcut tables; regenerate with BIG_BLESS_GUIDE=1",
            page.display()
        );
        let updated = format!("{}{expected}{}", &text[..start], &text[end..]);
        big_os_kit::storage::atomic_write(&page, updated.as_bytes())
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", page.display()));
    }
}

/// The calling program's repository: Cargo runs a test in its package's
/// directory, and this crate may be checked out anywhere else.
fn repository_root() -> PathBuf {
    let package =
        std::env::current_dir().unwrap_or_else(|error| panic!("no working directory: {error}"));
    package
        .ancestors()
        .find(|dir| dir.join("docs/user").is_dir())
        .unwrap_or_else(|| panic!("no docs/user above {}", package.display()))
        .to_owned()
}

fn render(
    language: &Language,
    translations: Option<&HashMap<String, String>>,
    groups: &[Group],
) -> String {
    let say = |msgid: &str| match translations {
        None => msgid.to_owned(),
        Some(map) => map
            .get(msgid)
            .unwrap_or_else(|| panic!("`{msgid}` has no pt_BR translation"))
            .clone(),
    };
    let mut out = String::new();
    for group in groups {
        let _ = writeln!(out, "\n### {}\n", say(&group.title));
        let _ = writeln!(
            out,
            "| {} | {} |\n|---|---|",
            language.action, language.keys
        );
        for row in &group.rows {
            // The keypad's twin of a key is not a second way worth printing, and two
            // spellings of one key print once.
            let mut labels: Vec<String> = Vec::new();
            for key in row.keys.iter().filter(|key| !key.contains("KP_")) {
                let label = key_label(key);
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
            // A run such as Alt+1 to Alt+0 is shown by its ends.
            let keys = if labels.len() > 3 {
                let last = labels[labels.len() - 1]
                    .rsplit('+')
                    .next()
                    .unwrap_or_default();
                format!("`{}…{last}`", labels[0])
            } else {
                labels
                    .iter()
                    .map(|label| format!("`{label}`"))
                    .collect::<Vec<_>>()
                    .join(&format!(" {} ", language.or))
            };
            let _ = writeln!(out, "| {} | {keys} |", say(&row.label));
        }
    }
    out.push('\n');
    out
}

/// A key as a keyboard prints it: `<Primary><Shift>p` and `Ctrl+Shift+p` both
/// give `Ctrl+Shift+P`.
fn key_label(spec: &str) -> String {
    let (mut modifiers, key): (Vec<&str>, &str) = if spec.starts_with('<') {
        let end = spec.rfind('>').map_or(0, |at| at + 1);
        (
            spec[..end]
                .split(['<', '>'])
                .filter(|part| !part.is_empty())
                .collect(),
            &spec[end..],
        )
    } else {
        let mut parts: Vec<&str> = spec.split('+').collect();
        let key = parts.pop().unwrap_or_default();
        (parts, key)
    };
    let rank = |name: &str| match name {
        "Super" | "Meta" => 0,
        "Primary" | "Control" | "Ctrl" => 1,
        "Alt" => 2,
        _ => 3,
    };
    modifiers.sort_by_key(|name| rank(name));
    let mut label = String::new();
    for name in modifiers {
        label.push_str(match rank(name) {
            0 => "Super",
            1 => "Ctrl",
            2 => "Alt",
            _ => "Shift",
        });
        label.push('+');
    }
    label.push_str(&key_name(key));
    label
}

fn key_name(key: &str) -> String {
    match key {
        "Return" => "Enter".into(),
        "Escape" => "Esc".into(),
        "Left" => "←".into(),
        "Right" => "→".into(),
        "Up" => "↑".into(),
        "Down" => "↓".into(),
        "plus" => "+".into(),
        "minus" => "-".into(),
        "comma" => ",".into(),
        "period" => ".".into(),
        "question" => "?".into(),
        "bracketleft" => "[".into(),
        "bracketright" => "]".into(),
        "braceleft" => "[".into(),
        "braceright" => "]".into(),
        "equal" => "=".into(),
        "slash" => "/".into(),
        "less" => "<".into(),
        "underscore" => "_".into(),
        "parenright" => ")".into(),
        _ if key.chars().count() == 1 => key.to_uppercase(),
        _ => big_os_kit::first_capital::of(&key.replace('_', " ")),
    }
}

/// `msgid` to `msgstr` of a `.po` file, leaving out untranslated and fuzzy
/// entries. An entry with a context counts only for a `msgid` no plain entry has.
///
/// # Panics
///
/// When `path` cannot be read.
pub fn load_catalog(path: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    catalog_of(&text)
}

fn catalog_of(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let (mut id, mut translation, mut fuzzy) = (String::new(), String::new(), false);
    let (mut in_translation, mut in_context) = (false, false);
    // Entries end at a blank line; the extra one closes the last.
    for line in text.lines().chain(std::iter::once("")) {
        if let Some(rest) = line.strip_prefix("msgid ") {
            id = unquote(rest);
            in_translation = false;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            translation = unquote(rest);
            in_translation = true;
        } else if line.starts_with('"') {
            let target = if in_translation {
                &mut translation
            } else {
                &mut id
            };
            target.push_str(&unquote(line));
        } else if line.starts_with("#,") && line.contains("fuzzy") {
            fuzzy = true;
        } else if line.starts_with("msgctxt ") {
            in_context = true;
        } else if line.is_empty() {
            if !id.is_empty() && !translation.is_empty() && !fuzzy {
                let (id, translation) = (std::mem::take(&mut id), std::mem::take(&mut translation));
                if in_context {
                    map.entry(id).or_insert(translation);
                } else {
                    map.insert(id, translation);
                }
            }
            (id, translation, fuzzy) = (String::new(), String::new(), false);
            in_context = false;
        }
    }
    map
}

fn unquote(quoted: &str) -> String {
    let inner = quoted.trim().trim_matches('"');
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Tabs" of a window and "Tabs" of indentation are two messages.
    #[test]
    fn context_keeps_plain_translation() {
        let catalog = catalog_of(
            "msgid \"Tabs\"\nmsgstr \"Abas\"\n\nmsgctxt \"indentation\"\nmsgid \"Tabs\"\nmsgstr \"Tabulações\"\n",
        );
        assert_eq!(catalog.get("Tabs").map(String::as_str), Some("Abas"));
        let only_context =
            catalog_of("msgctxt \"markup tool\"\nmsgid \"Blur\"\nmsgstr \"Desfocar\"\n");
        assert_eq!(
            only_context.get("Blur").map(String::as_str),
            Some("Desfocar")
        );
    }

    #[test]
    fn key_labels_read_like_keycaps() {
        assert_eq!(key_label("<Primary><Shift>p"), "Ctrl+Shift+P");
        assert_eq!(key_label("<Control>question"), "Ctrl+?");
        assert_eq!(key_label("Super+Shift+Print"), "Super+Shift+Print");
        assert_eq!(key_label("<Alt>Page_Down"), "Alt+Page Down");
        assert_eq!(key_label("Super"), "Super");
        assert_eq!(key_label("Super+1…9"), "Super+1…9");
        assert_eq!(key_label("<Control>period"), "Ctrl+.");
        assert_eq!(key_label("<Control><Shift>braceleft"), "Ctrl+Shift+[");
        assert_eq!(key_label("<Control><Shift>braceright"), "Ctrl+Shift+]");
    }

    #[test]
    fn spellings_of_one_key_print_once() {
        let row = Row::new(
            "Previous",
            &["<Control><Shift>bracketleft", "<Control><Shift>braceleft"],
        );
        let language = &LANGUAGES[0];
        let out = render(
            language,
            None,
            &[Group {
                title: "T".into(),
                rows: vec![row],
            }],
        );
        assert!(out.contains("| Previous | `Ctrl+Shift+[` |"), "{out}");
    }
}
