// SPDX-License-Identifier: MIT

//! The decimal mark under the shared host's locale, in a process of its own:
//! the environment is process-global and the mark is read once.

#[test]
fn pinned_numeric_keeps_language_mark() {
    // Package builders often generate only C.UTF-8.
    // SAFETY: a private locale object, freed at once.
    unsafe {
        let pt_br = libc::newlocale(
            libc::LC_ALL_MASK,
            c"pt_BR.UTF-8".as_ptr(),
            std::ptr::null_mut(),
        );
        if pt_br.is_null() {
            eprintln!("skipped: the pt_BR.UTF-8 locale is not generated");
            return;
        }
        libc::freelocale(pt_br);
    }
    // The session's language over a message locale it overrides, then the
    // bootstrap that pins numbers to `C`.
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe {
        std::env::set_var("LC_ALL", "pt_BR.UTF-8");
        std::env::set_var("LC_MESSAGES", "en_US.UTF-8");
        big_os_kit::init_c_numeric_locale();
    }
    assert_eq!(std::env::var("LC_MESSAGES").as_deref(), Ok("pt_BR.UTF-8"));
    assert_eq!(big_os_kit::decimal_mark(), ',');
}
