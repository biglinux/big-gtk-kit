// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Blocking, typed access to the user's Secret Service keyring.
//!
//! Every operation in this module may block on D-Bus, collection unlock, or
//! persistent storage. GUI callers must schedule it on a worker thread.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::mpsc;

use gio::prelude::CancellableExt;
use std::time::Duration;

pub mod hand_off;

const SCHEMA_NAME: &str = "br.com.biglinux.Credentials";
const APPLICATION_ATTRIBUTE: &str = "application";
const CREDENTIAL_KIND_ATTRIBUTE: &str = "credential-kind";
const STABLE_REFERENCE_ATTRIBUTE: &str = "stable-reference";
const MAX_IDENTITY_BYTES: usize = 128;
const MAX_SECRET_BYTES: usize = 64 * 1024;

/// Validated non-secret identity for one stored credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigKeyringReference {
    application: String,
    credential_kind: String,
    stable_reference: String,
}

impl BigKeyringReference {
    /// Validate and build a stable keyring reference.
    ///
    /// # Errors
    ///
    /// Returns [`BigKeyringError::InvalidReference`] when any identity is
    /// empty, exceeds 128 bytes, or contains a control character.
    pub fn new(
        application: impl Into<String>,
        credential_kind: impl Into<String>,
        stable_reference: impl Into<String>,
    ) -> Result<Self, BigKeyringError> {
        let application = application.into();
        let credential_kind = credential_kind.into();
        let stable_reference = stable_reference.into();
        validate_identity(&application)?;
        validate_identity(&credential_kind)?;
        validate_identity(&stable_reference)?;
        Ok(Self {
            application,
            credential_kind,
            stable_reference,
        })
    }

    fn application(&self) -> &str {
        &self.application
    }

    fn credential_kind(&self) -> &str {
        &self.credential_kind
    }

    fn stable_reference(&self) -> &str {
        &self.stable_reference
    }

    fn attributes(&self) -> HashMap<&str, &str> {
        HashMap::from([
            (APPLICATION_ATTRIBUTE, self.application()),
            (CREDENTIAL_KIND_ATTRIBUTE, self.credential_kind()),
            (STABLE_REFERENCE_ATTRIBUTE, self.stable_reference()),
        ])
    }

    fn label(&self) -> String {
        format!("BigLinux {} {}", self.application, self.credential_kind)
    }
}

/// Bounded secret text, zeroed when dropped. It implements neither `Debug`
/// nor `Display`.
pub struct BigSecretValue(String);

impl BigSecretValue {
    /// Validate and own a secret value.
    ///
    /// # Errors
    ///
    /// Returns [`BigKeyringError::InvalidSecret`] for an empty value, a value
    /// exceeding 64 KiB, or an embedded NUL byte. Domain owners may impose
    /// narrower syntax; the shared keyring still accepts multiline passwords.
    pub fn new(secret: impl Into<String>) -> Result<Self, BigKeyringError> {
        let mut secret = secret.into();
        if secret.is_empty() || secret.len() > MAX_SECRET_BYTES || secret.contains('\0') {
            crate::secret_memory::wipe_text(&mut secret);
            return Err(BigKeyringError::InvalidSecret);
        }
        Ok(Self(secret))
    }

    /// Explicitly expose the secret to an authenticated protocol boundary.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper at the last responsible boundary.
    #[must_use]
    pub fn into_secret(mut self) -> String {
        std::mem::take(&mut self.0)
    }

    /// Copy this secret into a sealed anonymous file descriptor suitable for
    /// an inherited command input such as `sshpass -d`.
    ///
    /// The descriptor starts at offset zero, is close-on-exec by default, and
    /// cannot be modified, grown, or shrunk after this method returns. Callers
    /// must explicitly authorize inheritance for only the intended child.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when Linux cannot create, write, rewind, or seal
    /// the anonymous descriptor.
    pub fn sealed_memfd(&self) -> std::io::Result<OwnedFd> {
        crate::memfd::filled(
            "biglinux-secret",
            &[self.expose_secret().as_bytes(), b"\n"],
            true,
        )
    }
}

impl Drop for BigSecretValue {
    fn drop(&mut self) {
        crate::secret_memory::wipe_text(&mut self.0);
    }
}

