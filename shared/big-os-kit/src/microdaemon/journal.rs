// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "the microdaemon journal owns durable-job filesystem persistence"
)]

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{DurableJobJournalRecord, DurableJobSnapshot, DurableJobSpec};
use crate::file_io::{BigBoundedReadError, read_bounded};

/// The largest journal written or read: 16 MiB.
const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct DurableJobJournal {
    version: u32,
    jobs: Vec<DurableJobSnapshot>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    restart_specs: HashMap<String, DurableJobSpec>,
}

/// Load a durable-job journal shared by the BigLinux microdaemons.
///
/// A journal that does not parse, or one of a version not in
/// `accepted_versions` (a newer build's), is renamed to a
/// `.corrupt.<pid>-<nanos>` sibling and an empty set returned: the daemon
/// starts, and the file is kept for whoever reads it, never written over.
/// State recovery policy remains daemon-owned: callers decide which
/// non-terminal states become interrupted and how domain-specific details are
/// reconciled.
///
/// # Errors
///
/// Returns bounded-read, quarantine, or filesystem errors other than a missing
/// journal.
pub fn load_durable_job_journal(
    journal_path: &Path,
    accepted_versions: &[u32],
) -> io::Result<Vec<DurableJobJournalRecord>> {
    let bytes = match read_bounded_journal(journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let journal = match serde_json::from_slice::<DurableJobJournal>(&bytes) {
        Ok(journal) if accepted_versions.contains(&journal.version) => journal,
        _ => {
            quarantine_corrupt_journal(journal_path)?;
            return Ok(Vec::new());
        }
    };
    Ok(journal
        .jobs
        .into_iter()
        .map(|snapshot| DurableJobJournalRecord {
            restart_spec: journal.restart_specs.get(&snapshot.job_id).cloned(),
            snapshot,
        })
        .collect())
}

fn read_bounded_journal(journal_path: &Path) -> io::Result<Vec<u8>> {
    read_bounded(journal_path, MAX_JOURNAL_BYTES).map_err(|error| match error {
        BigBoundedReadError::Io(error) => error,
        BigBoundedReadError::TooLarge { .. } => io::Error::new(
            io::ErrorKind::InvalidData,
            "microdaemon journal exceeds size limit",
        ),
    })
}

/// Persist a durable-job journal atomically.
///
/// The write uses create-new temp files, `fsync` on the file, atomic rename, and
/// `fsync` on the parent directory. On write failure the temp file is removed.
///
/// # Errors
///
/// Returns serialization or filesystem errors.
pub fn persist_durable_job_journal(
    journal_path: &Path,
    version: u32,
    records: &[DurableJobJournalRecord],
) -> io::Result<()> {
    let encoded = encode_journal(journal_path, version, records)?;
    // Durable: recovery replays what this says, and a move whose source is
    // already gone must not be replayed from an older entry.
    crate::storage::atomic_write_private_durable(journal_path, &encoded)
}

/// [`persist_durable_job_journal`] without the two `fsync`s, for a journal
/// nothing replays: a power cut may leave an older complete journal, never a
/// torn one.
///
/// # Errors
///
/// Returns serialization or filesystem errors.
pub fn persist_job_journal(
    journal_path: &Path,
    version: u32,
    records: &[DurableJobJournalRecord],
) -> io::Result<()> {
    let encoded = encode_journal(journal_path, version, records)?;
    crate::storage::atomic_write_private(journal_path, &encoded)
}

/// Serialize `records` within the size limit, creating the journal's directory.
fn encode_journal(
    journal_path: &Path,
    version: u32,
    records: &[DurableJobJournalRecord],
) -> io::Result<Vec<u8>> {
    let Some(parent) = journal_path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journal path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;
    let journal = DurableJobJournal {
        version,
        jobs: records
            .iter()
            .map(|record| record.snapshot.clone())
            .collect(),
        restart_specs: records
            .iter()
            .filter_map(|record| {
                record
                    .restart_spec
                    .clone()
                    .map(|spec| (record.snapshot.job_id.clone(), spec))
            })
            .collect(),
    };
    let mut encoded = serde_json::to_vec_pretty(&journal).map_err(io::Error::other)?;
    if encoded.len() as u64 + 1 > MAX_JOURNAL_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "microdaemon journal exceeds size limit",
        ));
    }
    encoded.push(b'\n');
    Ok(encoded)
}

