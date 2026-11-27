// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "the storage wrapper owns blocking filesystem persistence"
)]

//! Atomic JSON persistence primitives for BigLinux apps.
//!
//! `BigAtomicJsonStore` writes through a `.<name>.tmp.<pid>.<seq>` sibling
//! file and renames it into place. `rename(2)` is atomic on the same
//! filesystem, so a reader never sees half a document; a partial write from a
//! crash stays in the temporary file until a write of the same file, a minute
//! or more after, removes it.
//!
//! Nothing here waits for the disk unless its name says `durable`: these stores
//! hold program state, and an `fsync` per write stalls the machine to save
//! seconds of it. A document replaced in place ([`atomic_replace_document`]), a
//! new document ([`atomic_create_document`], [`atomic_create_private`]) and the
//! callers of [`atomic_write_private_durable`] wait.
//!
//! `BigVersionedJsonStore` layers a `version` field on top: it reads its own
//! version and older ones, and refuses a newer one without rewriting it.
//!
//! # Examples
//!
//! ```
//! use big_os_kit::storage::BigAtomicJsonStore;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
//! struct Prefs { volume: u8 }
//!
//! let mut path = std::env::temp_dir();
//! path.push(format!("big-os-kit-storage-doctest-{}.json", std::process::id()));
//! let store: BigAtomicJsonStore<Prefs> = BigAtomicJsonStore::new(&path);
//! store.write(&Prefs { volume: 50 }).unwrap();
//! let loaded = store.read().unwrap().unwrap();
//! assert_eq!(loaded.volume, 50);
//! let _ = std::fs::remove_file(&path);
//! ```

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::marker::PhantomData;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{
    Gid, XattrFlags, fchown, fremovexattr, fsetxattr, getxattr, getxattr_query, listxattr,
    listxattr_query,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Owner-controlled advisory lock for serializing one durable document across processes.
pub struct BigStorageFileLock {
    /// Held for its lock: dropping it closes the file, which releases the lock.
    _file: fs::File,
}

/// The lock file beside `target`: `a.json.lock` for `a.json`, so `a.toml`
/// beside it takes a lock of its own.
fn lock_path(target: &Path) -> PathBuf {
    target.with_added_extension("lock")
}

impl BigStorageFileLock {
    /// Acquire a blocking exclusive lock on a private sibling lock file.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the parent, lock file, ownership check, or
    /// advisory lock cannot be established.
    pub fn acquire(target: &Path) -> std::io::Result<Self> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let file = crate::file_io::open_lock_file(&lock_path(target))?;
        loop {
            match file.lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Take the same lock without waiting for another writer: `WouldBlock`
    /// asks the caller to try later, so a GTK thread never freezes on it.
    ///
    /// # Errors
    ///
    /// As [`Self::acquire`], and `WouldBlock` while another writer holds it.
    pub fn try_acquire(target: &Path) -> std::io::Result<Self> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let file = crate::file_io::open_lock_file(&lock_path(target))?;
        // A child forked by another thread shares the lock until it execs, a
        // window of microseconds; a few short tries outlast it, while a real
        // writer still holds the lock after them.
        for attempt in 0.. {
            match file.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) if attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self { _file: file })
    }
}

use crate::file_io::{BigBoundedReadError, read_bounded};

/// Maximum accepted size for a persisted JSON document: 16 MiB.
///
/// Settings, session, and history stores are control-plane documents rather
/// than media payloads. Bounding them prevents corrupt or attacker-controlled
/// files from causing unbounded allocation during startup.
pub const MAX_STORAGE_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;

/// Failure modes when reading or writing.
#[derive(Debug)]
pub enum BigStorageError {
    /// Underlying I/O failure (open, read, write, rename).
    Io(std::io::Error),
    /// JSON parse/serialize error from `serde_json`.
    Json(serde_json::Error),
    /// The persisted document exceeded [`MAX_STORAGE_DOCUMENT_BYTES`].
    TooLarge {
        /// Maximum accepted byte count.
        max_bytes: u64,
    },
    /// Versioned payload written by a newer version than the store's.
    UnknownVersion {
        /// Version stamp found in the payload.
        found: u32,
        /// Highest version this binary knows how to read.
        supported_up_to: u32,
    },
}

