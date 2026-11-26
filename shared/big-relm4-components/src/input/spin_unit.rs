// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The unit a numeric field is written and read in.
//!
//! A unit changes only what the field shows and parses (`20 MB` for a stored
//! `20971520`, `100%`); the stored value keeps its shape.

use gtk::prelude::*;

/// How a stored number is written in the field a person edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BigSpinUnit {
    /// The number as stored.
    #[default]
    Plain,
    /// Stored in bytes, read and typed in megabytes.
    Megabytes,
    /// Stored in bytes, read and typed in kilobytes.
    Kilobytes,
    /// A percentage, written with its sign.
    Percent,
    /// A length on screen, written in pixels.
    Pixels,
    /// A duration, written in milliseconds.
    Milliseconds,
    /// A duration, written in seconds.
    Seconds,
}

impl BigSpinUnit {
    /// Bytes in one kilobyte, the scale the smaller stored byte counts are read at.
    const BYTES_PER_KILOBYTE: f64 = 1024.0;
    /// Bytes in one megabyte, the scale the stored byte counts are read at.
    const BYTES_PER_MEGABYTE: f64 = 1024.0 * 1024.0;

    /// Write a stored number the way the person reads it.
    #[must_use]
    pub fn display(self, value: f64) -> String {
        match self {
            Self::Plain => format!("{value:.0}"),
            Self::Percent => format!("{value:.0}%"),
            Self::Pixels => format!("{value:.0} px"),
            Self::Milliseconds => format!("{value:.0} ms"),
            Self::Seconds => format!("{value:.0} s"),
            Self::Megabytes => format!("{:.0} MB", value / Self::BYTES_PER_MEGABYTE),
            Self::Kilobytes => format!("{:.0} KB", value / Self::BYTES_PER_KILOBYTE),
        }
    }

    /// Read back a number typed in this unit, or `None` when it is not one.
    #[must_use]
    pub fn parse(self, text: &str) -> Option<f64> {
        // A pt_BR keyboard types "1,5"; the parser only knows the dot.
        let typed = text
            .trim()
            .trim_end_matches(|c: char| c == '%' || c.is_ascii_alphabetic() || c.is_whitespace())
            .trim()
            .replace(',', ".");
        let typed: f64 = typed.parse().ok()?;
        Some(match self {
            Self::Plain | Self::Percent | Self::Pixels | Self::Milliseconds | Self::Seconds => {
                typed
            }
            Self::Megabytes => typed * Self::BYTES_PER_MEGABYTE,
            Self::Kilobytes => typed * Self::BYTES_PER_KILOBYTE,
        })
    }
}

/// Widest text this unit ever shows, in characters.
///
/// A spin field is sized from the digits its adjustment allows, and the suffix
/// is not a digit. One spare character covers the caret.
fn width_chars_for(unit: BigSpinUnit, upper: f64) -> i32 {
    let widest = unit.display(upper).chars().count();
    i32::try_from(widest).unwrap_or(i32::MAX).saturating_add(1)
}

/// Write and read one spin button's number in `unit`.
///
/// Call this before the field is shown; it also writes the current value, since
/// nothing emits `output` until the value next changes.
pub fn install_spin_unit(spin: &gtk::SpinButton, unit: BigSpinUnit) {
    if unit == BigSpinUnit::Plain {
        return;
    }
    // A unit suffix is not a number, and a numeric field refuses to hold one:
    // with `numeric` left on, writing "100%" empties the field and zeroes the value.
    spin.set_numeric(false);
    spin.set_digits(0);
    spin.set_width_chars(
        spin.width_chars()
            .max(width_chars_for(unit, spin.adjustment().upper())),
    );
    spin.connect_output(move |spin| {
        spin.set_text(&unit.display(spin.value()));
        gtk::glib::Propagation::Stop
    });
    spin.connect_input(move |spin| Some(Ok(read_back(unit, &spin.text(), spin.value()))));
    // Writing the text directly, rather than asking for an update, keeps the
    // field from reading its own raw number back as if it were in this unit.
    spin.set_text(&unit.display(spin.value()));
}

