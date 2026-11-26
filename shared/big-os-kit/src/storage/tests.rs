// SPDX-License-Identifier: MIT

use super::*;
use serde::{Deserialize, Serialize, ser::Error as _};

fn set_xattr(path: &Path, name: &str, value: &[u8]) -> bool {
    let path = c_path(path);
    let name = std::ffi::CString::new(name).unwrap();
    // SAFETY: live NUL-terminated strings and value buffer.
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        ) == 0
    }
}

/// A store write does not wait for the disk, and stays atomic, private and
/// readable.
#[test]
fn store_write_reads_back_privately() {
    let path = storage_test_path("remember").join("layout.json");
    let store: BigVersionedJsonStore<Vec<u32>> = BigVersionedJsonStore::new(&path, 3);

    store.write(&vec![1, 2, 3]).unwrap();

    assert_eq!(store.read().unwrap(), Some(vec![1, 2, 3]));
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// The durable writer publishes exactly what the plain one does.
#[test]
fn durable_write_publishes_same_file() {
    let dir = storage_test_path("durable");
    fs::create_dir_all(&dir).unwrap();
    atomic_write_private_durable(&dir.join("journal"), b"entries").unwrap();

    assert_eq!(fs::read(dir.join("journal")).unwrap(), b"entries");
    assert_eq!(
        fs::metadata(dir.join("journal")).unwrap().mode() & 0o777,
        0o600
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn replaced_document_keeps_mode_and_xattrs() {
    let directory = storage_test_path("replace-keeps");
    fs::create_dir(&directory).unwrap();
    let target = directory.join("download.txt");
    fs::write(&target, b"old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let has_xattrs = set_xattr(&target, "user.xdg.origin.url", b"https://example.org/x");

    assert!(atomic_replace_document(&target, b"new", || true).unwrap());

    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o640);
    if has_xattrs {
        let kept = document_attributes(&target).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].1, b"https://example.org/x");
    }
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn replaced_document_keeps_its_link() {
    let directory = storage_test_path("replace-link");
    fs::create_dir(&directory).unwrap();
    let target = directory.join("notes.txt");
    let link = directory.join("notes-link.txt");
    fs::write(&target, b"old").unwrap();
    std::os::unix::fs::symlink("notes.txt", &link).unwrap();

    assert!(atomic_replace_document(&link, b"new", || true).unwrap());

    let still_link = fs::symlink_metadata(&link).unwrap().is_symlink();
    let written = fs::read(&target).unwrap();
    let _ = fs::remove_dir_all(&directory);
    assert!(still_link);
    assert_eq!(written, b"new");
}

#[test]
fn stale_replacement_changes_nothing() {
    let directory = storage_test_path("replace-stale");
    fs::create_dir(&directory).unwrap();
    let target = directory.join("notes.txt");
    fs::write(&target, b"theirs").unwrap();

    assert!(!atomic_replace_document(&target, b"ours", || false).unwrap());

    assert_eq!(fs::read(&target).unwrap(), b"theirs");
    assert_eq!(
        fs::read_dir(&directory).unwrap().count(),
        1,
        "no temporary left behind"
    );
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn new_document_follows_umask() {
    let directory = storage_test_path("create-document");
    fs::create_dir(&directory).unwrap();
    let target = directory.join("page.html");
    // Read, never set: the umask is process-wide and other tests run alongside.
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let umask = status
        .lines()
        .find_map(|line| line.strip_prefix("Umask:"))
        .map(|value| u32::from_str_radix(value.trim(), 8).unwrap())
        .unwrap();

    atomic_create_document(&target, b"<p>").unwrap();

    assert_eq!(
        fs::metadata(&target).unwrap().mode() & 0o7777,
        0o666 & !umask
    );
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn write_through_symlink_keeps_link() {
    let directory = storage_test_path("linked-document");
    fs::create_dir(&directory).unwrap();
    let real = directory.join("real.toml");
    let link = directory.join("link.toml");
    fs::write(&real, b"old").unwrap();
    std::os::unix::fs::symlink("real.toml", &link).unwrap();
    atomic_write(&link, b"new").unwrap();
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("real.toml"));
    assert_eq!(fs::read(&real).unwrap(), b"new");
    // The private writers replace the link with a file of their own.
    atomic_write_private(&link, b"private").unwrap();
    assert!(
        !fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&real).unwrap(), b"new");
    fs::remove_dir_all(directory).unwrap();
}

/// A document saved under a new name is created, one saved over an existing
/// name replaced, and one saved through a link written to the file it names
/// while the link stays.
#[test]
fn saved_document_follows_links() {
    let directory = storage_test_path("saved-document");
    fs::create_dir(&directory).unwrap();
    let real = directory.join("shot.png");
    let link = directory.join("latest.png");
    atomic_save_document(&real, b"first").unwrap();
    assert_eq!(fs::read(&real).unwrap(), b"first");
    atomic_save_document(&real, b"second").unwrap();
    assert_eq!(fs::read(&real).unwrap(), b"second");
    std::os::unix::fs::symlink("shot.png", &link).unwrap();
    atomic_save_document(&link, b"third").unwrap();
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("shot.png"));
    assert_eq!(fs::read(&real).unwrap(), b"third");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn private_creation_keeps_dangling_symlink() {
    let directory = storage_test_path("symlink-parent");
    fs::create_dir(&directory).unwrap();
    let target = directory.join("document");
    std::os::unix::fs::symlink("missing", &target).unwrap();
    assert_eq!(
        atomic_create_private(&target, b"ours").unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_link(&target).unwrap(), Path::new("missing"));
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn private_creation_has_one_winner() {
    let directory = storage_test_path("race-parent");
    fs::create_dir(&directory).unwrap();
    let target = directory.join("document");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for bytes in [b"first".as_slice(), b"second".as_slice()] {
        let (path, barrier) = (target.clone(), barrier.clone());
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            atomic_create_private(&path, bytes)
        }));
    }
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.kind() == std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    assert!(matches!(
        fs::read(&target).unwrap().as_slice(),
        b"first" | b"second"
    ));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn no_replace_keeps_non_utf8_destination() {
    use std::os::unix::ffi::OsStringExt as _;
    let directory = storage_test_path("raw-parent");
    fs::create_dir(&directory).unwrap();
    let target = directory.join(std::ffi::OsString::from_vec(b"name-\xff.txt".to_vec()));
    atomic_create_private(&target, b"ours").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"ours");
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn private_creation_never_overwrites_target() {
    let path = storage_test_path("exclusive");
    atomic_create_private(&path, b"first").unwrap();
    assert_eq!(
        atomic_create_private(&path, b"second").unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&path).unwrap(), b"first");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_file(path).unwrap();
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
struct Prefs {
    volume: u8,
    theme: String,
}

fn storage_test_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    p.push(format!("big-os-kit-storage-test-{pid}-{seq}-{name}.json"));
    p
}