impl std::fmt::Display for BigStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "storage i/o error: {e}"),
            Self::Json(e) => write!(f, "storage json error: {e}"),
            Self::TooLarge { max_bytes } => {
                write!(f, "storage document exceeds {max_bytes} bytes")
            }
            Self::UnknownVersion {
                found,
                supported_up_to,
            } => write!(
                f,
                "stored schema version {found} does not match supported {supported_up_to}"
            ),
        }
    }
}

impl std::error::Error for BigStorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::TooLarge { .. } => None,
            Self::UnknownVersion { .. } => None,
        }
    }
}

impl From<std::io::Error> for BigStorageError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for BigStorageError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Atomic JSON read/write keyed by file path.
pub struct BigAtomicJsonStore<T> {
    path: PathBuf,
    _marker: PhantomData<T>,
}

impl<T> Clone for BigAtomicJsonStore<T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> BigAtomicJsonStore<T> {
    /// Construct a store backed by `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            _marker: PhantomData,
        }
    }

    /// Return the target path backing this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<T: Serialize + DeserializeOwned> BigAtomicJsonStore<T> {
    /// Read and deserialize. Returns `Ok(None)` when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`BigStorageError::Io`] for filesystem read failures (other than missing
    /// file, which yields `Ok(None)`), [`BigStorageError::TooLarge`] when the document
    /// exceeds [`MAX_STORAGE_DOCUMENT_BYTES`], or [`BigStorageError::Json`] when parsing
    /// fails.
    pub fn read(&self) -> Result<Option<T>, BigStorageError> {
        match read_bounded(&self.path, MAX_STORAGE_DOCUMENT_BYTES) {
            Ok(bytes) => {
                let value: T = serde_json::from_slice(&bytes)?;
                Ok(Some(value))
            }
            Err(BigBoundedReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(BigBoundedReadError::Io(error)) => Err(BigStorageError::Io(error)),
            Err(BigBoundedReadError::TooLarge { max_bytes }) => {
                Err(BigStorageError::TooLarge { max_bytes })
            }
        }
    }

    /// Serialize and atomically replace the target file, 0600, without waiting
    /// for the disk (see the module note).
    ///
    /// # Errors
    ///
    /// Returns [`BigStorageError::Io`] for filesystem failures (mkdir, write or
    /// rename), [`BigStorageError::Json`] when serialization fails, or
    /// [`BigStorageError::TooLarge`], leaving the file as it was, when the
    /// document would be larger than [`Self::read`] accepts.
    pub fn write(&self, value: &T) -> Result<(), BigStorageError> {
        // Serialized first and written once: straight into the file, serde
        // issues a `write` per token, thousands for a small document.
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_STORAGE_DOCUMENT_BYTES {
            return Err(BigStorageError::TooLarge {
                max_bytes: MAX_STORAGE_DOCUMENT_BYTES,
            });
        }
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let mut temporary_store = PendingAtomicWrite::create_sibling(&self.path, Some(0o600))?;
        temporary_store.file_mut()?.write_all(&bytes)?;
        temporary_store.install(&self.path)?;
        Ok(())
    }
}

