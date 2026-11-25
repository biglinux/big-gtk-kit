// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Compact vertical scale popover for toolbar controls.

const DEFAULT_SPACING: i32 = 6;
const DEFAULT_HEIGHT: i32 = 160;

/// Display-free specification describing big vertical scale popover behaviour.
#[derive(Debug, Clone, PartialEq)]
pub struct BigVerticalScalePopoverSpec {
    /// Title.
    pub title: String,
    /// Accessible label.
    pub accessible_label: String,
    /// Minimum.
    pub minimum: f64,
    /// Maximum.
    pub maximum: f64,
    /// Step.
    pub step: f64,
    /// Initial.
    pub initial: f64,
    /// Height request.
    pub height_request: i32,
    /// Inverted.
    pub inverted: bool,
    /// Spacing.
    pub spacing: i32,
}

impl BigVerticalScalePopoverSpec {
    /// Creates a new instance.
    #[must_use]
    pub fn new(title: impl Into<String>, accessible_label: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            accessible_label: accessible_label.into(),
            minimum: 0.0,
            maximum: 1.0,
            step: 0.1,
            initial: 0.0,
            height_request: DEFAULT_HEIGHT,
            inverted: true,
            spacing: DEFAULT_SPACING,
        }
    }

    /// Configure the `range` setting and return the updated builder.
    ///
    /// The supplied `minimum` replaces any stored value; chain
    /// further `with_*` / setter calls to compose the full [`BigVerticalScalePopoverSpec`].
    #[must_use]
    pub fn range(mut self, minimum: f64, maximum: f64, step: f64) -> Self {
        self.minimum = minimum;
        self.maximum = maximum;
        self.step = step;
        self
    }

    /// Configure the `initial` setting and return the updated builder.
    ///
    /// The supplied `initial` replaces any stored value; chain
    /// further `with_*` / setter calls to compose the full [`BigVerticalScalePopoverSpec`].
    #[must_use]
    pub fn initial(mut self, initial: f64) -> Self {
        self.initial = initial;
        self
    }

    /// Configure the `height_request` setting and return the updated builder.
    ///
    /// The supplied `height_request` replaces any stored value; chain
    /// further `with_*` / setter calls to compose the full [`BigVerticalScalePopoverSpec`].
    #[must_use]
    pub fn height_request(mut self, height_request: i32) -> Self {
        self.height_request = height_request;
        self
    }
}

#[cfg(test)]
mod tests {}
