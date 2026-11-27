// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "the file_io wrapper owns blocking filesystem primitives"
)]

//! GTK-free filesystem helpers for UI/app crates that must not call `std::fs`
//! directly.

use std::ffi::CString;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Take};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{Advice, AtFlags, Mode, OFlags, fadvise, listxattr_query};

/// Open a regular file for verification without following a final symlink.
///
/// The nonblocking flag prevents a substituted FIFO from hanging during open;
/// descriptor metadata rejects other non-regular objects. This does not promise
/// nonblocking disk/network I/O, nor protect mutable parent path components.
/// Call from the existing blocking worker, never the GTK thread.
///
/// # Errors
/// Returns the open/stat error, or InvalidInput for a non-regular file.
pub fn open_regular_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    Ok(file)
}

/// Open `path` as one of this user's lock files, made if missing: a regular
/// file with one link, owned by the effective user, mode 0600 (set when it
/// differs, so a correct file's inode is never dirtied). A symlink is not
/// followed and a FIFO is not waited on. The caller takes the lock.
///
/// # Errors
///
/// The open or stat error, or `PermissionDenied` for any other kind of file.
pub fn open_lock_file(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != crate::microdaemon::current_effective_user_id()
        || metadata.nlink() != 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "a lock file is a regular file with one link, owned by this user",
        ));
    }
    if metadata.mode() & 0o7777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Tell the kernel `file` is read a little at a time: no readahead. On a
/// network mount, readahead turns a 72 KiB head read into about 450 KiB from
/// the server. Advice only: a filesystem that ignores it reads as before.
pub fn read_without_readahead(file: &File) {
    let _ = fadvise(file, 0, None, Advice::Random);
}

const MAX_DIRECTORY_ENTRY_PATHS: usize = 16_384;
static PRIVATE_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Process-owned private staging directory removed recursively on drop.
///
/// Creation is atomic and never adopts an existing path. This is appropriate
/// for bounded temporary output beside a final destination; callers must move
/// durable results out before this guard is dropped.
///
/// The name carries the owner's process id, and the guard holds a lock on the
/// directory. A process killed before its drop leaves its directory behind,
/// and the next creation with the same prefix in the same parent removes it
/// once it has gone an hour unchanged with no lock held.
pub struct OwnedPrivateDirectory {
    path: PathBuf,
    /// The directory, locked until the guard drops: a lock any PID namespace
    /// sees, where the process id in the name names no process.
    _lease: File,
}

impl OwnedPrivateDirectory {
    /// Create a uniquely named mode-`0700` child below `parent`.
    ///
    /// # Errors
    ///
    /// Returns the underlying filesystem error or `AlreadyExists` after the
    /// bounded collision budget is exhausted.
    pub fn create(parent: &Path, prefix: &str) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        // A NUL is refused by the path syscall itself, with InvalidInput.
        if prefix.is_empty() || prefix.contains('/') || prefix.contains('\\') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "private directory prefix is invalid",
            ));
        }
        remove_abandoned(parent, prefix, &crate::mount_table::mountinfo());
        for _ in 0..128 {
            let sequence = PRIVATE_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{prefix}-{}-{sequence}", std::process::id()));
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&path) {
                Ok(()) => {
                    let lease = fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                        .open(&path)
                        .and_then(|lease| lease.lock().map(|()| lease));
                    return match lease {
                        Ok(lease) => Ok(Self {
                            path,
                            _lease: lease,
                        }),
                        Err(error) => {
                            let _ = fs::remove_dir(&path);
                            Err(error)
                        }
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate private directory",
        ))
    }

    /// Absolute or caller-relative path created and owned by this guard.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OwnedPrivateDirectory {
    fn drop(&mut self) {
        let _ = remove_tree(&self.path);
    }
}