fn quarantine_corrupt_journal(journal_path: &Path) -> io::Result<()> {
    if !journal_path.exists() {
        return Ok(());
    }
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let quarantine_path =
        journal_path.with_extension(format!("json.corrupt.{}-{suffix}", std::process::id()));
    crate::file_io::rename(journal_path, &quarantine_path)?;
    let parent = journal_path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal path has no parent"))?;
    // A bare file name's parent is empty: the working directory.
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    // Synced before a new journal can take its name.
    File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;

    fn journal_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "big-os-kit-journal-{name}-{}-{}/jobs.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ))
    }

    #[test]
    fn persisted_journal_is_private() {
        let path = journal_path("permissions");

        persist_durable_job_journal(&path, 1, &[]).expect("persist journal");

        assert_eq!(
            fs::metadata(&path)
                .expect("journal metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().expect("parent"));
    }

    #[test]
    fn oversized_journal_is_rejected() {
        let path = journal_path("oversized");
        fs::create_dir_all(path.parent().expect("parent")).expect("journal parent");
        let file = File::create(&path).expect("journal file");
        file.set_len(MAX_JOURNAL_BYTES + 1)
            .expect("oversized sparse journal");

        let error = load_durable_job_journal(&path, &[1]).expect_err("oversized journal");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().expect("parent"));
    }

    #[test]
    fn journal_holds_16_mib() {
        const LIMIT: usize = 16 << 20;
        let path = journal_path("at-limit");
        let record = |text_len| DurableJobJournalRecord {
            snapshot: DurableJobSnapshot {
                job_id: "full".to_owned(),
                job_class: "test".to_owned(),
                state: super::super::DurableJobState::Failed,
                progress: None,
                status_text: Some("x".repeat(text_len)),
                details: None,
            },
            restart_spec: None,
        };
        let frame = encode_journal(&path, 1, &[record(0)])
            .expect("empty record")
            .len();
        let full = [record(LIMIT - frame)];

        persist_job_journal(&path, 1, &full).expect("a journal of 16 MiB is written");

        assert_eq!(fs::metadata(&path).expect("journal").len(), LIMIT as u64);
        assert_eq!(load_durable_job_journal(&path, &[1]).expect("load"), full);
        let over = persist_job_journal(&path, 1, &[record(LIMIT - frame + 1)])
            .expect_err("a byte more is refused");
        assert_eq!(over.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().expect("parent"));
    }

    #[test]
    fn oversized_persist_keeps_last_valid_journal() {
        let path = journal_path("oversized-persist");
        persist_durable_job_journal(&path, 1, &[]).expect("seed valid journal");
        let seeded = fs::read(&path).expect("read seeded journal");
        let oversized = DurableJobJournalRecord {
            snapshot: DurableJobSnapshot {
                job_id: "oversized".to_owned(),
                job_class: "test".to_owned(),
                state: super::super::DurableJobState::Failed,
                progress: None,
                status_text: Some("x".repeat(MAX_JOURNAL_BYTES as usize)),
                details: None,
            },
            restart_spec: None,
        };

        let error = persist_durable_job_journal(&path, 1, &[oversized])
            .expect_err("oversized persist must fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).expect("previous journal survives"), seeded);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().expect("parent"));
    }

    #[test]
    fn corrupt_journal_empties_after_quarantine() {
        let path = journal_path("quarantine");
        fs::create_dir_all(path.parent().expect("parent")).expect("journal parent");
        fs::write(&path, b"not json").expect("corrupt journal");

        let records = load_durable_job_journal(&path, &[1]).expect("quarantine corrupt journal");

        assert!(records.is_empty());
        assert!(!path.exists());
        let quarantined = fs::read_dir(path.parent().expect("parent"))
            .expect("read quarantine parent")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|candidate| candidate.to_string_lossy().contains(".corrupt."))
            .expect("quarantine sibling");
        assert_eq!(
            fs::read(&quarantined).expect("quarantine content"),
            b"not json"
        );
        let _ = fs::remove_file(quarantined);
        let _ = fs::remove_dir(path.parent().expect("parent"));
    }

    #[test]
    fn invalid_utf8_journal_is_durably_quarantined() {
        let path = journal_path("invalid-utf8");
        fs::create_dir_all(path.parent().expect("parent")).expect("journal parent");
        fs::write(&path, [0xff, 0xfe, 0xfd]).expect("invalid utf8 journal");

        let records = load_durable_job_journal(&path, &[1]).expect("quarantine invalid utf8");

        assert!(records.is_empty());
        assert!(!path.exists());
        let quarantined = fs::read_dir(path.parent().expect("parent"))
            .expect("read quarantine parent")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|candidate| candidate.to_string_lossy().contains(".corrupt."))
            .expect("quarantine sibling");
        let _ = fs::remove_file(quarantined);
        let _ = fs::remove_dir(path.parent().expect("parent"));
    }
}
