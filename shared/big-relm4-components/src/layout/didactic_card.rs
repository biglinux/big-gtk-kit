// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Standard BigLinux didactic cards for settings and onboarding UI.

use adw::prelude::*;
use relm4::gtk;

/// The illustration's side, the `min-width`/`min-height` of its CSS below.
const ILLUSTRATION_SIZE: i32 = 92;

pub(crate) const DEFAULT_DIDACTIC_CARD_SPACING: i32 = 18;
pub(crate) const DEFAULT_DIDACTIC_TEXT_SPACING: i32 = 4;
pub(crate) const DEFAULT_DIDACTIC_DESCRIPTION_MAX_CHARS: i32 = 76;

/// Visual layout flavor selected when realizing a
/// [`BigDidacticCardSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BigDidacticCardLayout {
    /// Illustration on the left, text + optional control on the right.
    Horizontal,
    /// Illustration spans the full width above the text column.
    FullWidth,
    /// Like `Horizontal` but with no embedded control widget.
    Summary,
    /// Plain card with no illustration at all.
    Plain,
}

/// What a didactic card shows and how it lays it out, built without a display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigDidacticCardSpec {
    /// The GResource folder holding the illustration, `/org/app/illustrations`.
    pub resource_prefix: String,
    /// The illustration's file in `resource_prefix`; `None` draws no picture.
    pub illustration_name: Option<String>,
    /// The heading, and the card's accessible name.
    pub title: String,
    /// The paragraph under the title, which describes the card and its control
    /// to a screen reader; empty shows none.
    pub description: String,
    /// How the illustration, text and control are arranged.
    pub layout: BigDidacticCardLayout,
    /// Pixels between the illustration, the text and the control.
    pub card_spacing: i32,
    /// Pixels between the title and the description.
    pub text_spacing: i32,
    /// The width, in characters, at which the description wraps.
    pub description_max_chars: i32,
}

impl BigDidacticCardSpec {
    /// A horizontal card with the illustration `illustration_name` from
    /// `resource_prefix`.
    #[must_use]
    pub fn new(
        resource_prefix: impl Into<String>,
        illustration_name: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            resource_prefix: resource_prefix.into(),
            illustration_name: Some(illustration_name.into()),
            title: title.into(),
            description: description.into(),
            layout: BigDidacticCardLayout::Horizontal,
            card_spacing: DEFAULT_DIDACTIC_CARD_SPACING,
            text_spacing: DEFAULT_DIDACTIC_TEXT_SPACING,
            description_max_chars: DEFAULT_DIDACTIC_DESCRIPTION_MAX_CHARS,
        }
    }

    /// A card of text alone, with no illustration.
    #[must_use]
    pub fn plain(title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            resource_prefix: String::new(),
            illustration_name: None,
            title: title.into(),
            description: description.into(),
            layout: BigDidacticCardLayout::Plain,
            card_spacing: DEFAULT_DIDACTIC_CARD_SPACING,
            text_spacing: DEFAULT_DIDACTIC_TEXT_SPACING,
            description_max_chars: DEFAULT_DIDACTIC_DESCRIPTION_MAX_CHARS,
        }
    }

    /// Switch the layout to `FullWidth` (illustration spans card).
    #[must_use]
    pub fn full_width(mut self) -> Self {
        self.layout = BigDidacticCardLayout::FullWidth;
        self
    }

    /// Switch the layout to `Summary` (no embedded control).
    #[must_use]
    pub fn summary(mut self) -> Self {
        self.layout = BigDidacticCardLayout::Summary;
        self
    }

    /// Override the inter-element spacing values. Negative inputs are
    /// clamped to 0.
    #[must_use]
    pub fn spacing(mut self, card_spacing: i32, text_spacing: i32) -> Self {
        self.card_spacing = card_spacing.max(0);
        self.text_spacing = text_spacing.max(0);
        self
    }

    /// Join `resource_prefix` with `illustration_name` into the full
    /// GResource path. Returns `None` when the spec carries no
    /// illustration.
    #[must_use]
    pub fn resource_path(&self) -> Option<String> {
        self.illustration_name
            .as_ref()
            .map(|name| format!("{}/{}", self.resource_prefix.trim_end_matches('/'), name))
    }
}