#[test]
fn missing_file_returns_none() {
    let store: BigAtomicJsonStore<Prefs> = BigAtomicJsonStore::new(storage_test_path("missing"));
    assert!(store.read().unwrap().is_none());
}

#[test]
fn round_trip_preserves_payload() {
    let path = storage_test_path("round-trip");
    let store: BigAtomicJsonStore<Prefs> = BigAtomicJsonStore::new(&path);
    let prefs = Prefs {
        volume: 33,
        theme: "dark".to_string(),
    };
    store.write(&prefs).unwrap();
    let loaded = store.read().unwrap().unwrap();
    assert_eq!(loaded, prefs);
    let _ = fs::remove_file(&path);
}

#[test]
fn atomic_write_creates_exact_bytes() {
    let path = storage_test_path("atomic-create");
    atomic_write(&path, b"hello\0world").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"hello\0world");
    let _ = fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn atomic_write_preserves_existing_target_mode() {
    let path = storage_test_path("atomic-preserve-mode");
    fs::write(&path, b"old").unwrap();
    // Group and other write bits, which the umask would strip from a new file.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

    atomic_write(&path, b"new").unwrap();

    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o666
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn private_atomic_write_uses_private_mode() {
    let path = storage_test_path("atomic-private");
    atomic_write_private(&path, b"private").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"private");
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let _ = fs::remove_file(&path);
}

