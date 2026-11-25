// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Rows and sections of the keyboard-shortcuts dialog.
//!
//! An app describes the keys it lists; `big_workspace::keymap_dialog` turns
//! them into rows of its searchable dialog. Callers pass already-translated
//! section titles and row labels; i18n stays application side per crate
//! convention.

/// One shortcut row inside a section.
///
/// Prefer [`BigShortcutRow::action`] for rows backed by a registered
/// `GAction`: the dialog reads the accelerator from the application's accel
/// map (`gtk_application_get_accels_for_action`), so the displayed keys match
/// the real binding. Use [`BigShortcutRow::accels`] for keys a widget handles
/// itself.
pub enum BigShortcutRow {
    /// Row bound to a registered action; the dialog reads its accelerator.
    Action {
        /// Translated row label.
        title: String,
        /// Fully-qualified action name, e.g. `app.quit` or `win.copy-selection`.
        action_name: String,
    },
    /// Row with explicit accelerators in [`gtk::accelerator_parse`] format.
    Accels {
        /// Translated row label.
        title: String,
        /// Accelerators like `<Primary>N`.
        accels: Vec<String>,
    },
}

impl BigShortcutRow {
    /// Row resolved from a registered action's accelerator.
    pub fn action(title: impl Into<String>, action_name: impl Into<String>) -> Self {
        Self::Action {
            title: title.into(),
            action_name: action_name.into(),
        }
    }

    /// Row with explicit accelerators (skipped when empty).
    pub fn accels(title: impl Into<String>, accels: Vec<String>) -> Self {
        Self::Accels {
            title: title.into(),
            accels,
        }
    }
}

/// One titled section of shortcut rows.
///
/// `title` and each row label are already translated by the caller; i18n stays
/// application side per crate convention.
pub struct BigShortcutsSection {
    /// Translated section heading, e.g. "Tabs" or "Files".
    pub title: String,
    /// Rows shown under the heading.
    pub rows: Vec<BigShortcutRow>,
}

impl BigShortcutsSection {
    /// Convenience constructor for a translated section.
    pub fn new(title: impl Into<String>, rows: Vec<BigShortcutRow>) -> Self {
        Self {
            title: title.into(),
            rows,
        }
    }
}
