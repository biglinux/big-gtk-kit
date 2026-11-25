// SPDX-License-Identifier: MIT

//! Shared protocol primitives for BigLinux per-user microdaemons.
//!
//! The framework uses small GTK-free daemons for durable work that must survive
//! a host or module crash. This module defines the stable command/event frame.
//!
//! The wire representation is a four-byte big-endian length prefix followed by
//! a JSON frame: easy to inspect in tests, stable across languages, and
//! independent of GTK/Relm4.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

mod journal;
mod lifecycle;
mod socket;
mod stream;
pub use journal::{load_durable_job_journal, persist_durable_job_journal, persist_job_journal};
pub use lifecycle::{MicrodaemonClientLease, MicrodaemonIdleExit, MicrodaemonJobLease};
pub use socket::{
    bind_owner_socket, connect_socket, current_effective_user_id, is_current_user_peer,
    peer_process_id, prepare_owner_directory, runs_under_user_manager, user_runtime_directory,
};
pub use stream::{MicrodaemonStreamFrame, MicrodaemonStreamRetention, payload_bytes};

/// Current microdaemon protocol version.
pub(crate) const MICRODAEMON_PROTOCOL_VERSION: u16 = 1;

/// Maximum accepted JSON frame bytes after the four-byte length prefix.
pub const MAX_MICRODAEMON_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Official daemon identifiers covered by the shared protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MicrodaemonId {
    /// Durable terminal PTY session daemon.
    Ptyd,
    /// Durable file operation daemon.
    Fileopsd,
    /// Screen/camera/microphone recording daemon.
    Recordd,
    /// BigShell durable session-state daemon.
    Sessiond,
    /// Optional file/media indexing daemon.
    Indexd,
}

impl MicrodaemonId {
    /// Stable manifest and socket identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ptyd => "bigterminal-ptyd",
            Self::Fileopsd => "bigfiles-fileopsd",
            Self::Recordd => "big-recordd",
            Self::Sessiond => "bigshell-sessiond",
            Self::Indexd => "big-indexd",
        }
    }

    /// Socket file name under the daemon's private runtime directory.
    #[must_use]
    pub const fn socket_file_name(self) -> &'static str {
        match self {
            Self::Ptyd => "ptyd.sock",
            Self::Fileopsd => "fileopsd.sock",
            Self::Recordd => "recordd.sock",
            Self::Sessiond => "sessiond.sock",
            Self::Indexd => "indexd.sock",
        }
    }
}

/// A durable job submitted to one of the microdaemons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurableJobSpec {
    /// Caller-provided idempotency key. Daemons may reuse it as the public job id.
    pub job_key: String,
    /// Daemon-local class such as `copy`, `video-conversion`, or `screen-recording`.
    pub job_class: String,
    /// Whether the job should continue without an attached UI client.
    pub is_durable: bool,
    /// Job-specific JSON arguments owned by the target daemon.
    pub arguments: serde_json::Value,
}

/// Current known state of a daemon job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DurableJobState {
    /// Accepted but not started.
    Queued,
    /// Actively running.
    Running,
    /// Cancel request has been accepted and cleanup is in progress.
    Cancelling,
    /// Completed successfully.
    Completed,
    /// Stopped before completion by an explicit cancel request.
    Cancelled,
    /// Interrupted by daemon restart or external failure.
    Interrupted,
    /// Failed and requires caller-visible recovery or retry.
    Failed,
}

/// Snapshot returned by list/attach operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurableJobSnapshot {
    /// Stable daemon job id.
    pub job_id: String,
    /// Original daemon-local job class.
    pub job_class: String,
    /// Current state.
    pub state: DurableJobState,
    /// Last known progress, when the daemon can estimate it.
    pub progress: Option<JobProgress>,
    /// Optional short user-facing status owned by the daemon.
    pub status_text: Option<String>,
    /// Optional daemon-owned structured details for richer clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Journal row pairing a public snapshot with the original restartable spec.
#[derive(Debug, Clone, PartialEq)]
pub struct DurableJobJournalRecord {
    /// Last persisted job snapshot.
    pub snapshot: DurableJobSnapshot,
    /// Original validated job spec, when the daemon supports restart.
    pub restart_spec: Option<DurableJobSpec>,
}

/// Bounded progress report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    /// Completed work units.
    pub completed_units: u64,
    /// Total work units when known.
    pub total_units: Option<u64>,
}

