// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "directory_listing owns metadata-preserving directory scans for app crates"
)]

//! GTK-free directory metadata scans for file-manager style app surfaces.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::fs::{AtFlags, Mode, OFlags, Statx, StatxFlags, StatxTimestamp};

use serde::{Deserialize, Serialize};

/// Stable Unix identity captured from one no-follow metadata snapshot.
///
/// File-operation clients use this value as a stale-entry hint. The process
/// that performs the mutation must still open the path safely and compare the
/// resulting descriptor metadata before acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalEntryIdentity {
    /// Filesystem device id.
    pub device: u64,
    /// Inode number within [`Self::device`].
    pub inode: u64,
    /// Unix mode including the entry kind bits.
    pub mode: u32,
    /// Byte length at capture time.
    pub size: u64,
    /// Modification timestamp seconds.
    pub modified_seconds: i64,
    /// Modification timestamp nanoseconds.
    pub modified_nanoseconds: i64,
    /// Metadata-change timestamp seconds.
    pub changed_seconds: i64,
    /// Metadata-change timestamp nanoseconds.
    pub changed_nanoseconds: i64,
}

impl LocalEntryIdentity {
    /// Capture an identity from already-obtained metadata.
    #[must_use]
    pub fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    /// The identity in one `statx` answer of at least `STATX_BASIC_STATS`.
    fn from_statx(stat: &Statx) -> Self {
        Self {
            device: libc::makedev(stat.stx_dev_major, stat.stx_dev_minor),
            inode: stat.stx_ino,
            mode: u32::from(stat.stx_mode),
            size: stat.stx_size,
            modified_seconds: stat.stx_mtime.tv_sec,
            modified_nanoseconds: i64::from(stat.stx_mtime.tv_nsec),
            changed_seconds: stat.stx_ctime.tv_sec,
            changed_nanoseconds: i64::from(stat.stx_ctime.tv_nsec),
        }
    }

    /// Return whether descriptor metadata still matches every captured field.
    #[must_use]
    pub fn matches_metadata(self, metadata: &fs::Metadata) -> bool {
        self == Self::from_metadata(metadata)
    }

    /// Whether `other` is this entry again: the same inode, of the same kind.
    /// Its content, size and times may have changed since.
    #[must_use]
    pub const fn is_same_entry(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.mode & libc::S_IFMT == other.mode & libc::S_IFMT
    }

    /// Return whether this identity represents a regular file.
    #[must_use]
    pub const fn is_regular_file(self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }

    /// Return whether this identity represents a directory.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }

    /// Return whether this identity represents a symbolic link.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFLNK
    }
}

/// One directory scan plus the identity of the directory that was read.
#[derive(Clone, Debug)]
pub struct DirectoryListingSnapshot {
    /// Identity of the loaded directory after following the selected directory
    /// path. Mutation code revalidates its device/inode through an open fd.
    pub directory_identity: LocalEntryIdentity,
    /// Child entries returned by the scan.
    pub entries: Vec<DirectoryListingEntry>,
    /// Some entries could not be read and are missing from `entries`: an
    /// entry whose metadata failed (a broken file on a network share), or a
    /// directory stream that failed part of the way through.
    pub incomplete: bool,
}

/// One child entry returned by [`read_directory_listing`].
#[derive(Clone, Debug)]
pub struct DirectoryListingEntry {
    /// File name exactly as reported by the directory iterator.
    pub name: OsString,
    /// Full path to the child entry.
    pub path: PathBuf,
    /// Whether the entry itself is a directory.
    ///
    /// Symlinks are not followed. A symlink to a directory has
    /// `is_symlink == true` and `is_dir == false`.
    pub is_dir: bool,
    /// Whether the entry itself is a symbolic link.
    pub is_symlink: bool,
    /// Link target exactly as stored in the directory entry.
    ///
    /// This is read without following the target and remains available for
    /// broken links. Non-symlink entries always contain `None`.
    pub link_target: Option<OsString>,
    /// Whether the entry leads to a directory when followed once: true for
    /// real directories and for symlinks whose target is a directory. File
    /// managers navigate and group by this; `size`/`modified` stay no-follow.
    pub targets_directory: bool,
    /// Whether the entry is a symlink whose target does not exist.
    pub link_broken: bool,
    /// Whether the entry itself is a regular file.
    pub is_file: bool,
    /// Byte length from symlink metadata.
    pub size: u64,
    /// Modification time from symlink metadata, or the Unix epoch when the
    /// platform cannot report one.
    pub modified: SystemTime,
    /// Birth time in seconds since the epoch, from the same `statx` (which
    /// the standard library always asks it of), or `None` where the file
    /// system or the kernel does not keep one.
    pub created_seconds: Option<i64>,
    /// Last access time in seconds since the epoch.
    pub accessed_seconds: i64,
    /// No-follow Unix identity captured with the displayed metadata.
    pub identity: LocalEntryIdentity,
    /// Numeric owner from the same no-follow metadata snapshot.
    pub uid: u32,
    /// Numeric group from the same no-follow metadata snapshot.
    pub gid: u32,
}

