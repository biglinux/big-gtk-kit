// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "tests use temp daemon files and do not run on a UI main loop"
)]

use super::*;
use std::error::Error as _;
use std::io::Cursor;

#[test]
fn command_frame_round_trips() {
    let frame = MicrodaemonFrame::command(
        MicrodaemonId::Fileopsd,
        MicrodaemonCommand::Submit {
            job: DurableJobSpec {
                job_key: "copy-1".to_owned(),
                job_class: "copy".to_owned(),
                is_durable: true,
                arguments: serde_json::json!({
                    "source": "/tmp/source",
                    "target": "/tmp/target"
                }),
            },
        },
    );

    let encoded = encode_microdaemon_frame(&frame).expect("frame encodes");
    let decoded = decode_microdaemon_frame(&encoded).expect("frame decodes");

    assert_eq!(decoded, frame);
}

#[test]
fn empty_job_list_round_trips() {
    let frame = MicrodaemonFrame::event(
        MicrodaemonId::Fileopsd,
        MicrodaemonEvent::JobList { jobs: Vec::new() },
    );

    let encoded = encode_microdaemon_frame(&frame).expect("frame encodes");
    let decoded = decode_microdaemon_frame(&encoded).expect("frame decodes");

    assert_eq!(decoded, frame);
}

#[test]
fn stream_events_round_trip() {
    let frames = [
        MicrodaemonFrame::event(
            MicrodaemonId::Ptyd,
            MicrodaemonEvent::StreamRetention {
                job_id: "pty-1".to_owned(),
                retention: MicrodaemonStreamRetention::new(0, 1),
            },
        ),
        MicrodaemonFrame::event(
            MicrodaemonId::Ptyd,
            MicrodaemonEvent::StreamFrame {
                job_id: "pty-1".to_owned(),
                frame: MicrodaemonStreamFrame::new(0, b"hello".to_vec()),
            },
        ),
        MicrodaemonFrame::event(
            MicrodaemonId::Ptyd,
            MicrodaemonEvent::StreamEnded {
                job_id: "pty-1".to_owned(),
                next_sequence: 1,
            },
        ),
    ];

    for frame in frames {
        assert_eq!(frame.version, MICRODAEMON_PROTOCOL_VERSION);
        let encoded = encode_microdaemon_frame(&frame).expect("frame encodes");
        let decoded = decode_microdaemon_frame(&encoded).expect("frame decodes");

        assert_eq!(decoded, frame);
    }
}

#[test]
fn streaming_job_control_command_round_trips() {
    let command = MicrodaemonCommand::JobControl {
        job_id: "pty-1".to_owned(),
        control_class: "resize".to_owned(),
        arguments: serde_json::json!({
            "rows": 40,
            "cols": 120,
        }),
    };
    let frame = MicrodaemonFrame::command(MicrodaemonId::Ptyd, command);

    let encoded = encode_microdaemon_frame(&frame).expect("frame encodes");
    let decoded = decode_microdaemon_frame(&encoded).expect("frame decodes");

    assert_eq!(decoded, frame);
}

#[test]
fn restart_and_recover_commands_round_trip() {
    for command in [
        MicrodaemonCommand::Restart {
            job_id: "job-1".to_owned(),
        },
        MicrodaemonCommand::Recover {
            job_id: "job-1".to_owned(),
        },
    ] {
        let frame = MicrodaemonFrame::command(MicrodaemonId::Fileopsd, command);
        let encoded = encode_microdaemon_frame(&frame).expect("frame encodes");
        let decoded = decode_microdaemon_frame(&encoded).expect("frame decodes");

        assert_eq!(decoded, frame);
    }
}

#[test]
fn decode_rejects_truncated_prefix_and_body() {
    let short = decode_microdaemon_frame(&[0, 0, 0]);
    assert!(matches!(
        short,
        Err(MicrodaemonProtocolError::TruncatedFrame {
            needed: 4,
            available: 3
        })
    ));

    let truncated = decode_microdaemon_frame(&[0, 0, 0, 8, b'{', b'}']);
    assert!(matches!(
        truncated,
        Err(MicrodaemonProtocolError::TruncatedFrame {
            needed: 12,
            available: 6
        })
    ));
}

#[test]
fn decode_rejects_unsupported_version() {
    let mut frame = MicrodaemonFrame::command(MicrodaemonId::Fileopsd, MicrodaemonCommand::Ping);
    frame.version = MICRODAEMON_PROTOCOL_VERSION + 1;
    let body = serde_json::to_vec(&frame).expect("manual frame serializes");
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&(body.len() as u32).to_be_bytes());
    encoded.extend_from_slice(&body);

    let decoded = decode_microdaemon_frame(&encoded);

    assert!(matches!(
        decoded,
        Err(MicrodaemonProtocolError::UnsupportedVersion {
            found,
            supported: MICRODAEMON_PROTOCOL_VERSION
        }) if found == MICRODAEMON_PROTOCOL_VERSION + 1
    ));
}

