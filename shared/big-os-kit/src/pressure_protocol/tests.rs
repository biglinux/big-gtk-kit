// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
use super::*;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // In `/tmp`: a socket path must fit `sun_path` (108 bytes), and `TMPDIR` can be long.
        let path = PathBuf::from("/tmp").join(format!(
            "big-pressure-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("watch")
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn config(path: &Path) -> PressureConfig {
    PressureConfig {
        path: path.to_owned(),
        trigger: Vec::new(),
    }
}

#[test]
fn absent_empty_disabled_and_relative_watch() {
    for watch in [None, Some(OsStr::new("")), Some(OsStr::new("/dev/null"))] {
        assert!(PressureConfig::from_values(watch, None).unwrap().is_none());
    }
    assert!(PressureConfig::from_values(Some(OsStr::new("relative")), None).is_err());
}
#[test]
fn optional_and_binary_trigger() {
    let path = Some(OsStr::new("/sys/fs/cgroup/memory.pressure"));
    assert!(
        PressureConfig::from_values(path, None)
            .unwrap()
            .unwrap()
            .trigger
            .is_empty()
    );
    let trigger = base64::engine::general_purpose::STANDARD.encode(b"some 200000 2000000\0");
    assert_eq!(
        PressureConfig::from_values(path, Some(OsStr::new(&trigger)))
            .unwrap()
            .unwrap()
            .trigger,
        b"some 200000 2000000\0"
    );
    assert!(PressureConfig::from_values(path, Some(OsStr::new("%%%"))).is_err());
    assert!(PressureConfig::from_values(path, Some(OsStr::new(&"A".repeat(65537)))).is_err());
}
#[test]
fn psi_trigger_ends_in_nul() {
    // Without the NUL the kernel refuses the trigger with EINVAL.
    let config = PressureConfig::psi(Path::new("/proc/pressure/memory"), "some 150000 2000000");
    assert_eq!(config.trigger, b"some 150000 2000000\0");
}
#[test]
fn regular_file_waits_on_priority() {
    let dir = TempDir::new();
    let path = dir.path();
    std::fs::write(&path, b"unchanged").unwrap();
    let endpoint = Endpoint::open(&config(&path)).unwrap();
    assert_eq!(endpoint.events(), PollFlags::PRI);
    assert_eq!(endpoint.consume(PollFlags::IN).unwrap(), (false, false));
    assert_eq!(endpoint.consume(PollFlags::PRI).unwrap(), (true, false));
    assert_eq!(
        // SAFETY: live file descriptor, SEEK_CUR observes the read cursor.
        unsafe { libc::lseek(endpoint.fd.as_raw_fd(), 0, libc::SEEK_CUR) },
        0
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}
#[test]
fn fifo_drains_without_repeated_events() {
    let dir = TempDir::new();
    let path = dir.path();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path in private test directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let endpoint = Endpoint::open(&config(&path)).unwrap();
    // SAFETY: live owned descriptor; observes flags without changing them.
    let flags = unsafe { libc::fcntl(endpoint.fd.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
    let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
    writer.write_all(b"pressure").unwrap();
    assert_eq!(endpoint.events(), PollFlags::IN);
    assert_eq!(endpoint.consume(PollFlags::IN).unwrap(), (true, false));
    assert_eq!(endpoint.consume(PollFlags::IN).unwrap(), (false, false));
}
#[test]
fn unix_socket_trigger_drains_until_eof() {
    let dir = TempDir::new();
    let path = dir.path();
    let listener = UnixListener::bind(&path).unwrap();
    let mut cfg = config(&path);
    cfg.trigger = vec![0, 255, 10];
    let endpoint = Endpoint::open(&cfg).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut bytes = [0; 3];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, [0, 255, 10]);
    peer.write_all(b"pressure").unwrap();
    assert_eq!(endpoint.consume(PollFlags::IN).unwrap(), (true, false));
    assert_eq!(endpoint.consume(PollFlags::IN).unwrap(), (false, false));
    drop(peer);
    assert_eq!(
        endpoint.consume(PollFlags::IN | PollFlags::HUP).unwrap(),
        (false, true)
    );
}
#[test]
fn dropping_watch_stops_worker() {
    let dir = TempDir::new();
    let path = dir.path();
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = PressureWatch::start(&config(&path), move || {
        let _ = tx.send(());
    })
    .unwrap();
    let active = watch.activity();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    peer.write_all(b"pressure").unwrap();
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(watch);
    assert!(!active.load(Ordering::Acquire));
    assert_eq!(peer.read(&mut [0_u8; 1]).unwrap(), 0);
    assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
}
#[test]
fn bad_socket_paths_fail_cleanly() {
    let dir = TempDir::new();
    assert!(Endpoint::open(&config(&dir.0)).is_err());
    assert!(Endpoint::open(&config(&dir.path())).is_err());
    assert!(connect_nonblocking(Path::new(&format!("/{}", "x".repeat(256)))).is_err());
}
#[test]
fn final_notification_survives_peer_close() {
    let dir = TempDir::new();
    let path = dir.path();
    let listener = UnixListener::bind(&path).unwrap();
    let endpoint = Endpoint::open(&config(&path)).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.write_all(b"last").unwrap();
    drop(peer);
    assert_eq!(
        endpoint.consume(PollFlags::IN | PollFlags::HUP).unwrap(),
        (true, true)
    );
}

#[test]
fn fifo_watch_survives_short_writers() {
    let dir = TempDir::new();
    let path = dir.path();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path in the private test directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let watch = PressureWatch::start(&config(&path), move || {
        let _ = tx.send(());
    })
    .unwrap();
    for _ in 0..3 {
        let mut writer = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        writer.write_all(b"pressure").unwrap();
        drop(writer);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    assert!(watch.activity().load(Ordering::Acquire));
    drop(watch);
    assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
}
