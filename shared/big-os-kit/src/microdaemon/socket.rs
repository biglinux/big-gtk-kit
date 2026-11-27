// SPDX-License-Identifier: MIT

#![allow(
    clippy::disallowed_methods,
    reason = "microdaemon socket owner performs the private endpoint lifecycle"
)]

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions, Permissions, TryLockError};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};

/// Bind a private, owner-only Unix socket without replacing a live daemon.
///
/// The parent directory is forced to mode `0700` and the socket to `0600`.
/// An existing socket is removed only when no listener accepts a connection.
///
/// # Errors
///
/// Returns [`io::ErrorKind::AddrInUse`] when a live listener already owns the
/// socket, or a filesystem/socket error when the private endpoint cannot be
/// prepared.
pub fn bind_owner_socket(socket_path: &Path) -> io::Result<UnixListener> {
    let Some(parent) = socket_path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path has no parent",
        ));
    };
    prepare_owner_directory(parent)?;
    let _bind_lock = SocketBindLock::acquire(socket_path)?;
    match fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "existing endpoint is not a Unix socket",
                ));
            }
            if metadata.uid() != current_effective_user_id() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "existing socket is owned by another user",
                ));
            }
            match connect_socket(socket_path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "active microdaemon socket already exists",
                    ));
                }
                // Only connection refusal establishes a stale local endpoint.
                // Resource/permission failures must not unlink a live daemon.
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(socket_path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = through_short_path(socket_path, |path| UnixListener::bind(path))?;
    fs::set_permissions(socket_path, Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Connect to the Unix socket at `socket_path`, however long the path.
///
/// # Errors
///
/// Returns the error of opening the socket's directory or of connecting.
pub fn connect_socket(socket_path: &Path) -> io::Result<UnixStream> {
    through_short_path(socket_path, |path| UnixStream::connect(path))
}

/// `sun_path` holds 107 path bytes. A longer path is passed as
/// `/proc/self/fd/N/NAME`, through a descriptor of its directory, which names
/// the same file.
fn through_short_path<T>(
    socket_path: &Path,
    use_path: impl FnOnce(&Path) -> io::Result<T>,
) -> io::Result<T> {
    const SUN_PATH_BYTES: usize = 107;
    if socket_path.as_os_str().len() <= SUN_PATH_BYTES {
        return use_path(socket_path);
    }
    let (Some(parent), Some(name)) = (socket_path.parent(), socket_path.file_name()) else {
        return use_path(socket_path);
    };
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(parent)?;
    use_path(
        &Path::new("/proc/self/fd")
            .join(directory.as_raw_fd().to_string())
            .join(name),
    )
}

/// Create or validate an owner-only directory used for microdaemon state.
///
/// Existing symlinks, non-directories, and directories owned by another user
/// are rejected before permissions are changed.
///
/// # Errors
///
/// Returns a filesystem, ownership, or path-type error when the private
/// directory cannot be safely prepared.
pub fn prepare_owner_directory(directory: &Path) -> io::Result<()> {
    if !directory.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "owner directory must be absolute",
        ));
    }
    validate_or_create_owner_path(directory)?;
    fs::set_permissions(directory, Permissions::from_mode(0o700))
}

/// The user's runtime directory: `$XDG_RUNTIME_DIR` when it is absolute,
/// otherwise `biglinux-runtime` in the user's cache, made private.
///
/// The fallback goes through [`prepare_owner_directory`]: a symlink, or a
/// directory of another user's on the way, is refused rather than used.
///
/// # Errors
///
/// Returns the [`prepare_owner_directory`] error when the fallback directory
/// cannot be created or is not the current user's.
pub fn user_runtime_directory() -> io::Result<PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(directory);
    }
    let fallback = crate::settings::cache_home().join("biglinux-runtime");
    prepare_owner_directory(&fallback)?;
    Ok(fallback)
}

/// Whether this process may hand a daemon to the person's systemd user
/// manager: its runtime directory is the manager's and its session bus, when
/// it names one, is the manager's bus there. A session given a private runtime
/// directory or bus (a hidden test session) starts its daemons as its own
/// children instead; the manager would run them with the person's home,
/// journal and Trash.
#[must_use]
pub fn runs_under_user_manager() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|runtime| {
        belongs_to_user_manager(
            Path::new(&runtime),
            std::env::var_os("DBUS_SESSION_BUS_ADDRESS").as_deref(),
            current_effective_user_id(),
        )
    })
}

