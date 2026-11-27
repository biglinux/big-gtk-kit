// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "tests use temp runtime files and do not run on a UI main loop"
)]

//! Runtime-substrate integration tests using real child processes.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a failed fixture fails the test"
)]

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use big_os_kit::microdaemon::{
    DurableJobSpec, DurableJobState, JobProgress, MicrodaemonBody, MicrodaemonCommand,
    MicrodaemonEvent, MicrodaemonFrame, MicrodaemonId, load_durable_job_journal,
    read_microdaemon_json_frame, write_microdaemon_json_frame,
};
use big_os_kit::subprocess::{
    BigSubprocessChild, BigSubprocessOutputMode, BigSubprocessSpec, BigSubprocessStatus,
};

#[test]
fn durable_daemon_reattaches_progress() {
    let dir = unique_temp_dir("daemon-reattach");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let socket_path = dir.join("daemon.sock");
    let journal_path = dir.join("jobs.json");
    let mut daemon = spawn_test_daemon(&socket_path, &journal_path, Duration::from_secs(30));

    wait_for_socket(&socket_path);
    let mut first_client = connect_client(&socket_path);
    write_command(
        &mut first_client,
        MicrodaemonCommand::Submit {
            job: DurableJobSpec {
                job_key: "job-reattach-1".to_owned(),
                job_class: "copy".to_owned(),
                is_durable: true,
                arguments: serde_json::json!({"source": "/tmp/a", "target": "/tmp/b"}),
            },
        },
    );
    assert!(matches!(
        read_event(&mut first_client),
        MicrodaemonEvent::JobAccepted { job_id } if job_id == "job-reattach-1"
    ));
    assert!(matches!(
        read_event(&mut first_client),
        MicrodaemonEvent::JobSnapshot { snapshot }
            if snapshot.job_id == "job-reattach-1"
                && snapshot.progress == Some(JobProgress::new(1, Some(3)))
    ));
    drop(first_client);

    let mut reattached_client = connect_client(&socket_path);
    write_command(&mut reattached_client, MicrodaemonCommand::List);
    assert!(matches!(
        read_event(&mut reattached_client),
        MicrodaemonEvent::JobList { jobs }
            if jobs.iter().any(|job| job.job_id == "job-reattach-1"
                && job.progress == Some(JobProgress::new(1, Some(3))))
    ));
    write_command(
        &mut reattached_client,
        MicrodaemonCommand::Attach {
            job_id: "job-reattach-1".to_owned(),
        },
    );
    assert!(matches!(
        read_event(&mut reattached_client),
        MicrodaemonEvent::JobSnapshot { snapshot }
            if snapshot.job_id == "job-reattach-1"
                && snapshot.progress == Some(JobProgress::new(1, Some(3)))
    ));
    assert!(matches!(
        read_event(&mut reattached_client),
        MicrodaemonEvent::Progress { job_id, progress }
            if job_id == "job-reattach-1" && progress == JobProgress::new(1, Some(3))
    ));

    write_command(&mut reattached_client, MicrodaemonCommand::Shutdown);
    assert!(matches!(
        read_event(&mut reattached_client),
        MicrodaemonEvent::ShutdownAccepted
    ));
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn durable_daemon_exits_after_job() {
    let dir = unique_temp_dir("daemon-idle-exit");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let socket_path = dir.join("daemon.sock");
    let journal_path = dir.join("jobs.json");
    let mut daemon = spawn_test_daemon(&socket_path, &journal_path, Duration::from_millis(50));

    wait_for_socket(&socket_path);
    let mut client = connect_client(&socket_path);
    submit_job(
        &mut client,
        "job-idle-complete",
        "quick",
        serde_json::json!({"duration_ms": 20}),
    );
    assert!(matches!(
        read_event(&mut client),
        MicrodaemonEvent::JobAccepted { job_id } if job_id == "job-idle-complete"
    ));
    assert!(matches!(
        read_event(&mut client),
        MicrodaemonEvent::JobSnapshot { snapshot }
            if snapshot.job_id == "job-idle-complete"
                && snapshot.state == DurableJobState::Running
    ));
    drop(client);

    let status = wait_for_child_exit(&mut daemon, EXIT_DEADLINE);

    assert!(
        status.success(),
        "daemon should idle-exit cleanly: {status:?}"
    );
    let journal = load_durable_job_journal(&journal_path, &[1]).expect("load journal");
    assert!(
        journal
            .iter()
            .any(|record| record.snapshot.job_id == "job-idle-complete"
                && record.snapshot.state == DurableJobState::Completed),
        "the daemon exited before its job completed: {journal:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn durable_daemon_stays_while_job_runs() {
    let dir = unique_temp_dir("daemon-idle-running");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let socket_path = dir.join("daemon.sock");
    let journal_path = dir.join("jobs.json");
    let mut daemon = spawn_test_daemon(&socket_path, &journal_path, Duration::from_millis(50));

    wait_for_socket(&socket_path);
    let mut client = connect_client(&socket_path);
    submit_job(
        &mut client,
        "job-running",
        "slow",
        // Long past the liveness check below, so a descheduled test still
        // looks while the job runs.
        serde_json::json!({"duration_ms": 2000}),
    );
    assert!(matches!(
        read_event(&mut client),
        MicrodaemonEvent::JobAccepted { .. }
    ));
    assert!(matches!(
        read_event(&mut client),
        MicrodaemonEvent::JobSnapshot { .. }
    ));
    drop(client);
    std::thread::sleep(Duration::from_millis(120));

    assert!(
        daemon.try_wait().expect("poll daemon").is_none(),
        "daemon must stay alive while the durable job is running"
    );
    let status = wait_for_child_exit(&mut daemon, EXIT_DEADLINE);

    assert!(
        status.success(),
        "daemon should exit after the job ends: {status:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn streaming_daemon_reattaches_after_disconnect() {
    let dir = unique_temp_dir("daemon-stream-reattach");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let socket_path = dir.join("daemon.sock");
    let journal_path = dir.join("jobs.json");
    let mut daemon = spawn_test_daemon(&socket_path, &journal_path, Duration::from_millis(500));

    wait_for_socket(&socket_path);
    let mut first_client = connect_client(&socket_path);
    submit_job(
        &mut first_client,
        "job-stream",
        "stream-ticks",
        serde_json::json!({"ticks": 5, "interval_ms": 30}),
    );
    assert!(matches!(
        read_event(&mut first_client),
        MicrodaemonEvent::JobAccepted { .. }
    ));
    assert!(matches!(
        read_event(&mut first_client),
        MicrodaemonEvent::JobSnapshot { .. }
    ));
    write_command(
        &mut first_client,
        MicrodaemonCommand::AttachStream {
            job_id: "job-stream".to_owned(),
            after_sequence: None,
        },
    );
    assert!(matches!(
        read_event(&mut first_client),
        MicrodaemonEvent::StreamRetention { job_id, .. } if job_id == "job-stream"
    ));
    let first_sequence = read_stream_frame(&mut first_client, "job-stream").sequence;
    drop(first_client);

    let mut second_client = connect_client(&socket_path);
    write_command(
        &mut second_client,
        MicrodaemonCommand::AttachStream {
            job_id: "job-stream".to_owned(),
            after_sequence: Some(first_sequence),
        },
    );
    assert!(matches!(
        read_event(&mut second_client),
        MicrodaemonEvent::StreamRetention { job_id, retention }
            if job_id == "job-stream" && retention.next_sequence > first_sequence
    ));
    let remaining_sequences = read_stream_until_end(&mut second_client, "job-stream");

    assert_eq!(
        remaining_sequences,
        (first_sequence + 1..5).collect::<Vec<_>>(),
        "reattach resumes after the last consumed sequence, losing and repeating none"
    );
    drop(second_client);
    let status = wait_for_child_exit(&mut daemon, EXIT_DEADLINE);
    assert!(
        status.success(),
        "daemon should idle-exit after stream: {status:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn streaming_daemon_survives_killed_client() {
    let dir = unique_temp_dir("daemon-stream-client-kill");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let socket_path = dir.join("daemon.sock");
    let journal_path = dir.join("jobs.json");
    let mut daemon = spawn_test_daemon(&socket_path, &journal_path, Duration::from_millis(500));

    wait_for_socket(&socket_path);
    let mut submitter = connect_client(&socket_path);
    submit_job(
        &mut submitter,
        "job-stream-kill",
        "stream-ticks",
        serde_json::json!({"ticks": 8, "interval_ms": 100}),
    );
    assert!(matches!(
        read_event(&mut submitter),
        MicrodaemonEvent::JobAccepted { .. }
    ));
    assert!(matches!(
        read_event(&mut submitter),
        MicrodaemonEvent::JobSnapshot { .. }
    ));
    // The submitter stays connected, so the daemon has a client to live for
    // after the stream ends, however slowly this test runs.
    let mut killed_client = BigSubprocessSpec::builder()
        .program(test_client_bin())
        .arg(socket_path.as_os_str())
        .arg("job-stream-kill")
        .stdout(BigSubprocessOutputMode::Capture)
        .build()
        .spawn()
        .expect("spawn stream client");
    // The client writes a byte once a stream frame reached it.
    killed_client
        .take_stdout()
        .expect("stream client stdout")
        .read_exact(&mut [0])
        .expect("stream client attached");

    killed_client.kill().expect("kill stream client");
    let killed_status = killed_client.wait().expect("wait killed client");

    assert_eq!(killed_status.code(), None);
    assert!(
        daemon.try_wait().expect("poll daemon").is_none(),
        "daemon must survive client SIGKILL"
    );
    let mut second_client = connect_client(&socket_path);
    write_command(
        &mut second_client,
        MicrodaemonCommand::AttachStream {
            job_id: "job-stream-kill".to_owned(),
            after_sequence: None,
        },
    );
    // A tick still to come proves the stream went on past the killed client.
    assert!(matches!(
        read_event(&mut second_client),
        MicrodaemonEvent::StreamRetention { job_id, retention }
            if job_id == "job-stream-kill" && retention.next_sequence < 8
    ));
    let sequences = read_stream_until_end(&mut second_client, "job-stream-kill");

    assert!(
        sequences.iter().any(|sequence| *sequence > 0),
        "second client should receive stream progress after the first client died"
    );
    drop(second_client);
    drop(submitter);
    let status = wait_for_child_exit(&mut daemon, EXIT_DEADLINE);
    assert!(
        status.success(),
        "daemon should idle-exit after stream: {status:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

fn spawn_test_daemon(
    socket_path: &std::path::Path,
    journal_path: &std::path::Path,
    idle_grace: Duration,
) -> BigSubprocessChild {
    BigSubprocessSpec::builder()
        .program(test_daemon_bin())
        .arg(socket_path.as_os_str())
        .arg(journal_path.as_os_str())
        .arg(format!("--idle-grace-ms={}", idle_grace.as_millis()))
        .build()
        .spawn()
        .expect("spawn test daemon")
}

fn connect_client(socket_path: &std::path::Path) -> UnixStream {
    let stream = UnixStream::connect(socket_path).expect("connect daemon client");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set client read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set client write timeout");
    stream
}

fn submit_job(
    stream: &mut UnixStream,
    job_key: &str,
    job_class: &str,
    arguments: serde_json::Value,
) {
    write_command(
        stream,
        MicrodaemonCommand::Submit {
            job: DurableJobSpec {
                job_key: job_key.to_owned(),
                job_class: job_class.to_owned(),
                is_durable: true,
                arguments,
            },
        },
    );
}

fn write_command(stream: &mut UnixStream, command: MicrodaemonCommand) {
    write_microdaemon_json_frame(
        stream,
        &MicrodaemonFrame::command(MicrodaemonId::Fileopsd, command),
    )
    .expect("write daemon command");
}

fn read_event(stream: &mut UnixStream) -> MicrodaemonEvent {
    let frame: MicrodaemonFrame =
        read_microdaemon_json_frame(stream).expect("read daemon event frame");
    match frame.body {
        MicrodaemonBody::Event { event } => event,
        MicrodaemonBody::Command { command } => panic!("unexpected daemon command: {command:?}"),
    }
}

fn read_stream_frame(
    stream: &mut UnixStream,
    expected_job_id: &str,
) -> big_os_kit::microdaemon::MicrodaemonStreamFrame {
    loop {
        if let MicrodaemonEvent::StreamFrame { job_id, frame } = read_event(stream) {
            assert_eq!(job_id, expected_job_id);
            return frame;
        }
    }
}

fn read_stream_until_end(stream: &mut UnixStream, expected_job_id: &str) -> Vec<u64> {
    let mut sequences = Vec::new();
    loop {
        match read_event(stream) {
            MicrodaemonEvent::StreamFrame { job_id, frame } => {
                assert_eq!(job_id, expected_job_id);
                sequences.push(frame.sequence);
            }
            MicrodaemonEvent::StreamEnded { job_id, .. } => {
                assert_eq!(job_id, expected_job_id);
                return sequences;
            }
            event => panic!("unexpected stream event: {event:?}"),
        }
    }
}

/// Wait until the daemon accepts connections. The socket's file appears at
/// bind, a moment before the daemon listens on it, and a connection in that
/// moment is refused.
fn wait_for_socket(socket_path: &std::path::Path) {
    let start = Instant::now();
    while UnixStream::connect(socket_path).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "test daemon did not listen"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    // Pinned to `/tmp`, not `std::env::temp_dir()`: the daemon binds a Unix
    // socket under this directory, whose path must fit `sun_path` (108 bytes).
    // A long `$TMPDIR` (the test runner sets one so doctests get an executable
    // temp) would overflow that. Only file/socket creation happens here, so a
    // `noexec` `/tmp` is harmless: the daemon binary itself runs from `target`.
    std::path::Path::new("/tmp").join(format!(
        "big-os-kit-{name}-{}-{nanos}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// The most a daemon is given to exit. Each job step syncs the journal to disk
/// twice, which a drive busy with a build can hold for seconds; the wait ends
/// as soon as the daemon does.
const EXIT_DEADLINE: Duration = Duration::from_secs(10);

fn wait_for_child_exit(child: &mut BigSubprocessChild, timeout: Duration) -> BigSubprocessStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        assert!(
            start.elapsed() < timeout,
            "child did not exit within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn test_daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_big-os-kit-test-daemon")
}

fn test_client_bin() -> &'static str {
    env!("CARGO_BIN_EXE_big-os-kit-test-client")
}
