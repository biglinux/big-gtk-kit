// SPDX-License-Identifier: MIT

use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};
use rustix::io::Errno;

/// Tracks daemon clients and active jobs so an idle daemon can exit.
///
/// Daemons create one shared tracker, hold a client lease for each accepted
/// connection, and hold a job lease for each running durable job. The daemon may
/// exit when [`Self::should_exit`] becomes true.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::MicrodaemonIdleExit;
/// use std::time::Duration;
///
/// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
/// let client = idle_exit.client_connected();
/// assert!(!idle_exit.should_exit());
/// drop(client);
/// assert!(idle_exit.should_exit());
/// ```
#[derive(Clone, Debug)]
pub struct MicrodaemonIdleExit {
    state: Arc<Mutex<MicrodaemonIdleState>>,
    grace_period: Duration,
    /// Rung when the daemon becomes idle and by [`Self::wake`], so the accept
    /// loop can block in [`Self::wait_for_client`] instead of waking to ask.
    /// `None` only if the kernel refused an eventfd.
    bell: Option<Arc<OwnedFd>>,
}

/// How long the accept loop sleeps between looks when there is no eventfd.
const WAIT_WITHOUT_BELL: Duration = Duration::from_millis(500);

impl MicrodaemonIdleExit {
    /// Create a tracker with the grace period used after the daemon becomes idle.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonIdleExit;
    /// use std::time::Duration;
    ///
    /// let idle_exit = MicrodaemonIdleExit::new(Duration::from_millis(25));
    /// assert_eq!(idle_exit.grace_period(), Duration::from_millis(25));
    /// ```
    #[must_use]
    pub fn new(grace_period: Duration) -> Self {
        let bell = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .ok()
            .map(Arc::new);
        Self {
            state: Arc::new(Mutex::new(MicrodaemonIdleState::new())),
            grace_period,
            bell,
        }
    }

    /// Wake [`Self::wait_for_client`], after the caller set a stop flag.
    pub fn wake(&self) {
        if let Some(bell) = &self.bell {
            let _ = rustix::io::write(bell.as_ref(), &1_u64.to_ne_bytes());
        }
    }

    /// The descriptor [`Self::wake`] writes to, for a signal handler to ring
    /// with `write(2)`, which is async-signal-safe. `None` without an eventfd.
    #[must_use]
    pub fn wake_fd(&self) -> Option<RawFd> {
        self.bell.as_ref().map(|bell| bell.as_raw_fd())
    }

    /// How long until [`Self::should_exit`] turns true if nothing happens:
    /// `None` while a client or a job holds the daemon.
    #[must_use]
    pub fn time_until_exit(&self) -> Option<Duration> {
        self.with_state(|state| {
            if state.active_jobs != 0 || state.connected_clients != 0 {
                return None;
            }
            let idle_since = *state.idle_since.get_or_insert_with(Instant::now);
            Some(self.grace_period.saturating_sub(idle_since.elapsed()))
        })
    }

    /// Wait until `listener` has a client to accept, the daemon is woken, or
    /// (when `idle_exit_applies`) its idle grace runs out; true when there is
    /// a client.
    ///
    /// Blocks for as long as nothing can change, so a daemon holding sessions
    /// all day sleeps all day.
    pub fn wait_for_client(&self, listener: BorrowedFd<'_>, idle_exit_applies: bool) -> bool {
        let left = self.time_until_exit().filter(|_| idle_exit_applies);
        let deadline = match (&self.bell, left) {
            (None, _) => Some(WAIT_WITHOUT_BELL),
            (Some(_), None) => None,
            // One millisecond past the deadline, so the exit is not a hair early.
            (Some(_), Some(left)) => Some(left.saturating_add(Duration::from_millis(1))),
        };
        let until = deadline.map(|deadline| Instant::now() + deadline);
        let mut watched = vec![PollFd::new(&listener, PollFlags::IN)];
        if let Some(bell) = &self.bell {
            watched.push(PollFd::new(bell.as_ref(), PollFlags::IN));
        }
        // A signal (a child reaped) ends the wait early; the rest is waited.
        loop {
            let timeout = until.and_then(|until| {
                Timespec::try_from(until.saturating_duration_since(Instant::now())).ok()
            });
            if poll(&mut watched, timeout.as_ref()) != Err(Errno::INTR) {
                break;
            }
        }
        let client = watched[0].revents().contains(PollFlags::IN);
        if let Some(bell) = &self.bell {
            // Nonblocking: a bell nobody rang reads EAGAIN.
            let _ = rustix::io::read(bell.as_ref(), &mut [0_u8; 8]);
        }
        client
    }

    /// Return the configured idle grace period.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonIdleExit;
    /// use std::time::Duration;
    ///
    /// let idle_exit = MicrodaemonIdleExit::new(Duration::from_secs(2));
    /// assert_eq!(idle_exit.grace_period(), Duration::from_secs(2));
    /// ```
    #[must_use]
    pub const fn grace_period(&self) -> Duration {
        self.grace_period
    }