#[test]
fn replies_yield_only_their_event() {
    let hello = || MicrodaemonEvent::Hello {
        daemon_id: MicrodaemonId::Ptyd,
        capabilities: Vec::new(),
    };
    let reply = MicrodaemonFrame::event(MicrodaemonId::Ptyd, hello());
    assert_eq!(
        reply.into_event(MicrodaemonId::Ptyd).expect("own reply"),
        hello()
    );

    let foreign = MicrodaemonFrame::event(MicrodaemonId::Fileopsd, hello());
    let refused = foreign
        .into_event(MicrodaemonId::Ptyd)
        .expect_err("another daemon's reply");
    assert!(matches!(
        refused,
        MicrodaemonProtocolError::WrongDaemon {
            expected: MicrodaemonId::Ptyd,
            found: MicrodaemonId::Fileopsd
        }
    ));
    assert_eq!(
        std::io::Error::from(refused).kind(),
        std::io::ErrorKind::InvalidData
    );

    let command = MicrodaemonFrame::command(MicrodaemonId::Ptyd, MicrodaemonCommand::Ping);
    assert!(matches!(
        command.into_event(MicrodaemonId::Ptyd),
        Err(MicrodaemonProtocolError::UnexpectedCommand)
    ));

    let mut newer = MicrodaemonFrame::event(MicrodaemonId::Ptyd, hello());
    newer.version = MICRODAEMON_PROTOCOL_VERSION + 1;
    assert!(matches!(
        newer.into_event(MicrodaemonId::Ptyd),
        Err(MicrodaemonProtocolError::UnsupportedVersion { .. })
    ));

    let closed = MicrodaemonProtocolError::Io {
        source: std::io::Error::from(std::io::ErrorKind::ConnectionReset),
    };
    assert_eq!(
        std::io::Error::from(closed).kind(),
        std::io::ErrorKind::ConnectionReset
    );
}

#[test]
fn protocol_error_keeps_context() {
    let frame_too_large = MicrodaemonProtocolError::FrameTooLarge {
        actual: MAX_MICRODAEMON_FRAME_BYTES + 1,
        maximum: MAX_MICRODAEMON_FRAME_BYTES,
    };
    let message = frame_too_large.to_string();
    assert!(
        message.contains(&(MAX_MICRODAEMON_FRAME_BYTES + 1).to_string()),
        "{message}"
    );
    assert!(
        message.contains(&MAX_MICRODAEMON_FRAME_BYTES.to_string()),
        "{message}"
    );
    assert!(frame_too_large.source().is_none());

    let io = MicrodaemonProtocolError::Io {
        source: io_error("socket closed"),
    };
    assert!(io.to_string().contains("socket closed"));
    assert!(io.source().is_some());
}

fn io_error(message: &'static str) -> std::io::Error {
    std::io::Error::other(message)
}

#[test]
fn frame_size_limit_is_inclusive() {
    assert_eq!(MAX_MICRODAEMON_FRAME_BYTES, 16 * 1024 * 1024);

    let exact_body = vec![b' '; MAX_MICRODAEMON_FRAME_BYTES - 1]
        .into_iter()
        .chain(std::iter::once(b'0'))
        .collect::<Vec<_>>();
    let encoded = encode_microdaemon_json_body(&exact_body).unwrap();
    assert_eq!(encoded.len(), 4 + MAX_MICRODAEMON_FRAME_BYTES);
    assert_eq!(
        decode_microdaemon_json_frame::<u8>(&encoded).unwrap(),
        0,
        "a body exactly at the protocol limit must still be accepted"
    );

    let too_large = vec![b'0'; MAX_MICRODAEMON_FRAME_BYTES + 1];
    assert!(matches!(
        encode_microdaemon_json_body(&too_large),
        Err(MicrodaemonProtocolError::FrameTooLarge {
            actual,
            maximum: MAX_MICRODAEMON_FRAME_BYTES
        }) if actual == MAX_MICRODAEMON_FRAME_BYTES + 1
    ));
}

#[test]
fn decode_refuses_oversized_length() {
    assert!(matches!(
        decode_microdaemon_json_frame::<u8>(&[0, 0, 0, 0]),
        Err(MicrodaemonProtocolError::Json(_))
    ));

    let oversized_length = ((MAX_MICRODAEMON_FRAME_BYTES + 1) as u32).to_be_bytes();
    assert!(matches!(
        decode_microdaemon_json_frame::<u8>(&oversized_length),
        Err(MicrodaemonProtocolError::FrameTooLarge {
            actual,
            maximum: MAX_MICRODAEMON_FRAME_BYTES
        }) if actual == MAX_MICRODAEMON_FRAME_BYTES + 1
    ));
}

