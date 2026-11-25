// SPDX-License-Identifier: MIT

//! Pure redaction helpers for diagnostics reports.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// Display adapter that masks the middle characters of user-data-bearing text.
///
/// The first and last character remain visible for diagnostics correlation.
/// Strings with one or two characters collapse to `**` so tiny secrets are not
/// leaked.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::Redacted;
///
/// assert_eq!(Redacted("secret").to_string(), "s****t");
/// assert_eq!(format!("{:?}", Redacted("ab")), "**");
/// ```
pub struct Redacted<T: fmt::Display>(pub T);

impl<T: fmt::Display> fmt::Display for Redacted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.0.to_string();
        let characters: Vec<char> = text.chars().collect();
        match characters.len() {
            0 => formatter.write_str("<empty>"),
            1 | 2 => formatter.write_str("**"),
            length => {
                write!(formatter, "{}", characters[0])?;
                for _ in 1..length - 1 {
                    formatter.write_str("*")?;
                }
                write!(formatter, "{}", characters[length - 1])
            }
        }
    }
}

impl<T: fmt::Display> fmt::Debug for Redacted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Redaction rules used by diagnostics report assembly.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::{redact_diagnostics_text, BigRedactionRules};
///
/// let rules = BigRedactionRules::empty()
///     .with_xdg_root("xdg-config", "/home/user/.config")
///     .with_private_token("user");
/// let text = redact_diagnostics_text("/home/user/.config/big/app.conf", &rules);
/// assert_eq!(text, "<xdg-config>/big/app.conf");
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BigRedactionRules {
    xdg_roots: Vec<XdgRoot>,
    private_tokens: Vec<String>,
}