    /// Register one connected client and return its RAII lease.
    ///
    /// Dropping the lease decrements the connected-client count.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonIdleExit;
    /// use std::time::Duration;
    ///
    /// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
    /// let _client = idle_exit.client_connected();
    /// assert_eq!(idle_exit.connected_clients(), 1);
    /// ```
    #[must_use]
    pub fn client_connected(&self) -> MicrodaemonClientLease {
        self.with_state(|state| state.connected_clients += 1);
        MicrodaemonClientLease {
            idle_exit: self.clone(),
        }
    }

    /// Register one active job and return its RAII lease.
    ///
    /// Dropping the lease decrements the active-job count.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonIdleExit;
    /// use std::time::Duration;
    ///
    /// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
    /// let _job = idle_exit.job_started();
    /// assert_eq!(idle_exit.active_jobs(), 1);
    /// ```
    #[must_use]
    pub fn job_started(&self) -> MicrodaemonJobLease {
        self.with_state(|state| state.active_jobs += 1);
        MicrodaemonJobLease {
            idle_exit: self.clone(),
        }
    }

    /// Return the current connected-client count.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonIdleExit;
    /// use std::time::Duration;
    ///
    /// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
    /// assert_eq!(idle_exit.connected_clients(), 0);
    /// ```
    #[must_use]
    pub fn connected_clients(&self) -> usize {
        self.with_state(|state| state.connected_clients)
    }

    /// Return the current active-job count.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonIdleExit;
    /// use std::time::Duration;
    ///
    /// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
    /// assert_eq!(idle_exit.active_jobs(), 0);
    /// ```
    #[must_use]
    pub fn active_jobs(&self) -> usize {
        self.with_state(|state| state.active_jobs)
    }

    /// Return true after both counters are zero for the configured grace period.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::microdaemon::MicrodaemonIdleExit;
    /// use std::time::Duration;
    ///
    /// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
    /// assert!(idle_exit.should_exit());
    /// ```
    #[must_use]
    pub fn should_exit(&self) -> bool {
        self.with_state(|state| {
            if state.active_jobs != 0 || state.connected_clients != 0 {
                state.idle_since = None;
                return false;
            }
            let idle_since = *state.idle_since.get_or_insert_with(Instant::now);
            idle_since.elapsed() >= self.grace_period
        })
    }

    fn release_client(&self) {
        let idle = self.with_state(|state| {
            state.connected_clients = state.connected_clients.saturating_sub(1);
            state.mark_idle_if_empty()
        });
        // The grace clock starts now: the accept loop has to hear it.
        if idle {
            self.wake();
        }
    }

    fn release_job(&self) {
        let idle = self.with_state(|state| {
            state.active_jobs = state.active_jobs.saturating_sub(1);
            state.mark_idle_if_empty()
        });
        if idle {
            self.wake();
        }
    }

    fn with_state<T>(&self, action: impl FnOnce(&mut MicrodaemonIdleState) -> T) -> T {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        action(&mut state)
    }
}

/// RAII lease for one connected microdaemon client.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::MicrodaemonIdleExit;
/// use std::time::Duration;
///
/// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
/// let client = idle_exit.client_connected();
/// assert_eq!(idle_exit.connected_clients(), 1);
/// drop(client);
/// assert_eq!(idle_exit.connected_clients(), 0);
/// ```
#[derive(Debug)]
pub struct MicrodaemonClientLease {
    idle_exit: MicrodaemonIdleExit,
}

impl Drop for MicrodaemonClientLease {
    fn drop(&mut self) {
        self.idle_exit.release_client();
    }
}

/// RAII lease for one running durable microdaemon job.
///
/// # Examples
///
/// ```
/// use big_os_kit::microdaemon::MicrodaemonIdleExit;
/// use std::time::Duration;
///
/// let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
/// let job = idle_exit.job_started();
/// assert_eq!(idle_exit.active_jobs(), 1);
/// drop(job);
/// assert_eq!(idle_exit.active_jobs(), 0);
/// ```
#[derive(Debug)]
pub struct MicrodaemonJobLease {
    idle_exit: MicrodaemonIdleExit,
}

impl Drop for MicrodaemonJobLease {
    fn drop(&mut self) {
        self.idle_exit.release_job();
    }
}

#[derive(Debug)]
struct MicrodaemonIdleState {
    active_jobs: usize,
    connected_clients: usize,
    idle_since: Option<Instant>,
}

impl MicrodaemonIdleState {
    fn new() -> Self {
        Self {
            active_jobs: 0,
            connected_clients: 0,
            idle_since: Some(Instant::now()),
        }
    }

