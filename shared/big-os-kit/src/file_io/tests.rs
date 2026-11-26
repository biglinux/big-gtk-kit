// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "tests create temporary files for filesystem contract checks"
)]

use super::*;

/// The kernel's answer, not the owner's bit: a file without an execute bit
/// is refused, and `noexec` mounts such as `/proc` refuse every file
/// whatever its bits.
#[cfg(unix)]
#[test]
fn run_verdict_asks_the_kernel() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!("big-os-kit-run-{}", std::process::id()));
    fs::write(&path, b"#!/bin/sh\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let unmarked = may_run(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let marked = may_run(&path).unwrap();
    fs::remove_file(&path).unwrap();
    // The temporary folder may itself be `noexec`, which outranks the bits.
    let expected = if marked == RunVerdict::NoExecMount {
        (RunVerdict::NoExecMount, RunVerdict::NoExecMount)
    } else {
        (RunVerdict::NotPermitted, RunVerdict::Runs)
    };
    assert_eq!((unmarked, marked), expected);
    assert!(may_run(&path).is_err());
    assert_eq!(
        may_run(Path::new("/proc/self/status")).unwrap(),
        RunVerdict::NoExecMount
    );
}
use std::error::Error as _;
use std::sync::atomic::{AtomicU64, Ordering};

static FILE_IO_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