#[test]
fn stream_codec_honors_limit_boundaries() {
    let mut writer = Cursor::new(Vec::new());
    write_microdaemon_json_frame(&mut writer, &"ok").unwrap();
    assert!(
        writer.get_ref().len() > 4,
        "writer must contain length prefix plus JSON body"
    );
    writer.set_position(0);
    assert_eq!(
        read_microdaemon_json_frame::<String, _>(&mut writer).unwrap(),
        "ok"
    );

    let exact_body = vec![b' '; MAX_MICRODAEMON_FRAME_BYTES - 1]
        .into_iter()
        .chain(std::iter::once(b'0'))
        .collect::<Vec<_>>();
    let mut exact_encoded = Vec::with_capacity(4 + exact_body.len());
    exact_encoded.extend_from_slice(&(exact_body.len() as u32).to_be_bytes());
    exact_encoded.extend_from_slice(&exact_body);
    assert_eq!(
        read_microdaemon_json_frame::<u8, _>(&mut Cursor::new(exact_encoded)).unwrap(),
        0
    );

    let oversized_length = ((MAX_MICRODAEMON_FRAME_BYTES + 1) as u32).to_be_bytes();
    assert!(matches!(
        read_microdaemon_json_frame::<u8, _>(&mut Cursor::new(oversized_length)),
        Err(MicrodaemonProtocolError::FrameTooLarge {
            actual,
            maximum: MAX_MICRODAEMON_FRAME_BYTES
        }) if actual == MAX_MICRODAEMON_FRAME_BYTES + 1
    ));
}

#[test]
fn progress_clamps_to_known_total() {
    assert_eq!(
        JobProgress::new(120, Some(100)),
        JobProgress {
            completed_units: 100,
            total_units: Some(100),
        }
    );
}

#[test]
fn journal_round_trips_restart_specs() {
    let dir = std::env::temp_dir().join(format!("big-os-kit-journal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let journal_path = dir.join("jobs.json");
    let spec = DurableJobSpec {
        job_key: "copy-1".to_owned(),
        job_class: "copy".to_owned(),
        is_durable: true,
        arguments: serde_json::json!({"source": "/tmp/a", "target": "/tmp/b"}),
    };
    let records = [DurableJobJournalRecord {
        snapshot: DurableJobSnapshot {
            job_id: "copy-1".to_owned(),
            job_class: "copy".to_owned(),
            state: DurableJobState::Running,
            progress: Some(JobProgress::new(1, Some(2))),
            status_text: Some("copying".to_owned()),
            details: None,
        },
        restart_spec: Some(spec.clone()),
    }];

    persist_durable_job_journal(&journal_path, 2, &records).expect("persist journal");
    let loaded = load_durable_job_journal(&journal_path, &[1, 2]).expect("load persisted journal");

    assert_eq!(loaded, records);
    assert_eq!(loaded[0].restart_spec, Some(spec));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_job_journal_quarantines_corrupt_file() {
    let dir =
        std::env::temp_dir().join(format!("big-os-kit-corrupt-journal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let journal_path = dir.join("jobs.json");
    std::fs::write(&journal_path, "{not-json").expect("write corrupt journal");

    let loaded = load_durable_job_journal(&journal_path, &[1, 2]).expect("load corrupt journal");

    assert!(loaded.is_empty());
    assert!(!journal_path.exists());
    let quarantine_count = std::fs::read_dir(&dir)
        .expect("read temp dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
        .count();
    assert_eq!(quarantine_count, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn journal_read_errors_surface() {
    let dir =
        std::env::temp_dir().join(format!("big-os-kit-missing-journal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let missing_path = dir.join("missing.json");

    assert_eq!(
        load_durable_job_journal(&missing_path, &[1, 2]).expect("missing journal loads empty"),
        Vec::new()
    );
    assert!(
        load_durable_job_journal(&dir, &[1, 2]).is_err(),
        "read errors other than NotFound must surface to the daemon"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_job_journal_quarantines_unsupported_version() {
    let dir = std::env::temp_dir().join(format!(
        "big-os-kit-unsupported-journal-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let journal_path = dir.join("jobs.json");
    std::fs::write(
        &journal_path,
        serde_json::json!({
            "version": 999,
            "jobs": [],
        })
        .to_string(),
    )
    .expect("write unsupported journal");

    let loaded =
        load_durable_job_journal(&journal_path, &[1, 2]).expect("unsupported journal quarantines");

    assert!(loaded.is_empty());
    assert!(!journal_path.exists());
    let quarantine_count = std::fs::read_dir(&dir)
        .expect("read temp dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
        .count();
    assert_eq!(quarantine_count, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// BigFiles starts its daemon by this name when it is not beside the client,
/// the program `bigfiles-fileopsd` its manifest builds; the terminal finds its
/// daemon at `ptyd.sock` (docs/contracts/PTYD.md).
#[test]
fn daemon_ids_name_program_and_socket() {
    assert_eq!(MicrodaemonId::Fileopsd.as_str(), "bigfiles-fileopsd");
    assert_eq!(MicrodaemonId::Ptyd.socket_file_name(), "ptyd.sock");
}
