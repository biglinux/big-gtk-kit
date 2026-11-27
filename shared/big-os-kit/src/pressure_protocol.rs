// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! systemd pressure-service protocol, without GTK or a periodic polling timer.
//!
//! PSI files use POLLPRI and are never read. FIFO and AF_UNIX stream notifications
//! use POLLIN and are drained in bounded batches. Dropping a watch closes its
//! control socket and wakes the blocking poll, without joining on the UI thread.
//! See <https://systemd.io/PRESSURE/>.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use rustix::event::{PollFd, PollFlags, poll};
use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::net::{
    AddressFamily, SendFlags, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with,
};

/// The PID of the process in which one copy of the pressure watcher answers
/// for every copy, the module libraries' included (big-app-kit's
/// `memory_pressure::watch_for_process`).
pub const PROCESS_WATCHER: &str = "BIG_HOST_PRESSURE_WATCHER";

/// A pressure file to watch and the trigger written to it: offered by a service
/// manager, or a PSI file of the kernel's.
#[derive(Clone, Debug)]
pub struct PressureConfig {
    path: PathBuf,
    trigger: Vec<u8>,
}

impl PressureConfig {
    /// Parse values without mutating process-global environment in tests.
    /// An absent watch, an empty watch or `/dev/null` disables monitoring.
    ///
    /// # Errors
    /// Rejects relative paths, invalid base64 and oversized configuration data.
    pub fn from_values(watch: Option<&OsStr>, write: Option<&OsStr>) -> io::Result<Option<Self>> {
        let Some(watch) = watch else { return Ok(None) };
        if watch.is_empty() || watch == OsStr::new("/dev/null") {
            return Ok(None);
        }
        let path = PathBuf::from(watch);
        if !path.is_absolute() {
            return Err(invalid("pressure watch must be an absolute path"));
        }
        let trigger = match write {
            None => Vec::new(),
            Some(value) => {
                if value.as_bytes().len() > 64 * 1024 {
                    return Err(invalid("pressure trigger exceeds 64 KiB encoded"));
                }
                base64::engine::general_purpose::STANDARD
                    .decode(value.as_bytes())
                    .map_err(|_| invalid("invalid base64 pressure trigger"))?
            }
        };
        Ok(Some(Self { path, trigger }))
    }

    /// A PSI file watched with `trigger`, such as `"some 150000 2000000"`, for a
    /// watch no service manager offers: the whole system's `/proc/pressure/memory`.
    #[must_use]
    pub fn psi(path: &Path, trigger: &str) -> Self {
        // The kernel overwrites the last byte written with its terminator.
        let mut bytes = trigger.as_bytes().to_vec();
        bytes.push(0);
        Self {
            path: path.to_owned(),
            trigger: bytes,
        }
    }

    /// The whole system's memory stall: tasks stalled 150 ms within 2 s, the
    /// shortest window the kernel gives an unprivileged trigger.
    #[must_use]
    pub fn system_memory_stall() -> Self {
        Self::psi(Path::new("/proc/pressure/memory"), "some 150000 2000000")
    }

    /// Read the memory pressure variables once, during application setup.
    ///
    /// # Errors
    /// Returns the validation errors from [`Self::from_values`].
    pub fn from_environment() -> io::Result<Option<Self>> {
        Self::from_values(
            std::env::var_os(WATCH_VARIABLE).as_deref(),
            std::env::var_os("MEMORY_PRESSURE_WRITE").as_deref(),
        )
    }

    /// [`Self::from_environment`], or [`Self::system_memory_stall`] where no
    /// service manager set the watch; `None` where one turned it off, as
    /// systemd does with `/dev/null`.
    ///
    /// # Errors
    /// Returns the validation errors from [`Self::from_values`].
    pub fn from_environment_or_system() -> io::Result<Option<Self>> {
        if std::env::var_os(WATCH_VARIABLE).is_none() {
            return Ok(Some(Self::system_memory_stall()));
        }
        Self::from_environment()
    }
}

/// Where a service manager names the file to watch (systemd.exec(5)).
const WATCH_VARIABLE: &str = "MEMORY_PRESSURE_WATCH";

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Psi,
    Fifo,
    Socket,
}

struct Endpoint {
    fd: OwnedFd,
    kind: Kind,
}

impl Endpoint {
    fn open(config: &PressureConfig) -> io::Result<Self> {
        let kind = std::fs::metadata(&config.path)?.file_type();
        let mut endpoint = if kind.is_socket() {
            Self {
                fd: connect_nonblocking(&config.path)?,
                kind: Kind::Socket,
            }
        } else {
            if !kind.is_file() && !kind.is_fifo() {
                return Err(invalid("unsupported pressure transport"));
            }
            let file = OpenOptions::new()
                .read(true)
                // Without a trigger, do not demand write permission. A FIFO
                // remains nonblocking even before a publisher connects. When
                // writing a trigger, O_RDWR also prevents a FIFO SIGPIPE.
                .write(!config.trigger.is_empty())
                // std opens with O_CLOEXEC already.
                .custom_flags(OFlags::NONBLOCK.bits().cast_signed())
                .open(&config.path)?;
            let actual = file.metadata()?.file_type();
            if actual != kind {
                return Err(invalid("pressure transport changed during open"));
            }
            Self {
                fd: file.into(),
                kind: if kind.is_fifo() {
                    Kind::Fifo
                } else {
                    Kind::Psi
                },
            }
        };
        endpoint.write_trigger(&config.trigger)?;
        Ok(endpoint)
    }