fn file_io_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "big-os-kit-file-io-test-{}-{}-{name}",
        std::process::id(),
        FILE_IO_TEST_SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn bounded_read_refuses_only_oversize() {
    let dir = file_io_test_dir("bounded");
    let path = dir.join("data.bin");
    fs::write(&path, b"abcd").unwrap();

    assert_eq!(read_bounded(&path, 4).unwrap(), b"abcd");
    assert!(matches!(
        read_bounded(&path, 3).unwrap_err(),
        BigBoundedReadError::TooLarge { max_bytes: 3 }
    ));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn missing_file_reads_as_none() {
    let dir = file_io_test_dir("if-exists");
    let path = dir.join("state.json");

    assert_eq!(read_bounded_if_exists(&path, 4).unwrap(), None);
    fs::write(&path, b"abcd").unwrap();
    assert_eq!(
        read_bounded_if_exists(&path, 4).unwrap().as_deref(),
        Some(&b"abcd"[..])
    );
    assert_eq!(
        read_bounded_if_exists(&path, 3).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn modified_secs_reads_whole_seconds() {
    let dir = file_io_test_dir("modified");
    let path = dir.join("song.mp3");
    let file = File::create(&path).unwrap();
    let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_900);
    file.set_modified(stamp).unwrap();

    assert_eq!(modified_secs(&path), Some(1_700_000_000));
    fs::remove_file(&path).unwrap();
    assert_eq!(modified_secs(&path), None);

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn bounded_read_refuses_fifo_and_directory() {
    let dir = file_io_test_dir("bounded-fifo");
    let fifo = dir.join("fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

    for path in [&fifo, &dir] {
        let error = read_bounded(path, 8).unwrap_err();
        assert!(
            matches!(&error, BigBoundedReadError::Io(error) if error.kind() == std::io::ErrorKind::InvalidInput),
            "{error}"
        );
    }

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn bounded_read_error_keeps_cause() {
    let io_error = BigBoundedReadError::Io(std::io::Error::other("blocked"));
    assert_eq!(io_error.to_string(), "blocked");
    assert!(io_error.source().is_some());

    let too_large = BigBoundedReadError::TooLarge { max_bytes: 9 };
    assert_eq!(too_large.to_string(), "file exceeds 9 bytes");
    assert!(too_large.source().is_none());
}

#[test]
fn oversize_reads_as_invalid_data() {
    let missing: std::io::Error =
        BigBoundedReadError::Io(std::io::ErrorKind::NotFound.into()).into();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

    let too_large: std::io::Error = BigBoundedReadError::TooLarge { max_bytes: 9 }.into();
    assert_eq!(too_large.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn private_directory_is_owned_and_removed() {
    use std::os::unix::fs::PermissionsExt;

    let parent = file_io_test_dir("private-directory");
    let first_path;
    {
        let first = OwnedPrivateDirectory::create(&parent, ".stage").unwrap();
        let second = OwnedPrivateDirectory::create(&parent, ".stage").unwrap();
        first_path = first.path().to_owned();
        assert_ne!(first.path(), second.path());
        assert_eq!(
            fs::metadata(first.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::write(first.path().join("partial"), b"owned").unwrap();
    }
    assert!(!first_path.exists());
    let _ = fs::remove_dir_all(parent);
}

#[test]
fn private_directory_refuses_bad_prefix() {
    let parent = file_io_test_dir("private-prefix");
    for prefix in ["", "a/b", "a\\b", "a\0b"] {
        let error = OwnedPrivateDirectory::create(&parent, prefix).err();
        assert_eq!(
            error.map(|error| error.kind()),
            Some(std::io::ErrorKind::InvalidInput),
            "{prefix:?}"
        );
    }
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
    let _ = fs::remove_dir_all(parent);
}

#[test]
fn private_directory_skips_taken_names() {
    let parent = file_io_test_dir("private-taken");
    let next = PRIVATE_DIRECTORY_SEQUENCE.load(Ordering::Relaxed);
    let taken: Vec<_> = (next..next + 3)
        .map(|sequence| parent.join(format!(".stage-{}-{sequence}", std::process::id())))
        .collect();
    for path in &taken {
        fs::create_dir(path).unwrap();
    }

    let staged = OwnedPrivateDirectory::create(&parent, ".stage").unwrap();

    assert!(!taken.iter().any(|path| path == staged.path()));
    drop(staged);
    let _ = fs::remove_dir_all(parent);
}

/// A staging directory whose process died goes when the next one is made
/// beside it, and only that: not this process's own, not another user's
/// shape, not another prefix's.
#[test]
fn abandoned_staging_is_cleared() {
    use std::os::unix::fs::PermissionsExt as _;
    let parent = file_io_test_dir("private-abandoned");
    let private = |name: &str, mode: u32| {
        let staging = parent.join(name);
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("archive.zip"), b"half").unwrap();
        // A folder its program took the modes off.
        let locked = staging.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("part"), b"half").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Left two hours ago.
        let left = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
        fs::File::open(&staging)
            .unwrap()
            .set_modified(left)
            .unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(mode)).unwrap();
        staging
    };
    // No process ever has this id: it is above the kernel's limit.
    let dead = private(".stage-999999999-0", 0o700);
    let mine = private(&format!(".stage-{}-999", std::process::id()), 0o700);
    let shared = private(".stage-999999999-1", 0o755);
    let lookalikes = [
        ".stage-notes",
        ".stage-999999999-",
        ".stage-999999999-tmp",
        // `u32::from_str` takes a sign the name's rule does not.
        ".stage-+999999999-0",
    ]
    .map(|name| private(name, 0o700));
    // init never exits.
    let live = private(".stage-1-0", 0o700);
    // An owner in another PID namespace looks dead here but still writes.
    let recent = parent.join(".stage-999999998-0");
    fs::create_dir_all(&recent).unwrap();
    fs::set_permissions(&recent, fs::Permissions::from_mode(0o700)).unwrap();
    let other = private(".other-999999999-0", 0o700);

    let staged = OwnedPrivateDirectory::create(&parent, ".stage").unwrap();

    assert!(!dead.exists());
    assert!(mine.exists() && shared.exists() && live.exists() && other.exists());
    assert!(recent.exists());
    assert!(lookalikes.iter().all(|lookalike| lookalike.exists()));
    drop(staged);
    remove_tree(&parent).unwrap();
}

/// On a network share the process id may be another machine's, so a dead
/// owner's staging directory there stays; off the share, it goes.
#[test]
fn abandoned_staging_on_share_stays() {
    use std::os::unix::fs::PermissionsExt as _;
    let parent = file_io_test_dir("private-abandoned-share");
    let staging = parent.join(".stage-999999999-0");
    create_dir_all(&staging).unwrap();
    let left = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    fs::File::open(&staging)
        .unwrap()
        .set_modified(left)
        .unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    let point = fs::canonicalize(&parent)
        .unwrap()
        .to_string_lossy()
        .replace(' ', "\\040");
    let share = format!("36 25 0:53 / {point} rw,relatime - nfs4 server:/export rw\n");

    remove_abandoned(&parent, ".stage", &share);
    let kept = staging.exists();
    remove_abandoned(&parent, ".stage", "");

    assert!(kept);
    assert!(!staging.exists());
    let _ = fs::remove_dir_all(parent);
}

#[test]
fn private_directory_reports_missing_parent() {
    let parent = file_io_test_dir("private-missing");
    let error = OwnedPrivateDirectory::create(&parent.join("missing"), ".stage").err();
    assert_eq!(
        error.map(|error| error.kind()),
        Some(std::io::ErrorKind::NotFound)
    );
    let _ = fs::remove_dir_all(parent);
}

/// Refusing a FIFO opens nothing: a writer waiting for a reader is not woken
/// and its message is not lost.
#[test]
fn refused_fifo_keeps_its_writer_waiting() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = file_io_test_dir("fifo-writer");
    let fifo = dir.join("fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (opened, woken) = std::sync::mpsc::channel();
    let writer = std::thread::spawn({
        let fifo = fifo.clone();
        move || {
            let file = fs::OpenOptions::new().write(true).open(&fifo);
            opened.send(()).unwrap();
            file
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(100));

    assert!(read_bounded(&fifo, 8).is_err());
    assert!(
        woken
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "the writer is still waiting for a reader"
    );

    // Release the writer: a reader of our own.
    let _reader = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    writer.join().unwrap().unwrap();
    let _ = fs::remove_dir_all(dir);
}

/// A device and a link to one are refused; a link to a regular file reads
/// that file, and the kernel's own files still read.
#[test]
fn bounded_read_takes_only_regular_files() {
    let dir = file_io_test_dir("bounded-kinds");
    let text = dir.join("text");
    fs::write(&text, b"hello").unwrap();
    for (link, target) in [
        ("to-null", Path::new("/dev/null")),
        ("to-text", text.as_path()),
    ] {
        std::os::unix::fs::symlink(target, dir.join(link)).unwrap();
    }

    for path in [Path::new("/dev/null"), &dir.join("to-null")] {
        let error = read_bounded(path, 8).unwrap_err();
        assert!(
            matches!(&error, BigBoundedReadError::Io(error) if error.kind() == std::io::ErrorKind::InvalidInput),
            "{}: {error}",
            path.display()
        );
    }
    assert_eq!(read_bounded(&dir.join("to-text"), 8).unwrap(), b"hello");
    assert!(
        !read_bounded(Path::new("/proc/self/stat"), 4096)
            .unwrap()
            .is_empty()
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn system_words_drop_errno_tail() {
    assert_eq!(
        system_words(&std::io::Error::from_raw_os_error(libc::ENOENT)),
        "No such file or directory"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn xattr_preflight_sees_user_attribute() {
    let dir = file_io_test_dir("xattr");
    let path = dir.join("document");
    fs::write(&path, b"text").unwrap();
    assert!(!has_extended_attributes(&path).unwrap());

    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: live NUL-terminated strings and a live value buffer.
    let set = unsafe {
        libc::setxattr(
            name.as_ptr(),
            c"user.test".as_ptr(),
            b"1".as_ptr().cast(),
            1,
            0,
        )
    };
    assert_eq!(set, 0, "the test folder must take user attributes");

    assert!(has_extended_attributes(&path).unwrap());
    assert!(has_extended_attributes(&dir.join("missing")).is_err());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn file_listings_are_sorted_real_files() {
    let dir = file_io_test_dir("extension");
    let a = dir.join("a.toml");
    let b = dir.join("b.toml");
    let ignored = dir.join("ignored.txt");
    fs::write(&b, "name = \"b\"\n").unwrap();
    fs::write(&a, "name = \"a\"\n").unwrap();
    fs::write(&ignored, "ignored").unwrap();

    assert_eq!(read_to_string(&a).unwrap(), "name = \"a\"\n");
    assert_eq!(
        directory_file_paths_with_extension(&dir, "toml").unwrap(),
        vec![a, b]
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn file_predicates_tell_kinds_apart() {
    let dir = file_io_test_dir("predicates");
    let regular = dir.join("regular");
    let executable = dir.join("executable");
    let missing = dir.join("missing");
    fs::write(&regular, "plain").unwrap();
    fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
    }

    assert!(is_regular_file(&regular));
    assert!(!is_regular_file(&dir));
    assert!(!is_regular_file(&missing));
    assert_eq!(is_executable_file(&regular), !cfg!(unix));
    assert_eq!(is_executable_file(&executable), cfg!(unix));
    assert!(!is_executable_file(&dir));
    assert!(!is_executable_file(&missing));

    let _ = fs::remove_dir_all(dir);
}

/// A file this user may not read keeps the kernel's answer: it is neither a
/// missing `/proc` nor a missing file.
#[test]
fn unreadable_file_keeps_its_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = file_io_test_dir("unreadable");
    let path = dir.join("secret");
    fs::write(&path, "x").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();

    let opened = open_regular(&path).map(drop).map_err(|error| error.kind());
    let read = read_bounded_if_exists(&path, 16).map_err(|error| error.kind());

    let _ = fs::remove_dir_all(dir);
    assert_eq!(opened, Err(std::io::ErrorKind::PermissionDenied));
    assert_eq!(read, Err(std::io::ErrorKind::PermissionDenied));
}

#[test]
fn create_dir_keeps_taken_name() {
    let dir = file_io_test_dir("create-dir");
    let made = dir.join("made");

    create_dir(&made).unwrap();
    let again = create_dir(&made).map_err(|error| error.kind());

    let made_directory = made.is_dir();
    let _ = fs::remove_dir_all(dir);
    assert!(made_directory);
    assert_eq!(again, Err(std::io::ErrorKind::AlreadyExists));
}

#[test]
fn read_and_remove_surface_real_errors() {
    let dir = file_io_test_dir("remove");
    let path = dir.join("file");
    let missing = dir.join("missing");
    fs::write(&path, "x").unwrap();

    check_readable_file(&path).unwrap();
    assert!(check_readable_file(&missing).is_err());
    remove_file_if_exists(&path).unwrap();
    assert!(!path.exists());
    remove_file_if_exists(&path).unwrap();
    assert!(remove_file_if_exists(&dir).is_err());

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn directory_entries_bounded_before_allocation() {
    let dir = file_io_test_dir("bounded-directory");
    fs::write(dir.join("a"), "a").unwrap();
    fs::write(dir.join("b"), "b").unwrap();

    let error = directory_entry_paths_with_limit(&dir, 1).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("1-entry safety limit"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn rename_replaces_no_replace_keeps() {
    let dir = file_io_test_dir("rename-no-replace");
    let first = dir.join("first");
    let second = dir.join("second");
    let target = dir.join("target");
    fs::write(&first, "first").unwrap();

    rename_no_replace(&first, &target).unwrap();

    assert!(!first.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "first");
    fs::write(&second, "second").unwrap();

    let error = rename_no_replace(&second, &target).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&second).unwrap(), "second");
    assert_eq!(fs::read_to_string(&target).unwrap(), "first");

    rename(&second, &target).unwrap();

    assert!(!second.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "second");
    let _ = fs::remove_dir_all(dir);
}

/// Where `renameat2` takes no flags, a file is renamed through a link, and a
/// taken name is refused with both files as they were.
#[test]
fn link_rename_keeps_taken_name() {
    let dir = file_io_test_dir("rename-by-link");
    let first = dir.join("first");
    let second = dir.join("second");
    let target = dir.join("target");
    fs::write(&first, "first").unwrap();

    rename_by_link(&first, &target).unwrap();

    assert!(!first.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "first");
    fs::write(&second, "second").unwrap();

    let error = rename_by_link(&second, &target).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&second).unwrap(), "second");
    assert_eq!(fs::read_to_string(&target).unwrap(), "first");
    let _ = fs::remove_dir_all(dir);
}

/// A source that cannot leave its folder (not writable here) stays under its
/// one name: the link just made under the new name goes again.
#[test]
fn link_rename_undoes_its_link() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = file_io_test_dir("rename-by-link-stuck");
    let closed = dir.join("closed");
    create_dir_all(&closed).unwrap();
    let source = closed.join("source");
    fs::write(&source, "kept").unwrap();
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o500)).unwrap();
    let target = dir.join("target");

    let moved = rename_by_link(&source, &target);

    fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        moved.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::PermissionDenied)
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), "kept");
    assert!(!target.exists());
    let _ = fs::remove_dir_all(dir);
}

/// Where `renameat2` takes no flag, a folder is refused with the kernel's
/// `EINVAL` and stays: a folder cannot be linked.
#[test]
fn folder_rename_without_renameat2_is_refused() {
    let dir = file_io_test_dir("rename-folder-no-flag");
    let folder = dir.join("folder");
    create_dir_all(&folder).unwrap();
    let target = dir.join("target");
    let moved = std::thread::spawn({
        let (folder, target) = (folder.clone(), target.clone());
        move || {
            crate::storage::tests::refuse_on_this_thread(libc::SYS_renameat2, libc::EINVAL);
            rename_no_replace(&folder, &target).map_err(|error| error.raw_os_error())
        }
    })
    .join()
    .unwrap();
    assert_eq!(moved, Err(Some(libc::EINVAL)));
    assert!(folder.is_dir() && !target.exists());
    let _ = fs::remove_dir_all(dir);
}

/// A lock file is made private, and a second name for it is refused: a
/// lock reached through a hard link may be some other file of the user's.
#[test]
fn lock_file_private_and_singly_linked() {
    use std::os::unix::fs::MetadataExt;
    let dir = std::env::temp_dir().join(format!("big-os-kit-lock-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("made.lock");
    open_lock_file(&path).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);

    let linked = dir.join("linked.lock");
    fs::hard_link(&path, &linked).unwrap();
    let refused = open_lock_file(&linked).map(drop);
    assert_eq!(
        refused.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::PermissionDenied)
    );
    fs::remove_dir_all(&dir).unwrap();
}

/// A tree a confined program left: folders closed to everyone, a chain nested
/// past the kernel's 4096-byte path, and a link to a folder outside, which
/// keeps its own mode and content.
#[test]
fn remove_tree_takes_hostile_folders() {
    use std::os::unix::fs::PermissionsExt;
    let root = file_io_test_dir("remove-tree");
    let outside = file_io_test_dir("remove-tree-outside");
    fs::write(outside.join("kept"), b"kept").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o500)).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    let long = "d".repeat(200);
    let mut deep = std::fs::File::open(&root).unwrap();
    for _ in 0..25 {
        rustix::fs::mkdirat(&deep, long.as_str(), Mode::RWXU).unwrap();
        deep = rustix::fs::openat(
            &deep,
            long.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .unwrap()
        .into();
    }
    rustix::fs::mkdirat(&deep, "closed", Mode::RWXU).unwrap();
    rustix::fs::chmodat(&deep, "closed", Mode::empty(), AtFlags::empty()).unwrap();
    drop(deep);
    fs::create_dir(root.join("shut")).unwrap();
    fs::write(root.join("shut/page.png"), b"page").unwrap();
    fs::set_permissions(root.join("shut"), fs::Permissions::from_mode(0o000)).unwrap();

    let removed = remove_tree(&root);
    let left = root.exists();
    let outside_mode = fs::metadata(&outside).unwrap().permissions().mode() & 0o777;
    let kept = fs::read(outside.join("kept")).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(&outside).unwrap();
    removed.unwrap();
    assert!(!left);
    assert_eq!((outside_mode, kept.as_slice()), (0o500, b"kept".as_slice()));
}

/// A folder moved out from under the walk while it empties it stops the
/// removal there; it never climbs out of the tree into a sibling's files.
#[test]
fn remove_tree_stays_inside() {
    let parent = file_io_test_dir("remove-tree-moved");
    let root = parent.join("root");
    // Listed in creation order on the usual filesystems, and the walk takes
    // the last folder first: `o` waits while `a` is emptied.
    fs::create_dir_all(root.join("o")).unwrap();
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::create_dir_all(parent.join("o")).unwrap();
    fs::create_dir(parent.join("a")).unwrap();
    fs::write(parent.join("o/victim"), b"kept").unwrap();
    for index in 0..20_000 {
        fs::write(root.join(format!("a/b/{index}")), b"").unwrap();
    }
    // Move `b` once the walk is inside it, emptying it.
    let mover = {
        let root = root.clone();
        std::thread::spawn(move || {
            while fs::read_dir(root.join("a/b")).is_ok_and(|entries| entries.count() > 19_000) {}
            let _ = fs::rename(root.join("a/b"), root.join("b"));
        })
    };
    let _ = remove_tree(&root);
    mover.join().unwrap();
    let kept = parent.join("o/victim").exists();
    let _ = remove_tree(&parent);
    assert!(kept, "the walk deleted a file outside its tree");
}
