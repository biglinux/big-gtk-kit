// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! `file://` URIs and percent-encoded text, without GLib, for the processes
//! that run without it. A path is bytes: a name that is not UTF-8 comes back as
//! it is on disk, never with U+FFFD in it.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

/// The bytes `text` percent-encodes; `None` for a `%` not followed by two hex
/// digits.
#[must_use]
pub fn percent_decoded(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            bytes.push(u8::try_from(high * 16 + low).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    Some(bytes)
}

/// The addresses of a `text/uri-list` (RFC 2483), comment and blank lines
/// left out. Producers differ about `\r\n` and a final newline; neither
/// becomes an address.
#[must_use]
pub fn uri_list(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The local path `uri` names: `file:///…` or `file://localhost/…`, in any
/// letter case. `None` for another host's file, another scheme, a broken
/// escape, or an escaped `/` or NUL, which GLib refuses too: the first would
/// join two names into one.
#[must_use]
pub fn local_path(uri: &str) -> Option<PathBuf> {
    let rest = strip_prefix_ignoring_case(uri, "file://")?;
    let path = strip_prefix_ignoring_case(rest, "localhost").unwrap_or(rest);
    if !path.starts_with('/') || path.contains(['?', '#']) {
        return None;
    }
    let bytes = percent_decoded(path)?;
    let escapes_slash = path.match_indices('%').any(|(index, _)| {
        path.get(index + 1..index + 3)
            .is_some_and(|code| code.eq_ignore_ascii_case("2f"))
    });
    if escapes_slash || bytes.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt as _;

    use super::*;

    #[test]
    fn uri_list_skips_comments() {
        let list = b"# from a file manager\r\nfile:///home/a/x.txt\r\n\r\nfile:///home/a/y\n";
        assert_eq!(uri_list(list), ["file:///home/a/x.txt", "file:///home/a/y"]);
        assert!(uri_list(b"").is_empty());
    }

    /// RFC 8089 file URIs, with the bytes each escape stands for.
    #[test]
    fn local_paths_keep_their_bytes() {
        assert_eq!(
            local_path("file:///tmp/big%20shot.png"),
            Some(PathBuf::from("/tmp/big shot.png"))
        );
        assert_eq!(
            local_path("file://localhost/tmp/a"),
            Some(PathBuf::from("/tmp/a"))
        );
        assert_eq!(
            local_path("FILE://LocalHost/tmp/a"),
            Some(PathBuf::from("/tmp/a"))
        );
        assert_eq!(
            local_path("file:///tmp/caf%E9").map(|path| path.as_os_str().as_bytes().to_vec()),
            Some(b"/tmp/caf\xe9".to_vec())
        );
        for refused in [
            "file://server/tmp/a",
            "sftp:///tmp/a",
            "file:///tmp/%zz",
            "file:///tmp/a%2",
            "file:///tmp/a?b",
            "file:///tmp/a%2Fb",
            "file:///tmp/a%2fb",
            "file:///tmp/a%00b",
            "",
        ] {
            assert_eq!(local_path(refused), None, "{refused}");
        }
    }
}
