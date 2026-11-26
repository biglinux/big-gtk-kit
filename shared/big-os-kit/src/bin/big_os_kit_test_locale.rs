// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Test-only process that sets its locale on its only thread and prints the
//! answer: `set`, `missing` or the refusal.

fn main() {
    match big_os_kit::set_locale_from_environment() {
        Ok(true) => println!("set"),
        Ok(false) => println!("missing"),
        Err(error) => println!("refused: {error}"),
    }
}
