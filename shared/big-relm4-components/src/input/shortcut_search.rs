// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Finding a shortcut by what it does or by its keys: `close` finds the rows
//! titled with it, `ctrl+t` finds what Ctrl+T does, `Super` every shortcut
//! that holds Super.

use gtk::gdk::{self, ModifierType};

/// A shortcut search as somebody types it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutSearch {
    /// What was typed, trimmed and in lower case; empty finds every shortcut.
    text: String,
    /// The typed words read as keys, when all but at most one name a modifier:
    /// the modifiers named and that one other key.
    keys: Option<(ModifierType, Option<String>)>,
}

/// The modifier a typed word names, in the words keyboards and labels use.
fn modifier(word: &str) -> Option<ModifierType> {
    match word {
        "ctrl" | "control" | "ctl" | "primary" => Some(ModifierType::CONTROL_MASK),
        "alt" => Some(ModifierType::ALT_MASK),
        "shift" => Some(ModifierType::SHIFT_MASK),
        "super" | "meta" | "logo" => Some(ModifierType::SUPER_MASK),
        _ => None,
    }
}

impl ShortcutSearch {
    /// Read `typed`, where keys are joined by `+` or spaces.
    #[must_use]
    pub fn new(typed: &str) -> Self {
        let text = typed.trim().to_lowercase();
        let mut modifiers = ModifierType::empty();
        let mut key = None;
        let mut only_keys = true;
        // A `+` after the one joining keys is the Plus key, as GTK writes Ctrl++.
        let joined = text
            .strip_suffix('+')
            .map(str::trim_end)
            .filter(|rest| rest.ends_with('+'));
        for word in joined
            .unwrap_or(&text)
            .split(|letter: char| letter == '+' || letter.is_whitespace())
            .filter(|word| !word.is_empty())
            .chain(joined.map(|_| "plus"))
        {
            match modifier(word) {
                Some(named) => modifiers |= named,
                None if key.is_none() => key = Some(word.to_owned()),
                None => only_keys = false,
            }
        }
        let keys =
            (only_keys && (!modifiers.is_empty() || key.is_some())).then_some((modifiers, key));
        Self { text, keys }
    }