impl JobProgress {
    /// Build a bounded progress report.
    ///
    /// When `total_units` is known, `completed_units` is clamped to it so UI
    /// consumers never render progress above 100%.
    #[must_use]
    pub fn new(completed_units: u64, total_units: Option<u64>) -> Self {
        let completed_units =
            total_units.map_or(completed_units, |total| completed_units.min(total));
        Self {
            completed_units,
            total_units,
        }
    }
}

/// Commands sent from clients to daemons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum MicrodaemonCommand {
    /// Protocol handshake; daemon replies with [`MicrodaemonEvent::Hello`].
    Ping,
    /// Submit a new job.
    Submit {
        /// Job to enqueue.
        job: DurableJobSpec,
    },
    /// List currently known jobs.
    List,
    /// Attach to a job and replay its current snapshot/progress.
    Attach {
        /// Stable daemon job id.
        job_id: String,
    },
    /// Attach to a streaming job.
    ///
    /// `after_sequence` is the last sequence the caller already consumed. The
    /// daemon replays retained frames after it, then live-tails new frames.
    AttachStream {
        /// Stable daemon job id.
        job_id: String,
        /// Last sequence already consumed by this client.
        after_sequence: Option<u64>,
    },
    /// Send a daemon-owned control frame to an existing job.
    ///
    /// Streaming jobs use this for bidirectional side-channel actions that are
    /// not part of the output stream, such as terminal input or resize events.
    JobControl {
        /// Stable daemon job id.
        job_id: String,
        /// Daemon-local control class such as `input`, `resize`, or `signal`.
        control_class: String,
        /// Control-specific JSON arguments owned by the target daemon.
        arguments: serde_json::Value,
    },
    /// Cancel a job.
    Cancel {
        /// Stable daemon job id.
        job_id: String,
    },
    /// Restart a failed, interrupted, or completed job using daemon-owned state.
    Restart {
        /// Stable daemon job id.
        job_id: String,
    },
    /// Ask the daemon to recover/reconcile an interrupted job from its journal.
    Recover {
        /// Stable daemon job id.
        job_id: String,
    },
    /// Answer with the job's snapshot as soon as it differs from `known`, or
    /// with the unchanged one after the daemon's own ceiling.
    ///
    /// What a client following a job's progress asks instead of attaching
    /// again on a timer: the answer comes when there is something new to show.
    WaitForChange {
        /// Stable daemon job id.
        job_id: String,
        /// The snapshot the client is showing now.
        known: DurableJobSnapshot,
    },
    /// Acknowledge cleanup of a completed/cancelled/failed job.
    AckCleanup {
        /// Stable daemon job id.
        job_id: String,
    },
    /// Ask the daemon to drain/cancel according to its clean-shutdown policy.
    Shutdown,
}

/// Events sent by daemons to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum MicrodaemonEvent {
    /// Protocol handshake response.
    Hello {
        /// Daemon id that accepted the connection.
        daemon_id: MicrodaemonId,
        /// Human-readable capability tokens.
        capabilities: Vec<String>,
    },
    /// A submitted job was accepted.
    JobAccepted {
        /// Stable daemon job id.
        job_id: String,
    },
    /// Full job snapshot.
    JobSnapshot {
        /// Snapshot contents.
        snapshot: DurableJobSnapshot,
    },
    /// Full response to [`MicrodaemonCommand::List`].
    JobList {
        /// Current daemon jobs visible to the caller.
        jobs: Vec<DurableJobSnapshot>,
    },
    /// Progress-only update.
    Progress {
        /// Stable daemon job id.
        job_id: String,
        /// Latest progress.
        progress: JobProgress,
    },
    /// Retention window for a streaming job attach.
    StreamRetention {
        /// Stable daemon job id.
        job_id: String,
        /// Bounded replay window retained by the daemon.
        retention: MicrodaemonStreamRetention,
    },
    /// One retained or live streaming frame.
    StreamFrame {
        /// Stable daemon job id.
        job_id: String,
        /// Ordered stream payload.
        frame: MicrodaemonStreamFrame,
    },
    /// Streaming job channel reached end-of-stream.
    StreamEnded {
        /// Stable daemon job id.
        job_id: String,
        /// First sequence after the final frame.
        next_sequence: u64,
    },
    /// Terminal state update for a job.
    JobStateChanged {
        /// Stable daemon job id.
        job_id: String,
        /// New state.
        state: DurableJobState,
        /// Optional short diagnostic owned by the daemon.
        status_text: Option<String>,
    },
    /// Clean shutdown accepted.
    ShutdownAccepted,
    /// Protocol or job error.
    Error {
        /// Job id when the error belongs to a specific job.
        job_id: Option<String>,
        /// Stable machine-readable error code.
        code: String,
        /// Short diagnostic safe to log.
        message: String,
    },
}

