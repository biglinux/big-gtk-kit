// SPDX-License-Identifier: MIT
//! Whether the desktop's own processes leave core dumps. Off unless the
//! person switched crash reports on, which leaves the file [`path`]: a core
//! dump holds whatever the process held, and nobody asked for one by
//! default. Each process decides once, as it starts ([`apply`]).

use std::path::PathBuf;

/// The switch: present while crash reports are on.
#[must_use]
pub fn path() -> Option<PathBuf> {
    crate::settings::config_home_checked().map(|home| home.join("big-desktop/crash-reports"))
}

/// Whether the person switched crash reports on.
#[must_use]
pub fn wanted() -> bool {
    path().is_some_and(|path| path.exists())
}

/// Switch crash reports on or off for the processes that start from now on.
///
/// # Errors
///
/// When the switch cannot be written or removed.
pub fn set(on: bool) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("no configuration directory"))?;
    if on {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        crate::storage::atomic_write_private(&path, b"")
    } else {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// At the start of a desktop process, before any thread: no core dump
/// unless crash reports are on. Only the soft limit falls, and the programs
/// this process starts get the inherited one back
/// ([`crate::subprocess::give_back_limits`]). Answers whether dumps are on.
pub fn apply() -> bool {
    let wanted = wanted();
    if !wanted {
        crate::subprocess::lower_core_limit();
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_lives_in_configuration() {
        let path = path().expect("a configuration directory");
        assert!(path.ends_with("big-desktop/crash-reports"));
    }
}
