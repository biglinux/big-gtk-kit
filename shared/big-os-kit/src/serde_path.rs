// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Lossless path fields for JSON: UTF-8 strings retain their existing format;
//! other paths use Serde's native `OsString` representation.
//!
//! ```
//! use std::path::PathBuf;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Location {
//!     #[serde(with = "big_os_kit::serde_path")]
//!     path: PathBuf,
//!     #[serde(default, with = "big_os_kit::serde_path::optional")]
//!     fallback: Option<PathBuf>,
//! }
//! ```

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Serialize)]
#[serde(untagged)]
enum PathRef<'a> {
    Text(&'a str),
    Native(&'a OsStr),
}

fn path_ref(path: &Path) -> PathRef<'_> {
    path.to_str()
        .map_or_else(|| PathRef::Native(path.as_os_str()), PathRef::Text)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredPath {
    Text(String),
    Native(OsString),
}

impl From<StoredPath> for PathBuf {
    fn from(path: StoredPath) -> Self {
        match path {
            StoredPath::Text(text) => Self::from(text),
            StoredPath::Native(native) => Self::from(native),
        }
    }
}

/// Serialize a required path without replacing invalid UTF-8 bytes.
///
/// # Errors
///
/// The serializer's error.
pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    path_ref(path).serialize(serializer)
}

/// Deserialize a UTF-8 string or Serde's platform-native path representation.
///
/// # Errors
///
/// The deserializer's error for a value that is neither a string nor a path.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    StoredPath::deserialize(deserializer).map(PathBuf::from)
}

/// Matching adapter for nullable path fields.
pub mod optional {
    use super::*;

    /// Serialize a nullable path using the same lossless representation.
    ///
    /// # Errors
    ///
    /// The serializer's error.
    pub fn serialize<S: Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_deref().map(path_ref).serialize(serializer)
    }

    /// Deserialize a nullable UTF-8 or platform-native path.
    ///
    /// # Errors
    ///
    /// The deserializer's error for a value that is neither null, a string nor a
    /// path.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Option::<StoredPath>::deserialize(deserializer).map(|path| path.map(PathBuf::from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Paths {
        #[serde(with = "super")]
        required: PathBuf,
        #[serde(with = "optional")]
        optional: Option<PathBuf>,
    }

    #[test]
    fn native_paths_round_trip() {
        for byte in 0..=255 {
            for length in [0, 1, 2, 15, 128] {
                let path = PathBuf::from(OsString::from_vec(vec![byte; length]));
                let expected = Paths {
                    required: path.clone(),
                    optional: (length % 2 == 1).then_some(path.clone()),
                };
                let value = serde_json::to_value(&expected).unwrap();
                if let Some(text) = path.to_str() {
                    assert_eq!(value["required"], text);
                }
                assert_eq!(serde_json::from_value::<Paths>(value).unwrap(), expected);
            }
        }
    }

    #[test]
    fn malformed_native_path_representations_are_rejected() {
        for value in [
            serde_json::json!({"Unix":[256]}),
            serde_json::json!({"Unix":[-1]}),
            serde_json::json!({"Unix":"wrong"}),
            serde_json::json!({"Unknown":[]}),
            serde_json::json!(false),
        ] {
            assert!(
                serde_json::from_value::<Paths>(
                    serde_json::json!({"required":value,"optional":null})
                )
                .is_err()
            );
        }
    }
}