impl DirectoryListingEntry {
    /// The no-follow snapshot every listed entry carries, from one `statx` of
    /// `name` in `folder`, whose path is `path`'s parent.
    ///
    /// `is_symlink` comes from the same `statx` as the rest of the metadata
    /// rather than from a directory iterator's cached type, so one child read on
    /// its own answers exactly what the same child answers inside a full scan.
    /// Neither the entry nor a link's final target is mounted when it is an
    /// automount point; one on the way to a link's target is.
    fn capture(folder: BorrowedFd<'_>, name: &CStr, path: PathBuf) -> std::io::Result<Self> {
        let stat = rustix::fs::statx(
            folder,
            name,
            AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
            StatxFlags::BASIC_STATS | StatxFlags::BTIME,
        )?;
        let kind = u32::from(stat.stx_mode) & libc::S_IFMT;
        let is_symlink = kind == libc::S_IFLNK;
        let followed = is_symlink.then(|| {
            rustix::fs::statx(folder, name, AtFlags::NO_AUTOMOUNT, StatxFlags::TYPE)
                .map_err(std::io::Error::from)
        });
        let targets_directory = kind == libc::S_IFDIR
            || followed.as_ref().is_some_and(|target| {
                target
                    .as_ref()
                    .is_ok_and(|target| u32::from(target.stx_mode) & libc::S_IFMT == libc::S_IFDIR)
            });
        // Only a target that is not there is broken: a refused or unanswering
        // one exists, and its own folder page says what is wrong.
        let link_broken =
            followed.is_some_and(|target| target.is_err_and(|error| is_missing_target(&error)));
        let link_target = is_symlink
            .then(|| {
                rustix::fs::readlinkat(folder, name, Vec::new())
                    .map(|target| OsString::from_vec(target.into_bytes()))
            })
            .transpose()?;
        let has_birth = StatxFlags::from_bits_retain(stat.stx_mask).contains(StatxFlags::BTIME);
        Ok(Self {
            name: OsStr::from_bytes(name.to_bytes()).to_os_string(),
            path,
            is_dir: kind == libc::S_IFDIR,
            is_symlink,
            link_target,
            targets_directory,
            link_broken,
            is_file: kind == libc::S_IFREG,
            size: stat.stx_size,
            modified: system_time(stat.stx_mtime),
            created_seconds: has_birth
                .then_some(stat.stx_btime.tv_sec)
                .filter(|seconds| *seconds >= 0),
            accessed_seconds: stat.stx_atime.tv_sec,
            identity: LocalEntryIdentity::from_statx(&stat),
            uid: stat.stx_uid,
            gid: stat.stx_gid,
        })
    }
}

/// `time` as a [`SystemTime`], or the Unix epoch past what one can hold.
fn system_time(time: StatxTimestamp) -> SystemTime {
    let nanoseconds = Duration::from_nanos(u64::from(time.tv_nsec));
    let whole = match u64::try_from(time.tv_sec) {
        Ok(seconds) => UNIX_EPOCH.checked_add(Duration::from_secs(seconds)),
        Err(_) => UNIX_EPOCH.checked_sub(Duration::from_secs(time.tv_sec.unsigned_abs())),
    };
    whole
        .and_then(|whole| whole.checked_add(nanoseconds))
        .unwrap_or(UNIX_EPOCH)
}

