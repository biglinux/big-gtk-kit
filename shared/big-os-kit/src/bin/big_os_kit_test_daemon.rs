// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "test daemon removes sockets outside any UI main loop"
)]

//! Test-only durable microdaemon for the big-os-kit lifecycle contracts.

#![expect(
    clippy::expect_used,
    reason = "a test fixture: its failures fail the test that runs it"
)]

use std::collections::HashMap;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use big_os_kit::microdaemon::{
    DurableJobJournalRecord, DurableJobSnapshot, DurableJobSpec, DurableJobState, JobProgress,
    MicrodaemonBody, MicrodaemonCommand, MicrodaemonEvent, MicrodaemonFrame, MicrodaemonId,
    MicrodaemonIdleExit, MicrodaemonJobLease, MicrodaemonStreamFrame, MicrodaemonStreamRetention,
    load_durable_job_journal, persist_durable_job_journal, read_microdaemon_json_frame,
    write_microdaemon_json_frame,
};

const JOURNAL_VERSION: u32 = 1;
const STREAM_RETAINED_FRAMES: usize = 64;

fn main() -> io::Result<()> {
    let TestDaemonArgs {
        socket_path,
        journal_path,
        idle_grace,
    } = TestDaemonArgs::parse();
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;

    let daemon = TestDaemon::load(journal_path, idle_grace)?;
    while !daemon.is_shutdown_requested() && !daemon.idle_exit.should_exit() {
        match listener.accept() {
            Ok((stream, _addr)) => daemon.spawn_connection(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

struct TestDaemonArgs {
    socket_path: PathBuf,
    journal_path: PathBuf,
    idle_grace: Duration,
}

impl TestDaemonArgs {
    fn parse() -> Self {
        let mut idle_grace = Duration::from_secs(30);
        let mut paths = Vec::new();
        for arg in std::env::args_os().skip(1) {
            let arg_text = arg.to_string_lossy();
            if let Some(milliseconds) = arg_text.strip_prefix("--idle-grace-ms=") {
                idle_grace = Duration::from_millis(milliseconds.parse().expect("valid idle grace"));
            } else {
                paths.push(PathBuf::from(arg));
            }
        }
        Self {
            socket_path: paths.first().expect("socket path argument").clone(),
            journal_path: paths.get(1).expect("journal path argument").clone(),
            idle_grace,
        }
    }
}

#[derive(Clone)]
struct TestDaemon {
    journal_path: PathBuf,
    shared: Arc<Mutex<TestDaemonShared>>,
    idle_exit: MicrodaemonIdleExit,
    shutdown_requested: Arc<AtomicBool>,
}

impl TestDaemon {
    fn load(journal_path: PathBuf, idle_grace: Duration) -> io::Result<Self> {
        let records = load_durable_job_journal(&journal_path, &[JOURNAL_VERSION])?
            .into_iter()
            .map(|record| (record.snapshot.job_id.clone(), record))
            .collect();
        Ok(Self {
            journal_path,
            shared: Arc::new(Mutex::new(TestDaemonShared {
                records,
                streams: HashMap::new(),
                subscribers: Vec::new(),
            })),
            idle_exit: MicrodaemonIdleExit::new(idle_grace),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
        })
    }

    fn spawn_connection(&self, stream: UnixStream) {
        let daemon = self.clone();
        std::thread::spawn(move || {
            let _client_lease = daemon.idle_exit.client_connected();
            let _ = daemon.handle_connection(stream);
        });
    }

    fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    fn handle_connection(&self, stream: UnixStream) -> io::Result<()> {
        let mut reader = stream.try_clone()?;
        let writer = Arc::new(Mutex::new(stream));
        loop {
            let frame = match read_microdaemon_json_frame::<MicrodaemonFrame, _>(&mut reader) {
                Ok(frame) => frame,
                Err(_) => return Ok(()),
            };
            let MicrodaemonBody::Command { command } = frame.body else {
                continue;
            };
            match command {
                MicrodaemonCommand::Ping => write_event(
                    &writer,
                    MicrodaemonEvent::Hello {
                        daemon_id: MicrodaemonId::Fileopsd,
                        capabilities: vec![
                            "test-daemon".to_owned(),
                            "reattach".to_owned(),
                            "idle-exit".to_owned(),
                            "streaming".to_owned(),
                        ],
                    },
                )?,
                MicrodaemonCommand::Submit { job } => self.submit_job(&writer, job)?,
                MicrodaemonCommand::List => write_event(
                    &writer,
                    MicrodaemonEvent::JobList {
                        jobs: self.snapshots(),
                    },
                )?,
                MicrodaemonCommand::Attach { job_id } => self.attach_job(&writer, job_id)?,
                MicrodaemonCommand::AttachStream {
                    job_id,
                    after_sequence,
                } => self.attach_stream(&writer, job_id, after_sequence)?,
                MicrodaemonCommand::Shutdown => {
                    write_event(&writer, MicrodaemonEvent::ShutdownAccepted)?;
                    self.shutdown_requested.store(true, Ordering::Release);
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    fn submit_job(&self, writer: &Arc<Mutex<UnixStream>>, job: DurableJobSpec) -> io::Result<()> {
        let snapshot = running_snapshot(&job);
        self.persist_record(DurableJobJournalRecord {
            snapshot: snapshot.clone(),
            restart_spec: Some(job.clone()),
        })?;
        write_event(
            writer,
            MicrodaemonEvent::JobAccepted {
                job_id: snapshot.job_id.clone(),
            },
        )?;
        write_event(writer, MicrodaemonEvent::JobSnapshot { snapshot })?;

        match job.job_class.as_str() {
            "quick" | "slow" => {
                let job_lease = self.idle_exit.job_started();
                self.spawn_completion_job(job, job_lease);
            }
            "stream-ticks" => {
                let job_lease = self.idle_exit.job_started();
                self.spawn_stream_job(job, job_lease)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn attach_job(&self, writer: &Arc<Mutex<UnixStream>>, job_id: String) -> io::Result<()> {
        let snapshot = self
            .shared
            .lock()
            .expect("test daemon state lock poisoned")
            .records
            .get(&job_id)
            .map(|record| record.snapshot.clone());
        if let Some(snapshot) = snapshot {
            write_event(
                writer,
                MicrodaemonEvent::JobSnapshot {
                    snapshot: snapshot.clone(),
                },
            )?;
            if let Some(progress) = snapshot.progress {
                write_event(writer, MicrodaemonEvent::Progress { job_id, progress })?;
            }
        } else {
            write_not_found(writer, job_id)?;
        }
        Ok(())
    }

    fn attach_stream(
        &self,
        writer: &Arc<Mutex<UnixStream>>,
        job_id: String,
        after_sequence: Option<u64>,
    ) -> io::Result<()> {
        let mut shared = self.shared.lock().expect("test daemon state lock poisoned");
        let Some(stream_log) = shared.streams.get(&job_id) else {
            drop(shared);
            return write_not_found(writer, job_id);
        };
        let retained_frames = stream_log
            .frames
            .iter()
            .filter(|frame| after_sequence.is_none_or(|sequence| frame.sequence > sequence))
            .cloned()
            .collect::<Vec<_>>();
        let retention = stream_log.retention();
        let is_ended = stream_log.is_ended;
        let next_sequence = stream_log.next_sequence;
        shared.subscribers.push(TestStreamSubscriber {
            job_id: job_id.clone(),
            writer: Arc::clone(writer),
        });

        let mut stream = writer.lock().expect("test daemon writer lock poisoned");
        write_event_to_stream(
            &mut stream,
            MicrodaemonEvent::StreamRetention {
                job_id: job_id.clone(),
                retention,
            },
        )?;
        for frame in retained_frames {
            write_event_to_stream(
                &mut stream,
                MicrodaemonEvent::StreamFrame {
                    job_id: job_id.clone(),
                    frame,
                },
            )?;
        }
        if is_ended {
            write_event_to_stream(
                &mut stream,
                MicrodaemonEvent::StreamEnded {
                    job_id,
                    next_sequence,
                },
            )?;
        }
        Ok(())
    }

    fn snapshots(&self) -> Vec<DurableJobSnapshot> {
        self.shared
            .lock()
            .expect("test daemon state lock poisoned")
            .records
            .values()
            .map(|record| record.snapshot.clone())
            .collect()
    }

    fn persist_record(&self, record: DurableJobJournalRecord) -> io::Result<()> {
        let mut shared = self.shared.lock().expect("test daemon state lock poisoned");
        shared
            .records
            .insert(record.snapshot.job_id.clone(), record);
        persist_shared_records(&self.journal_path, &shared.records)
    }

    fn spawn_completion_job(&self, job: DurableJobSpec, job_lease: MicrodaemonJobLease) {
        let daemon = self.clone();
        std::thread::spawn(move || {
            let _job_lease = job_lease;
            std::thread::sleep(job_duration(&job));
            let _ = daemon.complete_job(&job.job_key, JobProgress::new(1, Some(1)));
        });
    }

    fn spawn_stream_job(
        &self,
        job: DurableJobSpec,
        job_lease: MicrodaemonJobLease,
    ) -> io::Result<()> {
        {
            let mut shared = self.shared.lock().expect("test daemon state lock poisoned");
            shared
                .streams
                .insert(job.job_key.clone(), TestStreamLog::default());
        }
        let daemon = self.clone();
        std::thread::spawn(move || {
            let _job_lease = job_lease;
            let ticks = job_argument_u64(&job, "ticks", 5);
            let interval = Duration::from_millis(job_argument_u64(&job, "interval_ms", 25));
            for tick in 0..ticks {
                std::thread::sleep(interval);
                let frame =
                    MicrodaemonStreamFrame::new(tick, format!("tick-{tick}\n").into_bytes());
                let _ = daemon.append_stream_frame(&job.job_key, frame, ticks);
            }
            let _ = daemon.finish_stream(&job.job_key, ticks);
            let _ = daemon.complete_job(&job.job_key, JobProgress::new(ticks, Some(ticks)));
        });
        Ok(())
    }

    fn append_stream_frame(
        &self,
        job_id: &str,
        frame: MicrodaemonStreamFrame,
        total_ticks: u64,
    ) -> io::Result<()> {
        let subscribers = {
            let mut shared = self.shared.lock().expect("test daemon state lock poisoned");
            let stream_log = shared
                .streams
                .get_mut(job_id)
                .expect("stream log exists for streaming job");
            stream_log.push(frame.clone());
            if let Some(record) = shared.records.get_mut(job_id) {
                record.snapshot.progress =
                    Some(JobProgress::new(frame.sequence + 1, Some(total_ticks)));
            }
            persist_shared_records(&self.journal_path, &shared.records)?;
            shared
                .subscribers
                .iter()
                .filter(|subscriber| subscriber.job_id == job_id)
                .map(|subscriber| Arc::clone(&subscriber.writer))
                .collect::<Vec<_>>()
        };
        self.broadcast_stream_event(
            job_id,
            subscribers,
            MicrodaemonEvent::StreamFrame {
                job_id: job_id.to_owned(),
                frame,
            },
        );
        Ok(())
    }

    fn finish_stream(&self, job_id: &str, next_sequence: u64) -> io::Result<()> {
        let subscribers = {
            let mut shared = self.shared.lock().expect("test daemon state lock poisoned");
            if let Some(stream_log) = shared.streams.get_mut(job_id) {
                stream_log.is_ended = true;
                stream_log.next_sequence = next_sequence;
            }
            shared
                .subscribers
                .iter()
                .filter(|subscriber| subscriber.job_id == job_id)
                .map(|subscriber| Arc::clone(&subscriber.writer))
                .collect::<Vec<_>>()
        };
        self.broadcast_stream_event(
            job_id,
            subscribers,
            MicrodaemonEvent::StreamEnded {
                job_id: job_id.to_owned(),
                next_sequence,
            },
        );
        Ok(())
    }

    fn complete_job(&self, job_id: &str, progress: JobProgress) -> io::Result<()> {
        let mut shared = self.shared.lock().expect("test daemon state lock poisoned");
        if let Some(record) = shared.records.get_mut(job_id) {
            record.snapshot.state = DurableJobState::Completed;
            record.snapshot.progress = Some(progress);
            record.snapshot.status_text = Some("completed".to_owned());
        }
        persist_shared_records(&self.journal_path, &shared.records)
    }

    fn broadcast_stream_event(
        &self,
        job_id: &str,
        subscribers: Vec<Arc<Mutex<UnixStream>>>,
        event: MicrodaemonEvent,
    ) {
        let mut failed_writers = Vec::new();
        for writer in subscribers {
            let result = writer
                .lock()
                .map_err(|_| io::Error::other("writer lock poisoned"))
                .and_then(|mut stream| write_event_to_stream(&mut stream, event.clone()));
            if result.is_err() {
                failed_writers.push(writer);
            }
        }
        if failed_writers.is_empty() {
            return;
        }
        let mut shared = self.shared.lock().expect("test daemon state lock poisoned");
        shared.subscribers.retain(|subscriber| {
            subscriber.job_id != job_id
                || !failed_writers
                    .iter()
                    .any(|writer| Arc::ptr_eq(writer, &subscriber.writer))
        });
    }
}

struct TestDaemonShared {
    records: HashMap<String, DurableJobJournalRecord>,
    streams: HashMap<String, TestStreamLog>,
    subscribers: Vec<TestStreamSubscriber>,
}

#[derive(Default)]
struct TestStreamLog {
    frames: Vec<MicrodaemonStreamFrame>,
    is_ended: bool,
    next_sequence: u64,
}

impl TestStreamLog {
    fn push(&mut self, frame: MicrodaemonStreamFrame) {
        self.next_sequence = frame.sequence + 1;
        self.frames.push(frame);
        if self.frames.len() > STREAM_RETAINED_FRAMES {
            self.frames.remove(0);
        }
    }

    fn retention(&self) -> MicrodaemonStreamRetention {
        MicrodaemonStreamRetention::new(
            self.frames
                .first()
                .map_or(self.next_sequence, |frame| frame.sequence),
            self.next_sequence,
        )
    }
}

struct TestStreamSubscriber {
    job_id: String,
    writer: Arc<Mutex<UnixStream>>,
}

fn running_snapshot(job: &DurableJobSpec) -> DurableJobSnapshot {
    let (completed_units, total_units) = match job.job_class.as_str() {
        "quick" | "slow" => (0, Some(1)),
        "stream-ticks" => (0, Some(job_argument_u64(job, "ticks", 5))),
        _ => (1, Some(3)),
    };
    DurableJobSnapshot {
        job_id: job.job_key.clone(),
        job_class: job.job_class.clone(),
        state: DurableJobState::Running,
        progress: Some(JobProgress::new(completed_units, total_units)),
        status_text: Some("running from journal".to_owned()),
        details: Some(serde_json::json!({ "reattachable": true })),
    }
}

fn job_duration(job: &DurableJobSpec) -> Duration {
    Duration::from_millis(job_argument_u64(job, "duration_ms", 25))
}

fn job_argument_u64(job: &DurableJobSpec, key: &str, default: u64) -> u64 {
    job.arguments
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(default)
}

fn persist_shared_records(
    journal_path: &Path,
    records: &HashMap<String, DurableJobJournalRecord>,
) -> io::Result<()> {
    let mut ordered_records = records.values().cloned().collect::<Vec<_>>();
    ordered_records.sort_by(|left, right| left.snapshot.job_id.cmp(&right.snapshot.job_id));
    persist_durable_job_journal(journal_path, JOURNAL_VERSION, &ordered_records)
}

fn write_not_found(writer: &Arc<Mutex<UnixStream>>, job_id: String) -> io::Result<()> {
    write_event(
        writer,
        MicrodaemonEvent::Error {
            job_id: Some(job_id),
            code: "not-found".to_owned(),
            message: "job not found".to_owned(),
        },
    )
}

fn write_event(writer: &Arc<Mutex<UnixStream>>, event: MicrodaemonEvent) -> io::Result<()> {
    let mut stream = writer.lock().expect("test daemon writer lock poisoned");
    write_event_to_stream(&mut stream, event)
}

fn write_event_to_stream(writer: &mut UnixStream, event: MicrodaemonEvent) -> io::Result<()> {
    write_microdaemon_json_frame(
        writer,
        &MicrodaemonFrame::event(MicrodaemonId::Fileopsd, event),
    )
    .map_err(io::Error::other)
}
