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

    /// A core dump holds the process's secrets: none unless the switch is
    /// there. Checked in child runs, which own their limits and environment.
    #[test]
    fn core_dumps_follow_the_switch() {
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        const CHILD: &str = "BIG_OS_KIT_CHILD_CRASH_REPORTS";
        if let Some(switch) = std::env::var_os(CHILD) {
            let limit = getrlimit(Resource::Core);
            let on = switch == "on";
            // A hard limit of 0 leaves nothing to lower, only the switch to read.
            if limit.maximum == Some(0) {
                assert_eq!(apply(), on);
                return;
            }
            let raised = Rlimit {
                current: limit.maximum,
                maximum: limit.maximum,
            };
            setrlimit(Resource::Core, raised).unwrap();
            assert_eq!(apply(), on);
            assert_eq!(getrlimit(Resource::Core).current == Some(0), !on);
            return;
        }
        let config =
            std::env::temp_dir().join(format!("big-os-kit-crash-reports-{}", std::process::id()));
        std::fs::create_dir_all(config.join("big-desktop")).unwrap();
        for switch in ["off", "on"] {
            if switch == "on" {
                std::fs::write(config.join("big-desktop/crash-reports"), b"").unwrap();
            }
            let child = crate::subprocess::BigSubprocessSpec::builder()
                .program(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "crash_reports::tests::core_dumps_follow_the_switch",
                    "--quiet",
                ])
                .env_inherit(false)
                .env(CHILD, switch)
                .env("XDG_CONFIG_HOME", &config)
                .stdout(crate::subprocess::BigSubprocessOutputMode::Capture)
                .build()
                .run()
                .expect("child run");
            assert!(child.status.success(), "{switch}: {}", child.stdout_lossy());
        }
        std::fs::remove_dir_all(&config).unwrap();
    }
}