/// Read the metadata of one child, exactly as a full listing would read it.
///
/// A view whose folder monitor named a single entry patches that entry instead
/// of re-reading the folder, and this is the read it does.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the metadata cannot be read;
/// [`std::io::ErrorKind::NotFound`] means the entry is already gone.
/// [`std::io::ErrorKind::InvalidInput`] means `path` names no child.
pub fn read_listing_entry(path: &Path) -> std::io::Result<DirectoryListingEntry> {
    let name = path
        .file_name()
        .and_then(|name| CString::new(name.as_bytes()).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "listing entry path has no file name",
            )
        })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let folder = rustix::fs::open(
        parent,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    DirectoryListingEntry::capture(folder.as_fd(), &name, path.to_path_buf())
}

/// Read one directory and return child metadata without filtering or sorting.
///
/// Entries whose metadata disappears during the scan are skipped, and so are
/// entries whose metadata cannot be read: a caller that must know the list is
/// whole, such as one that totals a folder, reads
/// [`read_directory_listing_snapshot`] and its `incomplete`. Entry metadata
/// and link targets are read without following symlinks; `targets_directory`
/// follows a symlink once only to classify navigation targets.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when `path` cannot be read or the
/// directory iterator reports an error.
pub fn read_directory_listing(path: &Path) -> std::io::Result<Vec<DirectoryListingEntry>> {
    read_directory_listing_snapshot(path).map(|snapshot| snapshot.entries)
}

/// Read a directory and return both its identity and its child metadata.
///
/// The directory identity is a stale-view hint for durable operations. The
/// operation owner must re-open and compare descriptor metadata before writing.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the directory metadata
/// cannot be read, the directory cannot be opened or reading it fails partway.
/// An entry that cannot be read is left out and marks the snapshot
/// [`incomplete`], rather than hiding every other entry of the folder.
///
/// [`incomplete`]: DirectoryListingSnapshot::incomplete
pub fn read_directory_listing_snapshot(path: &Path) -> std::io::Result<DirectoryListingSnapshot> {
    let mut reader = DirectoryListingReader::open(path)?;
    let mut entries = Vec::new();
    loop {
        let batch = reader.next_batch(usize::MAX);
        if batch.is_empty() {
            break;
        }
        entries.extend(batch);
    }
    if let Some(error) = reader.failure.take() {
        return Err(error);
    }
    Ok(DirectoryListingSnapshot {
        directory_identity: reader.directory_identity,
        entries,
        incomplete: reader.incomplete,
    })
}

/// A directory scan taken a batch at a time, for a caller that shows the first
/// entries while the rest are still being read. The rules of
/// [`read_directory_listing_snapshot`] apply to every batch.
#[derive(Debug)]
pub struct DirectoryListingReader {
    /// The folder, opened once: its identity, its names and each one's
    /// metadata all come from this one directory, even if the path is renamed
    /// or replaced while it is read.
    children: rustix::fs::Dir,
    path: PathBuf,
    /// Threads reading a batch's metadata at once, more than one only on a
    /// local disk that does not spin, with cores to spare. Decided once a round
    /// holds [`LANES_FROM`] names, so a small folder asks neither.
    lanes: Option<usize>,
    /// What is mounted where the folder is, to decide `lanes` by.
    source: Option<String>,
    /// On a share, a FUSE mount or one the table does not name, a batch comes
    /// back once [`SLOW_MOUNT_BATCH`] has gone by, with what it read so far.
    may_stall: bool,
    /// The folder's names have all been read.
    ended: bool,
    directory_identity: LocalEntryIdentity,
    incomplete: bool,
    /// Why the folder itself stopped being read: a server that went away
    /// partway. What was read before it is not the folder.
    failure: Option<std::io::Error>,
}