/// Either a client command or a daemon event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MicrodaemonBody {
    /// Client-to-daemon command.
    Command {
        /// Command body.
        command: MicrodaemonCommand,
    },
    /// Daemon-to-client event.
    Event {
        /// Event body.
        event: MicrodaemonEvent,
    },
}

/// Versioned protocol frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicrodaemonFrame {
    /// Protocol version.
    pub version: u16,
    /// Target or source daemon id.
    pub daemon_id: MicrodaemonId,
    /// Frame body.
    pub body: MicrodaemonBody,
}

impl MicrodaemonFrame {
    /// Build a current-version command frame.
    #[must_use]
    pub const fn command(daemon_id: MicrodaemonId, command: MicrodaemonCommand) -> Self {
        Self {
            version: MICRODAEMON_PROTOCOL_VERSION,
            daemon_id,
            body: MicrodaemonBody::Command { command },
        }
    }

    /// Build a current-version event frame.
    #[must_use]
    pub const fn event(daemon_id: MicrodaemonId, event: MicrodaemonEvent) -> Self {
        Self {
            version: MICRODAEMON_PROTOCOL_VERSION,
            daemon_id,
            body: MicrodaemonBody::Event { event },
        }
    }

    /// Validate the protocol version.
    ///
    /// # Errors
    ///
    /// Returns [`MicrodaemonProtocolError::UnsupportedVersion`] when the frame
    /// was produced by a newer or invalid protocol.
    pub fn validate_version(&self) -> Result<(), MicrodaemonProtocolError> {
        if self.version == MICRODAEMON_PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(MicrodaemonProtocolError::UnsupportedVersion {
                found: self.version,
                supported: MICRODAEMON_PROTOCOL_VERSION,
            })
        }
    }

    /// The event a reply from daemon `from` carries.
    ///
    /// # Errors
    ///
    /// Returns [`MicrodaemonProtocolError::UnsupportedVersion`],
    /// [`MicrodaemonProtocolError::WrongDaemon`] when another daemon sent the
    /// frame, or [`MicrodaemonProtocolError::UnexpectedCommand`] when it holds
    /// a command.
    pub fn into_event(
        self,
        from: MicrodaemonId,
    ) -> Result<MicrodaemonEvent, MicrodaemonProtocolError> {
        self.validate_version()?;
        if self.daemon_id != from {
            return Err(MicrodaemonProtocolError::WrongDaemon {
                expected: from,
                found: self.daemon_id,
            });
        }
        match self.body {
            MicrodaemonBody::Event { event } => Ok(event),
            MicrodaemonBody::Command { .. } => Err(MicrodaemonProtocolError::UnexpectedCommand),
        }
    }
}

/// Protocol codec and validation failures.
#[derive(Debug)]
pub enum MicrodaemonProtocolError {
    /// JSON serialize/parse failed.
    Json(serde_json::Error),
    /// Frame length is larger than [`MAX_MICRODAEMON_FRAME_BYTES`].
    FrameTooLarge {
        /// Actual frame bytes.
        actual: usize,
        /// Maximum accepted frame bytes.
        maximum: usize,
    },
    /// Buffer does not contain the promised frame bytes.
    TruncatedFrame {
        /// Number of bytes required by the length prefix plus header.
        needed: usize,
        /// Number of bytes supplied by the caller.
        available: usize,
    },
    /// Frame version is unsupported.
    UnsupportedVersion {
        /// Version found in the frame.
        found: u16,
        /// Highest version supported by this build.
        supported: u16,
    },
    /// A reply came from another daemon than the one asked.
    WrongDaemon {
        /// Daemon the reply was expected from.
        expected: MicrodaemonId,
        /// Daemon the frame names.
        found: MicrodaemonId,
    },
    /// A reply held a command where an event belongs.
    UnexpectedCommand,
    /// Stream read/write failed.
    Io {
        /// Underlying I/O error.
        source: io::Error,
    },
}

