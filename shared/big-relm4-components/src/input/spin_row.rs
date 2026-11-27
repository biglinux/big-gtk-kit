// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A spin row a screen reader can reach.
//!
//! libadwaita makes `AdwSpinRow` a presentation, and GTK's AT-SPI bridge then
//! leaves out the row and the spin button inside it. As a list item both are in
//! the tree, and the spin button carries the row's title. The role is a
//! construct-only property, so a row made any other way cannot be fixed later.
//! Its title is drawn as typed, never read as Pango markup.

/// An `AdwSpinRow` builder whose row is a list item with a plain-text title.
pub fn builder() -> adw::builders::SpinRowBuilder {
    adw::SpinRow::builder()
        .accessible_role(gtk::AccessibleRole::ListItem)
        .use_markup(false)
}

/// [`adw::SpinRow::with_range`], as a list item.
#[must_use]
pub fn with_range(min: f64, max: f64, step: f64) -> adw::SpinRow {
    // Digits as libadwaita derives them: none for a whole step, else the
    // decimal places of the step.
    let digits = if step.abs() >= 1.0 || step == 0.0 {
        0
    } else {
        (step.abs().log10().floor().abs() as u32).min(20)
    };
    builder()
        .adjustment(&gtk::Adjustment::new(min, min, max, step, step * 10.0, 0.0))
        .climb_rate(step)
        .digits(digits)
        .numeric(true)
        .build()
}

#[cfg(test)]
mod tests {

    #[test]
    fn range_row_keeps_numeric_input() {
        big_testkit::harness::require_gtk_test(|| {
            let row = super::with_range(0.0, 1.0, 0.1);
            assert!(row.is_numeric());
            assert_eq!((row.digits(), row.climb_rate()), (1, 0.1));
        });
    }
}