impl DirectoryListingReader {
    /// Open `path` for reading.
    ///
    /// # Errors
    ///
    /// The errors of [`read_directory_listing_snapshot`] that stop the scan
    /// before its first entry.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        // A path that is no folder fails with the system's own "not a
        // directory", so its words come in the session's language like every
        // other listing failure.
        let folder = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let identity = rustix::fs::statx(
            &folder,
            c"",
            AtFlags::EMPTY_PATH,
            StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
        )?;
        // By the mount the folder is on, which a symlink on the way cannot hide.
        let table = crate::mount_table::mountinfo();
        let mount = crate::mount_table::mount_with_id(&table, identity.stx_mnt_id);
        let may_stall = mount
            .as_ref()
            .is_none_or(|mount| crate::mount_table::may_stall(mount.filesystem));
        Ok(Self {
            // The descriptor itself, not the folder's "." opened again, which a
            // folder that may be read but not searched refuses.
            children: rustix::fs::Dir::new(folder)?,
            path: path.to_path_buf(),
            lanes: may_stall.then_some(1),
            source: mount.filter(|_| !may_stall).map(|mount| mount.source),
            may_stall,
            ended: false,
            directory_identity: LocalEntryIdentity::from_statx(&identity),
            incomplete: false,
            failure: None,
        })
    }

    /// Identity of the directory that was opened.
    #[must_use]
    pub fn directory_identity(&self) -> LocalEntryIdentity {
        self.directory_identity
    }

    /// Whether an entry read so far was left out because it could not be read.
    /// Final once [`next_batch`](Self::next_batch) has returned nothing.
    #[must_use]
    pub fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// Why the folder could not be read to its end, asked once
    /// [`next_batch`](Self::next_batch) has returned nothing.
    pub fn take_failure(&mut self) -> Option<std::io::Error> {
        self.failure.take()
    }

    /// The next `limit` entries (at least one), or fewer; empty once none is
    /// left or once reading the folder failed. On a share or FUSE mount a
    /// batch also ends once it holds entries and 50 ms have gone by.
    pub fn next_batch(&mut self, limit: usize) -> Vec<DirectoryListingEntry> {
        // An empty batch means the end, so even a limit of 0 reads on.
        let limit = limit.max(1);
        let started = Instant::now();
        let mut entries = Vec::new();
        // Names read on several threads are looked at once a round of them is
        // in; a round whose every name vanished meanwhile is followed by
        // another, since an empty batch is the end.
        'batch: while entries.len() < limit && !self.ended && self.failure.is_none() {
            let mut names = Vec::new();
            while entries.len() + names.len() < limit {
                let child = match self.children.read() {
                    Some(Ok(child)) => child,
                    Some(Err(error)) => {
                        self.failure = Some(error.into());
                        break;
                    }
                    None => {
                        self.ended = true;
                        break;
                    }
                };
                let name = child.file_name();
                if name == c"." || name == c".." {
                    continue;
                }
                if self.lanes != Some(1) {
                    names.push(name.to_owned());
                    if names.len() == LANES_FROM && self.lanes.is_none() {
                        self.lanes = Some(self.decide_lanes());
                    }
                    continue;
                }
                let read = self.capture(name);
                self.keep(read, &mut entries);
                if self.may_stall && !entries.is_empty() && started.elapsed() >= SLOW_MOUNT_BATCH {
                    break 'batch;
                }
            }
            for read in self.capture_together(&names) {
                self.keep(read, &mut entries);
            }
        }
        entries
    }

    fn capture(&self, name: &CStr) -> std::io::Result<DirectoryListingEntry> {
        let path = self.path.join(OsStr::from_bytes(name.to_bytes()));
        DirectoryListingEntry::capture(self.children.fd()?, name, path)
    }

    /// Add `read` to `entries`, or note why it is left out.
    fn keep(
        &mut self,
        read: std::io::Result<DirectoryListingEntry>,
        entries: &mut Vec<DirectoryListingEntry>,
    ) {
        match read {
            Ok(entry) => entries.push(entry),
            // Removed between the read and the look: it is simply gone.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => self.incomplete = true,
        }
    }

    /// Threads for the rest of the folder: half the cores, at most
    /// [`MAX_LANES`], on a disk the kernel says does not spin.
    fn decide_lanes(&self) -> usize {
        if !self
            .source
            .as_deref()
            .is_some_and(crate::mount_table::on_solid_state)
        {
            return 1;
        }
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        (cores / 2).clamp(1, MAX_LANES)
    }

    /// Each of `names` read on up to `lanes` threads, in the order given.
    fn capture_together(&self, names: &[CString]) -> Vec<std::io::Result<DirectoryListingEntry>> {
        let capture =
            |part: &[CString]| -> Vec<_> { part.iter().map(|name| self.capture(name)).collect() };
        let lanes = self.lanes.unwrap_or(1);
        if names.len() < LANES_FROM || lanes == 1 {
            return capture(names);
        }
        let part_size = names.len().div_ceil(lanes);
        std::thread::scope(|scope| {
            let parts: Vec<_> = names
                .chunks(part_size)
                .map(|part| {
                    // A thread the system will not start leaves its part to this one.
                    std::thread::Builder::new()
                        .spawn_scoped(scope, move || capture(part))
                        .map_err(|_| part)
                })
                .collect();
            parts
                .into_iter()
                .flat_map(|part| match part {
                    Ok(thread) => thread
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                    Err(part) => capture(part),
                })
                .collect()
        })
    }
}