/// Remove the folder `path` and everything in it, for a folder a confined
/// program wrote: each folder is opened from its parent's descriptor without
/// following a link and given mode `0700` first, so a folder it closed to
/// everyone, or nested past the kernel's path length, still goes. The walk
/// climbs back through `..` and stops where that is not the folder it left,
/// or where a folder is on another filesystem: one moved during the walk
/// stops the removal, never moves it outside `path`.
///
/// # Errors
///
/// The first folder that cannot be opened, made the owner's or removed, an
/// entry that cannot be unlinked, or a folder moved during the walk.
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    /// A folder being emptied: its name in its parent, its device and inode,
    /// and the folders in it still to remove.
    struct Level {
        name: Option<CString>,
        identity: (u64, u64),
        folders: Vec<CString>,
    }
    let identity = |folder: &std::os::fd::OwnedFd| {
        rustix::fs::fstat(folder).map(|stat| (stat.st_dev, stat.st_ino))
    };
    let moved = || std::io::Error::other("a folder moved while its tree was removed");
    let mut current = open_owned_folder(rustix::fs::CWD, path, None)?;
    let root = identity(&current)?;
    let mut levels = vec![Level {
        name: None,
        identity: root,
        folders: clear_files(&current)?,
    }];
    while let Some(level) = levels.last_mut() {
        if let Some(child) = level.folders.pop() {
            current = open_owned_folder(&current, child.as_c_str(), Some(root.0))?;
            let child_identity = identity(&current)?;
            let folders = clear_files(&current)?;
            levels.push(Level {
                name: Some(child),
                identity: child_identity,
                folders,
            });
        } else if let Some(Level {
            name: Some(name), ..
        }) = levels.pop()
        {
            current = rustix::fs::openat(
                &current,
                c"..",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            if levels.last().map(|parent| parent.identity) != Some(identity(&current)?) {
                return Err(moved());
            }
            rustix::fs::unlinkat(&current, name.as_c_str(), AtFlags::REMOVEDIR)?;
        }
    }
    drop(current);
    fs::remove_dir(path)
}

/// Open the folder `name` under `parent` for listing, as its owner's with
/// mode `0700`; a link is refused, and so is a folder off `device` before its
/// mode changes.
fn open_owned_folder<Fd: std::os::fd::AsFd, P: rustix::path::Arg>(
    parent: Fd,
    name: P,
    device: Option<u64>,
) -> std::io::Result<std::os::fd::OwnedFd> {
    let place = rustix::fs::openat(
        parent,
        name,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    if let Some(device) = device
        && rustix::fs::fstat(&place)?.st_dev != device
    {
        return Err(std::io::Error::other(
            "a folder moved while its tree was removed",
        ));
    }
    // Through the descriptor, so the folder opened is the one whose mode changed.
    let named = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&place));
    rustix::fs::chmod(named.as_str(), Mode::RWXU)?;
    Ok(rustix::fs::open(
        named.as_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// Unlink every entry of `folder` that is not a folder, links included, and
/// return the names of the folders in it.
fn clear_files(folder: &std::os::fd::OwnedFd) -> std::io::Result<Vec<CString>> {
    let mut folders = Vec::new();
    for entry in rustix::fs::Dir::read_from(folder)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let kind = match entry.file_type() {
            rustix::fs::FileType::Unknown => rustix::fs::FileType::from_raw_mode(
                rustix::fs::statat(folder, name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode,
            ),
            known => known,
        };
        if kind == rustix::fs::FileType::Directory {
            folders.push(name.to_owned());
        } else {
            rustix::fs::unlinkat(folder, name, AtFlags::empty())?;
        }
    }
    Ok(folders)
}

/// Remove the [`OwnedPrivateDirectory`] folders `{prefix}-{pid}-{sequence}` of
/// `parent` whose process id names no process, unlocked and an hour unchanged
/// (the hour covers creation before the lock). Never on a share, whose owner
/// may run elsewhere, nor where `mount_table` (mountinfo) lacks `parent`.
fn remove_abandoned(parent: &Path, prefix: &str, mount_table: &str) {
    const ABANDONED_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    if !crate::mount_table::is_local(mount_table, parent) {
        return;
    }
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix(prefix)?.strip_prefix('-'))
            .and_then(|rest| rest.split_once('-'))
            .filter(|(pid, sequence)| {
                !pid.is_empty()
                    && pid.bytes().all(|byte| byte.is_ascii_digit())
                    && !sequence.is_empty()
                    && sequence.bytes().all(|byte| byte.is_ascii_digit())
            })
            .and_then(|(pid, _)| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() || Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let unchanged = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= ABANDONED_AGE);
        if metadata.is_dir()
            && unchanged
            && metadata.uid() == crate::microdaemon::current_effective_user_id()
            && metadata.permissions().mode() & 0o7777 == 0o700
        {
            // Held while the tree goes, so a second cleaner leaves it alone.
            let Ok(lease) = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(entry.path())
            else {
                continue;
            };
            if lease.try_lock().is_err() {
                continue;
            }
            // A confined program may have left folders it took the modes off.
            let _ = remove_tree(&entry.path());
        }
    }
}