impl std::fmt::Display for MicrodaemonProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "microdaemon json error: {error}"),
            Self::FrameTooLarge { actual, maximum } => {
                write!(
                    f,
                    "microdaemon frame has {actual} bytes; maximum is {maximum}"
                )
            }
            Self::TruncatedFrame { needed, available } => write!(
                f,
                "microdaemon frame needs {needed} bytes but only {available} bytes were supplied"
            ),
            Self::UnsupportedVersion { found, supported } => {
                write!(
                    f,
                    "microdaemon protocol version {found} is unsupported; expected {supported}"
                )
            }
            Self::WrongDaemon { expected, found } => {
                write!(
                    f,
                    "microdaemon reply came from {found:?}; expected {expected:?}"
                )
            }
            Self::UnexpectedCommand => f.write_str("microdaemon reply holds a command frame"),
            Self::Io { source } => write!(f, "microdaemon stream i/o error: {source}"),
        }
    }
}

impl std::error::Error for MicrodaemonProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::FrameTooLarge { .. }
            | Self::TruncatedFrame { .. }
            | Self::UnsupportedVersion { .. }
            | Self::WrongDaemon { .. }
            | Self::UnexpectedCommand => None,
            Self::Io { source } => Some(source),
        }
    }
}

impl From<serde_json::Error> for MicrodaemonProtocolError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<io::Error> for MicrodaemonProtocolError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

/// A stream failure keeps its kind; every other failure is invalid data.
impl From<MicrodaemonProtocolError> for io::Error {
    fn from(error: MicrodaemonProtocolError) -> Self {
        match error {
            MicrodaemonProtocolError::Io { source } => source,
            error => Self::new(io::ErrorKind::InvalidData, error),
        }
    }
}

/// Encode a frame as a four-byte length prefix followed by JSON bytes.
///
/// # Errors
///
/// Returns [`MicrodaemonProtocolError::FrameTooLarge`] when the serialized JSON
/// exceeds [`MAX_MICRODAEMON_FRAME_BYTES`], or [`MicrodaemonProtocolError::Json`]
/// when serialization fails.
pub fn encode_microdaemon_frame(
    frame: &MicrodaemonFrame,
) -> Result<Vec<u8>, MicrodaemonProtocolError> {
    frame.validate_version()?;
    encode_microdaemon_json_frame(frame)
}

/// Encode any serde payload with the microdaemon length-prefixed JSON codec.
///
/// This is the shared framing primitive for typed worker payloads that do not
/// use [`MicrodaemonFrame`] as their semantic envelope.
///
/// # Errors
///
/// Returns [`MicrodaemonProtocolError::FrameTooLarge`] when the serialized JSON
/// exceeds [`MAX_MICRODAEMON_FRAME_BYTES`], or [`MicrodaemonProtocolError::Json`]
/// when serialization fails.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::{decode_microdaemon_json_frame, encode_microdaemon_json_frame};
/// use serde::{Deserialize, Serialize};
/// #[derive(Debug, PartialEq, Serialize, Deserialize)]
/// struct Payload { value: u8 }
/// let encoded = encode_microdaemon_json_frame(&Payload { value: 7 }).unwrap();
/// let decoded: Payload = decode_microdaemon_json_frame(&encoded).unwrap();
/// assert_eq!(decoded, Payload { value: 7 });
/// ```
pub fn encode_microdaemon_json_frame<T: Serialize>(
    payload: &T,
) -> Result<Vec<u8>, MicrodaemonProtocolError> {
    let mut body = serde_json::to_vec(payload)?;
    let encoded = encode_microdaemon_json_body(&body);
    crate::secret_memory::wipe(&mut body);
    encoded
}

fn encode_microdaemon_json_body(body: &[u8]) -> Result<Vec<u8>, MicrodaemonProtocolError> {
    if body.len() > MAX_MICRODAEMON_FRAME_BYTES {
        return Err(MicrodaemonProtocolError::FrameTooLarge {
            actual: body.len(),
            maximum: MAX_MICRODAEMON_FRAME_BYTES,
        });
    }
    let mut encoded = Vec::with_capacity(4 + body.len());
    encoded.extend_from_slice(&(body.len() as u32).to_be_bytes());
    encoded.extend_from_slice(body);
    Ok(encoded)
}

