// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! File modification times, as file rows, previews and dialogs show them.

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
}