/// Remove what writers of `target` killed mid-write left beside it: the
/// temporaries of `atomic_sibling_temp_path` whose process is gone, owned by
/// this user, untouched for `DEAD_TEMP_AGE`, in a folder on a local mount. One
/// listing of the folder per write, under 2 ms for 10,000 files.
fn remove_dead_writers_temps(target: &Path) {
    // A PID names no process here when its writer runs in another PID
    // namespace; a live writer touches its temporary more often than this.
    const DEAD_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(60);
    let Some(name) = target.file_name() else {
        return;
    };
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // On a share, the writer may run on another host.
    if !crate::mount_table::is_local(&crate::mount_table::mountinfo(), parent) {
        return;
    }
    let prefix = format!(".{}.tmp.", name.to_string_lossy());
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some((pid, seq)) = file_name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|rest| rest.split_once('.'))
        else {
            continue;
        };
        let Ok(pid) = pid.parse::<libc::pid_t>() else {
            continue;
        };
        if pid <= 0 || !seq.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        // Signal 0 only asks whether the process exists.
        let gone = rustix::process::Pid::from_raw(pid).is_some_and(|pid| {
            rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH)
        });
        let ours = entry.metadata().is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.uid() == crate::microdaemon::current_effective_user_id()
                && metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age >= DEAD_TEMP_AGE)
        });
        if gone && ours {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn atomic_sibling_temp_path(target: &Path) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "store".to_string());
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{name}.tmp.{pid}.{seq}"))
}

/// Atomically write `bytes` to `path`: write a uniquely-named sibling temp file,
/// then rename it over the target. A crash mid-write leaves the previous
/// contents intact (a plain `fs::write` truncates first). It does not wait for
/// the disk. The temp file
/// is created in the target's parent directory so the rename stays within one
/// filesystem (a cross-filesystem rename fails). Creates the parent dir if
/// missing.
///
/// GTK-free and blocking: offload it (e.g. via
/// `big_relm4_components::task::spawn_blocking_result`) so a slow/network
/// filesystem can't stall the UI thread.
///
/// Existing Unix permission bits are copied to the replacement inode; new files
/// use the process umask. Hard links to an old inode remain on the old contents.
/// Use [`atomic_write_private`] for control-plane files that must always be 0600.
///
/// A symbolic link is written through: the file it points at is replaced and
/// the link stays, as a dotfile manager (stow, chezmoi) expects of a
/// document kept in its tree. The private writers never follow one.
///
/// # Errors
/// Returns the underlying [`std::io::Error`] from the dir create, temp write
/// or rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let linked = fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    let target = if linked {
        // A link to nothing is replaced by the document.
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    atomic_write_with_mode(&target, bytes, existing_target_mode(&target)?, false)
}

/// Atomically write a private control-plane document with Unix mode 0600.
///
/// Use this for settings, credentials, session state, and other data that must
/// not inherit a broader mode from an existing target. Like [`atomic_write`], it
/// does not wait for the disk.
///
/// # Errors
///
/// The I/O error of creating, writing or renaming the temporary file beside
/// `path`.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    atomic_write_with_mode(path, bytes, Some(0o600), false)
}

/// [`atomic_write_private`], then wait until the file and its directory entry
/// are on the disk: for a private file a power cut would take for good, such as
/// a recovery journal whose order matters or text nobody has saved yet. Each
/// call costs two `fsync`s, which on a busy drive is hundreds of milliseconds;
/// settings and caches never pay it.
///
/// # Errors
/// As [`atomic_write_private`], plus the file and parent-directory `fsync`.
pub fn atomic_write_private_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    atomic_write_with_mode(path, bytes, Some(0o600), true)
}

/// Publish a complete private file only when the destination does not exist.
/// On Linux, `renameat2(RENAME_NOREPLACE)` publishes without requiring hard links
/// (including filesystems such as FAT when the kernel/filesystem supports it).
/// An unsupported no-replace syscall falls back to same-filesystem hard-link
/// publication, never to an overwriting rename. A racing destination is preserved.
/// Parent directories are not created implicitly. Waits until the file and its
/// directory entry are on the disk, so an error after publication can mean the
/// complete file exists but directory durability was not confirmed.
///
/// # Errors
/// Returns AlreadyExists rather than replacing another file, or the I/O failure.
pub fn atomic_create_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    atomic_create(path, bytes, 0o600)
}

