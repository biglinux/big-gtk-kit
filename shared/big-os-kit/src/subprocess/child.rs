// SPDX-License-Identifier: MIT

use super::{BigSubprocessError, BigSubprocessStatus};
use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};
use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, kill_process, kill_process_group, waitid,
};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Duration;

/// Handle to a running child spawned via [`crate::subprocess::BigSubprocessSpec::spawn`].
///
/// The caller owns the lifecycle: read the exposed pipes, then
/// [`Self::wait`] (or poll with [`Self::try_wait`]) or [`Self::kill`].
/// Dropping the handle without waiting leaves the child running and, on
/// Unix, unreaped until this process exits: call `wait`/`kill` explicitly.
#[derive(Clone, Debug)]
pub struct BigSubprocessChild {
    child: Arc<Mutex<std::process::Child>>,
    id: u32,
    unix_process_group: bool,
}

/// Hand a long-lived child to the process-wide detached-child reaper.
///
/// This is for children meant to outlive the call that launched them (desktop
/// applications, detached helpers). A dedicated waiter per child would retain one native
/// thread/stack for the entire child lifetime. The reaper owns all such handles from one
/// lazily-created thread, which sleeps until a child ends or a new one arrives.
///
/// If the reaper cannot be created or has failed, the child is killed and synchronously
/// reaped rather than being dropped into a zombie-prone state.
pub fn reap_detached(mut child: BigSubprocessChild) {
    static REAPER: OnceLock<Option<(mpsc::Sender<BigSubprocessChild>, Arc<OwnedFd>)>> =
        OnceLock::new();

    let reaper = REAPER.get_or_init(|| {
        let bell = match eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK) {
            Ok(bell) => Arc::new(bell),
            Err(error) => {
                log::warn!("could not start detached-child reaper: {error}");
                return None;
            }
        };
        let (sender, receiver) = mpsc::channel();
        let rung = Arc::clone(&bell);
        match std::thread::Builder::new()
            .name("big-subprocess-reaper".to_owned())
            .spawn(move || child_reaper(&receiver, &rung))
        {
            Ok(_) => Some((sender, bell)),
            Err(error) => {
                log::warn!("could not start detached-child reaper: {error}");
                None
            }
        }
    });

    if let Some((sender, bell)) = reaper
        && sender.send(child.clone()).is_ok()
    {
        let _ = rustix::io::write(bell.as_ref(), &1_u64.to_ne_bytes());
        return;
    }

    // SIGKILL, which no child can ignore, so the wait below ends.
    match child.kill() {
        Ok(()) => {
            let _ = child.wait();
        }
        Err(error) => log::warn!("could not stop a detached child nobody can reap: {error}"),
    }
}

/// How often to look at a child the kernel could not give a pidfd for.
///
/// Only then: every other child wakes the reaper through its pidfd when it ends,
/// so a session with a terminal daemon alive all day does not wake it ten
/// times a second to ask.
const POLL_WITHOUT_PIDFD: Duration = Duration::from_millis(100);

/// A detached child and the descriptor that becomes readable when it ends.
struct Watched {
    child: BigSubprocessChild,
    pidfd: Option<OwnedFd>,
}

fn watch(child: BigSubprocessChild) -> Watched {
    let pidfd = pidfd(child.id());
    Watched { child, pidfd }
}

/// A descriptor that becomes readable when the child `pid` ends, or `None`
/// where the kernel has no `pidfd_open`.
///
/// Only for a child of ours not yet reaped: its pid cannot have been reused.
pub(super) fn pidfd(pid: u32) -> Option<OwnedFd> {
    (pid > 0).then(|| crate::pidfd::open(pid).ok()).flatten()
}

/// Sleep until a child of ours ends, instead of sleeping a fixed step and
/// asking `try_wait` again.
///
/// Open it once per child, before the loop that waits: the pidfd becomes
/// readable the moment the child exits, so the loop wakes then, or after the
/// longest nap the caller allows for things that cannot wake it (a cancel flag,
/// a deadline). Without `pidfd_open` it naps the fallback step.
#[derive(Debug)]
pub struct ExitWatch(Option<OwnedFd>);