/// Redacted failures from the system keyring boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BigKeyringError {
    /// A non-secret identity was malformed.
    #[error("invalid keyring reference")]
    InvalidReference,
    /// The secret was empty, too large, or contained an embedded NUL.
    #[error("credential value is invalid")]
    InvalidSecret,
    /// Secret Service could not be reached.
    #[error("system keyring is unavailable")]
    Unavailable,
    /// The keyring or target collection is locked.
    #[error("system keyring is locked")]
    Locked,
    /// No secret matched the validated reference.
    #[error("credential is not stored")]
    MissingSecret,
    /// Secret Service rejected the operation without safe additional detail.
    #[error("system keyring operation failed")]
    BackendFailure,
    /// Secret Service did not answer in time, typically while it asked for
    /// the keyring to be unlocked and nobody did.
    #[error("system keyring did not answer")]
    NoAnswer,
    /// A just-stored secret did not match a subsequent lookup.
    #[error("stored credential could not be verified")]
    VerificationMismatch,
}

/// Blocking Secret Service adapter backed by the default system collection.
#[derive(Debug, Default, Clone, Copy)]
pub struct BigSystemKeyring;

impl BigSystemKeyring {
    /// Store or replace a secret. This is a blocking operation.
    ///
    /// # Errors
    ///
    /// Returns a redacted [`BigKeyringError`] when Secret Service cannot store
    /// the value, and [`BigKeyringError::NoAnswer`] after [`LOOKUP_LIMIT`]
    /// without an answer.
    pub fn store(
        &self,
        reference: &BigKeyringReference,
        secret: &BigSecretValue,
    ) -> Result<(), BigKeyringError> {
        let (stored, timed_out) = within(LOOKUP_LIMIT, |cancellable| {
            libsecret::password_store_sync(
                Some(&schema()),
                reference.attributes(),
                Some(libsecret::COLLECTION_DEFAULT.as_str()),
                &reference.label(),
                secret.expose_secret(),
                Some(cancellable),
            )
        });
        stored.map_err(|error| answer_error(error, timed_out))
    }

    /// Look up a secret. This is a blocking operation.
    ///
    /// # Errors
    ///
    /// Returns [`BigKeyringError::MissingSecret`] when the reference has no
    /// stored value, or another redacted backend error.
    /// Returns [`BigKeyringError::NoAnswer`] after [`LOOKUP_LIMIT`] without
    /// an answer.
    pub fn lookup(
        &self,
        reference: &BigKeyringReference,
    ) -> Result<BigSecretValue, BigKeyringError> {
        let (found, timed_out) = within(LOOKUP_LIMIT, |cancellable| {
            libsecret::password_lookup_sync(
                Some(&schema()),
                reference.attributes(),
                Some(cancellable),
            )
        });
        let secret = found
            .map_err(|error| answer_error(error, timed_out))?
            .ok_or(BigKeyringError::MissingSecret)?;
        BigSecretValue::new(secret.as_str())
    }

    /// Clear a secret if present. Missing entries are successful. This is a
    /// blocking operation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when Secret Service rejects the clear,
    /// and [`BigKeyringError::NoAnswer`] after [`LOOKUP_LIMIT`] without an
    /// answer.
    pub fn clear(&self, reference: &BigKeyringReference) -> Result<(), BigKeyringError> {
        let (cleared, timed_out) = within(LOOKUP_LIMIT, |cancellable| {
            libsecret::password_clear_sync(
                Some(&schema()),
                reference.attributes(),
                Some(cancellable),
            )
        });
        cleared
            .map(drop)
            .map_err(|error| answer_error(error, timed_out))
    }
}

/// The error of a call [`within`] cancelled, or the backend's own.
fn answer_error(error: glib::Error, timed_out: bool) -> BigKeyringError {
    if timed_out {
        BigKeyringError::NoAnswer
    } else {
        classify_backend_error(error)
    }
}

fn schema() -> libsecret::Schema {
    libsecret::Schema::new(
        SCHEMA_NAME,
        libsecret::SchemaFlags::NONE,
        HashMap::from([
            (
                APPLICATION_ATTRIBUTE,
                libsecret::SchemaAttributeType::String,
            ),
            (
                CREDENTIAL_KIND_ATTRIBUTE,
                libsecret::SchemaAttributeType::String,
            ),
            (
                STABLE_REFERENCE_ATTRIBUTE,
                libsecret::SchemaAttributeType::String,
            ),
        ]),
    )
}

fn validate_identity(value: &str) -> Result<(), BigKeyringError> {
    if value.is_empty() || value.len() > MAX_IDENTITY_BYTES || value.chars().any(char::is_control) {
        Err(BigKeyringError::InvalidReference)
    } else {
        Ok(())
    }
}

