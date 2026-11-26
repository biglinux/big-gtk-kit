// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The locale set on a process's only thread, from the language its
//! environment names: one the system has, and one it lacks.

use big_os_kit::subprocess::{BigSubprocessOutputMode, BigSubprocessSpec};

#[test]
fn locale_set_on_only_thread() {
    for (lang, expected) in [("C.UTF-8", "set"), ("xx_XX.UTF-8", "missing")] {
        let output = BigSubprocessSpec::builder()
            .program(env!("CARGO_BIN_EXE_big-os-kit-test-locale"))
            .env_inherit(false)
            .env("LANG", lang)
            .stdout(BigSubprocessOutputMode::Capture)
            .build()
            .run()
            .expect("the locale probe runs");
        assert_eq!(output.stdout_lossy().trim(), expected, "{lang}");
    }
}