impl BigRedactionRules {
    /// Create rules with no allowed XDG roots or private tokens.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            xdg_roots: Vec::new(),
            private_tokens: Vec::new(),
        }
    }

    /// Create rules from XDG and identity environment variables.
    #[must_use]
    pub fn from_environment() -> Self {
        let mut rules = Self::empty();

        if let Some(config_home) = crate::settings::config_home_checked() {
            rules = rules.with_xdg_root("xdg-config", config_home);
        }
        if let Some(data_home) = crate::settings::data_home_checked() {
            rules = rules.with_xdg_root("xdg-data", data_home);
        }
        if let Some(cache_home) = crate::settings::cache_home_checked() {
            rules = rules.with_xdg_root("xdg-cache", cache_home);
        }
        if let Some(state_home) = crate::settings::state_home_checked() {
            rules = rules.with_xdg_root("xdg-state", state_home);
        }
        if let Some(runtime_dir) = env_path("XDG_RUNTIME_DIR") {
            rules = rules.with_xdg_root("xdg-runtime", runtime_dir);
        }

        for name in ["USER", "USERNAME", "LOGNAME", "HOSTNAME"] {
            if let Ok(value) = env::var(name) {
                rules = rules.with_private_token(value);
            }
        }

        rules
    }

    /// Add an XDG root whose absolute prefix is replaced by `<marker>`.
    #[must_use]
    pub fn with_xdg_root(mut self, marker: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let path = normalize_path_string(path.into());
        if !path.is_empty() {
            self.xdg_roots.push(XdgRoot {
                marker: marker.into(),
                path,
            });
            self.xdg_roots
                .sort_by_key(|root| std::cmp::Reverse(root.path.len()));
        }
        self
    }

    /// Add a private token, such as a username or hostname, to redact from text.
    #[must_use]
    pub fn with_private_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into();
        if token.chars().count() >= 3 && !self.private_tokens.contains(&token) {
            self.private_tokens.push(token);
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct XdgRoot {
    marker: String,
    path: String,
}

/// Redact paths, identity tokens, and secret assignments from diagnostics text.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::{redact_diagnostics_text, BigRedactionRules};
///
/// let rules = BigRedactionRules::empty().with_private_token("workstation");
/// let redacted = redact_diagnostics_text("HOST=workstation API_TOKEN=abc123", &rules);
/// assert_eq!(redacted, "HOST=<redacted> API_TOKEN=<redacted>");
/// ```
#[must_use]
pub fn redact_diagnostics_text(text: &str, rules: &BigRedactionRules) -> String {
    let without_secrets = redact_secret_assignments(text);
    let without_private_paths = redact_paths_in_text(&without_secrets, rules);
    redact_private_tokens(&without_private_paths, rules)
}

/// Redact one absolute path according to XDG-root rules.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::{redact_diagnostics_path, BigRedactionRules};
///
/// let rules = BigRedactionRules::empty().with_xdg_root("xdg-config", "/home/user/.config");
/// assert_eq!(
///     redact_diagnostics_path("/home/user/.config/big/app.conf", &rules),
///     "<xdg-config>/big/app.conf"
/// );
/// assert_eq!(
///     redact_diagnostics_path("/home/user/secret/file.txt", &rules),
///     "<redacted>/file.txt"
/// );
/// ```
#[must_use]
pub fn redact_diagnostics_path(path: &str, rules: &BigRedactionRules) -> String {
    for root in &rules.xdg_roots {
        if path == root.path {
            return format!("<{}>", root.marker);
        }
        if let Some(relative) = path.strip_prefix(&root.path)
            && let Some(relative) = relative.strip_prefix('/')
        {
            return format!("<{}>/{relative}", root.marker);
        }
    }
    path_basename(path)
        .map(|basename| format!("<redacted>/{basename}"))
        .unwrap_or_else(|| "<redacted>".to_owned())
}

/// Redact a filesystem path while keeping only the final filename component.
///
/// This is the lightweight path redaction contract for media logs that do not
/// need full XDG-root diagnostics mapping.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::redact_path;
///
/// assert_eq!(redact_path("/home/alice/movies/v.mkv"), "<redacted>/v.mkv");
/// assert_eq!(redact_path("/"), "<redacted-path>");
/// ```
#[must_use]
pub fn redact_path(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    path.file_name().map_or_else(
        || "<redacted-path>".to_owned(),
        |name| format!("<redacted>/{}", name.to_string_lossy()),
    )
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn normalize_path_string(path: PathBuf) -> String {
    let mut path = path.to_string_lossy().into_owned();
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

fn redact_paths_in_text(text: &str, rules: &BigRedactionRules) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find('/') {
        let start = cursor + relative_start;
        output.push_str(&text[cursor..start]);
        let end = path_token_end(text, start);
        let candidate = &text[start..end];
        let (path, trailing) = split_trailing_punctuation(candidate);
        if path.is_empty() {
            output.push_str(candidate);
        } else {
            output.push_str(&redact_diagnostics_path(path, rules));
            output.push_str(trailing);
        }
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    output
}

fn path_token_end(text: &str, start: usize) -> usize {
    text[start..]
        .char_indices()
        .find_map(|(offset, character)| {
            (offset > 0 && is_path_boundary(character)).then_some(start + offset)
        })
        .unwrap_or(text.len())
}

fn is_path_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '<' | '>')
}

fn split_trailing_punctuation(candidate: &str) -> (&str, &str) {
    let mut end = candidate.len();
    while end > 0 {
        let Some(character) = candidate[..end].chars().next_back() else {
            break;
        };
        if matches!(character, ',' | ';' | ')' | ']' | '}') {
            end -= character.len_utf8();
        } else {
            break;
        }
    }
    (&candidate[..end], &candidate[end..])
}

fn path_basename(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .or_else(|| {
            Path::new(path)
                .components()
                .next_back()
                .map(|component| component.as_os_str())
        })
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn redact_private_tokens(text: &str, rules: &BigRedactionRules) -> String {
    let mut redacted = text.to_owned();
    for token in &rules.private_tokens {
        redacted = redacted.replace(token, "<redacted>");
    }
    redacted
}

fn redact_secret_assignments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut search_from = 0;
    while let Some(relative_eq) = text[search_from..].find('=') {
        let eq = search_from + relative_eq;
        let key_start = secret_key_start(text, eq);
        let key = &text[key_start..eq];
        if is_secret_key(key) {
            let value_start = eq + 1;
            let value_end = secret_value_end(text, value_start);
            output.push_str(&text[cursor..value_start]);
            output.push_str("<redacted>");
            cursor = value_end;
            search_from = value_end;
        } else {
            search_from = eq + 1;
        }
    }
    output.push_str(&text[cursor..]);
    output
}

fn secret_key_start(text: &str, eq: usize) -> usize {
    let mut start = eq;
    for (index, character) in text[..eq].char_indices().rev() {
        if is_secret_key_character(character) {
            start = index;
        } else {
            break;
        }
    }
    start
}

fn is_secret_key_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

pub(crate) fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "access_key",
        "private_key",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

fn secret_value_end(text: &str, value_start: usize) -> usize {
    text[value_start..]
        .char_indices()
        .find_map(|(offset, character)| {
            (offset > 0 && matches!(character, ' ' | '\n' | '\t' | ',' | ';'))
                .then_some(value_start + offset)
        })
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_rules() -> BigRedactionRules {
        BigRedactionRules::empty()
            .with_xdg_root("xdg-config", "/home/user/.config")
            .with_xdg_root("xdg-data", "/home/user/.local/share")
            .with_private_token("alice")
            .with_private_token("workstation")
    }

    #[test]
    fn redacts_non_xdg_paths_to_basename() {
        assert_eq!(
            redact_diagnostics_path("/home/user/secret/file.txt", &fixture_rules()),
            "<redacted>/file.txt"
        );
    }

    #[test]
    fn keeps_xdg_relative_path_without_username() {
        assert_eq!(
            redact_diagnostics_path("/home/user/.config/big/app.conf", &fixture_rules()),
            "<xdg-config>/big/app.conf"
        );
    }

    #[test]
    fn redacts_tokens_and_env_secrets() {
        let text = "host=workstation user=alice API_TOKEN=abc123 path=/home/user/secret/file.txt";

        let redacted = redact_diagnostics_text(text, &fixture_rules());

        assert_eq!(
            redacted,
            "host=<redacted> user=<redacted> API_TOKEN=<redacted> path=<redacted>/file.txt"
        );
    }

    #[test]
    fn display_redaction_keeps_only_outer_characters() {
        assert_eq!(Redacted("secret").to_string(), "s****t");
        assert_eq!(format!("{:?}", Redacted("secret")), "s****t");
    }

    #[test]
    fn display_redaction_collapses_short_values() {
        assert_eq!(Redacted("a").to_string(), "**");
        assert_eq!(Redacted("ab").to_string(), "**");
        assert_eq!(Redacted("").to_string(), "<empty>");
    }

    #[test]
    fn lightweight_path_redaction_keeps_filename_only() {
        assert_eq!(redact_path("/home/alice/movies/v.mkv"), "<redacted>/v.mkv");
        assert_eq!(redact_path("/"), "<redacted-path>");
    }
}
