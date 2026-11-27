// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! One word of text for a shell the program does not choose: the shell of a
//! terminal it types into, or the login shell a server hands a command to.
//!
//! POSIX shells and fish read single quotes differently: fish takes `\\` and
//! `\'` inside them as escapes, so the usual `'\''` form lets a name holding a
//! backslash before a quote end the word early in fish and run the rest as a
//! command. Here a quote and a backslash never sit inside single quotes: each
//! is a double-quoted word of its own (`"'"`, `"\\"`), which every one of
//! them reads alike.

/// `text` as one shell word that sh, bash, zsh, dash, ksh and fish read back
/// as `text`.
///
/// Control characters pass through unchanged: a caller that types the word
/// into a terminal refuses them first, since a newline there runs the line.
#[must_use]
pub fn shell_word(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len().saturating_add(2));
    let mut inside = false;
    for character in text.chars() {
        let alone = match character {
            '\'' => "\"'\"",
            '\\' => "\"\\\\\"",
            _ => {
                if !inside {
                    quoted.push('\'');
                    inside = true;
                }
                quoted.push(character);
                continue;
            }
        };
        if inside {
            quoted.push('\'');
            inside = false;
        }
        quoted.push_str(alone);
    }
    if inside || quoted.is_empty() {
        if quoted.is_empty() {
            quoted.push('\'');
        }
        quoted.push('\'');
    }
    quoted
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::shell_word;
    use crate::subprocess::BigSubprocessSpec;

    #[test]
    fn quotes_and_backslashes_stand_alone() {
        assert_eq!(shell_word(""), "''");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("O'Brien"), "'O'\"'\"'Brien'");
        assert_eq!(shell_word("'"), "\"'\"");
        assert_eq!(shell_word("a\\b"), "'a'\"\\\\\"'b'");
        assert_eq!(shell_word("x\\'"), "'x'\"\\\\\"\"'\"");
        assert_eq!(shell_word("-rf"), "'-rf'");
        assert_eq!(shell_word("$(id)`id`"), "'$(id)`id`'");
    }

    /// Every shell installed here reads each hostile word back as itself.
    #[test]
    fn shells_read_the_word_back() {
        let hostile = [
            "plain",
            "with space",
            "O'Brien",
            "back\\slash",
            "x\\'; touch /nonexistent/pwned; echo \\'",
            "\\\\",
            "trailing\\",
            "$(touch /nonexistent/pwned)",
            "`id` $HOME ${HOME} ~ *",
            "-n",
            "\"double\" quotes",
            "",
        ];
        let shells: Vec<_> = ["sh", "bash", "zsh", "dash", "ksh", "fish"]
            .into_iter()
            .map(|name| Path::new("/usr/bin").join(name))
            .filter(|path| path.is_file())
            .collect();
        assert!(shells.contains(&Path::new("/usr/bin/sh").to_path_buf()));
        for shell in &shells {
            for text in hostile {
                let output = BigSubprocessSpec::builder()
                    .program(shell)
                    .args(["-c".to_owned(), format!("printf %s {}", shell_word(text))])
                    .env("HOME", "/nonexistent")
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .run()
                    .expect("shell runs");
                assert!(output.status.success(), "{shell:?}: {output:?}");
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout),
                    text,
                    "{shell:?} read {text:?}"
                );
            }
        }
    }
}
