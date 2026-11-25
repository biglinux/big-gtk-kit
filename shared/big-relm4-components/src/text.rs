// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Text / number formatting helpers shared across BigLinux GTK apps.

/// Format a byte count in binary units, e.g. `3.4 MiB`, `12 GiB`, `512 B`,
/// with the decimal mark of the person's language (`3,4 MiB` in `pt_BR`).
///
/// The file managers' style: a second decimal place on a number over ten adds
/// nothing a person reads, so it is dropped there. Shared by every product that
/// shows a file or disk size, so one file rounds one way.
#[must_use]
pub fn binary_size(bytes: u64) -> String {
    size_with_mark(bytes, big_os_kit::decimal_mark())
}

/// The name of `content_type` in the person's language, as a file manager
/// shows it: the shared-mime-info comment, else GIO's description.
#[must_use]
pub fn file_type_name(content_type: &str) -> String {
    let mut data_dirs = vec![gtk::glib::user_data_dir()];
    data_dirs.extend(gtk::glib::system_data_dirs());
    big_os_kit::mime_comment::translated_comment(
        content_type,
        &gtk::glib::language_names(),
        &data_dirs,
    )
    .unwrap_or_else(|| gtk::gio::content_type_get_description(content_type).to_string())
}

/// A transfer rate in the units of [`binary_size`], `1,2 MiB/s` in `pt_BR`.
#[must_use]
pub fn binary_rate(bytes_per_second: u64) -> String {
    rate_with_mark(bytes_per_second, big_os_kit::decimal_mark())
}

fn rate_with_mark(bytes_per_second: u64, mark: char) -> String {
    format!("{}/s", size_with_mark(bytes_per_second, mark))
}

fn size_with_mark(bytes: u64, mark: char) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sizes are displayed with at most one decimal"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit]).replacen('.', mark.encode_utf8(&mut [0; 4]), 1)
    }
}

/// `value` to one decimal place with the decimal mark of the person's language,
/// `628,0` in `pt_BR`: the number of a rate, which keeps the units of its place.
#[must_use]
pub fn one_decimal(value: f64) -> String {
    decimal_with_mark(value, big_os_kit::decimal_mark())
}

fn decimal_with_mark(value: f64, mark: char) -> String {
    format!("{value:.1}").replacen('.', mark.encode_utf8(&mut [0; 4]), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_rounds_in_binary_units() {
        assert_eq!(size_with_mark(999, ','), "999 B");
        assert_eq!(size_with_mark(2_355, ','), "2,3 KiB");
        assert_eq!(size_with_mark(10 * 1024, ','), "10 KiB");
        assert_eq!(size_with_mark(3 * 1024 * 1024, '.'), "3.0 MiB");
    }

    #[test]
    fn rate_reads_in_binary_units() {
        assert_eq!(rate_with_mark(0, ','), "0 B/s");
        assert_eq!(rate_with_mark(1_258_291, ','), "1,2 MiB/s");
    }

    #[test]
    fn rate_number_takes_mark() {
        assert_eq!(decimal_with_mark(628.04, ','), "628,0");
        assert_eq!(decimal_with_mark(1.56, '.'), "1.6");
        assert_eq!(decimal_with_mark(12.0, ','), "12,0");
    }
}
