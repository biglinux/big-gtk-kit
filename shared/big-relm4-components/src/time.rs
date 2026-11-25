// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Shared media and dialog time formatting.
//!
//! Pure helpers used by transports, queue rows, file-info dialogs, and editor
//! controls. All formatters tolerate `NaN`, infinities, and negative input by
//! returning a stable placeholder instead of panicking.

/// When a file last changed, on this machine's clock and in this machine's
/// own way of writing a date; empty for no time at all.
#[must_use]
pub fn format_mtime(unix_secs: i64) -> String {
    // Read once: looking the zone up again checks /etc/localtime for every row.
    thread_local!(static LOCAL: gtk::glib::TimeZone = gtk::glib::TimeZone::local());
    if unix_secs == 0 {
        return String::new();
    }
    gtk::glib::DateTime::from_unix_utc(unix_secs)
        .and_then(|when| LOCAL.with(|zone| when.to_timezone(zone)))
        .and_then(|when| when.format("%x %H:%M"))
        .map_or_else(|_| String::new(), Into::into)
}

/// Format `total_secs` for a media library row, e.g. `"1h 05min"` or `"5 min"`.
///
/// Floors to minute precision and omits the hours field when the duration is
/// under an hour.
///
/// # Examples
///
/// ```
/// use big_relm4_components::time::duration_hm;
///
/// assert_eq!(duration_hm(300.0), "5 min");
/// assert_eq!(duration_hm(3900.0), "1h 05min");
/// ```
#[must_use]
pub fn duration_hm(total_secs: f64) -> String {
    if total_secs <= 0.0 || !total_secs.is_finite() {
        return "0 min".to_string();
    }
    let total_mins = (total_secs / 60.0) as u64;
    let hours = total_mins / 60;
    let mins = total_mins % 60;
    if hours > 0 {
        format!("{hours}h {mins:02}min")
    } else {
        format!("{mins} min")
    }
}

/// Parse a user-entered time string into seconds.
///
/// Accepts plain decimal seconds (`"123.5"`), `M:SS` form (`"2:30"`) and
/// `H:MM:SS` form (`"1:02:03"`), with spaces around any field.
///
/// No field is bounded, because people type both halves of the same thought:
/// `"90"` alone is a minute and a half, and `"1:90"` is two and a half minutes.
/// Anything else (a negative number, a fourth field, an empty field, a word)
/// returns [`None`] so the caller can keep the value it already had. Guessing
/// what a half-typed time meant is how a typo becomes a cut in the wrong place.
///
/// # Examples
///
/// ```
/// use big_relm4_components::time::parse_playback_time;
///
/// assert_eq!(parse_playback_time("90"), Some(90.0));
/// assert_eq!(parse_playback_time("1:90"), Some(150.0));
/// assert_eq!(parse_playback_time(" 1:02:03 "), Some(3723.0));
/// assert_eq!(parse_playback_time("-5"), None);
/// ```
#[must_use]
pub fn parse_playback_time(input: &str) -> Option<f64> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let mut seconds = 0.0;
    for (index, field) in input.split(':').enumerate() {
        let field = field.trim();
        if field.is_empty() || index >= 3 {
            return None;
        }
        let value: f64 = field.parse().ok()?;
        if value < 0.0 || !value.is_finite() {
            return None;
        }
        seconds = seconds * 60.0 + value;
    }
    Some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The time on screen is the one the person's own clock shows. The
    /// expectation is built through a different GLib path than the one under
    /// test.
    #[test]
    fn mtime_uses_local_clock() {
        assert_eq!(format_mtime(0), "");
        const NEW_YEAR_UTC: i64 = 1_704_067_200;
        let local = gtk::glib::DateTime::from_unix_utc(NEW_YEAR_UTC)
            .and_then(|when| when.to_local())
            .expect("a real instant has a local time");
        let shown = format_mtime(NEW_YEAR_UTC);
        assert!(
            shown.ends_with(&format!("{:02}:{:02}", local.hour(), local.minute())),
            "not the local clock: {shown}"
        );
    }

    #[test]
    fn duration_hm_formats_library_totals() {
        assert_eq!(duration_hm(0.0), "0 min");
        assert_eq!(duration_hm(300.0), "5 min");
        assert_eq!(duration_hm(3900.0), "1h 05min");
        assert_eq!(duration_hm(f64::INFINITY), "0 min");
        assert_eq!(duration_hm(f64::NAN), "0 min");
    }

    #[test]
    fn playback_time_accepts_all_forms() {
        assert_eq!(parse_playback_time("123.5"), Some(123.5));
        assert_eq!(parse_playback_time("2:30"), Some(150.0));
        assert_eq!(parse_playback_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_playback_time(" 1 : 02 : 03 "), Some(3723.0));
    }

    /// No field is bounded: people type "90" for a minute and a half, and
    /// "1:90" for two and a half minutes, and mean it both times.
    #[test]
    fn playback_time_fields_overflow() {
        assert_eq!(parse_playback_time("90"), Some(90.0));
        assert_eq!(parse_playback_time("1:90"), Some(150.0));
        assert_eq!(parse_playback_time("0:0:90"), Some(90.0));
    }

    /// A half-typed time is refused so the caller keeps the value it had; a
    /// guess here becomes a cut in the wrong place.
    #[test]
    fn playback_time_refuses_nonsense() {
        assert_eq!(parse_playback_time("-5"), None);
        assert_eq!(parse_playback_time("1:-5"), None);
        assert_eq!(parse_playback_time("inf"), None);
        assert_eq!(parse_playback_time("1:inf"), None);
        assert_eq!(parse_playback_time("abc:def"), None);
        assert_eq!(parse_playback_time("1:2:3:4"), None);
        assert_eq!(parse_playback_time("1::3"), None);
        assert_eq!(parse_playback_time(":30"), None);
        assert_eq!(parse_playback_time(""), None);
    }
}