/// Decode one complete length-prefixed frame.
///
/// # Errors
///
/// Returns [`MicrodaemonProtocolError::TruncatedFrame`] for incomplete buffers,
/// [`MicrodaemonProtocolError::FrameTooLarge`] for oversized frames,
/// [`MicrodaemonProtocolError::Json`] for malformed JSON, or
/// [`MicrodaemonProtocolError::UnsupportedVersion`] for version mismatch.
pub fn decode_microdaemon_frame(
    bytes: &[u8],
) -> Result<MicrodaemonFrame, MicrodaemonProtocolError> {
    let frame: MicrodaemonFrame = decode_microdaemon_json_frame(bytes)?;
    frame.validate_version()?;
    Ok(frame)
}

/// Decode one complete microdaemon length-prefixed JSON payload.
///
/// # Errors
///
/// Returns [`MicrodaemonProtocolError::TruncatedFrame`] for incomplete buffers,
/// [`MicrodaemonProtocolError::FrameTooLarge`] for oversized frames, or
/// [`MicrodaemonProtocolError::Json`] for malformed JSON.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::{decode_microdaemon_json_frame, encode_microdaemon_json_frame};
/// let encoded = encode_microdaemon_json_frame(&42u8).unwrap();
/// let decoded: u8 = decode_microdaemon_json_frame(&encoded).unwrap();
/// assert_eq!(decoded, 42);
/// ```
pub fn decode_microdaemon_json_frame<T: DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, MicrodaemonProtocolError> {
    if bytes.len() < 4 {
        return Err(MicrodaemonProtocolError::TruncatedFrame {
            needed: 4,
            available: bytes.len(),
        });
    }
    let length = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if length > MAX_MICRODAEMON_FRAME_BYTES {
        return Err(MicrodaemonProtocolError::FrameTooLarge {
            actual: length,
            maximum: MAX_MICRODAEMON_FRAME_BYTES,
        });
    }
    let needed = 4 + length;
    if bytes.len() < needed {
        return Err(MicrodaemonProtocolError::TruncatedFrame {
            needed,
            available: bytes.len(),
        });
    }
    Ok(serde_json::from_slice(&bytes[4..needed])?)
}

/// Write a typed payload to a stream using the microdaemon frame codec.
///
/// # Errors
///
/// Returns protocol serialization errors or stream write errors.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::{read_microdaemon_json_frame, write_microdaemon_json_frame};
/// use std::io::Cursor;
/// let mut bytes = Cursor::new(Vec::new());
/// write_microdaemon_json_frame(&mut bytes, &"ok").unwrap();
/// bytes.set_position(0);
/// let decoded: String = read_microdaemon_json_frame(&mut bytes).unwrap();
/// assert_eq!(decoded, "ok");
/// ```
pub fn write_microdaemon_json_frame<T: Serialize, W: Write>(
    writer: &mut W,
    payload: &T,
) -> Result<(), MicrodaemonProtocolError> {
    let mut encoded = encode_microdaemon_json_frame(payload)?;
    let written = writer.write_all(&encoded).and_then(|()| writer.flush());
    // A frame may carry a password handed to a job.
    crate::secret_memory::wipe(&mut encoded);
    written?;
    Ok(())
}

/// Read one typed payload from a stream using the microdaemon frame codec.
///
/// # Errors
///
/// Returns protocol parse errors or stream read errors.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::{read_microdaemon_json_frame, write_microdaemon_json_frame};
/// use std::io::Cursor;
/// let mut bytes = Cursor::new(Vec::new());
/// write_microdaemon_json_frame(&mut bytes, &123u16).unwrap();
/// bytes.set_position(0);
/// let decoded: u16 = read_microdaemon_json_frame(&mut bytes).unwrap();
/// assert_eq!(decoded, 123);
/// ```
pub fn read_microdaemon_json_frame<T: DeserializeOwned, R: Read>(
    reader: &mut R,
) -> Result<T, MicrodaemonProtocolError> {
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_MICRODAEMON_FRAME_BYTES {
        return Err(MicrodaemonProtocolError::FrameTooLarge {
            actual: length,
            maximum: MAX_MICRODAEMON_FRAME_BYTES,
        });
    }
    let mut body = vec![0u8; length];
    let read = reader.read_exact(&mut body);
    let decoded = read.map(|()| serde_json::from_slice(&body));
    // A frame may carry a password handed to a job.
    crate::secret_memory::wipe(&mut body);
    Ok(decoded??)
}

#[cfg(test)]
mod tests;