    /// Whether nothing was typed, so every shortcut is found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether the shortcut titled `title`, whose keys read `label` on screen and
    /// are `keys` (the modifiers held and the key's name, `t` or `Return`), is one
    /// this search finds: its title holds what was typed, or its label when what
    /// was typed reads as no keys, or one of its
    /// keys holds every modifier typed and is the key typed, by its name or as
    /// GTK shows it (`Return` reads `Enter`).
    #[must_use]
    pub fn finds(&self, title: &str, label: &str, keys: &[(ModifierType, &str)]) -> bool {
        // `+` and spaces join keys alike: "ctrl page up" reads "Ctrl+Page Up".
        let joined = |text: &str| {
            text.to_lowercase()
                .split(|letter: char| letter == '+' || letter.is_whitespace())
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let typed = joined(&self.text);
        // Typed as keys, the label is no text to search: "ctrl+alt+t" is not
        // "Ctrl+Alt+Tab", whose key is a whole name.
        let label_holds = self.keys.is_none()
            && (label.to_lowercase().contains(&self.text)
                || (!typed.is_empty() && joined(label).contains(&typed)));
        if self.text.is_empty() || title.to_lowercase().contains(&self.text) || label_holds {
            return true;
        }
        let Some((modifiers, key)) = &self.keys else {
            return false;
        };
        keys.iter().any(|(held, name)| {
            // GTK names the logo key Meta in some accelerators.
            let held = if held.contains(ModifierType::META_MASK) {
                *held | ModifierType::SUPER_MASK
            } else {
                *held
            };
            held.contains(*modifiers)
                && key.as_deref().is_none_or(|key| {
                    name.eq_ignore_ascii_case(key)
                        || shown_key(name).is_some_and(|shown| shown == key)
                })
        })
    }
}

/// The key `name` as GTK shows it, in lower case.
fn shown_key(name: &str) -> Option<String> {
    let key = gdk::Key::from_name(name)?;
    let shown = gtk::accelerator_get_label(key, ModifierType::empty()).to_lowercase();
    Some(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The searches the shortcut pages promise: a name, `ctrl+t`, `Super`.
    #[test]
    fn finds_by_name_or_key() {
        big_testkit::harness::require_gtk_test(|| {
            let control = ModifierType::CONTROL_MASK;
            let shift = ModifierType::SHIFT_MASK;
            let logo = ModifierType::SUPER_MASK;
            let new_tab = ("New tab", "Ctrl+T", vec![(control, "t")]);
            let reopen = (
                "Reopen closed tab",
                "Shift+Ctrl+T",
                vec![(control | shift, "t")],
            );
            let close = ("Close window", "Ctrl+W", vec![(control, "w")]);
            let menu = ("Open the menu", "Super", vec![(logo, "")]);
            let terminal = (
                "Terminal",
                "Super + T",
                vec![(ModifierType::META_MASK, "t")],
            );
            let unbound = ("Zoom in", "Disabled", vec![]);
            let zoom_out = ("Shrink text", "Ctrl+-", vec![(control, "minus")]);
            let zoom_in = ("Enlarge text", "Ctrl++", vec![(control, "plus")]);
            let page = ("Previous page", "Ctrl+Page Up", vec![(control, "Page_Up")]);
            let switch = (
                "Switch window",
                "Ctrl+Alt+Tab",
                vec![(control | ModifierType::ALT_MASK, "Tab")],
            );
            let open_terminal = (
                "Open terminal",
                "Ctrl+Alt+T",
                vec![(control | ModifierType::ALT_MASK, "t")],
            );
            let rows = [
                &new_tab,
                &reopen,
                &close,
                &menu,
                &terminal,
                &unbound,
                &zoom_out,
                &zoom_in,
                &page,
                &switch,
                &open_terminal,
            ];
            for (typed, expected) in [
                (
                    "",
                    vec![
                        "New tab",
                        "Reopen closed tab",
                        "Close window",
                        "Open the menu",
                        "Terminal",
                        "Zoom in",
                        "Shrink text",
                        "Enlarge text",
                        "Previous page",
                        "Switch window",
                        "Open terminal",
                    ],
                ),
                (
                    "ctrl+t",
                    vec!["New tab", "Reopen closed tab", "Open terminal"],
                ),
                (
                    "Ctrl + T",
                    vec!["New tab", "Reopen closed tab", "Open terminal"],
                ),
                ("ctrl+shift+t", vec!["Reopen closed tab"]),
                // Typed as keys, Ctrl+Alt+Tab's label holds the text but not the key.
                ("ctrl+alt+t", vec!["Open terminal"]),
                (
                    "ctrl+",
                    vec![
                        "New tab",
                        "Reopen closed tab",
                        "Close window",
                        "Shrink text",
                        "Enlarge text",
                        "Previous page",
                        "Switch window",
                        "Open terminal",
                    ],
                ),
                ("Super", vec!["Open the menu", "Terminal"]),
                ("super+t", vec!["Terminal"]),
                ("w", vec!["New tab", "Close window", "Switch window"]),
                ("close", vec!["Reopen closed tab", "Close window"]),
                ("zoom", vec!["Zoom in"]),
                ("ctrl -", vec!["Shrink text"]),
                ("ctrl minus", vec!["Shrink text"]),
                // GTK writes Ctrl and the Plus key "Ctrl++".
                ("Ctrl++", vec!["Enlarge text"]),
                ("ctrl + +", vec!["Enlarge text"]),
                ("ctrl plus", vec!["Enlarge text"]),
                ("ctrl page up", vec!["Previous page"]),
                (
                    "+",
                    vec![
                        "New tab",
                        "Reopen closed tab",
                        "Close window",
                        "Terminal",
                        "Shrink text",
                        "Enlarge text",
                        "Previous page",
                        "Switch window",
                        "Open terminal",
                    ],
                ),
                ("ctrl+q", vec![]),
                ("paste the text", vec![]),
            ] {
                let search = ShortcutSearch::new(typed);
                let found: Vec<&str> = rows
                    .iter()
                    .filter(|(title, label, keys)| search.finds(title, label, keys))
                    .map(|(title, _, _)| *title)
                    .collect();
                assert_eq!(found, expected, "{typed:?}");
            }
        });
    }
}
