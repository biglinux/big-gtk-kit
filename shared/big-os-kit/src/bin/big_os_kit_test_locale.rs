// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Test-only process that sets its locale on its only thread and prints the
//! answer: `set`, `missing` or the refusal. Given `pin-numeric`, it pins the
//! numeric locale instead and prints the `LANG` and `LC_ALL` left after it.

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|mode| mode == "pin-numeric")
    {
        // SAFETY: no other thread has started in this process.
        unsafe { big_os_kit::init_c_numeric_locale() };
        println!(
            "{:?} {:?}",
            std::env::var_os("LANG"),
            std::env::var_os("LC_ALL")
        );
        return;
    }
    match big_os_kit::set_locale_from_environment() {
        Ok(true) => println!("set"),
        Ok(false) => println!("missing"),
        Err(error) => println!("refused: {error}"),
    }
}
