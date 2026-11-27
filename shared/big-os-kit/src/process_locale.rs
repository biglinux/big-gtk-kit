// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The process locale: numbers in `C` for native libraries, and the decimal
//! mark people read.

/// The person's own numeric locale (`LC_ALL`, else `LC_NUMERIC`, else empty),
/// kept by [`init_c_numeric_locale`] for the programs started for them
/// ([`crate::subprocess::person_environment`]). In the environment, not a
/// static: a module library links its own copy of this crate.
pub(crate) const PERSON_NUMERIC_LOCALE: &str = "BIG_PERSON_LC_NUMERIC";

/// Force the process numeric locale to `C` before any worker thread starts.
///
/// Native libraries that parse machine-readable decimal values through libc
/// require a dot separator even when the user session uses another locale.
/// Every program this process starts inherits the pin; one started for the
/// person gets their own back only through
/// [`crate::subprocess::person_environment`].
///
/// # Safety
///
/// No other thread may run: it changes the environment and calls `setlocale`,
/// neither of which another thread may do or read at the same time.
pub unsafe fn init_c_numeric_locale() {
    // The categories `LC_ALL` sets, numbers aside.
    const CATEGORIES: [&str; 12] = [
        "LANG",
        "LC_CTYPE",
        "LC_COLLATE",
        "LC_TIME",
        "LC_MONETARY",
        "LC_MESSAGES",
        "LC_PAPER",
        "LC_NAME",
        "LC_ADDRESS",
        "LC_TELEPHONE",
        "LC_MEASUREMENT",
        "LC_IDENTIFICATION",
    ];
    // SAFETY: the caller guarantees no other thread runs.
    unsafe {
        // A second pin would keep `C` as the person's.
        if std::env::var_os(PERSON_NUMERIC_LOCALE).is_none() {
            let person = ["LC_ALL", "LC_NUMERIC"]
                .into_iter()
                .filter_map(std::env::var_os)
                .find(|value| !value.is_empty())
                .unwrap_or_default();
            std::env::set_var(PERSON_NUMERIC_LOCALE, person);
        }
        // A user-set LC_ALL overrides every LC_* variable when gettext later
        // calls `setlocale(LC_ALL, "")`, undoing the LC_NUMERIC pin below:
        // libmpv's `mpv_create` then returns NULL under, for example,
        // LC_ALL=pt_BR.UTF-8. It becomes each other category instead, so an
        // LC_MESSAGES it overrode stays overridden. An empty LC_ALL is unset
        // to glibc, and leaves the categories as they are.
        if let Some(session_locale) = std::env::var_os("LC_ALL") {
            if !session_locale.is_empty() {
                for category in CATEGORIES {
                    std::env::set_var(category, &session_locale);
                }
            }
            std::env::remove_var("LC_ALL");
        }
        std::env::set_var("LC_NUMERIC", "C");
        libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr());
    }
}

/// Set every locale category from the environment, as `setlocale(LC_ALL, "")`
/// does; `false` when the environment names a locale this system lacks, which
/// leaves the locale as it was.
///
/// `setlocale` races with any other thread reading the locale or the
/// environment, so this runs only while the process has one thread.
///
/// # Errors
///
/// `/proc/self/task` cannot be read, or another thread runs.
pub fn set_locale_from_environment() -> std::io::Result<bool> {
    let threads = std::fs::read_dir("/proc/self/task")?.count();
    if threads != 1 {
        return Err(std::io::Error::other(format!(
            "{threads} threads run: the locale is set before any other starts"
        )));
    }
    // SAFETY: this is the process's only thread, and nothing it runs between
    // the count and the call starts another.
    let set = unsafe { libc::setlocale(libc::LC_ALL, c"".as_ptr()) };
    Ok(!set.is_null())
}

/// The decimal mark of the language the person reads, `,` for `pt_BR`.
///
/// Taken from the numeric data of the message locale the environment names,
/// as `setlocale(LC_ALL, "")` resolves it: [`init_c_numeric_locale`] pins the
/// process's own `LC_NUMERIC` to `C`, which always answers `.`. Read once.
pub fn decimal_mark() -> char {
    static MARK: std::sync::OnceLock<char> = std::sync::OnceLock::new();
    *MARK.get_or_init(|| {
        let Some(messages) = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty())
            .and_then(|value| std::ffi::CString::new(value).ok())
        else {
            return '.';
        };
        // SAFETY: `messages` is NUL-terminated; the locale object is private to
        // this call and freed once read.
        unsafe {
            let numeric = libc::newlocale(
                libc::LC_NUMERIC_MASK,
                messages.as_ptr(),
                std::ptr::null_mut(),
            );
            if numeric.is_null() {
                return '.';
            }
            let mark = std::ffi::CStr::from_ptr(libc::nl_langinfo_l(libc::RADIXCHAR, numeric))
                .to_str()
                .ok()
                .and_then(|text| text.chars().next())
                .unwrap_or('.');
            libc::freelocale(numeric);
            mark
        }
    })
}

#[cfg(test)]
mod tests {
    /// Beside a second thread the locale is left alone and the call refused;
    /// the single-thread case runs in `tests/process_locale.rs`.
    #[test]
    fn locale_refused_beside_thread() {
        let (stop, wait) = std::sync::mpsc::channel::<()>();
        let second = std::thread::spawn(move || wait.recv());
        let beside = super::set_locale_from_environment();
        drop(stop);
        let _ = second.join();
        assert!(beside.is_err(), "{beside:?}");
    }
}
