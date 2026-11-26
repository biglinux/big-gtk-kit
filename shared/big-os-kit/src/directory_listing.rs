// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "directory_listing owns metadata-preserving directory scans for app crates"
)]

//! GTK-free directory metadata scans for file-manager style app surfaces.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// The child's `lstat`, asked relative to the open folder: the same call
    /// [`fs::symlink_metadata`] makes, without walking the whole path again.
    fn from_dir_entry(dir_entry: fs::DirEntry) -> std::io::Result<Self> {
        let metadata = dir_entry.metadata()?;
        Self::capture(dir_entry.path(), dir_entry.file_name(), &metadata)
    }

    /// The no-follow snapshot every listed entry carries, from its `lstat`.
    ///
    /// `is_symlink` comes from the same `lstat` as the rest of the metadata
    /// rather than from a directory iterator's cached type, so one child read on
    /// its own answers exactly what the same child answers inside a full scan.
    fn capture(path: PathBuf, name: OsString, metadata: &fs::Metadata) -> std::io::Result<Self> {
        let is_symlink = metadata.is_symlink();
        let followed = is_symlink.then(|| fs::metadata(&path));
        let targets_directory = metadata.is_dir()
            || followed
                .as_ref()
                .is_some_and(|target| target.as_ref().is_ok_and(fs::Metadata::is_dir));
        // Only a target that is not there is broken: a refused or unanswering
        // one exists, and its own folder page says what is wrong.
        let link_broken =
            followed.is_some_and(|target| target.is_err_and(|error| is_missing_target(&error)));
        let link_target = is_symlink
            .then(|| fs::read_link(&path).map(PathBuf::into_os_string))
            .transpose()?;
        Ok(Self {
            name,
            path,
            is_dir: metadata.is_dir(),
            is_symlink,
            link_target,
            targets_directory,
            link_broken,
            is_file: metadata.is_file(),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            created_seconds: metadata
                .created()
                .ok()
                .and_then(|created| created.duration_since(UNIX_EPOCH).ok())
                .and_then(|since| i64::try_from(since.as_secs()).ok()),
            accessed_seconds: metadata.atime(),
            identity: LocalEntryIdentity::from_metadata(metadata),
            uid: metadata.uid(),
            gid: metadata.gid(),
        })
    }
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
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "listing entry path has no file name",
        )
    })?;
    let metadata = fs::symlink_metadata(path)?;
    DirectoryListingEntry::capture(path.to_path_buf(), name.to_os_string(), &metadata)
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
    children: fs::ReadDir,
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
        let directory_metadata = fs::metadata(path)?;
        if !directory_metadata.is_dir() {
            // The system's own error, so its words come in the session's
            // language like every other listing failure.
            return Err(std::io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        Ok(Self {
            children: fs::read_dir(path)?,
            directory_identity: LocalEntryIdentity::from_metadata(&directory_metadata),
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

    /// The next `limit` entries, or fewer at the end; empty once none is left
    /// or once reading the folder failed.
    pub fn next_batch(&mut self, limit: usize) -> Vec<DirectoryListingEntry> {
        let mut entries = Vec::new();
        while entries.len() < limit && self.failure.is_none() {
            let dir_entry = match self.children.next() {
                Some(Ok(dir_entry)) => dir_entry,
                Some(Err(error)) => {
                    self.failure = Some(error);
                    break;
                }
                None => break,
            };
            match DirectoryListingEntry::from_dir_entry(dir_entry) {
                Ok(entry) => entries.push(entry),
                // Removed between the read and the look: it is simply gone.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => self.incomplete = true,
            }
        }
        entries
    }
}

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
        fs::create_dir_all(&dir).unwrap();
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
