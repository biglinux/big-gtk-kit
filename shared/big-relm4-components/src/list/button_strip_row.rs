// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! An action row with a strip of buttons at its end.

use adw::prelude::*;
use relm4::gtk;

use super::row_core::row_text;

/// What a compact button-strip row is built from.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigButtonStripRowSpec {
    /// The row's title, shown as plain text unless `allow_markup`.
    pub title: String,
    /// The line under the title, shown as `title` is.
    pub subtitle: Option<String>,
    /// The themed icon before the title.
    pub prefix_icon_name: Option<String>,
    /// Pixels between the buttons; a negative value is 0.
    pub spacing: i32,
    /// Whether the row itself answers a click.
    pub activatable: bool,
    /// Whether `title` and `subtitle` are Pango markup rather than plain text.
    pub allow_markup: bool,
}

impl BigButtonStripRowSpec {
    /// A row titled `title` in plain text, with no subtitle or icon.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            prefix_icon_name: None,
            spacing: 4,
            activatable: false,
            allow_markup: false,
        }
    }

    /// Show `subtitle` under the title.
    #[must_use]
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Show the themed icon `icon_name` before the title.
    #[must_use]
    pub fn prefix_icon_name(mut self, icon_name: impl Into<String>) -> Self {
        self.prefix_icon_name = Some(icon_name.into());
        self
    }

    /// Put `spacing` pixels between the buttons; a negative value is 0.
    #[must_use]
    pub fn spacing(mut self, spacing: i32) -> Self {
        self.spacing = spacing.max(0);
        self
    }

    /// Let the row itself answer a click.
    #[must_use]
    pub fn activatable(mut self, activatable: bool) -> Self {
        self.activatable = activatable;
        self
    }

    /// What the row is built with: the text as GTK reads it, the spacing clamped.
    #[must_use]
    pub fn resolved(&self) -> BigButtonStripRowResolved {
        BigButtonStripRowResolved {
            title: row_text(&self.title, self.allow_markup),
            subtitle: self
                .subtitle
                .as_ref()
                .map(|subtitle| row_text(subtitle, self.allow_markup)),
            prefix_icon_name: self.prefix_icon_name.clone(),
            spacing: self.spacing.max(0),
            activatable: self.activatable,
        }
    }
}

/// What a [`BigButtonStripRowSpec`] builds its row with, computed without a
/// display.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigButtonStripRowResolved {
    /// The title as Pango markup.
    pub title: String,
    /// The subtitle as Pango markup.
    pub subtitle: Option<String>,
    /// The themed icon before the title.
    pub prefix_icon_name: Option<String>,
    /// Pixels between the buttons, never negative.
    pub spacing: i32,
    /// Whether the row itself answers a click.
    pub activatable: bool,
}

/// Built row plus its button strip.
#[derive(Debug, Clone)]
pub struct BigButtonStripRow {
    root: adw::ActionRow,
    strip: gtk::Box,
}

impl BigButtonStripRow {
    /// Build the row `spec` describes, with an empty strip.
    #[must_use]
    pub fn new(spec: BigButtonStripRowSpec) -> Self {
        let resolved = spec.resolved();
        let mut builder = adw::ActionRow::builder()
            .title(resolved.title.as_str())
            .activatable(resolved.activatable);

        if let Some(subtitle) = resolved.subtitle.as_deref() {
            builder = builder.subtitle(subtitle);
        }

        let root = builder.build();

        if let Some(icon_name) = resolved.prefix_icon_name.as_deref() {
            root.add_prefix(&gtk::Image::from_icon_name(icon_name));
        }

        let strip = gtk::Box::builder()
            .spacing(resolved.spacing)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        root.add_suffix(&strip);

        Self { root, strip }
    }

    /// Append `widget` to the horizontal button strip.
    pub fn append(&self, widget: &impl IsA<gtk::Widget>) {
        self.strip.append(widget);
    }

    /// The row, to place in a list.
    #[must_use]
    pub fn root(&self) -> &adw::ActionRow {
        &self.root
    }

    /// The horizontal box holding the buttons.
    #[must_use]
    pub fn strip(&self) -> &gtk::Box {
        &self.strip
    }

    /// The row, to place in a list, giving up the wrapper.
    #[must_use]
    pub fn into_root(self) -> adw::ActionRow {
        self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_clamps_negative_spacing() {
        let resolved = BigButtonStripRowSpec::new("Transform")
            .spacing(-10)
            .resolved();

        assert_eq!(resolved.spacing, 0);
    }

    #[test]
    fn resolved_escapes_markup_by_default() {
        let resolved = BigButtonStripRowSpec::new("<b>Transform</b>")
            .subtitle("<i>Duration</i>")
            .resolved();

        assert_eq!(resolved.title, "&lt;b&gt;Transform&lt;/b&gt;");
        assert_eq!(
            resolved.subtitle.as_deref(),
            Some("&lt;i&gt;Duration&lt;/i&gt;")
        );
    }
}