/// Write and read one spin row's number in `unit`, as [`install_spin_unit`].
pub fn install_spin_row_unit(row: &adw::SpinRow, unit: BigSpinUnit) {
    if unit == BigSpinUnit::Plain {
        return;
    }
    row.set_numeric(false);
    row.set_width_chars(
        row.width_chars()
            .max(width_chars_for(unit, row.adjustment().upper())),
    );
    row.connect_output(move |row| {
        row.set_text(&unit.display(row.value()));
        true
    });
    row.connect_input(move |row| Some(Ok(read_back(unit, &row.text(), row.value()))));
    row.set_text(&unit.display(row.value()));
}

/// The value a unit field's `text` stands for: the field's own text for
/// `value`, rounded, keeps it, and so does text that is not a number, which
/// reported as a failed conversion would send the control to its maximum.
fn read_back(unit: BigSpinUnit, text: &str, value: f64) -> f64 {
    if text == unit.display(value) {
        return value;
    }
    unit.parse(text).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_number_reads_back_stored_value() {
        const TWENTY_MEGABYTES: f64 = 20.0 * 1024.0 * 1024.0;

        assert_eq!(BigSpinUnit::Megabytes.display(TWENTY_MEGABYTES), "20 MB");
        assert_eq!(
            BigSpinUnit::Megabytes.parse("20 MB"),
            Some(TWENTY_MEGABYTES)
        );
        assert_eq!(BigSpinUnit::Kilobytes.display(4096.0), "4 KB");
        assert_eq!(BigSpinUnit::Kilobytes.parse("4 KB"), Some(4096.0));
        assert_eq!(BigSpinUnit::Percent.display(100.0), "100%");
        assert_eq!(BigSpinUnit::Percent.parse("100%"), Some(100.0));
        assert_eq!(BigSpinUnit::Pixels.display(96.0), "96 px");
        assert_eq!(BigSpinUnit::Milliseconds.display(3000.0), "3000 ms");
        assert_eq!(BigSpinUnit::Seconds.display(600.0), "600 s");
        // The field is typed into, so it also has to take what a person types.
        assert_eq!(BigSpinUnit::Megabytes.parse("20"), Some(TWENTY_MEGABYTES));
        assert_eq!(BigSpinUnit::Percent.parse("1,5"), Some(1.5));
        assert_eq!(BigSpinUnit::Percent.parse("nada"), None);
    }

    /// A value the field shows rounded is kept when the field is read back,
    /// and a step smaller than the unit still moves it.
    #[test]
    fn rounded_value_survives_read_back() {
        big_testkit::harness::require_gtk_test(|| {
            const MEGABYTE: f64 = 1024.0 * 1024.0;
            let spin = gtk::SpinButton::new(
                Some(&gtk::Adjustment::new(
                    1.5 * MEGABYTE,
                    1024.0,
                    100.0 * MEGABYTE,
                    MEGABYTE,
                    MEGABYTE,
                    0.0,
                )),
                1.0,
                0,
            );
            install_spin_unit(&spin, BigSpinUnit::Megabytes);
            assert_eq!(spin.text(), "2 MB");
            spin.update();
            assert_eq!(spin.value(), 1.5 * MEGABYTE);
            spin.set_value(1024.0);
            spin.update();
            assert_eq!(spin.value(), 1024.0);
            spin.set_text("3");
            spin.update();
            assert_eq!(spin.value(), 3.0 * MEGABYTE, "typed text is read");

            let row = adw::SpinRow::new(
                Some(&gtk::Adjustment::new(150.0, 0.0, 200.0, 0.5, 10.0, 0.0)),
                1.0,
                0,
            );
            install_spin_row_unit(&row, BigSpinUnit::Percent);
            for _ in 0..3 {
                row.update();
                row.set_value(row.value() + 0.5);
            }
            row.update();
            assert_eq!(row.value(), 151.5);
        });
    }
}
