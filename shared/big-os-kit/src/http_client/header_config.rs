// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "http-client owns an anonymous curl config descriptor"
)]

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use super::HttpError;

pub(super) struct CurlRequestConfig {
    descriptor: File,
    path: PathBuf,
}

impl CurlRequestConfig {
    pub(super) fn write(url: &str, headers: &[(&str, &str)]) -> Result<Self, HttpError> {
        let contents = render_request_config(url, headers)?;
        let descriptor =
            crate::memfd::filled("big-os-kit-curl-request", &[contents.as_bytes()], true)
                .map(File::from)
                .map_err(|error| HttpError::RequestConfig {
                    step: "write",
                    error,
                })?;
        Ok(Self {
            path: PathBuf::from(format!("/proc/self/fd/{}", descriptor.as_raw_fd())),
            descriptor,
        })
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(super) fn configure_command(&self, command: &mut Command) {
        command.arg("--config").arg(&self.path);
        self.inherit_descriptor(command);
    }

    fn inherit_descriptor(&self, command: &mut Command) {
        let descriptor = self.descriptor.as_raw_fd();
        // SAFETY: the closure performs only async-signal-safe `fcntl` calls
        // between fork and exec. It clears CLOEXEC in the curl child only;
        // the parent and unrelated children retain CLOEXEC.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(descriptor, libc::F_GETFD);
                if flags == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

fn render_request_config(url: &str, headers: &[(&str, &str)]) -> Result<String, HttpError> {
    validate_config_value(url, "URL")?;
    let mut output = format!("url = \"{url}\"\n");
    for (name, value) in headers {
        validate_header_name(name)?;
        validate_header_value(value)?;
        output.push_str("header = \"");
        output.push_str(name);
        output.push_str(": ");
        output.push_str(value);
        output.push_str("\"\n");
    }
    Ok(output)
}

/// Accept an HTTP field name: an RFC 9110 token of at most 256 bytes.
///
/// # Errors
///
/// [`HttpError::HeaderName`] when `name` is empty, longer than 256 bytes, or
/// holds a byte outside the token characters.
pub fn validate_header_name(name: &str) -> Result<(), HttpError> {
    let token = |byte: u8| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte);
    if name.is_empty() || name.len() > 256 || !name.bytes().all(token) {
        return Err(HttpError::HeaderName);
    }
    Ok(())
}

pub(super) fn validate_header_value(value: &str) -> Result<(), HttpError> {
    validate_config_value(value, "HTTP header")
}

fn validate_config_value(value: &str, field: &'static str) -> Result<(), HttpError> {
    if value.contains('\r') || value.contains('\n') || value.contains('"') || value.contains('\\') {
        return Err(HttpError::UnsafeValue { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn headers_render_as_curl_config() {
        let rendered = render_request_config(
            "https://api.example.test/items?token=secret",
            &[
                ("Authorization", "Bearer secret"),
                ("Accept", "text/event-stream"),
            ],
        )
        .unwrap();
        assert_eq!(
            rendered,
            "url = \"https://api.example.test/items?token=secret\"\nheader = \"Authorization: Bearer secret\"\nheader = \"Accept: text/event-stream\"\n"
        );
    }

    #[test]
    fn header_names_are_tokens() {
        for name in ["Accept", "X-Api-Key", "a!#$%&'*+-.^_`|~9"] {
            assert!(validate_header_name(name).is_ok(), "{name}");
        }
        let long = "a".repeat(257);
        for name in [
            "",
            "Bad Name",
            "Bad:Name",
            "Bad\nName",
            "Bad\"Name",
            "Bad\\Name",
            "Ação",
            &long,
        ] {
            assert!(validate_header_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_quote_backslash_and_newline_values() {
        assert!(validate_header_value("ok").is_ok());
        assert!(validate_header_value("bad\nInjected: yes").is_err());
        assert!(validate_header_value("bad\"quote").is_err());
        assert!(validate_header_value(r"bad\slash").is_err());
    }

    #[test]
    fn anonymous_config_closes_on_drop() {
        let config = CurlRequestConfig::write(
            "https://api.example.test/items?token=secret",
            &[("Authorization", "Bearer secret")],
        )
        .unwrap();
        let path = config.path.clone();
        let metadata = path.metadata().unwrap();
        assert!(path.starts_with("/proc/self/fd"));
        drop(config);
        if let Ok(reused_metadata) = path.metadata() {
            assert_ne!(
                (reused_metadata.dev(), reused_metadata.ino()),
                (metadata.dev(), metadata.ino()),
                "the original anonymous config remained open after drop"
            );
        }
    }

    #[test]
    fn only_curl_inherits_sealed_descriptor() {
        let marker = "known-auth-marker";
        let config = CurlRequestConfig::write(
            "https://api.example.test/items",
            &[("Authorization", marker)],
        )
        .unwrap();

        let unrelated = Command::new("cat").arg(config.path()).output().unwrap();
        assert!(!unrelated.status.success());

        let mut curl_child = Command::new("cat");
        curl_child.arg(config.path());
        config.inherit_descriptor(&mut curl_child);
        let output = curl_child.output().unwrap();

        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains(marker));
        assert!(!config.path().to_string_lossy().contains(marker));
    }

    #[test]
    fn rejects_config_injection_characters() {
        assert!(render_request_config("https://example.test/path", &[]).is_ok());
        assert!(render_request_config("https://example.test/\nnext", &[]).is_err());
        assert!(render_request_config("https://example.test/\"next", &[]).is_err());
        assert!(render_request_config(r"https://example.test/\next", &[]).is_err());
        // An API key that would end its line and write the next one.
        let hostile = "key\"\nurl = \"file:///etc/passwd";
        assert!(matches!(
            render_request_config("https://example.test/", &[("X-Api-Key", hostile)]),
            Err(HttpError::UnsafeValue {
                field: "HTTP header"
            })
        ));
    }
}
