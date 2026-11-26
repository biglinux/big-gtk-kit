// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The process locale: numbers in `C` for native libraries, and the decimal
//! mark people read.

/// Force the process numeric locale to `C` before any worker thread starts.
///
/// Native libraries that parse machine-readable decimal values through libc
/// require a dot separator even when the user session uses another locale.
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
    /// An empty `LC_ALL` is unset, as glibc reads it: the language stays the
    /// one `LANG` names. Checked in a child run, which owns its environment.
    #[test]
    fn empty_lc_all_keeps_lang() {
        if std::env::var_os("BIG_OS_KIT_CHILD_LOCALE").is_some() {
            // SAFETY: the child runs this one test, on the only thread that
            // touches the environment.
            unsafe { super::init_c_numeric_locale() };
            assert_eq!(std::env::var("LANG").as_deref(), Ok("pt_BR.UTF-8"));
            assert_eq!(std::env::var_os("LC_ALL"), None);
            return;
        }
        let child = crate::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "process_locale::tests::empty_lc_all_keeps_lang",
                "--quiet",
            ])
            .env_inherit(false)
            .env("BIG_OS_KIT_CHILD_LOCALE", "1")
            .env("LC_ALL", "")
            .env("LANG", "pt_BR.UTF-8")
            .build()
            .run()
            .expect("child run");
        assert!(child.status.success(), "{}", child.stdout_lossy());
    }
}