    fn write_trigger(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            // NOSIGNAL keeps a closed socket peer from terminating the application.
            let written = if self.kind == Kind::Socket {
                rustix::net::send(&self.fd, bytes, SendFlags::NOSIGNAL)
            } else {
                rustix::io::write(&self.fd, bytes)
            };
            let written = match written {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => written,
                Err(Errno::INTR) => continue,
                // Never wait for a stuck peer during GTK startup. A full
                // transport is reported as unavailable instead of blocking.
                Err(error) => return Err(error.into()),
            };
            bytes = &bytes[written..];
        }
        Ok(())
    }

    fn events(&self) -> PollFlags {
        if self.kind == Kind::Psi {
            PollFlags::PRI
        } else {
            PollFlags::IN
        }
    }

    // Returns (pressure observed, transport closed). Drain data before HUP so
    // the last notification sent immediately before disconnect is not lost.
    fn consume(&self, events: PollFlags) -> io::Result<(bool, bool)> {
        let closed = events.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL);
        if self.kind == Kind::Psi {
            return Ok((events.contains(PollFlags::PRI) && !closed, closed));
        }
        if !events.contains(PollFlags::IN) {
            return Ok((false, closed));
        }
        let mut pressure = false;
        let mut buffer = [0_u8; 4096];
        // A peer sending continuously cannot delay shutdown indefinitely.
        for _ in 0..16 {
            match rustix::io::read(&self.fd, &mut buffer) {
                Ok(0) => return Ok((pressure, true)),
                Ok(_) => pressure = true,
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => return Ok((pressure, closed)),
                Err(error) => return Err(error.into()),
            }
        }
        Ok((pressure, closed))
    }
}

fn connect_nonblocking(path: &Path) -> io::Result<OwnedFd> {
    let address = SocketAddrUnix::new(path).map_err(|_| invalid("invalid pressure socket path"))?;
    let fd = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    connect(&fd, &address)?;
    Ok(fd)
}

/// Owned subscription. Drop wakes its worker without joining on the UI thread.
/// A callback already in progress may finish; UI owners must guard delivery.
pub struct PressureWatch {
    control: Option<UnixStream>,
    active: Arc<AtomicBool>,
}

impl PressureWatch {
    /// Start a single blocking watcher. `notify` runs off the GTK thread and
    /// must dispatch UI work rather than perform it directly.
    ///
    /// # Errors
    /// Returns transport, trigger-write, or worker creation errors.
    pub fn start(
        config: &PressureConfig,
        mut notify: impl FnMut() + Send + 'static,
    ) -> io::Result<Self> {
        let mut endpoint = Endpoint::open(config)?;
        let config = config.clone();
        let (control, stop) = UnixStream::pair()?;
        let active = Arc::new(AtomicBool::new(true));
        let worker_active = Arc::clone(&active);
        std::thread::Builder::new()
            .name("memory-pressure".into())
            .spawn(move || {
                struct InactiveOnDrop(Arc<AtomicBool>);
                impl Drop for InactiveOnDrop {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _inactive = InactiveOnDrop(worker_active.clone());
                loop {
                    let mut fds = [
                        PollFd::new(&endpoint.fd, endpoint.events()),
                        PollFd::new(&stop, PollFlags::IN),
                    ];
                    match poll(&mut fds, None) {
                        Ok(_) => {}
                        Err(Errno::INTR) => continue,
                        Err(_) => break,
                    }
                    let [watched, stopped] = fds.map(|fd| fd.revents());
                    if !stopped.is_empty() || !worker_active.load(Ordering::Acquire) {
                        break;
                    }
                    match endpoint.consume(watched) {
                        Ok((pressure, closed)) => {
                            if pressure {
                                notify();
                            }
                            if closed {
                                // A FIFO writer may connect for each individual
                                // notification. Reopening its read end clears
                                // Linux's persistent HUP without periodic wakeups.
                                // Open the replacement before dropping the old
                                // reader so a concurrent writer never loses both.
                                if endpoint.kind == Kind::Fifo
                                    && !watched.intersects(PollFlags::ERR | PollFlags::NVAL)
                                {
                                    match Endpoint::open(&config) {
                                        Ok(reopened) if reopened.kind == Kind::Fifo => {
                                            endpoint = reopened;
                                            continue;
                                        }
                                        _ => break,
                                    }
                                }
                                break;
                            }
                        }
                        Err(error) => {
                            log::debug!("pressure watch stopped: {error}");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            control: Some(control),
            active,
        })
    }

    /// Delivery guard for callbacks already queued to a main context.
    #[must_use]
    pub fn activity(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active)
    }
}

impl Drop for PressureWatch {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.control.take();
    }
}

#[cfg(test)]
mod tests;