fn belongs_to_user_manager(runtime: &Path, bus: Option<&OsStr>, uid: libc::uid_t) -> bool {
    let manager = Path::new("/run/user").join(uid.to_string());
    let manager_bus = manager.join("bus");
    // A D-Bus address lists `transport:key=value,…` entries separated by `;`.
    let on_manager_bus = |address: &str| {
        address.split(';').any(|entry| {
            entry.strip_prefix("unix:").is_some_and(|keys| {
                keys.split(',').any(|key| {
                    key.strip_prefix("path=")
                        .is_some_and(|path| Path::new(path) == manager_bus)
                })
            })
        })
    };
    runtime == manager
        && bus
            .filter(|address| !address.is_empty())
            .is_none_or(|address| address.to_str().is_some_and(on_manager_bus))
}

fn validate_or_create_owner_path(directory: &Path) -> io::Result<()> {
    let effective_uid = current_effective_user_id();
    let mut current = PathBuf::new();
    let mut entered_owner_tree = false;
    for component in directory.components() {
        match component {
            Component::RootDir => current.push(component),
            Component::Normal(_) => {
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => {
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                format!(
                                    "owner directory ancestor is not a real directory: {}",
                                    current.display()
                                ),
                            ));
                        }
                        let owner = metadata.uid();
                        if owner != 0 && owner != effective_uid {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                format!(
                                    "owner directory ancestor belongs to another user: {}",
                                    current.display()
                                ),
                            ));
                        }
                        if entered_owner_tree && owner != effective_uid {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "owner directory escaped its user-owned ancestor",
                            ));
                        }
                        entered_owner_tree |= owner == effective_uid;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        match fs::DirBuilder::new().mode(0o700).create(&current) {
                            Ok(()) => entered_owner_tree = true,
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                                let metadata = fs::symlink_metadata(&current)?;
                                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidInput,
                                        "racing owner path is not a real directory",
                                    ));
                                }
                                let owner = metadata.uid();
                                if owner != effective_uid && (owner != 0 || entered_owner_tree) {
                                    return Err(io::Error::new(
                                        io::ErrorKind::PermissionDenied,
                                        "racing owner path has unsafe ownership",
                                    ));
                                }
                                entered_owner_tree |= owner == effective_uid;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "owner directory contains unsupported path components",
                ));
            }
        }
    }
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.uid() != effective_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "owner directory is owned by another user",
        ));
    }
    Ok(())
}

struct SocketBindLock {
    file: File,
}