    /// Start the grace clock if nothing holds the daemon; true when it did.
    fn mark_idle_if_empty(&mut self) -> bool {
        let idle = self.active_jobs == 0 && self.connected_clients == 0;
        if idle {
            self.idle_since = Some(Instant::now());
        }
        idle
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    /// A descriptor that never becomes readable, standing in for a listener
    /// nobody connects to: the read end of a pipe whose write end stays open.
    fn quiet_listener() -> (OwnedFd, OwnedFd) {
        let (reader, writer) = std::io::pipe().expect("a pipe");
        (reader.into(), writer.into())
    }

    /// [`MicrodaemonIdleExit::wait_for_client`] on another thread: whether it
    /// saw a client and how long after `since` it returned, or `None` while it
    /// still waits after `limit`.
    fn wait_within(
        idle_exit: &MicrodaemonIdleExit,
        listener: &OwnedFd,
        since: Instant,
        limit: Duration,
    ) -> Option<(bool, Duration)> {
        let idle_exit = idle_exit.clone();
        let listener = listener.try_clone().expect("the listener's duplicate");
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let client = idle_exit.wait_for_client(listener.as_fd(), true);
            let _ = sender.send((client, since.elapsed()));
        });
        receiver.recv_timeout(limit).ok()
    }

    #[test]
    fn wait_ends_with_idle_grace() {
        let since = Instant::now();
        let idle_exit = MicrodaemonIdleExit::new(Duration::from_millis(80));
        let (listener, _writer) = quiet_listener();
        let (client, waited) = wait_within(&idle_exit, &listener, since, Duration::from_secs(2))
            .expect("slept past the grace");
        assert!(!client);
        assert!(
            waited >= Duration::from_millis(70),
            "woke early: {waited:?}"
        );
        assert!(idle_exit.should_exit());
    }

    #[test]
    fn held_daemon_sleeps_until_released() {
        let idle_exit = MicrodaemonIdleExit::new(Duration::from_secs(30));
        let (listener, _writer) = quiet_listener();
        let client = idle_exit.client_connected();
        let since = Instant::now();
        let leaving = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(client);
        });
        let (client, waited) = wait_within(&idle_exit, &listener, since, Duration::from_secs(5))
            .expect("never heard the client leave");
        assert!(!client);
        assert!(
            waited >= Duration::from_millis(140),
            "woke before the client left"
        );
        leaving.join().unwrap();
    }

    #[test]
    fn held_daemon_has_no_exit_time() {
        let idle_exit = MicrodaemonIdleExit::new(Duration::from_secs(30));
        let client = idle_exit.client_connected();
        assert_eq!(idle_exit.time_until_exit(), None);
        drop(client);
        let job = idle_exit.job_started();
        assert_eq!(idle_exit.time_until_exit(), None);
        drop(job);
        assert!(
            idle_exit
                .time_until_exit()
                .is_some_and(|left| left > Duration::from_secs(29))
        );
    }

    #[test]
    fn one_wake_ends_one_wait() {
        let idle_exit = MicrodaemonIdleExit::new(Duration::from_secs(30));
        let (listener, _writer) = quiet_listener();
        // Held: no grace to run out, so only the bell ends a wait.
        let _client = idle_exit.client_connected();
        idle_exit.wake();

        let woken = wait_within(&idle_exit, &listener, Instant::now(), Duration::from_secs(2));
        assert_eq!(woken.map(|(client, _)| client), Some(false));
        let again = wait_within(&idle_exit, &listener, Instant::now(), Duration::from_millis(100));
        assert!(again.is_none(), "the bell rang once and was heard once");
        idle_exit.wake();
    }

    #[test]
    fn leases_count_clients_and_jobs() {
        let idle_exit = MicrodaemonIdleExit::new(Duration::ZERO);
        let client_a = idle_exit.client_connected();
        let client_b = idle_exit.client_connected();
        let job_a = idle_exit.job_started();
        let job_b = idle_exit.job_started();

        assert_eq!(idle_exit.connected_clients(), 2);
        assert_eq!(idle_exit.active_jobs(), 2);
        assert!(!idle_exit.should_exit());

        drop(client_a);
        drop(job_a);
        assert_eq!(idle_exit.connected_clients(), 1);
        assert_eq!(idle_exit.active_jobs(), 1);
        assert!(!idle_exit.should_exit());

        drop(client_b);
        assert_eq!(idle_exit.connected_clients(), 0);
        assert_eq!(idle_exit.active_jobs(), 1);
        assert!(!idle_exit.should_exit());

        drop(job_b);
        assert_eq!(idle_exit.connected_clients(), 0);
        assert_eq!(idle_exit.active_jobs(), 0);
        assert!(idle_exit.should_exit());
    }

    #[test]
    fn idle_exit_waits_for_grace_period() {
        let idle_exit = MicrodaemonIdleExit::new(Duration::from_millis(40));
        let client = idle_exit.client_connected();
        drop(client);

        assert!(
            !idle_exit.should_exit(),
            "daemon must not exit before the grace period elapses"
        );
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            idle_exit.should_exit(),
            "daemon exits after both counters are zero for the grace period"
        );
    }

    #[test]
    fn idle_grace_waits_for_both_leases() {
        let idle_exit = MicrodaemonIdleExit::new(Duration::from_millis(40));
        let client = idle_exit.client_connected();
        let job = idle_exit.job_started();

        drop(client);
        std::thread::sleep(Duration::from_millis(60));
        drop(job);

        assert!(
            !idle_exit.should_exit(),
            "a remaining job must prevent the disconnected client from arming idle exit"
        );
        std::thread::sleep(Duration::from_millis(60));
        assert!(idle_exit.should_exit());
    }
}