impl ExitWatch {
    /// Watch the child `pid`, which must be ours and not yet reaped.
    #[must_use]
    pub fn new(pid: u32) -> Self {
        Self(pidfd(pid))
    }

    /// Sleep until the child ends or `at_most` passes, whichever is first; the
    /// caller asks `try_wait` next either way.
    pub fn sleep(&self, at_most: Duration) {
        sleep_until_exit(self.0.as_ref(), at_most, Duration::from_millis(20));
    }
}

/// Sleep until the process behind `pidfd` ends or `timeout` passes; with no
/// pidfd, just sleep `fallback`. Either way the caller asks `try_wait` next.
pub(super) fn sleep_until_exit(pidfd: Option<&OwnedFd>, timeout: Duration, fallback: Duration) {
    let Some(pidfd) = pidfd else {
        std::thread::sleep(fallback.min(timeout));
        return;
    };
    let timeout = Timespec::try_from(timeout).unwrap_or(Timespec {
        tv_sec: i64::MAX,
        tv_nsec: 0,
    });
    let _ = poll(&mut [PollFd::new(pidfd, PollFlags::IN)], Some(&timeout));
}

fn child_reaper(receiver: &mpsc::Receiver<BigSubprocessChild>, bell: &OwnedFd) {
    let mut children = Vec::<Watched>::new();
    loop {
        while let Ok(child) = receiver.try_recv() {
            children.push(watch(child));
        }

        let mut index = 0;
        while index < children.len() {
            match children[index].child.try_wait() {
                Ok(Some(_)) => {
                    children.swap_remove(index);
                }
                Ok(None) => index += 1,
                Err(error) => {
                    log::warn!("could not reap detached child: {error}");
                    children.swap_remove(index);
                }
            }
        }

        let mut watched: Vec<PollFd<'_>> = std::iter::once(bell)
            .chain(children.iter().filter_map(|watched| watched.pidfd.as_ref()))
            .map(|fd| PollFd::new(fd, PollFlags::IN))
            .collect();
        let without_pidfd = Timespec::try_from(POLL_WITHOUT_PIDFD).ok();
        let timeout = if children.iter().all(|watched| watched.pidfd.is_some()) {
            None
        } else {
            without_pidfd.as_ref()
        };
        let _ = poll(&mut watched, timeout);
        if watched[0].revents().contains(PollFlags::IN) {
            let mut rung = [0_u8; 8];
            let _ = rustix::io::read(bell, &mut rung);
        }
    }
}

impl BigSubprocessChild {
    pub(super) fn new(child: std::process::Child, unix_process_group: bool) -> Self {
        Self {
            id: child.id(),
            child: Arc::new(Mutex::new(child)),
            unix_process_group,
        }
    }