/// Realized widget wrapper around a [`BigDidacticCardSpec`].
#[derive(Debug, Clone)]
pub struct BigDidacticCard {
    root: gtk::Box,
}

impl BigDidacticCard {
    /// Build the card `spec` describes, with `control` beside or under its text
    /// (none in the [`BigDidacticCardLayout::Summary`] layout).
    #[must_use]
    pub fn new(spec: BigDidacticCardSpec, control: Option<&impl IsA<gtk::Widget>>) -> Self {
        let (text, description) = text_column(&spec);
        let root = match spec.layout {
            BigDidacticCardLayout::Horizontal => horizontal_card(&spec, &text, control),
            BigDidacticCardLayout::FullWidth => full_width_card(&spec, &text, control),
            BigDidacticCardLayout::Summary => horizontal_card::<gtk::Widget>(&spec, &text, None),
            BigDidacticCardLayout::Plain => plain_card(&spec, &text, control),
        };
        root.set_accessible_role(gtk::AccessibleRole::Group);
        root.update_property(&[gtk::accessible::Property::Label(&spec.title)]);
        // The description is read with the card and with the control in it,
        // which is what takes the focus.
        if let Some(description) = description {
            let described: Vec<gtk::Widget> = std::iter::once(root.clone().upcast())
                .chain(control.map(|control| control.as_ref().clone().upcast()))
                .collect();
            for accessible in &described {
                big_gtk_kit::accessible::relate(
                    accessible,
                    gtk::AccessibleRelation::DescribedBy,
                    &[description.upcast_ref()],
                );
            }
        }
        Self { root }
    }

    /// The card's box, to place in a page.
    #[must_use]
    pub fn root(&self) -> &gtk::Box {
        &self.root
    }

    /// The card's box, to place in a page, giving up the wrapper.
    #[must_use]
    pub fn into_root(self) -> gtk::Box {
        self.root
    }
}

/// The box of a card built from `spec` with `control`.
#[must_use]
pub fn didactic_card(spec: BigDidacticCardSpec, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    BigDidacticCard::new(spec, Some(control)).into_root()
}

fn horizontal_card<T: IsA<gtk::Widget>>(
    spec: &BigDidacticCardSpec,
    text: &gtk::Box,
    control: Option<&T>,
) -> gtk::Box {
    let card = card_root(gtk::Orientation::Horizontal, spec.card_spacing);
    if let Some(picture) = illustration(spec) {
        card.append(&picture);
    }
    card.append(text);
    if let Some(control) = control {
        control.as_ref().set_valign(gtk::Align::Center);
        card.append(control);
    }
    card
}

fn plain_card<T: IsA<gtk::Widget>>(
    spec: &BigDidacticCardSpec,
    text: &gtk::Box,
    control: Option<&T>,
) -> gtk::Box {
    let card = card_root(gtk::Orientation::Horizontal, spec.card_spacing);
    card.append(text);
    if let Some(control) = control {
        control.as_ref().set_valign(gtk::Align::Center);
        card.append(control);
    }
    card
}

fn full_width_card<T: IsA<gtk::Widget>>(
    spec: &BigDidacticCardSpec,
    text: &gtk::Box,
    control: Option<&T>,
) -> gtk::Box {
    let card = card_root(gtk::Orientation::Vertical, spec.text_spacing * 2);
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(spec.card_spacing)
        .build();
    if let Some(picture) = illustration(spec) {
        header.append(&picture);
    }
    header.append(text);
    card.append(&header);

    if let Some(control) = control {
        control.as_ref().set_hexpand(true);
        control.as_ref().set_margin_top(spec.text_spacing * 2);
        card.append(control);
    }

    card
}