/// Run one operation inside a new mode-`0700` directory that is always removed.
///
/// The directory is created atomically below `parent` and never adopts an
/// existing path. `operation` may move completed output elsewhere; every item
/// left behind is recursively removed before this function returns.
///
/// # Errors
///
/// Returns staging creation errors through `E::from`, or the operation error.
pub fn with_owned_private_directory<T, E, F>(
    parent: &Path,
    prefix: &str,
    operation: F,
) -> Result<T, E>
where
    E: From<std::io::Error>,
    F: FnOnce(&Path) -> Result<T, E>,
{
    let staging = OwnedPrivateDirectory::create(parent, prefix)?;
    operation(staging.path())
}

/// Error returned by [`read_bounded`].
#[derive(Debug)]
pub enum BigBoundedReadError {
    /// Underlying file open/read failure.
    Io(std::io::Error),
    /// The file exceeded the caller's configured byte budget.
    TooLarge {
        /// Maximum accepted byte count.
        max_bytes: u64,
    },
}

impl fmt::Display for BigBoundedReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::TooLarge { max_bytes } => write!(formatter, "file exceeds {max_bytes} bytes"),
        }
    }
}

impl std::error::Error for BigBoundedReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TooLarge { .. } => None,
        }
    }
}

/// An oversized file reads as [`std::io::ErrorKind::InvalidData`]; an I/O
/// failure keeps its own error, so a caller can still tell `NotFound` apart.
impl From<BigBoundedReadError> for std::io::Error {
    fn from(error: BigBoundedReadError) -> Self {
        match error {
            BigBoundedReadError::Io(error) => error,
            too_large @ BigBoundedReadError::TooLarge { .. } => {
                Self::new(std::io::ErrorKind::InvalidData, too_large.to_string())
            }
        }
    }
}

impl From<std::io::Error> for BigBoundedReadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Read at most `max_bytes` from a regular file, following symlinks.
///
/// The helper reads one extra byte to distinguish an exact-size file from an
/// oversized file. Callers that import user-selected documents can surface
/// [`BigBoundedReadError::TooLarge`] as a product error instead of risking an
/// unbounded allocation. A FIFO, device or socket is refused without being
/// opened: a path from an untrusted tree may name one, and opening it wakes a
/// FIFO's waiting writer, runs a device's open hook (a serial board resets) or
/// gives a session leader a controlling terminal.
///
/// # Errors
///
/// Returns [`BigBoundedReadError::Io`] for open/read failures (`InvalidInput`
/// for a path that is not a regular file, `Unsupported` where `/proc` is not
/// mounted), or [`BigBoundedReadError::TooLarge`] when the file exceeds
/// `max_bytes`.
///
/// # Examples
///
/// ```
/// use big_os_kit::file_io::read_bounded;
/// use big_os_kit::storage::atomic_write;
///
/// let path = std::env::temp_dir()
///     .join(format!("big-os-kit-read-bounded-doc-{}.txt", std::process::id()));
/// atomic_write(&path, b"hello").unwrap();
/// assert_eq!(read_bounded(&path, 8).unwrap(), b"hello");
/// let _ = std::fs::remove_file(path);
/// ```
pub fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, BigBoundedReadError> {
    read_bounded_from(open_regular(path)?, max_bytes)
}

/// Read at most `max_bytes` from `file`, which the caller opened with its own
/// flags ([`open_regular`], [`open_regular_no_follow`]).
///
/// # Errors
///
/// [`BigBoundedReadError::Io`] with `InvalidInput` when `file` is not a
/// regular file, or for a read failure; [`BigBoundedReadError::TooLarge`]
/// when it holds more than `max_bytes`, even when it grows while read.
pub fn read_bounded_from(file: File, max_bytes: u64) -> Result<Vec<u8>, BigBoundedReadError> {
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a regular file",
        )
        .into());
    }
    let mut bytes = Vec::new();
    let mut limited: Take<File> = file.take(max_bytes.saturating_add(1));
    limited.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(BigBoundedReadError::TooLarge { max_bytes });
    }
    Ok(bytes)
}