/// How long a call waits for Secret Service, which may be asking the person
/// to unlock the keyring: time to type a password, and not forever when
/// nobody is there, as when a program restores a connection at login.
pub const LOOKUP_LIMIT: Duration = Duration::from_secs(60);

/// Run `work` with a cancellable that is cancelled once `limit` has passed,
/// and say whether it was.
fn within<T>(limit: Duration, work: impl FnOnce(&gio::Cancellable) -> T) -> (T, bool) {
    let cancellable = gio::Cancellable::new();
    let (finished, waiting) = mpsc::channel::<()>();
    let watch = cancellable.clone();
    let watchdog = std::thread::spawn(move || {
        if waiting.recv_timeout(limit) == Err(mpsc::RecvTimeoutError::Timeout) {
            watch.cancel();
        }
    });
    let result = work(&cancellable);
    drop(finished);
    let _ = watchdog.join();
    (result, cancellable.is_cancelled())
}

fn classify_backend_error(error: glib::Error) -> BigKeyringError {
    if error.matches(libsecret::Error::IsLocked) {
        BigKeyringError::Locked
    } else if error.is::<libsecret::Error>() {
        BigKeyringError::BackendFailure
    } else {
        BigKeyringError::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_past_limit_is_cancelled() {
        let (spins, timed_out) = within(Duration::from_millis(50), |cancellable| {
            let mut spins = 0;
            while !cancellable.is_cancelled() && spins < 400 {
                std::thread::sleep(Duration::from_millis(5));
                spins += 1;
            }
            spins
        });
        assert!(timed_out);
        assert!(spins < 400, "the work saw the cancel");
        let started = std::time::Instant::now();
        assert_eq!(within(Duration::from_secs(30), |_| 7), (7, false));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "no wait for the limit"
        );
    }

    #[test]
    fn reference_exposes_non_secret_attributes() {
        let reference = BigKeyringReference::new("bigaudio", "listenbrainz-token", "primary")
            .expect("valid reference");
        assert_eq!(reference.application(), "bigaudio");
        assert_eq!(reference.credential_kind(), "listenbrainz-token");
        assert_eq!(reference.stable_reference(), "primary");
        assert_eq!(reference.attributes().len(), 3);
    }

    #[test]
    fn reference_rejects_invalid_values() {
        assert_eq!(
            BigKeyringReference::new("", "kind", "reference").unwrap_err(),
            BigKeyringError::InvalidReference
        );
        assert_eq!(
            BigKeyringReference::new("app", "kind\n", "reference").unwrap_err(),
            BigKeyringError::InvalidReference
        );
        assert_eq!(
            BigKeyringReference::new("app", "kind", "x".repeat(MAX_IDENTITY_BYTES + 1))
                .unwrap_err(),
            BigKeyringError::InvalidReference
        );
    }

    #[test]
    fn secret_validation_redacts_errors() {
        assert!(matches!(
            BigSecretValue::new(""),
            Err(BigKeyringError::InvalidSecret)
        ));
        assert!(matches!(
            BigSecretValue::new("x".repeat(MAX_SECRET_BYTES + 1)),
            Err(BigKeyringError::InvalidSecret)
        ));
        assert!(BigSecretValue::new("line one\nline two").is_ok());
        let marker = "known-secret-marker";
        let error = BigSecretValue::new(format!("{marker}\0")).err().unwrap();
        assert!(!format!("{error:?} {error}").contains(marker));
    }

    #[test]
    fn secret_memfd_is_rewound_and_sealed() {
        use std::fs::File;
        use std::io::Read;
        use std::os::fd::AsFd;

        let secret = BigSecretValue::new("not-a-real-password").expect("valid fixture");
        let fd = secret.sealed_memfd().expect("sealed memfd");
        let mut reader = File::from(fd.as_fd().try_clone_to_owned().expect("clone fd"));
        let mut contents = String::new();
        reader.read_to_string(&mut contents).expect("read memfd");
        assert_eq!(contents, "not-a-real-password\n");
        let all = rustix::fs::SealFlags::SEAL
            | rustix::fs::SealFlags::SHRINK
            | rustix::fs::SealFlags::GROW
            | rustix::fs::SealFlags::WRITE;
        let seals = rustix::fs::fcntl_get_seals(&fd).expect("its seals");
        assert!(seals.contains(all), "{seals:?}");
    }
}
