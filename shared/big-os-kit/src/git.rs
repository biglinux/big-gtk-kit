// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `git` on a repository whose configuration is not the person's own choice: a
//! folder from a download or a USB stick can carry a `.git/config` that names
//! commands.

/// The `git` this crate runs.
pub const GIT: &str = "/usr/bin/git";

/// Options before the subcommand that keep the repository's configuration
/// from running a command: no filesystem monitor, no hooks, no pager, no
/// signature check. Diff drivers, `textconv` drivers and filters have names a
/// repository chooses, so they cannot be switched off here: a caller that
/// diffs passes `--no-ext-diff` and `--no-textconv`, and one that reads the
/// working tree confines the process. The empty `diff.external` replaces the
/// repository's own, so a diff without `--no-ext-diff` fails instead of
/// running it; a driver an attribute names still runs then.
pub const HARDENING: [&str; 10] = {
    let mut options = [""; 10];
    let mut index = 0;
    while index < HARDENING_KEEPING_HOOKS.len() {
        options[index] = HARDENING_KEEPING_HOOKS[index];
        index += 1;
    }
    options[index] = "-c";
    options[index + 1] = "core.hooksPath=/dev/null";
    options
};

/// [`HARDENING`] with the repository's hooks left on, for a change the person
/// asks for (staging, a commit, a pull), which runs them as a terminal would.
pub const HARDENING_KEEPING_HOOKS: [&str; 8] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "diff.external=",
    "-c",
    "core.pager=cat",
    "-c",
    "log.showSignature=false",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess::BigSubprocessSpec;

    /// A diff under [`HARDENING`] runs none of the commands a repository names
    /// for it: without `--no-ext-diff` it fails, and with both flags it is
    /// git's own.
    #[test]
    fn repository_diff_commands_never_run() {
        let repository = std::env::temp_dir().join(format!("big-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repository);
        std::fs::create_dir_all(&repository).unwrap();
        let ran = repository.join("ran");
        let git = |args: &[&str]| {
            BigSubprocessSpec::builder()
                .program(GIT)
                .args(HARDENING)
                .arg("-C")
                .arg(&repository)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .build()
                .run()
                .expect("git runs")
        };
        let mark = format!("touch {}", ran.display());
        assert!(git(&["init", "--quiet"]).status.success());
        for (key, value) in [
            ("diff.external", mark.as_str()),
            ("diff.named.command", mark.as_str()),
            ("diff.named.textconv", mark.as_str()),
        ] {
            assert!(git(&["config", key, value]).status.success());
        }
        std::fs::write(repository.join(".gitattributes"), "file diff=named\n").unwrap();
        for name in ["file", "plain"] {
            std::fs::write(repository.join(name), "one\n").unwrap();
            assert!(git(&["add", name]).status.success());
            std::fs::write(repository.join(name), "two\n").unwrap();
        }
        let unflagged = git(&["diff", "--", "plain"]);
        let diff = git(&["diff", "--no-ext-diff", "--no-textconv", "--", "file"]);
        let commands_ran = ran.exists();
        std::fs::remove_dir_all(&repository).unwrap();
        assert!(!unflagged.status.success(), "the empty diff.external fails");
        assert!(diff.status.success(), "{}", diff.stderr_lossy());
        assert!(!commands_ran);
        assert!(
            diff.stdout_lossy().contains("\n-one\n+two\n"),
            "{}",
            diff.stdout_lossy()
        );
    }
}