/// `path` opened for reading when it names a regular file, following symlinks.
///
/// The type is read through an `O_PATH` descriptor, which opens nothing; the
/// file is then opened through `/proc/self/fd`, which is that same inode
/// whatever has taken its name since. [`read_bounded`] reads through it.
///
/// # Errors
///
/// The open error; `InvalidInput` for anything but a regular file, which is
/// never opened; `Unsupported` where `/proc` is not mounted.
pub fn open_regular(path: &Path) -> std::io::Result<File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let named = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(path)?;
    if !named.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY)
        .open(format!("/proc/self/fd/{}", named.as_raw_fd()))
        .map_err(|error| {
            // The descriptor's link names the inode even once it is unlinked,
            // so a missing link is a missing /proc.
            if error.kind() == std::io::ErrorKind::NotFound {
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "/proc is not mounted: a file is opened through its descriptor",
                )
            } else {
                error
            }
        })
}

/// [`read_bounded`] for a file that may not exist yet, such as saved state:
/// `None` when it is missing.
///
/// # Errors
///
/// Any other open or read failure, and [`std::io::ErrorKind::InvalidData`]
/// for a file over `max_bytes`.
pub fn read_bounded_if_exists(path: &Path, max_bytes: u64) -> std::io::Result<Option<Vec<u8>>> {
    match read_bounded(path, max_bytes) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(BigBoundedReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// When `path` was last modified, in whole seconds since the Unix epoch;
/// `None` when it cannot be read or predates the epoch.
#[must_use]
pub fn modified_secs(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    )
}

/// Read a UTF-8 text file through the os-kit filesystem boundary.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the file cannot be read.
///
/// # Examples
///
/// ```
/// use big_os_kit::file_io::read_to_string;
/// use big_os_kit::storage::atomic_write;
///
/// let path = std::env::temp_dir()
///     .join(format!("big-os-kit-read-string-doc-{}.txt", std::process::id()));
/// atomic_write(&path, b"name = \"demo\"\n").unwrap();
/// assert!(read_to_string(&path).unwrap().contains("demo"));
/// let _ = std::fs::remove_file(path);
/// ```
pub fn read_to_string(path: &Path) -> std::io::Result<String> {
    fs::read_to_string(path)
}

/// Create a directory and all missing parents through the os-kit filesystem boundary.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the directory cannot be created.
pub fn create_dir_all(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

/// Create one directory, never over an entry that has its name, through the
/// os-kit filesystem boundary.
///
/// # Errors
///
/// Returns [`std::io::ErrorKind::AlreadyExists`] when the name is taken, or the
/// underlying [`std::io::Error`] when the directory cannot be created.
pub fn create_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

/// Return sorted direct child paths through the shared filesystem boundary.
///
/// The caller decides whether each path is a file, directory, or symlink and
/// owns any recursion budget. A single directory is capped at 16,384 entries
/// so an untrusted selection cannot force an unbounded allocation.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the directory cannot be read.
pub(crate) fn directory_entry_paths(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    directory_entry_paths_with_limit(directory, MAX_DIRECTORY_ENTRY_PATHS)
}

fn directory_entry_paths_with_limit(
    directory: &Path,
    max_entries: usize,
) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        if paths.len() >= max_entries {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("directory exceeds the {max_entries}-entry safety limit"),
            ));
        }
        paths.push(entry?.path());
    }
    paths.sort();
    Ok(paths)
}

/// Rename or move a filesystem entry through the os-kit filesystem boundary.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the rename fails.
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> std::io::Result<()> {
    #[expect(clippy::disallowed_methods, reason = "the owner of a move")]
    fs::rename(from, to)
}