/// Publish a new document only when the destination does not exist, with the
/// mode any program gives a new file: `0666` less the process umask.
///
/// Same publication and disk rules as [`atomic_create_private`]; use that one
/// for files only this user may read.
///
/// # Errors
/// Returns AlreadyExists rather than replacing another file, or the I/O failure.
pub fn atomic_create_document(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    atomic_create(path, bytes, 0o666)
}

/// [`atomic_create_private`] and [`atomic_create_document`], the new file made
/// with `mode` less the umask.
fn atomic_create(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut pending = PendingAtomicWrite::create_sibling(path, Some(mode))?;
    let file = pending.file_mut()?;
    file.write_all(bytes)?;
    file.sync_all()?;
    pending.install_new(path)?;
    drop(pending);
    sync_parent_directory(path)
}

/// Replace an existing document in one step, keeping what describes it: its
/// mode, its group, its `user.*` extended attributes (a browser's download
/// origin, desktop tags) and its access ACL. A link is written through, to the
/// file it names, and stays a link.
///
/// Waits until the copy and its directory entry are on the disk: for documents
/// the person saved. `still_current` runs after the new copy is complete and
/// right before the rename; when it answers `false` nothing is published and
/// this returns `Ok(false)`. A file owned by another user, or one carrying
/// attributes an unprivileged writer cannot set again (`security.*`,
/// `trusted.*`), or one whose group this user is not in, is refused with
/// [`std::io::ErrorKind::Unsupported`] and left as it was; anything but a
/// regular file (a folder, a FIFO, a device) with
/// [`std::io::ErrorKind::InvalidInput`].
///
/// # Errors
/// Returns the refusal above or the I/O failure; the original is untouched
/// unless the result is `Ok(true)`.
#[cfg(target_os = "linux")]
pub fn atomic_replace_document(
    path: &Path,
    bytes: &[u8],
    still_current: impl FnOnce() -> bool,
) -> std::io::Result<bool> {
    let path = &written_through(path)?;
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only a regular file is replaced",
        ));
    }
    if metadata.uid() != crate::microdaemon::current_effective_user_id() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the file belongs to another user",
        ));
    }
    let attributes = document_attributes(path)?;
    let mut pending = PendingAtomicWrite::create_sibling(path, Some(0o600))?;
    let file = pending.file_mut()?;
    file.write_all(bytes)?;
    if metadata.gid() != file.metadata()?.gid() {
        fchown(&*file, None, Some(Gid::from_raw(metadata.gid()))).map_err(|error| {
            if error == rustix::io::Errno::PERM {
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "the file's group is not one of this user's",
                )
            } else {
                error.into()
            }
        })?;
    }
    for (name, value) in &attributes {
        fsetxattr(&*file, name, value, XattrFlags::empty())?;
    }
    // A folder's default ACL gives the new copy an access ACL the original
    // did not have, which can let another user read it.
    if !attributes
        .iter()
        .any(|(name, _)| name.as_bytes() == b"system.posix_acl_access")
    {
        match fremovexattr(&*file, c"system.posix_acl_access") {
            Ok(()) | Err(rustix::io::Errno::NODATA | rustix::io::Errno::NOTSUP) => {}
            Err(error) => return Err(error.into()),
        }
    }
    // After the ACL: setting an access ACL rewrites the group bits of the mode.
    file.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o7777))?;
    file.sync_all()?;
    if !still_current() {
        return Ok(false);
    }
    pending.install(path)?;
    sync_parent_directory(path)?;
    Ok(true)
}

/// Save a document under the name the person chose: a new file as
/// [`atomic_create_document`] makes it, an existing one replaced as
/// [`atomic_replace_document`] does. A link is written through, to the file it
/// names, and stays a link.
///
/// # Errors
/// Returns the refusals of those two, a link that names nothing, or the I/O
/// failure.
#[cfg(target_os = "linux")]
pub fn atomic_save_document(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = written_through(path)?;
    // Replace a file seen to exist at once: creating it first would write the
    // whole document twice. A file made after this look still comes back as
    // `AlreadyExists`.
    if fs::symlink_metadata(&target).is_ok() {
        return atomic_replace_document(&target, bytes, || true).map(drop);
    }
    match atomic_create_document(&target, bytes) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            atomic_replace_document(&target, bytes, || true).map(drop)
        }
        written => written,
    }
}