#[test]
fn pending_write_skips_stale_temp() {
    let occupied = storage_test_path("occupied-temp");
    let available = storage_test_path("available-temp");
    fs::write(&occupied, b"stale").unwrap();
    let mut candidates = [occupied.clone(), available.clone()].into_iter();

    let pending = PendingAtomicWrite::create_sibling_with(Some(0o600), || {
        candidates.next().expect("two candidate paths")
    })
    .unwrap();

    assert_eq!(pending.path, available);
    drop(pending);
    assert!(
        occupied.exists(),
        "collision handling must not remove stale data"
    );
    assert!(
        !available.exists(),
        "drop must clean the allocated temp file"
    );
    let _ = fs::remove_file(occupied);
}

#[test]
fn atomic_write_leaves_no_temp() {
    let path = storage_test_path("atomic-overwrite");
    atomic_write(&path, b"first").unwrap();
    atomic_write(&path, b"second-longer").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"second-longer");
    // The sibling temp file (`.<name>.tmp.<pid>.<seq>`) must be renamed away,
    // not left behind.
    let parent = path.parent().unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let stray: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&format!(".{name}.tmp."))
        })
        .collect();
    assert!(stray.is_empty(), "temp file left behind: {stray:?}");
    let _ = fs::remove_file(&path);
}

#[test]
fn write_replaces_existing_content() {
    let path = storage_test_path("replace");
    let store: BigAtomicJsonStore<Prefs> = BigAtomicJsonStore::new(&path);
    store
        .write(&Prefs {
            volume: 1,
            theme: "a".into(),
        })
        .unwrap();
    store
        .write(&Prefs {
            volume: 99,
            theme: "b".into(),
        })
        .unwrap();
    let loaded = store.read().unwrap().unwrap();
    assert_eq!(loaded.volume, 99);
    assert_eq!(loaded.theme, "b");
    let _ = fs::remove_file(&path);
}

#[test]
fn write_leaves_no_temp_file() {
    let path = storage_test_path("no-leak");
    let store: BigAtomicJsonStore<Prefs> = BigAtomicJsonStore::new(&path);
    store
        .write(&Prefs {
            volume: 5,
            theme: "x".into(),
        })
        .unwrap();
    let parent = path.parent().unwrap();
    let stem = format!(".{}", path.file_name().unwrap().to_string_lossy());
    let leftover = fs::read_dir(parent)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with(&stem));
    assert!(!leftover, "no .tmp sibling should remain after rename");
    let _ = fs::remove_file(&path);
}

#[derive(Deserialize)]
struct SerializationFailure;

impl Serialize for SerializationFailure {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(S::Error::custom("intentional serialization failure"))
    }
}

#[test]
fn serialization_failure_removes_pending_temp_file() {
    let path = storage_test_path("serialize-failure");
    let store = BigAtomicJsonStore::new(&path);
    assert!(store.write(&SerializationFailure).is_err());

    let parent = path.parent().unwrap();
    let prefix = format!(".{}.tmp.", path.file_name().unwrap().to_string_lossy());
    let has_pending_temp = fs::read_dir(parent)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix));
    assert!(!has_pending_temp, "failed write left a temp file behind");
    assert!(!path.exists(), "failed write installed the target file");
}