/// Atomically rename a filesystem entry without replacing an existing target.
///
/// Linux `renameat2(RENAME_NOREPLACE)` keeps this safe on filesystems where
/// hard links are unavailable. NFS, and FUSE servers without `rename2`, refuse
/// the flag with `EINVAL` (a kernel or filesystem without it says `ENOSYS` or
/// `EOPNOTSUPP`): there a file is linked under the new name, which
/// fails when the name is taken, then unlinked from the old one, so a power
/// cut between the two leaves it under both names. A folder cannot be linked
/// and keeps the `EINVAL`. Callers must never degrade to an overwriting rename.
///
/// # Errors
///
/// Returns [`std::io::ErrorKind::AlreadyExists`] when `to` exists, an invalid
/// input error for paths containing NUL, or the underlying filesystem error.
pub fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    let source = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains NUL")
    })?;
    let target = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target path contains NUL")
    })?;
    let error = match rustix::fs::renameat_with(
        rustix::fs::CWD,
        &source,
        rustix::fs::CWD,
        &target,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => return Ok(()),
        Err(
            error @ (rustix::io::Errno::INVAL
            | rustix::io::Errno::NOSYS
            | rustix::io::Errno::OPNOTSUPP),
        ) => std::io::Error::from(error),
        Err(error) => return Err(error.into()),
    };
    // EINVAL for a folder is also a folder moved into itself, and a folder
    // cannot be linked.
    if fs::symlink_metadata(from)?.is_dir() {
        return Err(error);
    }
    rename_by_link(from, to)
}

/// [`rename_no_replace`] for a file where `renameat2` takes no flags: linked
/// under `to`, which fails when the name is taken, then unlinked from `from`.
fn rename_by_link(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::hard_link(from, to)?;
    if let Err(error) = fs::remove_file(from) {
        // The file stays where it was, under its one name.
        let _ = fs::remove_file(to);
        return Err(error);
    }
    Ok(())
}

/// Return sorted child file paths whose extension equals `extension`: regular
/// files, or links to one, never a folder named like one.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the directory cannot be read.
///
/// # Examples
///
/// ```
/// use big_os_kit::file_io::directory_file_paths_with_extension;
/// use big_os_kit::storage::atomic_write;
///
/// let dir = std::env::temp_dir()
///     .join(format!("big-os-kit-extension-doc-{}", std::process::id()));
/// std::fs::create_dir_all(&dir).unwrap();
/// atomic_write(&dir.join("a.toml"), b"id = \"a\"\n").unwrap();
/// atomic_write(&dir.join("b.txt"), b"ignored").unwrap();
/// let paths = directory_file_paths_with_extension(&dir, "toml").unwrap();
/// assert_eq!(paths.len(), 1);
/// let _ = std::fs::remove_dir_all(dir);
/// ```
pub fn directory_file_paths_with_extension(
    directory: &Path,
    extension: &str,
) -> std::io::Result<Vec<PathBuf>> {
    Ok(directory_entry_paths(directory)?
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|candidate| candidate == extension)
                && is_regular_file(path)
        })
        .collect())
}

/// Return whether `path` is a regular file.
///
/// # Examples
///
/// ```
/// use big_os_kit::file_io::is_regular_file;
/// use big_os_kit::storage::atomic_write;
///
/// let path = std::env::temp_dir()
///     .join(format!("big-os-kit-regular-file-doc-{}", std::process::id()));
/// atomic_write(&path, b"x").unwrap();
/// assert!(is_regular_file(&path));
/// let _ = std::fs::remove_file(path);
/// ```
#[must_use]
pub fn is_regular_file(path: &Path) -> bool {
    path.is_file()
}

/// Return whether `path` is an executable regular file.
///
/// On Unix this checks any execute bit. On non-Unix platforms, a regular file
/// is treated as executable because POSIX mode bits are unavailable.
///
/// # Examples
///
/// ```
/// use big_os_kit::file_io::is_executable_file;
/// use big_os_kit::storage::atomic_write;
///
/// let path = std::env::temp_dir()
///     .join(format!("big-os-kit-executable-doc-{}", std::process::id()));
/// atomic_write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
/// #[cfg(unix)]
/// {
///     use std::os::unix::fs::PermissionsExt;
///     let mut permissions = std::fs::metadata(&path).unwrap().permissions();
///     permissions.set_mode(0o755);
///     std::fs::set_permissions(&path, permissions).unwrap();
/// }
/// assert_eq!(is_executable_file(&path), cfg!(unix));
/// let _ = std::fs::remove_file(path);
/// ```
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// What stands between this process and running a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunVerdict {
    /// The kernel would let it start as it is; whether its format or its
    /// interpreter then loads is not asked.
    Runs,
    /// Its permission bits do not let this account run it, or it is no
    /// regular file (a folder is searchable, never run).
    NotPermitted,
    /// It sits on a file system mounted without the right to run programs
    /// (`noexec`), which no permission bit changes.
    NoExecMount,
}