// The card's look without a stylesheet of the program's own, which overrides it
// (see `ensure_didactic_card_css_installed`).
const DIDACTIC_CARD_BASE_CSS: &str = "
.didactic-card {
    /* Variables, not the named card colors: a named color resolves from the
       process-wide style manager, which a program tinting each window's chrome
       leaves as it is. The variables follow the window's own values. */
    background-color: var(--card-bg-color);
    color: var(--card-fg-color);
    border: 1px solid alpha(currentColor, 0.08);
    border-radius: 14px;
    padding: 14px 18px;
    margin: 6px 0;
    box-shadow: 0 1px 3px alpha(black, 0.08),
                0 1px 2px alpha(black, 0.06);
    transition: background-color 200ms ease;
}

.didactic-card .didactic-title {
    font-weight: 600;
    font-size: 11pt;
}

.didactic-card .didactic-description {
    font-size: 9.5pt;
    opacity: 0.78;
}

.didactic-card picture.didactic-illustration {
    background-color: transparent;
    padding: 0;
    min-width: 92px;
    min-height: 92px;
}
";

fn ensure_didactic_card_css_installed() {
    // One step below application priority: any product stylesheet installed
    // at STYLE_PROVIDER_PRIORITY_APPLICATION overrides this default
    // regardless of selector specificity or load order.
    crate::css::install(
        DIDACTIC_CARD_BASE_CSS,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION - 1,
    );
}

fn card_root(orientation: gtk::Orientation, spacing: i32) -> gtk::Box {
    ensure_didactic_card_css_installed();
    let card = gtk::Box::builder()
        .orientation(orientation)
        .spacing(spacing)
        .hexpand(true)
        .build();
    card.add_css_class("didactic-card");
    card
}

fn illustration(spec: &BigDidacticCardSpec) -> Option<gtk::Picture> {
    let path = spec.resource_path()?;
    // Drawn by the suite's image path at the size the card shows it
    // (`ILLUSTRATION_SIZE`), and shared through its cache: a dialog opened
    // again reuses the texture instead of decoding the SVG once more.
    let picture = gtk::Picture::new();
    let height = ILLUSTRATION_SIZE.saturating_mul(picture.scale_factor().max(1));
    let weak = picture.downgrade();
    big_image::icon::resource_texture(&path, height, move |texture| {
        if let Some(picture) = weak.upgrade() {
            picture.set_paintable(Some(&texture));
        }
    });
    picture.add_css_class("didactic-illustration");
    picture.set_valign(gtk::Align::Center);
    picture.set_accessible_role(gtk::AccessibleRole::Presentation);
    Some(picture)
}

/// The title and description, and the description label when there is one.
fn text_column(spec: &BigDidacticCardSpec) -> (gtk::Box, Option<gtk::Label>) {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(spec.text_spacing)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();

    let title = gtk::Label::builder()
        .label(&spec.title)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["didactic-title"])
        .build();
    column.append(&title);

    let description = (!spec.description.is_empty()).then(|| {
        let description = gtk::Label::builder()
            .label(&spec.description)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(spec.description_max_chars)
            .css_classes(["didactic-description"])
            .build();
        column.append(&description);
        description
    });

    (column, description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_path_trims_trailing_slash() {
        let spec = BigDidacticCardSpec::new("/app/illustrations/", "x.svg", "Title", "Body");
        assert_eq!(
            spec.resource_path().as_deref(),
            Some("/app/illustrations/x.svg")
        );
    }

    #[test]
    fn description_describes_card_and_control() {
        big_testkit::harness::require_gtk_test(|| {
            let control = gtk::Switch::new();
            let card = BigDidacticCard::new(
                BigDidacticCardSpec::plain("Unlimited Scrollback", "Keep all output."),
                Some(&control),
            );
            for described in [
                card.root().upcast_ref::<gtk::Accessible>(),
                control.upcast_ref(),
            ] {
                assert!(gtk::test_accessible_has_relation(
                    described,
                    gtk::AccessibleRelation::DescribedBy
                ));
            }
        });
    }

    #[test]
    fn plain_card_has_no_resource_path() {
        let spec = BigDidacticCardSpec::plain("Title", "Body");
        assert_eq!(spec.resource_path(), None);
        assert_eq!(spec.layout, BigDidacticCardLayout::Plain);
    }
}