#[test]
fn document_bound_is_16_mib() {
    const LIMIT: usize = 16 << 20;
    let path = storage_test_path("at-limit");
    let mut bytes = vec![b'x'; LIMIT];
    bytes[0] = b'"';
    bytes[LIMIT - 1] = b'"';
    fs::write(&path, &bytes).unwrap();
    let store: BigAtomicJsonStore<String> = BigAtomicJsonStore::new(&path);

    assert_eq!(
        store.read().unwrap().map(|text| text.len()),
        Some(LIMIT - 2)
    );

    bytes.insert(1, b'x');
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        store.read(),
        Err(BigStorageError::TooLarge { max_bytes }) if max_bytes == LIMIT as u64
    ));
    let _ = fs::remove_file(&path);
}

#[test]
fn oversized_write_keeps_document() {
    let path = storage_test_path("oversized-write");
    let store: BigAtomicJsonStore<String> = BigAtomicJsonStore::new(&path);
    store.write(&"kept".to_owned()).unwrap();

    let text = "x".repeat(16 << 20);
    assert!(matches!(
        store.write(&text),
        Err(BigStorageError::TooLarge { .. })
    ));
    assert_eq!(store.read().unwrap().as_deref(), Some("kept"));
    let _ = fs::remove_file(&path);
}