/// The file a document named `path` is written to: the one a link names.
#[cfg(target_os = "linux")]
fn written_through(path: &Path) -> std::io::Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_symlink()) {
        fs::canonicalize(path)
    } else {
        Ok(path.to_path_buf())
    }
}

/// The extended attributes of `path` a replacement must carry, name and value.
#[cfg(target_os = "linux")]
fn document_attributes(path: &Path) -> std::io::Result<Vec<(std::ffi::CString, Vec<u8>)>> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt as _;

    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let names = xattr_buffer(|| listxattr_query(&path), |buffer| listxattr(&path, buffer))?;
    let mut kept = Vec::new();
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = CStr::from_bytes_with_nul(&[name, &[0]].concat())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?
            .to_owned();
        let bytes = name.to_bytes();
        if !(bytes.starts_with(b"user.") || bytes == b"system.posix_acl_access") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!(
                    "the attribute {} cannot be kept without privileges",
                    String::from_utf8_lossy(bytes)
                ),
            ));
        }
        if let Some(value) = attribute_value(&path, &name)? {
            kept.push((name, value));
        }
    }
    Ok(kept)
}

/// The value of the attribute `name` of `path`; `None` once it is gone, as
/// when it was removed between the list and the read.
#[cfg(target_os = "linux")]
fn attribute_value(
    path: &std::ffi::CStr,
    name: &std::ffi::CStr,
) -> std::io::Result<Option<Vec<u8>>> {
    let value = xattr_buffer(
        || getxattr_query(path, name),
        |buffer| getxattr(path, name, buffer),
    );
    match value {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::NODATA.raw_os_error()) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Ask an xattr call its size with `query`, then `fill` a buffer that large,
/// again while the attributes grow between the two.
#[cfg(target_os = "linux")]
fn xattr_buffer(
    query: impl Fn() -> rustix::io::Result<usize>,
    fill: impl Fn(&mut [u8]) -> rustix::io::Result<usize>,
) -> std::io::Result<Vec<u8>> {
    loop {
        let size = match query() {
            Ok(size) => size,
            Err(rustix::io::Errno::NOTSUP) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut buffer = vec![0_u8; size];
        match fill(&mut buffer) {
            Ok(filled) => {
                buffer.truncate(filled);
                return Ok(buffer);
            }
            Err(rustix::io::Errno::RANGE) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn atomic_write_with_mode(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    durable: bool,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let mut temporary_write = PendingAtomicWrite::create_sibling(path, mode)?;
    {
        let file = temporary_write.file_mut()?;
        // The umask narrowed the mode given to open.
        if let Some(mode) = mode {
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        file.write_all(bytes)?;
        if durable {
            file.sync_all()?;
        }
    }
    temporary_write.install(path)?;
    if durable {
        sync_parent_directory(path)?;
    }
    Ok(())
}

fn existing_target_mode(path: &Path) -> std::io::Result<Option<u32>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.permissions().mode() & 0o7777)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

struct PendingAtomicWrite {
    path: PathBuf,
    file: Option<fs::File>,
}

impl PendingAtomicWrite {
    fn create_sibling(target: &Path, mode: Option<u32>) -> std::io::Result<Self> {
        remove_dead_writers_temps(target);
        Self::create_sibling_with(mode, || atomic_sibling_temp_path(target))
    }

    fn create_sibling_with(
        mode: Option<u32>,
        mut next_path: impl FnMut() -> PathBuf,
    ) -> std::io::Result<Self> {
        const MAX_TEMP_ATTEMPTS: usize = 128;
        for _ in 0..MAX_TEMP_ATTEMPTS {
            let path = next_path();
            match Self::create(path, mode) {
                Ok(pending) => return Ok(pending),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique sibling temporary file",
        ))
    }

    fn create(path: PathBuf, mode: Option<u32>) -> std::io::Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        if let Some(mode) = mode {
            options.mode(mode);
        }
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }

    fn file_mut(&mut self) -> std::io::Result<&mut fs::File> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("pending atomic write no longer owns its file"))
    }

    fn install_new(&mut self, target: &Path) -> std::io::Result<()> {
        self.file.take();
        crate::file_io::rename_no_replace(&self.path, target)?;
        self.path.clear();
        Ok(())
    }

    fn install(&mut self, target: &Path) -> std::io::Result<()> {
        self.file.take();
        #[expect(clippy::disallowed_methods, reason = "the atomic writers' one rename")]
        fs::rename(&self.path, target)?;
        self.path.clear();
        Ok(())
    }
}

impl Drop for PendingAtomicWrite {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    fs::File::open(parent.unwrap_or_else(|| Path::new(".")))?.sync_all()
}

/// Versioned wrapper. Stores `{ "version": N, "data": ... }`.
pub struct BigVersionedJsonStore<T> {
    inner: BigAtomicJsonStore<serde_json::Value>,
    current_version: u32,
    _marker: PhantomData<T>,
}

impl<T> Clone for BigVersionedJsonStore<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            current_version: self.current_version,
            _marker: PhantomData,
        }
    }
}