impl SocketBindLock {
    fn acquire(socket_path: &Path) -> io::Result<Self> {
        let path = socket_path.with_extension(format!(
            "{}.lock",
            socket_path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("socket")
        ));
        let file = crate::file_io::open_lock_file(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another process is binding the microdaemon socket",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
        Ok(Self { file })
    }
}

impl Drop for SocketBindLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Return whether a Unix-stream peer has the same effective user as the daemon.
///
/// # Errors
///
/// Returns an OS error when peer credentials cannot be queried.
pub fn is_current_user_peer(stream: &UnixStream) -> io::Result<bool> {
    Ok(peer_credentials(stream.as_fd())?.uid == current_effective_user_id())
}

/// The process id of the peer of a connected Unix socket, as it was when the
/// peer connected. A process outside this PID namespace reads as 0.
///
/// # Errors
///
/// Returns an OS error when `socket` is no connected Unix socket.
pub fn peer_process_id(socket: BorrowedFd<'_>) -> io::Result<libc::pid_t> {
    Ok(peer_credentials(socket)?.pid)
}

/// Return the daemon process effective user id used for endpoint ownership.
#[must_use]
pub fn current_effective_user_id() -> libc::uid_t {
    rustix::process::geteuid().as_raw()
}

// Not rustix's `socket_peercred`: its `UCred` holds a non-zero `Pid`, and a peer
// outside this PID namespace reads as pid 0.
fn peer_credentials(socket: BorrowedFd<'_>) -> io::Result<libc::ucred> {
    // SAFETY: zeroed `ucred` is immediately initialized by `getsockopt`.
    let mut credentials = unsafe { std::mem::zeroed::<libc::ucred>() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the file descriptor and output pointers are valid for `length`.
    let result = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result == 0 {
        Ok(credentials)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn socket_path(name: &str) -> std::path::PathBuf {
        // In `/tmp`: a socket path must fit `sun_path` (108 bytes), and `TMPDIR` can be long.
        std::path::Path::new("/tmp").join(format!(
            "big-os-kit-{name}-{}-{}.d/daemon.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ))
    }

    #[test]
    fn private_session_starts_own_daemon() {
        let bus = |address: &str| Some(OsStr::new(address).to_owned());
        for (runtime, address, managed) in [
            ("/run/user/1000", None, true),
            ("/run/user/1000/", bus(""), true),
            ("/run/user/1000", bus("unix:path=/run/user/1000/bus"), true),
            (
                "/run/user/1000",
                bus("unix:guid=ab,path=/run/user/1000/bus"),
                true,
            ),
            ("/run/user/1001", None, false),
            ("/home/p/.cache/test-runtime", None, false),
            (
                "/run/user/1000",
                bus("unix:path=/run/user/1000/gui-session.x/bus"),
                false,
            ),
            ("/run/user/1000", bus("unix:tmpdir=/dev/shm/bg.x"), false),
        ] {
            assert_eq!(
                belongs_to_user_manager(Path::new(runtime), address.as_deref(), 1000),
                managed,
                "{runtime} {address:?}"
            );
        }
    }

    #[test]
    fn owner_socket_is_private() {
        let path = socket_path("private-socket");
        let listener = bind_owner_socket(&path).expect("bind owner socket");
        let client = UnixStream::connect(&path).expect("connect owner client");
        let (server, _) = listener.accept().expect("accept owner client");

        assert_eq!(
            fs::metadata(path.parent().expect("parent"))
                .expect("parent metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(is_current_user_peer(&server).expect("peer credentials"));

        drop(client);
        drop(server);
        drop(listener);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn long_socket_path_is_reachable() {
        let path = socket_path(&"deep".repeat(20));
        assert!(path.as_os_str().len() > 107, "{}", path.display());

        let listener = bind_owner_socket(&path).expect("bind past sun_path");
        let client = connect_socket(&path).expect("connect past sun_path");
        let (server, _) = listener.accept().expect("accept");

        assert!(is_current_user_peer(&server).expect("peer credentials"));
        drop(client);
        drop(listener);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn live_socket_is_kept() {
        let path = socket_path("live-socket");
        let listener = bind_owner_socket(&path).expect("bind first socket");

        let error = bind_owner_socket(&path).expect_err("live socket must survive");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(UnixStream::connect(&path).is_ok());
        drop(listener);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn stale_socket_is_replaced() {
        let path = socket_path("stale-socket");
        let listener = bind_owner_socket(&path).expect("bind stale socket");
        drop(listener);

        let rebound = bind_owner_socket(&path).expect("replace stale socket");

        assert!(UnixStream::connect(&path).is_ok());
        drop(rebound);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn regular_file_at_socket_is_kept() {
        let path = socket_path("regular-file");
        prepare_owner_directory(path.parent().expect("parent")).expect("private parent");
        fs::write(&path, b"keep me").expect("regular endpoint fixture");

        let error = bind_owner_socket(&path).expect_err("regular file must be preserved");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&path).expect("preserved file"), b"keep me");
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn concurrent_binds_have_exactly_one_winner() {
        let path = socket_path("concurrent-bind");
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let threads = (0..2)
            .map(|_| {
                let path = path.clone();
                let start = start.clone();
                let release = release.clone();
                let result_sender = result_sender.clone();
                std::thread::spawn(move || {
                    start.wait();
                    match bind_owner_socket(&path) {
                        Ok(listener) => {
                            result_sender.send(Ok(())).expect("send bind result");
                            while !release.load(std::sync::atomic::Ordering::Acquire) {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                            drop(listener);
                        }
                        Err(error) => result_sender
                            .send(Err(error.kind()))
                            .expect("send bind error"),
                    }
                })
            })
            .collect::<Vec<_>>();
        drop(result_sender);
        start.wait();
        let results = (0..2)
            .map(|_| result_receiver.recv_timeout(std::time::Duration::from_secs(2)))
            .collect::<Result<Vec<_>, _>>();
        release.store(true, std::sync::atomic::Ordering::Release);
        for thread in threads {
            thread.join().expect("bind thread");
        }
        let results = results.expect("concurrent bind results");

        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "concurrent bind results: {results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(io::ErrorKind::AddrInUse)))
                .count(),
            1
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    /// A socket this user cannot connect to (mode 000) may still have a
    /// listener: it is kept, and the bind refused.
    #[test]
    fn unreachable_socket_is_kept() {
        let path = socket_path("unreachable-socket");
        let listener = bind_owner_socket(&path).expect("bind first socket");
        fs::set_permissions(&path, Permissions::from_mode(0o000)).expect("mode 000");

        let error = bind_owner_socket(&path).expect_err("unreachable socket must survive");

        fs::set_permissions(&path, Permissions::from_mode(0o600)).expect("mode 600");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            UnixStream::connect(&path).is_ok(),
            "the listener still owns it"
        );
        drop(listener);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    /// An existing directory open to others is made private; a relative one
    /// is refused before anything is made.
    #[test]
    fn owner_directory_is_absolute_and_private() {
        let path = socket_path("open-directory");
        let directory = path.parent().expect("parent");
        fs::DirBuilder::new()
            .mode(0o755)
            .create(directory)
            .expect("an open directory");
        fs::set_permissions(directory, Permissions::from_mode(0o755)).expect("mode 755");

        prepare_owner_directory(directory).expect("made private");

        let mode = fs::metadata(directory)
            .expect("directory")
            .permissions()
            .mode();
        let _ = fs::remove_dir_all(directory);
        assert_eq!(mode & 0o777, 0o700);
        let relative = Path::new("big-os-kit-relative-owner-directory");
        let error = prepare_owner_directory(relative).expect_err("relative refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!relative.exists());
    }

    #[test]
    fn owner_directory_rejects_symlink_ancestor() {
        let generated = socket_path("ancestor-symlink");
        let root = generated.parent().expect("generated parent");
        let real = root.with_extension("real");
        let linked = root.with_extension("linked");
        fs::create_dir_all(&real).expect("real ancestor");
        std::os::unix::fs::symlink(&real, &linked).expect("ancestor symlink");

        let error = prepare_owner_directory(&linked.join("child"))
            .expect_err("symlink ancestor must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_file(&linked);
        let _ = fs::remove_dir(&real);
    }

    /// Without `XDG_RUNTIME_DIR` the runtime directory is a private one in
    /// the user's cache, out of the temporary folder every account can write
    /// to. Checked in a child run of this test, which gets that environment.
    #[test]
    fn runtime_fallback_lives_in_cache() {
        if let Some(cache) = std::env::var_os("BIG_OS_KIT_CHILD_CACHE") {
            let directory = user_runtime_directory().expect("a runtime directory");
            assert_eq!(directory, PathBuf::from(cache).join("biglinux-runtime"));
            let mode = fs::metadata(&directory)
                .expect("created")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
            return;
        }
        let cache = socket_path("runtime-fallback");
        let cache = cache.parent().expect("a directory");
        let child = crate::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "microdaemon::socket::tests::runtime_fallback_lives_in_cache",
                "--quiet",
            ])
            .env_inherit(false)
            .env("BIG_OS_KIT_CHILD_CACHE", cache)
            .env("XDG_CACHE_HOME", cache)
            .build()
            .run()
            .expect("child run");
        let _ = fs::remove_dir_all(cache);
        assert!(child.status.success(), "{}", child.stdout_lossy());
    }

    /// The peer of a socket pair is this process; a descriptor that is no
    /// socket is refused, never read as pid 0.
    #[test]
    fn peer_process_id_reads_the_peer() {
        let (ours, _theirs) = UnixStream::pair().expect("pair");
        let pid = peer_process_id(ours.as_fd()).expect("peer pid");
        assert_eq!(u32::try_from(pid).ok(), Some(std::process::id()));
        let file = File::open("/proc/self/stat").expect("file");
        assert!(peer_process_id(file.as_fd()).is_err());
    }
}
