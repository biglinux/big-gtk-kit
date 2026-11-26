// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The group of rows on a preferences page.

use adw::prelude::*;

/// A preferences group's heading and visibility, built without a display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigPreferencesGroupSpec {
    /// The heading over the rows; empty shows none.
    pub title: String,
    /// The line under the heading.
    pub description: Option<String>,
    /// Whether the group starts shown.
    pub visible: bool,
}

impl BigPreferencesGroupSpec {
    /// A shown group headed `title`, with no description.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            visible: true,
        }
    }

    /// A group without a heading, for a button strip or compact controls.
    #[must_use]
    pub fn untitled() -> Self {
        Self::new("")
    }

    /// Show `description` under the heading.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Start the group shown or hidden.
    #[must_use]
    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Build the group this describes.
    #[must_use]
    pub fn build(&self) -> adw::PreferencesGroup {
        let mut builder = adw::PreferencesGroup::builder().title(&self.title);
        if let Some(description) = &self.description {
            builder = builder.description(description);
        }
        let group = builder.build();
        group.set_visible(self.visible);
        group
    }
}
