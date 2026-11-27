// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The log backend of a standalone app.

/// Send `log` records to stderr, `warn` and above unless `RUST_LOG` says otherwise.
///
/// Call once at the top of `main`. Without a backend every `log::warn!` of the
/// app and of the shared crates it uses is dropped. Hosted apps do not call it:
/// `big-host` installs the logger for its own copy of `log`, and each module
/// library's composition installs one for its copy.
pub fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
}
