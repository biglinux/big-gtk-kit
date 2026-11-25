// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `git` on a repository whose configuration is not the person's own choice: a
//! folder from a download or a USB stick can carry a `.git/config` that names
//! commands.

/// The `git` this suite runs.
pub const GIT: &str = "/usr/bin/git";

/// Options before the subcommand that keep the repository's configuration
/// from running a command: no filesystem monitor, no hooks, no external diff,
/// no pager, no signature check. Filters and `textconv` drivers have names a
/// repository chooses, so they cannot be switched off here: a caller that
/// diffs passes `--no-textconv`, and one that reads the working tree confines
/// the process.
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