    /// OS process id of the child.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// Take the captured stdout pipe (present only when the spec's stdout
    /// mode is [`crate::subprocess::BigSubprocessOutputMode::Capture`]).
    /// Returns `None` after the first take.
    #[must_use]
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.lock_child().ok()?.stdout.take()
    }

    /// Take the captured stderr pipe; see [`Self::take_stdout`].
    #[must_use]
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.lock_child().ok()?.stderr.take()
    }

    /// Take the stdin pipe (present only when the spec set a `stdin`
    /// payload). Returns `None` after the first take, and once any clone of
    /// this handle has waited: [`Self::wait`] closes a stdin nobody took.
    #[must_use]
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.lock_child().ok()?.stdin.take()
    }

    /// Poll without blocking. Returns `Ok(None)` while the child runs.
    ///
    /// # Errors
    ///
    /// Returns [`BigSubprocessError::Io`] on a wait syscall failure.
    pub fn try_wait(&mut self) -> Result<Option<BigSubprocessStatus>, BigSubprocessError> {
        self.lock_child()?
            .try_wait()
            .map(|opt| opt.map(|status| BigSubprocessStatus::Exited(status.code())))
            .map_err(BigSubprocessError::Io)
    }

    /// Block until the child exits, first closing a stdin pipe nobody took,
    /// as [`std::process::Child::wait`] does. Every clone shares that pipe: a
    /// reaper thread that waits on a clone at once closes it for the owner,
    /// who takes stdin with [`Self::take_stdin`] before handing the clone over.
    ///
    /// Polls with short lock holds instead of a blocking `wait(2)` under the
    /// handle lock, so a concurrent [`Self::terminate`]/[`Self::kill`] from a
    /// cancellation hook is never blocked behind a waiter (clone-handle
    /// deadline-kill path).
    ///
    /// # Errors
    ///
    /// Returns [`BigSubprocessError::Io`] on a wait syscall failure.
    pub fn wait(&mut self) -> Result<BigSubprocessStatus, BigSubprocessError> {
        drop(self.lock_child()?.stdin.take());
        // Blocks on a pidfd rather than on the child itself: the lock stays
        // free, so another holder can still kill it.
        let pidfd = pidfd(self.id());
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            sleep_until_exit(pidfd.as_ref(), Duration::MAX, Duration::from_millis(5));
        }
    }

    /// Kill the child (best-effort; idempotent on an already-exited child).
    ///
    /// # Errors
    ///
    /// Returns [`BigSubprocessError::Io`] if the kill syscall fails for a
    /// still-running child.
    pub fn kill(&mut self) -> Result<(), BigSubprocessError> {
        #[cfg(unix)]
        if self.unix_process_group {
            return self.signal_process_group(Signal::KILL);
        }
        self.lock_child()?.kill().map_err(BigSubprocessError::Io)
    }

    /// Request graceful child termination with SIGTERM on Unix.
    ///
    /// # Errors
    ///
    /// Returns [`BigSubprocessError::Io`] when the process handle cannot be
    /// inspected or the signal syscall fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use big_os_kit::subprocess::BigSubprocessSpec;
    ///
    /// let mut child = BigSubprocessSpec::builder()
    ///     .program("sleep")
    ///     .arg("60")
    ///     .build()
    ///     .spawn()
    ///     .unwrap();
    /// child.terminate().unwrap();
    /// let _ = child.wait();
    /// ```
    pub fn terminate(&mut self) -> Result<(), BigSubprocessError> {
        #[cfg(unix)]
        {
            if self.unix_process_group {
                self.signal_process_group(Signal::TERM)
            } else {
                self.signal_child(Signal::TERM)
            }
        }
        #[cfg(not(unix))]
        {
            self.kill()
        }
    }

    fn lock_child(&self) -> Result<MutexGuard<'_, std::process::Child>, BigSubprocessError> {
        self.child
            .lock()
            .map_err(|_| BigSubprocessError::Io(std::io::Error::other("child lock poisoned")))
    }

    #[cfg(unix)]
    fn signal_child(&self, signal: Signal) -> Result<(), BigSubprocessError> {
        let mut child = self.lock_child()?;
        if child.try_wait().map_err(BigSubprocessError::Io)?.is_some() {
            return Ok(());
        }
        // The lock and the `try_wait` above keep the pid this child's: nobody
        // else reaps it before the signal lands.
        kill_process(Pid::from_child(&child), signal)
            .map_err(|error| BigSubprocessError::Io(error.into()))
    }

    #[cfg(unix)]
    fn signal_process_group(&self, signal: Signal) -> Result<(), BigSubprocessError> {
        let child = self.lock_child()?;
        let pid = Pid::from_child(&child);
        // A leader that exited but was not reaped still holds its pid, so the
        // group id is still this group's and its other members get the
        // signal. Once reaped, the id may belong to someone else. WNOWAIT
        // leaves the child for `std::process::Child`, and the lock keeps other
        // holders from reaping it before the kill below.
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        match waitid(WaitId::Pid(pid), options) {
            Ok(_) => {}
            // Reaped already: the id may now name another group.
            Err(Errno::CHILD) => return Ok(()),
            Err(error) => return Err(BigSubprocessError::Io(error.into())),
        }
        // The child was spawned with `setsid`, so its pid is also the group id.
        match kill_process_group(pid, signal) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(error) => Err(BigSubprocessError::Io(error.into())),
        }
    }
}