/// Whether this process may run `path`, by the kernel's own answer:
/// `access(X_OK)` with this process's ids and groups, after the mount's
/// `noexec` flag. Blocking: on a network mount it can wait.
///
/// # Errors
///
/// Returns the underlying error when `path` cannot be looked at, or
/// `InvalidInput` when it contains a NUL byte.
#[cfg(unix)]
pub fn may_run(path: &Path) -> std::io::Result<RunVerdict> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let stats = rustix::fs::statvfs(&c_path)?;
    if stats.f_flag.contains(rustix::fs::StatVfsMountFlags::NOEXEC) {
        return Ok(RunVerdict::NoExecMount);
    }
    let mode = rustix::fs::stat(&c_path)?.st_mode;
    if rustix::fs::FileType::from_raw_mode(mode) != rustix::fs::FileType::RegularFile {
        return Ok(RunVerdict::NotPermitted);
    }
    match rustix::fs::access(&c_path, rustix::fs::Access::EXEC_OK) {
        Ok(()) => Ok(RunVerdict::Runs),
        Err(rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) => Ok(RunVerdict::NotPermitted),
        Err(error) => Err(error.into()),
    }
}

/// Check that `path` can be opened for reading.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the file cannot be opened.
///
/// # Examples
///
/// ```
/// use big_os_kit::file_io::check_readable_file;
/// use big_os_kit::storage::atomic_write;
///
/// let path = std::env::temp_dir()
///     .join(format!("big-os-kit-readable-doc-{}", std::process::id()));
/// atomic_write(&path, b"x").unwrap();
/// check_readable_file(&path).unwrap();
/// let _ = std::fs::remove_file(path);
/// ```
pub fn check_readable_file(path: &Path) -> std::io::Result<()> {
    File::open(path).map(|_| ())
}

/// Remove a file, ignoring `NotFound`.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] for failures other than a missing
/// file.
///
/// # Examples
///
/// ```
/// use big_os_kit::file_io::remove_file_if_exists;
/// use big_os_kit::storage::atomic_write;
///
/// let path = std::env::temp_dir()
///     .join(format!("big-os-kit-remove-doc-{}", std::process::id()));
/// atomic_write(&path, b"x").unwrap();
/// remove_file_if_exists(&path).unwrap();
/// remove_file_if_exists(&path).unwrap();
/// ```
pub fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The system's words for `error`, in the person's language once the
/// program has set the locale, without the "(os error 13)" tail Rust adds.
#[must_use]
pub fn system_words(error: &std::io::Error) -> String {
    let text = error.to_string();
    match (error.raw_os_error(), text.rfind(" (os error ")) {
        (Some(_), Some(tail)) => text[..tail].to_owned(),
        _ => text,
    }
}

#[cfg(test)]
mod tests;

/// Whether a Linux file has any extended attributes (including access ACLs).
///
/// This is a conservative preflight for writers that cannot preserve metadata.
/// It does not claim to enumerate namespaces hidden by security policy. Symlinks
/// are followed, like document reads. Call only from an I/O worker.
///
/// # Errors
/// Propagates inaccessible paths and I/O failures. Unsupported xattrs mean none.
#[cfg(target_os = "linux")]
pub fn has_extended_attributes(path: &Path) -> std::io::Result<bool> {
    match listxattr_query(path) {
        Ok(size) => Ok(size > 0),
        Err(rustix::io::Errno::NOTSUP) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod regular_descriptor_tests {
    use super::*;
    #[test]
    fn descriptor_verification_refuses_symlinks_and_directories() {
        let directory = std::env::temp_dir().join(format!(
            "os-kit-verify-{}-{}",
            std::process::id(),
            PRIVATE_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("document");
        fs::write(&path, b"ours").unwrap();
        assert!(open_regular_no_follow(&path).is_ok());
        let link = directory.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(open_regular_no_follow(&link).is_err());
        assert!(open_regular_no_follow(&directory).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