#[test]
fn unknown_envelope_key_is_read() {
    let path = storage_test_path("unknown-envelope-field");
    let envelope = serde_json::json!({
        "version": 2,
        "data": { "volume": 8, "theme": "ok" },
        "future": true
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
    let store: BigVersionedJsonStore<Prefs> = BigVersionedJsonStore::new(&path, 2);

    assert_eq!(
        store.read().unwrap(),
        Some(Prefs {
            volume: 8,
            theme: "ok".into()
        })
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn versioned_store_keeps_newer_document() {
    let path = storage_test_path("newer-refused");
    let envelope = serde_json::json!({
        "version": 3,
        "data": { "volume": 8, "theme": "ok" }
    });
    let bytes = serde_json::to_vec_pretty(&envelope).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let store: BigVersionedJsonStore<Prefs> = BigVersionedJsonStore::new(&path, 2);

    assert!(matches!(
        store.read(),
        Err(BigStorageError::UnknownVersion {
            found: 3,
            supported_up_to: 2
        })
    ));
    // Refused, and left exactly as it was: this build cannot promise to keep
    // fields it does not know, so it must not replace the file either.
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let _ = fs::remove_file(&path);
}

/// A document written by an older build is read, not treated as corruption.
///
/// Under an equality check an older file comes back as `UnknownVersion`,
/// which callers read as "this is broken". The terminal would rename the
/// person's saved sessions to `.broken` and carry on with an empty list, and
/// the next edit would publish that empty list as the real file.
#[test]
fn versioned_store_reads_older_document() {
    let path = storage_test_path("older-accepted");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "data": { "volume": 8, "theme": "ok" }
        }))
        .unwrap(),
    )
    .unwrap();
    let store: BigVersionedJsonStore<Prefs> = BigVersionedJsonStore::new(&path, 2);

    let read = store.read().expect("an older document is readable");
    assert_eq!(
        read,
        Some(Prefs {
            volume: 8,
            theme: "ok".to_owned()
        })
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn error_display_and_source() {
    use std::error::Error as _;
    // Display renders the wrapped message; source() exposes the cause for
    // wrapping variants and None for the version mismatch.
    let io = BigStorageError::Io(std::io::Error::other("boom"));
    assert!(
        io.to_string().contains("boom"),
        "Display must render the i/o cause"
    );
    assert!(io.source().is_some(), "Io must expose its source");

    let unknown = BigStorageError::UnknownVersion {
        found: 5,
        supported_up_to: 2,
    };
    let display_message = unknown.to_string();
    assert!(
        display_message.contains('5') && display_message.contains('2'),
        "got {display_message:?}"
    );
    assert!(
        unknown.source().is_none(),
        "version mismatch has no inner source"
    );
}

#[test]
fn read_surfaces_non_missing_errors() {
    // A read failure other than "file not found" (here the path is a
    // directory) must propagate as an error, never be swallowed as Ok(None).
    let dir = std::env::temp_dir().join(format!(
        "bigosk-isdir-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let store: BigAtomicJsonStore<Prefs> = BigAtomicJsonStore::new(&dir);
    assert!(
        store.read().is_err(),
        "reading a directory must error, not return None",
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn write_creates_missing_parent_dirs() {
    // Both the store and the free atomic_write must `mkdir -p` the parent
    // before writing the temp file.
    let base = std::env::temp_dir().join(format!(
        "bigosk-mkdir-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let nested = base.join("a").join("b").join("prefs.json");
    let store: BigAtomicJsonStore<Prefs> = BigAtomicJsonStore::new(&nested);
    store
        .write(&Prefs {
            volume: 7,
            theme: "z".into(),
        })
        .unwrap();
    assert_eq!(store.read().unwrap().unwrap().volume, 7);

    let raw = base.join("c").join("d").join("blob.bin");
    atomic_write(&raw, b"xyz").unwrap();
    assert_eq!(fs::read(&raw).unwrap(), b"xyz");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn versioned_reads_current_version() {
    // version == current must read straight through: pins exact-match
    // acceptance alongside the mismatch denial test.
    let path = storage_test_path("current-version");
    let envelope = serde_json::json!({
        "version": 2,
        "data": { "volume": 8, "theme": "ok" }
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
    let store: BigVersionedJsonStore<Prefs> = BigVersionedJsonStore::new(&path, 2);
    assert_eq!(store.read().unwrap().unwrap().volume, 8);
    let _ = fs::remove_file(&path);
}

#[test]
fn versioned_write_then_read_round_trips() {
    // store.write must actually persist: the by-hand-envelope tests never
    // call it. A no-op write would make the subsequent read return None.
    let path = storage_test_path("versioned-write");
    let store: BigVersionedJsonStore<Prefs> = BigVersionedJsonStore::new(&path, 2);
    store
        .write(&Prefs {
            volume: 71,
            theme: "wr".into(),
        })
        .unwrap();
    let loaded = store
        .read()
        .unwrap()
        .expect("written value must be readable");
    assert_eq!(loaded.volume, 71);
    assert_eq!(loaded.theme, "wr");
    let _ = fs::remove_file(&path);
}

/// A save killed at any point leaves the whole old document or the whole new
/// one, and a save into the folder a minute on removes the dead writer's copy.
#[cfg(target_os = "linux")]
#[test]
fn replaced_document_survives_kill() {
    const VERSIONS: [u8; 2] = *b"ab";
    const SIZE: usize = 128 << 10;
    if let Some(path) = std::env::var_os("BIG_OS_KIT_CHILD_SAVES") {
        for version in VERSIONS.iter().cycle() {
            atomic_replace_document(Path::new(&path), &vec![*version; SIZE], || true).unwrap();
        }
    }
    let dir = storage_test_path("killed-save");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("notes.txt");
    fs::write(&path, vec![VERSIONS[0]; SIZE]).unwrap();
    for kill in 0..50_u64 {
        let mut child = crate::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "storage::tests::replaced_document_survives_kill",
                "--quiet",
            ])
            .env("BIG_OS_KIT_CHILD_SAVES", &path)
            .build()
            .spawn()
            .expect("saving child");
        // Spread over the start, the copy, the sync and the rename.
        std::thread::sleep(std::time::Duration::from_millis(20 + kill * 37 % 180));
        child.kill().expect("kill");
        let _ = child.wait();
        let bytes = fs::read(&path).unwrap();
        assert!(
            bytes.len() == SIZE && VERSIONS.iter().any(|v| bytes.iter().all(|b| b == v)),
            "kill {kill}: a document of {} bytes, neither version whole",
            bytes.len()
        );
    }
    atomic_replace_document(&path, b"saved", || true).unwrap();
    let mut left: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(left.len() > 1, "no save was killed with its copy on disk");
    let minute_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(61);
    for name in &left {
        let file = fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap();
        file.set_modified(minute_ago).unwrap();
    }
    atomic_replace_document(&path, b"saved", || true).unwrap();
    left = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(left, ["notes.txt"]);
}

fn storage_test_dir(name: &str) -> PathBuf {
    let dir = storage_test_path(name);
    fs::create_dir(&dir).unwrap();
    dir
}

fn c_path(path: &Path) -> std::ffi::CString {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap()
}

#[test]
fn lock_refuses_symlinked_lock_file() {
    let dir = storage_test_dir("lock-link");
    let target = dir.join("tags.json");
    let elsewhere = dir.join("elsewhere");
    fs::write(&elsewhere, b"").unwrap();
    std::os::unix::fs::symlink(&elsewhere, target.with_extension("json.lock")).unwrap();

    assert!(BigStorageFileLock::acquire(&target).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn lock_refuses_non_regular_file() {
    let dir = storage_test_dir("lock-fifo");
    let target = dir.join("tags.json");
    let lock_path = c_path(&target.with_extension("json.lock"));
    // SAFETY: a live NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(lock_path.as_ptr(), 0o600) }, 0);

    let error = BigStorageFileLock::acquire(&target).err();

    assert_eq!(
        error.map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn lock_tightens_mode_only_once() {
    let dir = storage_test_dir("lock-mode");
    let target = dir.join("tags.json");
    let lock_path = target.with_extension("json.lock");
    fs::write(&lock_path, b"").unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();

    drop(BigStorageFileLock::acquire(&target).unwrap());
    let tightened = fs::metadata(&lock_path).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    drop(BigStorageFileLock::acquire(&target).unwrap());
    let again = fs::metadata(&lock_path).unwrap();

    assert_eq!(tightened.mode() & 0o7777, 0o600);
    assert_eq!(
        (again.ctime(), again.ctime_nsec()),
        (tightened.ctime(), tightened.ctime_nsec()),
        "a lock file with the right mode is left untouched"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A signal that lands while the lock is waited for does not end the wait.
#[test]
fn lock_wait_survives_signal() {
    extern "C" fn ignore(_: libc::c_int) {}
    let dir = storage_test_dir("lock-signal");
    let target = dir.join("tags.json");
    let held = BigStorageFileLock::acquire(&target).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "kept: a handler that only interrupts a blocked call"
    )]
    // SAFETY: a zeroed sigaction is valid; without SA_RESTART a blocked
    // flock returns EINTR when the handler runs.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = ignore as extern "C" fn(libc::c_int) as libc::sighandler_t;
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
            0
        );
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn({
        move || {
            // SAFETY: pthread_self has no preconditions.
            sender.send(unsafe { libc::pthread_self() }).unwrap();
            BigStorageFileLock::acquire(&target).map(drop)
        }
    });
    let thread = receiver.recv().unwrap();
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        // SAFETY: the waiter is joined below, so its handle is still valid.
        unsafe { libc::pthread_kill(thread, libc::SIGUSR1) };
    }
    drop(held);

    waiter
        .join()
        .unwrap()
        .expect("the wait goes on after a signal");
    let _ = fs::remove_dir_all(&dir);
}

/// Only a temporary whose writer is gone and that sat a minute is taken: a
/// PID that no process can have is gone, a sequence that is not a number is
/// not a temporary, and PID 1 exists even though this user may not signal it.
#[test]
fn dead_writer_rule_spares_others() {
    let dir = storage_test_dir("dead-temps");
    let minute_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(61);
    for name in [
        format!(".notes.txt.tmp.{}.3", i32::MAX),
        format!(".notes.txt.tmp.{}.x", i32::MAX),
        ".notes.txt.tmp.1.3".to_owned(),
    ] {
        fs::File::create(dir.join(name))
            .unwrap()
            .set_modified(minute_ago)
            .unwrap();
    }
    fs::File::create(dir.join(format!(".notes.txt.tmp.{}.4", i32::MAX))).unwrap();

    atomic_write(&dir.join("notes.txt"), b"saved").unwrap();

    let mut left: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            ".notes.txt.tmp.1.3".to_owned(),
            format!(".notes.txt.tmp.{}.4", i32::MAX),
            format!(".notes.txt.tmp.{}.x", i32::MAX),
            "notes.txt".to_owned(),
        ]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn replaced_document_keeps_group() {
    let dir = storage_test_dir("replace-group");
    let target = dir.join("shared.txt");
    fs::write(&target, b"old").unwrap();
    // SAFETY: getgroups fills at most `count` entries of a live buffer.
    let groups = unsafe {
        let count = libc::getgroups(0, std::ptr::null_mut());
        let mut groups = vec![0; usize::try_from(count).unwrap()];
        let filled = libc::getgroups(count, groups.as_mut_ptr());
        groups.truncate(usize::try_from(filled).unwrap());
        groups
    };
    // SAFETY: getegid has no preconditions.
    let new_file_group = unsafe { libc::getegid() };
    let dir_group = fs::metadata(&dir).unwrap().gid();
    let other = groups
        .into_iter()
        .find(|group| *group != new_file_group && *group != dir_group)
        .expect("the test user must belong to a second group");
    std::os::unix::fs::chown(&target, None, Some(other)).unwrap();

    assert!(atomic_replace_document(&target, b"new", || true).unwrap());

    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(fs::metadata(&target).unwrap().gid(), other);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn replaced_document_keeps_access_acl() {
    const ACL_USER_OBJ: u16 = 0x01;
    const ACL_USER: u16 = 0x02;
    const ACL_GROUP_OBJ: u16 = 0x04;
    const ACL_MASK: u16 = 0x10;
    const ACL_OTHER: u16 = 0x20;
    let entry = |tag: u16, perm: u16, id: u32| {
        [tag.to_le_bytes(), perm.to_le_bytes()]
            .concat()
            .into_iter()
            .chain(id.to_le_bytes())
            .collect::<Vec<_>>()
    };
    let dir = storage_test_dir("replace-acl");
    let target = dir.join("shared.txt");
    fs::write(&target, b"old").unwrap();
    let reader = crate::microdaemon::current_effective_user_id() + 1;
    let acl = [
        2_u32.to_le_bytes().to_vec(),
        entry(ACL_USER_OBJ, 6, u32::MAX),
        entry(ACL_USER, 4, reader),
        entry(ACL_GROUP_OBJ, 4, u32::MAX),
        entry(ACL_MASK, 4, u32::MAX),
        entry(ACL_OTHER, 0, u32::MAX),
    ]
    .concat();
    assert!(
        set_xattr(&target, "system.posix_acl_access", &acl),
        "the test folder must take access ACLs"
    );
    let before = document_attributes(&target).unwrap();

    assert!(atomic_replace_document(&target, b"new", || true).unwrap());

    assert_eq!(document_attributes(&target).unwrap(), before);
    assert!(
        before
            .iter()
            .any(|(name, _)| name.as_bytes() == b"system.posix_acl_access")
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The attributes grew between the size query and the read: ask again.
#[test]
fn attribute_read_retries_grown_list() {
    let (queries, fills) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
    let value = xattr_buffer(
        || {
            queries.set(queries.get() + 1);
            Ok(4)
        },
        |buffer| {
            fills.set(fills.get() + 1);
            if fills.get() == 1 {
                return Err(rustix::io::Errno::RANGE);
            }
            assert_eq!(buffer.len(), 4);
            buffer.copy_from_slice(b"abcd");
            Ok(4)
        },
    )
    .unwrap();

    assert_eq!(value, b"abcd");
    assert_eq!((queries.get(), fills.get()), (2, 2));
}

#[test]
fn missing_attribute_reads_as_none() {
    let dir = storage_test_dir("attribute-missing");
    let file = dir.join("plain");
    fs::write(&file, b"").unwrap();

    assert_eq!(
        attribute_value(&c_path(&file), c"user.missing").unwrap(),
        None
    );
    assert!(attribute_value(&c_path(&dir.join("gone")), c"user.missing").is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn target_mode_reports_unreadable_path() {
    let dir = storage_test_dir("target-mode");
    let file = dir.join("plain");
    fs::write(&file, b"").unwrap();

    assert_eq!(existing_target_mode(&dir.join("missing")).unwrap(), None);
    assert!(existing_target_mode(&file.join("child")).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn private_creation_needs_parent() {
    let dir = storage_test_dir("create-no-parent");

    let error = atomic_create_private(&dir.join("missing").join("doc"), b"x").err();

    assert_eq!(
        error.map(|error| error.kind()),
        Some(std::io::ErrorKind::NotFound)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parent_sync_reports_missing_directory() {
    let dir = storage_test_dir("sync-missing");
    assert!(sync_parent_directory(&dir.join("missing").join("doc")).is_err());
    let _ = fs::remove_dir_all(&dir);
}

/// Without `renameat2`, as on an older kernel or some network mounts, a new
/// document is published by a hard link instead.
#[test]
fn creation_without_renameat2_links() {
    let dir = storage_test_dir("no-renameat2");
    let path = dir.join("doc");

    let created = std::thread::spawn({
        let path = path.clone();
        move || {
            refuse_on_this_thread(libc::SYS_renameat2, libc::ENOSYS);
            atomic_create_private(&path, b"linked")
        }
    })
    .join()
    .unwrap();

    created.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"linked");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    let _ = fs::remove_dir_all(&dir);
}

/// Make `syscall` fail with `errno` on the calling thread and the threads it
/// starts, through a seccomp filter.
pub(crate) fn refuse_on_this_thread(syscall: libc::c_long, errno: libc::c_int) {
    use libc::{BPF_ABS, BPF_JEQ, BPF_JMP, BPF_K, BPF_LD, BPF_RET, BPF_W, sock_filter};
    let statement = |code: u32, k: u32| sock_filter {
        code: u16::try_from(code).unwrap(),
        jt: 0,
        jf: 0,
        k,
    };
    let filter = [
        statement(
            BPF_LD | BPF_W | BPF_ABS,
            u32::try_from(std::mem::offset_of!(libc::seccomp_data, nr)).unwrap(),
        ),
        sock_filter {
            jf: 1,
            ..statement(BPF_JMP | BPF_JEQ | BPF_K, u32::try_from(syscall).unwrap())
        },
        statement(
            BPF_RET | BPF_K,
            libc::SECCOMP_RET_ERRNO | u32::try_from(errno).unwrap(),
        ),
        statement(BPF_RET | BPF_K, libc::SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: u16::try_from(filter.len()).unwrap(),
        filter: filter.as_ptr().cast_mut(),
    };
    let (one, zero) = (libc::c_ulong::from(1_u8), libc::c_ulong::from(0_u8));
    // SAFETY: the variadic arguments are full-width; `program` points at
    // `filter`, both alive for the call, and the kernel copies the filter.
    unsafe {
        assert_eq!(
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, one, zero, zero, zero),
            0
        );
        assert_eq!(
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::c_ulong::from(libc::SECCOMP_MODE_FILTER),
                &raw const program
            ),
            0
        );
    }
}

#[test]
fn try_lock_refuses_while_held() {
    let dir = storage_test_dir("lock-try");
    let target = dir.join("recent.jsonl");
    let held = BigStorageFileLock::acquire(&target).unwrap();

    let refused = BigStorageFileLock::try_acquire(&target).err();
    drop(held);

    assert_eq!(
        refused.map(|error| error.kind()),
        Some(std::io::ErrorKind::WouldBlock)
    );
    assert!(BigStorageFileLock::try_acquire(&target).is_ok());
    let _ = fs::remove_dir_all(&dir);
}
