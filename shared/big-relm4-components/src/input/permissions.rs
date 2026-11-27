// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Nine Unix access bits, without ownership, special bits or filesystem writes.

use big_gtk_kit::tooltip::name_control;
use std::rc::Rc;

use adw::prelude::*;

/// Shared access choices. The consumer owns validation and publication.
#[derive(Clone)]
pub struct BigPermissionEditor {
    rows: [adw::ActionRow; 3],
    toggles: [gtk::ToggleButton; 9],
}

impl BigPermissionEditor {
    /// Build owner/group/others rows with translated visible and accessible labels.
    #[must_use]
    pub fn new(mode: u16, directory: bool, translate: impl Fn(&'static str) -> String) -> Self {
        let actions = [
            translate("Read"),
            translate("Change"),
            if directory {
                translate("Open")
            } else {
                translate("Run")
            },
        ];
        let descriptions = if directory {
            [
                translate("Owner can read"),
                translate("Owner can change"),
                translate("Owner can open"),
                translate("Group can read"),
                translate("Group can change"),
                translate("Group can open"),
                translate("Others can read"),
                translate("Others can change"),
                translate("Others can open"),
            ]
        } else {
            [
                translate("Owner can read"),
                translate("Owner can change"),
                translate("Owner can run"),
                translate("Group can read"),
                translate("Group can change"),
                translate("Group can run"),
                translate("Others can read"),
                translate("Others can change"),
                translate("Others can run"),
            ]
        };
        let toggles = std::array::from_fn(|index| {
            let description = &descriptions[index];
            let toggle = gtk::ToggleButton::builder()
                .label(&actions[index % 3])
                .active(mode & (1 << (8 - index)) != 0)
                .build();
            name_control(&toggle, description);
            if toggle.is_active() {
                toggle.add_css_class("suggested-action");
            }
            toggle.connect_toggled(|toggle| {
                if toggle.is_active() {
                    toggle.add_css_class("suggested-action");
                } else {
                    toggle.remove_css_class("suggested-action");
                }
            });
            toggle
        });
        let people = [translate("Owner"), translate("Group"), translate("Others")];
        let rows = std::array::from_fn(|index| {
            let row = adw::ActionRow::builder().title(&people[index]).build();
            let choices = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            choices.add_css_class("linked");
            choices.set_valign(gtk::Align::Center);
            for toggle in &toggles[index * 3..index * 3 + 3] {
                choices.append(toggle);
            }
            row.add_suffix(&choices);
            row
        });
        Self { rows, toggles }
    }

    /// Add these rows to the consumer's preferences group.
    #[must_use]
    pub fn rows(&self) -> &[adw::ActionRow; 3] {
        &self.rows
    }

    /// Return only the nine ordinary access bits.
    #[must_use]
    pub fn mode(&self) -> u16 {
        self.toggles
            .iter()
            .enumerate()
            .fold(0, |mode, (index, toggle)| {
                mode | if toggle.is_active() {
                    1 << (8 - index)
                } else {
                    0
                }
            })
    }

    /// Show the nine access bits of `mode`, such as a mode read back after a
    /// change. Observers are told as for a click.
    pub fn set_mode(&self, mode: u16) {
        for (index, toggle) in self.toggles.iter().enumerate() {
            toggle.set_active(mode & (1 << (8 - index)) != 0);
        }
    }

    /// Offer the access bits for change, or only show them: each choice
    /// itself, so assistive technology reads it as unavailable too.
    pub fn set_sensitive(&self, sensitive: bool) {
        for row in &self.rows {
            row.set_sensitive(sensitive);
        }
        for toggle in &self.toggles {
            toggle.set_sensitive(sensitive);
        }
    }

    /// Observe changed access bits. Capture consumer widgets weakly in the callback.
    pub fn connect_changed(&self, callback: impl Fn(u16) + 'static) {
        let callback = Rc::new(callback);
        let toggles = self.toggles.each_ref().map(gtk::ToggleButton::downgrade);
        for toggle in &self.toggles {
            let toggles = toggles.clone();
            let callback = Rc::clone(&callback);
            toggle.connect_toggled(move |_| {
                let mode = toggles.iter().enumerate().fold(0, |mode, (index, toggle)| {
                    mode | if toggle.upgrade().is_some_and(|toggle| toggle.is_active()) {
                        1 << (8 - index)
                    } else {
                        0
                    }
                });
                callback(mode);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_bits_round_trip() {
        big_testkit::harness::require_gtk_test(|| {
            for mode in 0..=0o777 {
                let editor = BigPermissionEditor::new(mode | 0o7000, false, str::to_owned);
                assert_eq!(editor.mode(), mode);
            }
            let editor = BigPermissionEditor::new(0, true, str::to_owned);
            let observed = Rc::new(std::cell::Cell::new(0));
            let changed = Rc::clone(&observed);
            editor.connect_changed(move |mode| changed.set(mode));
            editor.toggles[0].set_active(true);
            assert_eq!(observed.get(), 0o400);
            let weak = editor.toggles[0].downgrade();
            drop(editor);
            assert!(weak.upgrade().is_none());
        });
    }
}