/// A key beside `version` and `data` is ignored, so a newer build's envelope still reads.
#[derive(Serialize, Deserialize, Debug)]
struct VersionedEnvelope {
    version: u32,
    data: serde_json::Value,
}

impl<T: Serialize + DeserializeOwned> BigVersionedJsonStore<T> {
    /// Construct a store that writes `current_version` and reads it and older
    /// versions; a newer one is refused without rewriting the source bytes.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, current_version: u32) -> Self {
        Self {
            inner: BigAtomicJsonStore::new(path),
            current_version,
            _marker: PhantomData,
        }
    }

    /// Return the target path backing this versioned store.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::storage::BigVersionedJsonStore;
    ///
    /// let path = std::env::temp_dir().join("big-versioned-path-doc.json");
    /// let store: BigVersionedJsonStore<serde_json::Value> =
    ///     BigVersionedJsonStore::new(&path, 1);
    /// assert_eq!(store.path(), path.as_path());
    /// ```
    #[must_use]
    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Read a payload written by this version or an older one.
    /// Returns `Ok(None)` when the file does not exist.
    ///
    /// An older document reads, since callers take a refusal for corruption. A
    /// newer one is refused: this build cannot keep fields it does not know
    /// when it writes the file back, so a caller leaves that file alone.
    ///
    /// # Errors
    ///
    /// Returns [`BigStorageError::Io`] on read failures, [`BigStorageError::TooLarge`]
    /// for oversized documents, [`BigStorageError::Json`] on parse failures, or
    /// [`BigStorageError::UnknownVersion`] when the document was written by a
    /// newer version than this build knows.
    pub fn read(&self) -> Result<Option<T>, BigStorageError> {
        let Some(raw) = self.inner.read()? else {
            return Ok(None);
        };
        let envelope: VersionedEnvelope = serde_json::from_value(raw)?;
        if envelope.version > self.current_version {
            return Err(BigStorageError::UnknownVersion {
                found: envelope.version,
                supported_up_to: self.current_version,
            });
        }
        let value: T = serde_json::from_value(envelope.data)?;
        Ok(Some(value))
    }

    /// Serialize with the current version label.
    ///
    /// # Errors
    ///
    /// Returns [`BigStorageError::Json`] on serialization failure or
    /// [`BigStorageError::Io`] on filesystem failure.
    pub fn write(&self, value: &T) -> Result<(), BigStorageError> {
        let envelope = VersionedEnvelope {
            version: self.current_version,
            data: serde_json::to_value(value)?,
        };
        let raw = serde_json::to_value(&envelope)?;
        self.inner.write(&raw)
    }
}

#[cfg(test)]
pub(crate) mod tests;
