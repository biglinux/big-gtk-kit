// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};

/// One retained or live streaming frame for a durable daemon job.
///
/// Streaming daemons use the same one-socket JSON frame protocol as ordinary
/// jobs: each byte chunk is carried by a [`super::MicrodaemonEvent::StreamFrame`]
/// event and ordered by this sequence number.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::MicrodaemonStreamFrame;
///
/// let frame = MicrodaemonStreamFrame::new(7, b"tick\n".to_vec());
/// assert_eq!(frame.sequence, 7);
/// assert_eq!(frame.bytes, b"tick\n");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicrodaemonStreamFrame {
    /// Monotonic sequence within one job stream, starting at zero.
    pub sequence: u64,
    /// Opaque stream payload bytes.
    #[serde(with = "payload_bytes")]
    pub bytes: Vec<u8>,
}

/// Carry payload bytes as base64 instead of a JSON array of numbers.
///
/// Serialized as numbers, an 8 KiB chunk becomes 37 KiB of JSON that costs an
/// order of magnitude more to parse; as base64 it is 11 KiB. Use it as
/// `#[serde(with = ...)]` on a byte payload.
pub mod payload_bytes {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize `bytes` as one base64 string.
    ///
    /// # Errors
    ///
    /// The serializer's error.
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    /// `bytes` as a JSON base64 string, for a payload held in a `Value`.
    pub fn to_json(bytes: &[u8]) -> serde_json::Value {
        serde_json::Value::String(STANDARD.encode(bytes))
    }

    /// Read a base64 string back into bytes.
    ///
    /// # Errors
    ///
    /// The deserializer's error for a value that is not a string or not base64.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

impl MicrodaemonStreamFrame {
    /// Build a stream frame from a sequence and opaque payload bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonStreamFrame;
    ///
    /// let frame = MicrodaemonStreamFrame::new(0, vec![1, 2, 3]);
    /// assert_eq!(frame.bytes, vec![1, 2, 3]);
    /// ```
    #[must_use]
    pub const fn new(sequence: u64, bytes: Vec<u8>) -> Self {
        Self { sequence, bytes }
    }
}

/// Bounded retention window advertised before replaying stream frames.
///
/// Historical stream replay is best effort: a daemon may only retain recent
/// frames. Clients that request frames older than `first_retained_sequence`
/// should treat the stream as live-tail from the retained window.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::MicrodaemonStreamRetention;
///
/// let retention = MicrodaemonStreamRetention::new(3, 9);
/// assert_eq!(retention.first_retained_sequence, 3);
/// assert_eq!(retention.next_sequence, 9);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicrodaemonStreamRetention {
    /// First sequence still available for replay.
    pub first_retained_sequence: u64,
    /// Next sequence the daemon will assign to a live frame.
    pub next_sequence: u64,
}

impl MicrodaemonStreamRetention {
    /// Build a stream-retention descriptor.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonStreamRetention;
    ///
    /// let retention = MicrodaemonStreamRetention::new(0, 1);
    /// assert_eq!(retention.next_sequence, 1);
    /// ```
    #[must_use]
    pub const fn new(first_retained_sequence: u64, next_sequence: u64) -> Self {
        Self {
            first_retained_sequence,
            next_sequence,
        }
    }
}

#[cfg(test)]
mod payload_tests {
    use super::MicrodaemonStreamFrame;

    #[test]
    fn payloads_travel_as_base64() {
        let frame = MicrodaemonStreamFrame::new(3, b"\x1b[0m ol\xc3\xa1\n".to_vec());
        let json = serde_json::to_string(&frame).expect("serialize frame");
        assert!(
            json.contains("\"bytes\":\""),
            "payload must be a string, not an array: {json}"
        );
        assert_eq!(
            serde_json::from_str::<MicrodaemonStreamFrame>(&json).expect("round trip"),
            frame
        );
    }

    #[test]
    fn value_payload_reads_back() {
        let bytes = b"\x1b[0m ol\xc3\xa1\n".to_vec();
        let json = serde_json::json!({
            "sequence": 4,
            "bytes": super::payload_bytes::to_json(&bytes),
        });
        assert_eq!(
            serde_json::from_value::<MicrodaemonStreamFrame>(json).expect("frame from value"),
            MicrodaemonStreamFrame::new(4, bytes)
        );
    }

    #[test]
    fn base64_form_is_smaller_than_array() {
        let payload = vec![0xE2_u8; 8192];
        let frame = MicrodaemonStreamFrame::new(0, payload.clone());
        let encoded = serde_json::to_string(&frame).expect("serialize");
        let as_array = serde_json::to_string(&serde_json::json!({
            "sequence": 0,
            "bytes": payload,
        }))
        .expect("serialize array form");
        assert!(
            as_array.len() * 2 > encoded.len() * 5,
            "base64 must be at least 2.5 times smaller: {} vs {}",
            as_array.len(),
            encoded.len()
        );
    }
}