/// The most threads one listing reads metadata on: past four, a fast disk
/// gave no more, and every listing open at once takes as many.
const MAX_LANES: usize = 4;

/// Fewer names than this are read on the calling thread: starting threads
/// costs more than they save.
const LANES_FROM: usize = 512;

/// How long a batch on a share or FUSE mount reads before it returns what it
/// has, so a slow server shows rows as they come.
const SLOW_MOUNT_BATCH: Duration = Duration::from_millis(50);

/// Whether following a link failed because its target is not there: missing,
/// not a folder on the way, or a loop of links.
#[must_use]
pub fn is_missing_target(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    ) || error.raw_os_error() == Some(libc::ELOOP)
}

/// The nine permission bits of `mode` as `ls -l` spells them: `rwxr-xr--`.
#[must_use]
pub fn format_permissions(mode: u32) -> String {
    let mut text = String::with_capacity(9);
    for (read, write, execute) in [
        (0o400, 0o200, 0o100),
        (0o040, 0o020, 0o010),
        (0o004, 0o002, 0o001),
    ] {
        text.push(if mode & read != 0 { 'r' } else { '-' });
        text.push(if mode & write != 0 { 'w' } else { '-' });
        text.push(if mode & execute != 0 { 'x' } else { '-' });
    }
    text
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests create temporary directories for scan contract checks"
    )]

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn permissions_spell_unix_triplets() {
        assert_eq!(format_permissions(0o754), "rwxr-xr--");
        assert_eq!(format_permissions(0o4755), "rwxr-xr-x");
        assert_eq!(format_permissions(0), "---------");
    }

    static DIRECTORY_LISTING_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn directory_listing_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "big-os-kit-directory-listing-test-{}-{}-{name}",
            std::process::id(),
            DIRECTORY_LISTING_TEST_SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        // A failed run under a reused PID leaves its files behind.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        dir
    }

    /// Birth and access times are the ones `stat` reports: `%W`, 0 where the
    /// file system keeps no birth time, and `%X`.
    #[test]
    fn times_match_stat() {
        let dir = directory_listing_test_dir("times");
        let file = dir.join("dated");
        fs::write(&file, b"x").unwrap();
        let accessed = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_accessed(accessed))
            .unwrap();
        let entry = read_listing_entry(&file).unwrap();
        let stat = crate::subprocess::BigSubprocessSpec::builder()
            .program("stat")
            .args(["-c", "%W %X"])
            .arg(&file)
            .stdout(crate::subprocess::BigSubprocessOutputMode::Capture)
            .build()
            .run()
            .unwrap();
        fs::remove_dir_all(&dir).unwrap();
        let said = stat.stdout_lossy();
        let (birth, access) = said.trim().split_once(' ').unwrap();
        let (birth, access): (i64, i64) = (birth.parse().unwrap(), access.parse().unwrap());
        assert_eq!(access, 1_700_000_000);
        assert_eq!(entry.accessed_seconds, access);
        assert_eq!(entry.created_seconds, (birth != 0).then_some(birth));
    }

    /// Entries that cannot be read are left out and the listing says it is
    /// short, instead of the whole folder failing. A folder that may be read
    /// but not searched (`r--`) lists its names and refuses each one's
    /// metadata: the same shape as a share that fails one entry.
    #[test]
    fn unreadable_entries_mark_listing_short() {
        use std::os::unix::fs::PermissionsExt;
        let dir = directory_listing_test_dir("unreadable");
        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("inside"), b"x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o400)).unwrap();
        let listed = read_directory_listing_snapshot(&locked);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        let listed = listed.unwrap();
        assert!(listed.incomplete);
        assert!(listed.entries.is_empty());
        assert!(!read_directory_listing_snapshot(&dir).unwrap().incomplete);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder whose read fails partway, as a share does when its server
    /// goes away, is an error: what came before it is not the folder.
    #[test]
    fn failed_read_is_an_error() {
        let dir = directory_listing_test_dir("failed-read");
        fs::write(dir.join("file"), b"x").unwrap();
        let read = std::thread::spawn({
            let dir = dir.clone();
            move || {
                let mut reader = DirectoryListingReader::open(&dir).unwrap();
                crate::storage::tests::refuse_on_this_thread(libc::SYS_getdents64, libc::EIO);
                let batch = reader.next_batch(usize::MAX);
                (
                    batch.len(),
                    reader.take_failure().map(|error| error.raw_os_error()),
                )
            }
        })
        .join()
        .unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read, (0, Some(Some(libc::EIO))));
    }

    /// The whole read fails the same way, instead of a short folder.
    #[test]
    fn failed_read_fails_the_snapshot() {
        let dir = directory_listing_test_dir("failed-snapshot");
        fs::write(dir.join("file"), b"x").unwrap();
        let read = std::thread::spawn({
            let dir = dir.clone();
            move || {
                crate::storage::tests::refuse_on_this_thread(libc::SYS_getdents64, libc::EIO);
                read_directory_listing_snapshot(&dir).map_err(|error| error.raw_os_error())
            }
        })
        .join()
        .unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            read.map(|snapshot| snapshot.entries.len()),
            Err(Some(libc::EIO))
        );
    }

    /// An entry removed between the folder's read and its own look is gone,
    /// not unreadable: the listing stays whole.
    #[test]
    fn vanished_entry_keeps_listing_whole() {
        let dir = directory_listing_test_dir("vanished");
        fs::write(dir.join("file"), b"x").unwrap();
        let read = std::thread::spawn({
            let dir = dir.clone();
            move || {
                let mut reader = DirectoryListingReader::open(&dir).unwrap();
                // Every look at an entry now finds nothing, as after a removal.
                crate::storage::tests::refuse_on_this_thread(libc::SYS_statx, libc::ENOENT);
                let batch = reader.next_batch(usize::MAX);
                (batch.len(), reader.incomplete())
            }
        })
        .join()
        .unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read, (0, false));
    }

    /// Read on several threads, a batch holds the same entries in the same
    /// order as read on one, and still notes the one it could not read.
    #[test]
    fn lanes_keep_order_and_gaps() {
        use std::os::unix::fs::PermissionsExt;
        let dir = directory_listing_test_dir("lanes");
        for index in 0..LANES_FROM * 3 {
            fs::write(dir.join(format!("file-{index}")), b"x").unwrap();
        }
        // A link, read with its target, and a folder of entries no look may
        // reach.
        std::os::unix::fs::symlink("file-0", dir.join("link")).unwrap();
        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();
        for index in 0..LANES_FROM {
            fs::write(locked.join(format!("hidden-{index}")), b"x").unwrap();
        }
        let read = |folder: &Path, lanes: usize| {
            let mut reader = DirectoryListingReader::open(folder).unwrap();
            reader.lanes = Some(lanes);
            let entries: Vec<_> = reader
                .next_batch(usize::MAX)
                .into_iter()
                .map(|entry| (entry.name, entry.identity, entry.link_target))
                .collect();
            (entries, reader.incomplete())
        };
        let (one, one_short) = read(&dir, 1);
        let (four, four_short) = read(&dir, 4);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
        let (locked_four, locked_short) = read(&locked, 4);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(one.len(), LANES_FROM * 3 + 2);
        assert_eq!(four, one);
        assert!(!one_short && !four_short);
        assert!(locked_four.is_empty() && locked_short);
    }

    /// A batch whose names all vanish before their look reads on, when names
    /// are gathered for several threads too: an empty batch is taken for the
    /// end of the folder.
    #[test]
    fn vanished_round_reads_on() {
        let dir = directory_listing_test_dir("vanished-round");
        for index in 0..8 {
            fs::write(dir.join(format!("file-{index}")), b"x").unwrap();
        }
        // The folder's own order, which the reader below reads it in.
        let order: Vec<OsString> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let mut reader = DirectoryListingReader::open(&dir).unwrap();
        reader.lanes = Some(4);
        // The first look fetches a run of names; the two the next batch reads
        // first then go before it looks at them.
        let primed = reader.children.read().unwrap().unwrap();
        let skip = usize::from(primed.file_name() != c"." && primed.file_name() != c"..");
        for name in &order[skip..skip + 2] {
            fs::remove_file(dir.join(name)).unwrap();
        }
        let batch = reader.next_batch(2);
        fs::remove_dir_all(&dir).unwrap();
        // The two names after the vanished ones, which only a reader that met
        // them in this order and read on gives.
        let names: Vec<_> = batch.into_iter().map(|entry| entry.name).collect();
        assert_eq!(names, order[skip + 2..skip + 4], "{order:?}");
        assert!(!reader.incomplete());
    }

    /// A local folder is found on its mount, and is not taken for a share
    /// that may stall; a small one decides no threads.
    #[test]
    fn local_folder_is_no_share() {
        let dir = directory_listing_test_dir("local");
        fs::write(dir.join("file"), b"x").unwrap();
        let mut reader = DirectoryListingReader::open(&dir).unwrap();
        let entries = reader.next_batch(usize::MAX);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!reader.may_stall);
        assert!(reader.source.is_some());
        assert_eq!(reader.lanes, None);
    }

    /// An empty batch is the end, so a limit of 0 still reads an entry.
    #[test]
    fn zero_limit_reads_on() {
        let dir = directory_listing_test_dir("zero-limit");
        fs::write(dir.join("file"), b"x").unwrap();
        let mut reader = DirectoryListingReader::open(&dir).unwrap();
        let batch = reader.next_batch(0);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(batch.len(), 1);
    }

    /// Reading in batches yields what one whole read does.
    #[test]
    fn batches_add_up_to_snapshot() {
        let dir = directory_listing_test_dir("batches");
        for index in 0..7 {
            fs::write(dir.join(format!("file-{index}")), b"x").unwrap();
        }
        let mut reader = DirectoryListingReader::open(&dir).unwrap();
        let mut sizes = Vec::new();
        let mut names = Vec::new();
        loop {
            let batch = reader.next_batch(3);
            if batch.is_empty() {
                break;
            }
            sizes.push(batch.len());
            names.extend(batch.into_iter().map(|entry| entry.name));
        }
        names.sort();
        let mut whole: Vec<_> = read_directory_listing(&dir)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        whole.sort();
        assert_eq!(sizes, [3, 3, 1]);
        assert_eq!(names, whole);
        assert!(!reader.incomplete());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A file listed as a folder fails as the system's "not a directory",
    /// which carries its words in the session's language.
    #[test]
    fn file_is_not_a_directory() {
        let dir = directory_listing_test_dir("not-a-directory");
        fs::write(dir.join("plain.txt"), b"x").unwrap();
        let error = read_directory_listing_snapshot(&dir.join("plain.txt")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
        assert!(error.raw_os_error().is_some());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_includes_hidden_entries() {
        let dir = directory_listing_test_dir("hidden");
        fs::write(dir.join(".hidden"), b"x").unwrap();
        fs::write(dir.join("visible"), b"x").unwrap();

        let mut names = read_directory_listing(&dir)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(names, [".hidden", "visible"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn scan_keeps_symlinks_unresolved() {
        use std::os::unix::fs::symlink;

        let dir = directory_listing_test_dir("symlink");
        fs::create_dir_all(dir.join("target")).unwrap();
        symlink(dir.join("target"), dir.join("target-link")).unwrap();

        let entries = read_directory_listing(&dir).unwrap();
        let link = entries
            .iter()
            .find(|entry| entry.name == "target-link")
            .expect("symlink entry is listed");

        assert!(link.is_symlink);
        assert_eq!(
            link.link_target.as_deref(),
            Some(dir.join("target").as_os_str())
        );
        assert!(!link.is_dir);
        assert!(
            link.targets_directory,
            "a symlink to a directory must stay navigable"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_file_is_no_directory() {
        use std::os::unix::fs::symlink;

        let dir = directory_listing_test_dir("symlink-file");
        fs::write(dir.join("plain.txt"), b"x").unwrap();
        symlink(dir.join("plain.txt"), dir.join("plain-link")).unwrap();

        let entries = read_directory_listing(&dir).unwrap();
        let link = entries
            .iter()
            .find(|entry| entry.name == "plain-link")
            .expect("symlink entry is listed");

        assert!(link.is_symlink);
        assert_eq!(
            link.link_target.as_deref(),
            Some(dir.join("plain.txt").as_os_str())
        );
        assert!(!link.targets_directory);
        assert!(!link.link_broken);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_keeps_its_stored_target() {
        use std::os::unix::fs::symlink;

        let dir = directory_listing_test_dir("broken-symlink");
        symlink("missing/target", dir.join("broken-link")).unwrap();

        let entries = read_directory_listing(&dir).unwrap();
        let link = entries
            .iter()
            .find(|entry| entry.name == "broken-link")
            .expect("broken symlink is listed");

        assert_eq!(
            link.link_target.as_deref(),
            Some(std::ffi::OsStr::new("missing/target"))
        );
        assert!(!link.targets_directory);
        assert!(link.link_broken);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Only a missing target is broken; one that cannot be read exists.
    #[cfg(unix)]
    #[test]
    fn unreadable_target_is_not_broken() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = directory_listing_test_dir("unreadable-target");
        fs::write(dir.join("plain.txt"), b"x").unwrap();
        symlink("plain.txt/inside", dir.join("under-file")).unwrap();
        fs::create_dir(dir.join("closed")).unwrap();
        fs::write(dir.join("closed/inner"), b"x").unwrap();
        symlink("closed/inner", dir.join("refused")).unwrap();
        fs::set_permissions(dir.join("closed"), fs::Permissions::from_mode(0o000)).unwrap();

        let entries = read_directory_listing(&dir).unwrap();
        fs::set_permissions(dir.join("closed"), fs::Permissions::from_mode(0o700)).unwrap();
        let broken = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .link_broken
        };
        assert!(broken("under-file"));
        assert!(!broken("refused"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_carries_nofollow_identities() {
        let dir = directory_listing_test_dir("identity");
        let file = dir.join("plain.txt");
        fs::write(&file, b"hello").unwrap();

        let snapshot = read_directory_listing_snapshot(&dir).unwrap();
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.name == "plain.txt")
            .unwrap();

        assert!(snapshot.directory_identity.is_directory());
        assert!(entry.identity.is_regular_file());
        assert!(
            entry
                .identity
                .matches_metadata(&fs::symlink_metadata(&file).unwrap())
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// Two readers of the same thing: a view that patches one row with
    /// [`read_listing_entry`] must not disagree with the listing the full scan
    /// built around it.
    #[cfg(unix)]
    #[test]
    fn one_entry_matches_full_scan() {
        use std::os::unix::fs::symlink;

        let dir = directory_listing_test_dir("one-entry");
        fs::write(dir.join("file"), b"content").unwrap();
        fs::create_dir_all(dir.join("folder")).unwrap();
        symlink(dir.join("folder"), dir.join("folder-link")).unwrap();
        symlink(dir.join("absent"), dir.join("broken-link")).unwrap();

        let scanned = read_directory_listing(&dir).unwrap();
        assert_eq!(scanned.len(), 4);
        for entry in &scanned {
            let alone = read_listing_entry(&entry.path).unwrap();
            assert_eq!(alone.name, entry.name);
            assert_eq!(alone.is_dir, entry.is_dir);
            assert_eq!(alone.is_file, entry.is_file);
            assert_eq!(alone.is_symlink, entry.is_symlink);
            assert_eq!(alone.targets_directory, entry.targets_directory);
            assert_eq!(alone.link_target, entry.link_target);
            assert_eq!(alone.size, entry.size);
            assert_eq!(alone.identity, entry.identity);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    /// An entry removed between the monitor's event and this read is the normal
    /// case, not an error worth reporting differently.
    #[test]
    fn vanished_entry_reads_as_missing() {
        let dir = directory_listing_test_dir("gone");
        assert_eq!(
            read_listing_entry(&dir.join("gone")).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(
            read_listing_entry(Path::new("/")).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
