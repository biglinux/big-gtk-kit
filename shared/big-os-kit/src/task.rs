// SPDX-License-Identifier: MIT

//! GTK-free task ownership, cancellation, and stale-delivery guards.
//!
//! A caller keeps a [`BigTaskToken`] for one operation, workers receive a
//! cloneable [`BigTaskSignal`], and dropping the token cancels the operation.
//! UI crates can use the delivery gate on their main context without this crate
//! depending on GTK.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::subprocess::{BigSubprocessError, BigSubprocessOutput, BigSubprocessSpec};

/// Owns one async operation; dropping it requests cancellation.
///
/// The token is not `Clone`: one owner controls the operation
/// lifetime, while background work uses [`BigTaskSignal`].
///
/// # Examples
///
/// ```
/// use big_os_kit::task::BigTaskToken;
/// use std::time::Duration;
///
/// let token = BigTaskToken::new(Duration::from_millis(50));
/// let signal = token.signal();
/// assert!(!signal.is_cancelled());
/// drop(token);
/// assert!(signal.is_cancelled());
/// ```
pub struct BigTaskToken {
    state: Arc<BigTaskState>,
}

impl BigTaskToken {
    /// Create a token with a hard-cancel deadline after cooperative cancellation.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(25));
    /// assert!(!token.signal().is_cancelled());
    /// ```
    #[must_use]
    pub fn new(cancellation_deadline: Duration) -> Self {
        Self {
            state: Arc::new(BigTaskState::new(cancellation_deadline)),
        }
    }

    /// Return a cloneable signal for worker threads or subprocess wrappers.
    ///
    /// Bind the token first and keep it for the operation:
    /// `let signal = BigTaskToken::new(d).signal();` drops the token at the end
    /// of the statement, which cancels the signal it just returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let signal = token.signal();
    /// assert!(!signal.is_cancelled());
    /// ```
    #[must_use]
    pub fn signal(&self) -> BigTaskSignal {
        BigTaskSignal {
            state: Arc::clone(&self.state),
        }
    }

    /// Capture a guard that suppresses stale delivery after cancellation.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let gate = token.delivery_gate();
    /// assert!(gate.deliver(|| {}));
    /// token.cancel();
    /// assert!(!gate.deliver(|| panic!("stale delivery")));
    /// ```
    #[must_use]
    pub fn delivery_gate(&self) -> BigTaskDeliveryGate {
        self.signal().delivery_gate()
    }

    /// Request cooperative cancellation and arm the hard-cancel deadline.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let signal = token.signal();
    /// token.cancel();
    /// assert!(signal.is_cancelled());
    /// ```
    pub fn cancel(&self) {
        self.state.cancel();
    }

    /// Mark the operation finished and disarm any registered hard-kill hook.
    /// Call it after the operation's last subprocess ended: one still running
    /// is no longer stopped by a later cancel.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(1));
    /// let fired = Arc::new(AtomicBool::new(false));
    /// token.signal().set_hard_cancel_hook({
    ///     let fired = Arc::clone(&fired);
    ///     move || fired.store(true, Ordering::Relaxed)
    /// });
    /// token.complete();
    /// token.cancel();
    /// assert!(!fired.load(Ordering::Relaxed));
    /// ```
    pub fn complete(&self) {
        self.state.complete();
    }
}

impl Drop for BigTaskToken {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Cloneable task signal passed to background work.
///
/// # Examples
///
/// ```
/// use big_os_kit::task::BigTaskToken;
/// use std::time::Duration;
///
/// let token = BigTaskToken::new(Duration::from_millis(10));
/// let signal = token.signal();
/// let worker = signal.clone();
/// drop(token);
/// assert!(signal.is_cancelled() && worker.is_cancelled());
/// ```
#[derive(Clone)]
pub struct BigTaskSignal {
    state: Arc<BigTaskState>,
}

impl BigTaskSignal {
    /// Returns `true` after the owner cancels or drops the token.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let signal = token.signal();
    /// token.cancel();
    /// assert!(signal.is_cancelled());
    /// ```
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    /// Register the hard-cancel hook fired after the deadline.
    ///
    /// If the token is already cancelled, the hook still fires: when the
    /// deadline already running passes, else one deadline after this call.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(1));
    /// let fired = Arc::new(AtomicBool::new(false));
    /// token.signal().set_hard_cancel_hook({
    ///     let fired = Arc::clone(&fired);
    ///     move || fired.store(true, Ordering::Relaxed)
    /// });
    /// token.cancel();
    /// std::thread::sleep(Duration::from_millis(20));
    /// assert!(fired.load(Ordering::Relaxed));
    /// ```
    pub fn set_hard_cancel_hook(&self, hook: impl FnOnce() + Send + 'static) {
        self.state.set_hard_cancel_hook(Box::new(hook));
    }

    /// Capture a guard for one eventual result delivery.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let gate = token.signal().delivery_gate();
    /// assert!(gate.is_current());
    /// ```
    #[must_use]
    pub fn delivery_gate(&self) -> BigTaskDeliveryGate {
        BigTaskDeliveryGate {
            state: Arc::clone(&self.state),
        }
    }

    /// Run a subprocess through [`BigSubprocessSpec::run_cancellable`].
    ///
    /// Cooperative cancellation flips this signal first; after the configured
    /// deadline a hard-cancel flag shared by every subprocess of the task is
    /// set, and the subprocess cancellation loop performs the kill path. The
    /// hook of [`Self::set_hard_cancel_hook`] stays the caller's. A task
    /// already cancelled or completed starts nothing.
    ///
    /// # Errors
    ///
    /// Returns [`BigSubprocessError::Cancelled`] when the task was already
    /// cancelled or completed, else the subprocess validation, spawn, I/O,
    /// timeout, or cancellation error from
    /// [`BigSubprocessSpec::run_cancellable`].
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::subprocess::BigSubprocessSpec;
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let out = token.signal()
    ///     .run_subprocess(&BigSubprocessSpec::builder().program("true").build())
    ///     .unwrap();
    /// assert!(out.status.success());
    /// ```
    pub fn run_subprocess(
        &self,
        spec: &BigSubprocessSpec,
    ) -> Result<BigSubprocessOutput, BigSubprocessError> {
        {
            let mut slot = self
                .state
                .hard_cancel_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.is_cancelled() || self.state.completed.load(Ordering::Acquire) {
                return Err(BigSubprocessError::Cancelled);
            }
            slot.running_subprocesses += 1;
        }
        let output = spec.run_cancellable(&self.state.hard_cancelled);
        self.state
            .hard_cancel_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running_subprocesses -= 1;
        output
    }
}

/// Guard for delivering one async result.
///
/// # Examples
///
/// ```
/// use big_os_kit::task::BigTaskToken;
/// use std::time::Duration;
///
/// let token = BigTaskToken::new(Duration::from_millis(10));
/// let gate = token.delivery_gate();
/// assert!(gate.deliver(|| {}));
/// token.cancel();
/// assert!(!gate.is_current());
/// ```
#[derive(Clone)]
pub struct BigTaskDeliveryGate {
    state: Arc<BigTaskState>,
}

impl BigTaskDeliveryGate {
    /// Returns `true` until the token is cancelled or completed; neither can
    /// be undone.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let gate = token.delivery_gate();
    /// token.cancel();
    /// assert!(!gate.is_current());
    /// ```
    #[must_use]
    pub fn is_current(&self) -> bool {
        !self.state.cancelled.load(Ordering::Acquire)
            && !self.state.completed.load(Ordering::Acquire)
    }

    /// Run `deliver` only when this gate is still current.
    ///
    /// Returns whether the closure ran.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_os_kit::task::BigTaskToken;
    /// use std::time::Duration;
    ///
    /// let token = BigTaskToken::new(Duration::from_millis(10));
    /// let gate = token.delivery_gate();
    /// assert!(gate.deliver(|| {}));
    /// ```
    pub fn deliver(&self, deliver: impl FnOnce()) -> bool {
        if self.is_current() {
            deliver();
            true
        } else {
            false
        }
    }
}

struct BigTaskState {
    cancelled: AtomicBool,
    completed: AtomicBool,
    hard_cancel_armed: AtomicBool,
    /// Set when the deadline passes; every subprocess of the task polls it.
    hard_cancelled: AtomicBool,
    cancellation_deadline: Duration,
    hard_cancel_hook: Mutex<HardCancelSlot>,
    hard_cancel_wake: Condvar,
}

/// What a passing deadline acts on; with neither, cancelling arms no deadline.
#[derive(Default)]
struct HardCancelSlot {
    hook: Option<Box<dyn FnOnce() + Send + 'static>>,
    running_subprocesses: usize,
}

impl BigTaskState {
    fn new(cancellation_deadline: Duration) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            hard_cancel_armed: AtomicBool::new(false),
            hard_cancelled: AtomicBool::new(false),
            cancellation_deadline,
            hard_cancel_hook: Mutex::new(HardCancelSlot::default()),
            hard_cancel_wake: Condvar::new(),
        }
    }

    fn cancel(self: &Arc<Self>) {
        self.cancelled.store(true, Ordering::Release);
        self.arm_hard_cancel();
    }

    fn complete(&self) {
        // Serialize completion and registration. Destructors and callbacks may
        // re-enter this state, so neither may run while its mutex is held.
        let old = {
            let mut slot = self
                .hard_cancel_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.completed.store(true, Ordering::Release);
            slot.hook.take()
        };
        self.hard_cancel_wake.notify_all();
        drop(old);
    }

    fn set_hard_cancel_hook(self: &Arc<Self>, hook: Box<dyn FnOnce() + Send + 'static>) {
        let old = {
            let mut slot = self
                .hard_cancel_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.completed.load(Ordering::Acquire) {
                // Release the lock before dropping the rejected callback.
                drop(slot);
                return;
            }
            slot.hook.replace(hook)
        };
        drop(old);
        if self.cancelled.load(Ordering::Acquire) {
            self.arm_hard_cancel();
        }
    }

    fn arm_hard_cancel(self: &Arc<Self>) {
        {
            let slot = self
                .hard_cancel_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.completed.load(Ordering::Acquire)
                || (slot.hook.is_none() && slot.running_subprocesses == 0)
                || self.hard_cancel_armed.swap(true, Ordering::AcqRel)
            {
                return;
            }
        }
        let state = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("task-hard-cancel".to_owned())
            .spawn(move || {
                let slot = state
                    .hard_cancel_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let (mut slot, _) = state
                    .hard_cancel_wake
                    .wait_timeout_while(slot, state.cancellation_deadline, |_| {
                        !state.completed.load(Ordering::Acquire)
                    })
                    .unwrap_or_else(|e| e.into_inner());
                let hook = slot.hook.take();
                let completed = state.completed.load(Ordering::Acquire);
                if !completed {
                    state.hard_cancelled.store(true, Ordering::Release);
                }
                state.hard_cancel_armed.store(false, Ordering::Release);
                drop(slot);
                // The hook was claimed before completion. A subsequent complete
                // cannot undo a callback already in progress.
                if !completed && let Some(hook) = hook {
                    hook();
                }
            });
        if let Err(error) = spawned {
            // Resource exhaustion must not panic in a GTK Drop or leave a
            // subprocess uncancellable. Fall back to immediate hard cancel.
            log::warn!("cannot start hard-cancel deadline: {error}; cancelling now");
            let hook = {
                let mut slot = self
                    .hard_cancel_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.hard_cancel_armed.store(false, Ordering::Release);
                self.hard_cancelled.store(true, Ordering::Release);
                slot.hook.take()
            };
            if let Some(hook) = hook {
                hook();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooperative_cancel_arms_no_timer() {
        for _ in 0..1000 {
            let token = BigTaskToken::new(Duration::from_secs(60));
            let signal = token.signal();
            assert!(!signal.is_cancelled());
            drop(token);
            assert!(signal.is_cancelled());
            assert!(!signal.state.hard_cancel_armed.load(Ordering::Acquire));
            assert_eq!(Arc::strong_count(&signal.state), 1);
        }
    }

    #[test]
    fn late_hook_still_fires() {
        let token = BigTaskToken::new(Duration::ZERO);
        token.cancel();
        let (tx, rx) = std::sync::mpsc::channel();
        token.signal().set_hard_cancel_hook(move || {
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("late hook is not lost");
    }

    #[test]
    fn hard_cancel_callback_completes_task() {
        let token = BigTaskToken::new(Duration::ZERO);
        let state = Arc::clone(&token.state);
        let (tx, rx) = std::sync::mpsc::channel();
        token.signal().set_hard_cancel_hook(move || {
            state.complete();
            let _ = tx.send(());
        });
        token.cancel();
        rx.recv_timeout(Duration::from_secs(2))
            .expect("callback must not hold the mutex");
    }

    #[test]
    fn complete_wakes_long_deadline() {
        let token = BigTaskToken::new(Duration::from_secs(60));
        token.signal().set_hard_cancel_hook(|| panic!("completed"));
        token.cancel();
        token.complete();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while token.state.hard_cancel_armed.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < until, "deadline did not wake");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn completion_drops_the_callback() {
        for _ in 0..64 {
            let token = BigTaskToken::new(Duration::ZERO);
            let signal = token.signal();
            let captured = Arc::new(());
            let weak = Arc::downgrade(&captured);
            std::thread::scope(|scope| {
                scope.spawn(move || signal.set_hard_cancel_hook(move || drop(captured)));
                token.complete();
            });
            assert!(weak.upgrade().is_none());
            assert!(token.state.hard_cancel_hook.lock().unwrap().hook.is_none());
        }
    }

    #[test]
    fn replaced_hook_drops_outside_lock() {
        struct CompleteOnDrop(Arc<BigTaskState>);
        impl Drop for CompleteOnDrop {
            fn drop(&mut self) {
                self.0.complete();
            }
        }
        let token = BigTaskToken::new(Duration::ZERO);
        let captured = CompleteOnDrop(Arc::clone(&token.state));
        token.signal().set_hard_cancel_hook(move || drop(captured));
        let signal = token.signal();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            signal.set_hard_cancel_hook(|| panic!("completed by old capture"));
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("capture destructor must not deadlock");
        worker.join().unwrap();
        assert!(token.state.completed.load(Ordering::Acquire));
    }

    #[test]
    fn drop_sets_cooperative_cancel_flag() {
        let token = BigTaskToken::new(Duration::from_millis(20));
        let signal = token.signal();

        drop(token);

        assert!(signal.is_cancelled());
    }

    #[test]
    fn delivery_gate_rejects_after_cancel() {
        let token = BigTaskToken::new(Duration::from_millis(20));
        let gate = token.delivery_gate();

        token.cancel();

        assert!(!gate.deliver(|| panic!("cancelled task must not deliver")));
    }

    #[test]
    fn delivery_gate_delivers_while_untouched() {
        let token = BigTaskToken::new(Duration::from_millis(20));
        let gate = token.delivery_gate();

        assert!(gate.is_current());
        let mut ran = false;
        assert!(gate.deliver(|| ran = true));
        assert!(ran, "a current gate must run the delivery closure");
    }

    #[test]
    fn delivery_gate_rejects_after_complete() {
        let token = BigTaskToken::new(Duration::from_millis(20));
        let gate = token.delivery_gate();

        token.complete();

        assert!(!gate.is_current());
        assert!(!gate.deliver(|| panic!("completed task must not deliver")));
    }

    #[test]
    fn complete_disarms_the_hard_cancel_hook() {
        let token = BigTaskToken::new(Duration::from_millis(1));
        let fired = Arc::new(AtomicBool::new(false));
        token.signal().set_hard_cancel_hook({
            let fired = Arc::clone(&fired);
            move || fired.store(true, Ordering::Release)
        });

        token.complete();
        token.cancel();
        std::thread::sleep(Duration::from_millis(30));

        assert!(
            !fired.load(Ordering::Acquire),
            "complete() must disarm the deadline hook"
        );
    }

    #[test]
    fn cooperative_cancel_before_deadline() {
        let token = BigTaskToken::new(Duration::from_millis(50));
        let signal = token.signal();
        let completed_cooperatively = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&completed_cooperatively);

        let started = std::time::Instant::now();
        let worker = std::thread::spawn(move || {
            while !signal.is_cancelled() {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "never cancelled"
                );
                std::thread::yield_now();
            }
            worker_done.store(true, Ordering::Release);
        });
        token.cancel();
        worker.join().expect("cooperative worker finishes");

        assert!(completed_cooperatively.load(Ordering::Acquire));
    }

    #[test]
    fn hard_cancel_hook_fires_after_deadline() {
        let token = BigTaskToken::new(Duration::from_millis(10));
        let fired = Arc::new(AtomicBool::new(false));
        token.signal().set_hard_cancel_hook({
            let fired = Arc::clone(&fired);
            move || fired.store(true, Ordering::Release)
        });

        token.cancel();
        for _ in 0..100 {
            if fired.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("hard cancel hook did not fire");
    }

    /// Runs five-second sleeps on clones of one signal, one per marker name,
    /// and cancels the task once every child has created its marker.
    fn cancel_started_sleeps(markers: &[&str]) {
        let dir = std::env::temp_dir().join(format!(
            "big-os-kit-task-{}-{}",
            std::process::id(),
            markers.join("-")
        ));
        // A run under the same PID may have left its markers behind.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("marker directory");
        let token = BigTaskToken::new(Duration::from_millis(20));
        let workers: Vec<_> = markers
            .iter()
            .map(|name| {
                let signal = token.signal();
                let marker = dir.join(name);
                std::thread::spawn(move || {
                    signal.run_subprocess(
                        &BigSubprocessSpec::builder()
                            .program("sh")
                            .args(["-c", r#"touch "$0" && exec sleep 5"#])
                            .arg(marker.into_os_string())
                            .allow_list(["sh"])
                            .timeout(Duration::from_secs(30))
                            .build(),
                    )
                })
            })
            .collect();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while markers.iter().any(|name| !dir.join(name).exists()) {
            assert!(std::time::Instant::now() < until, "a child never started");
            std::thread::sleep(Duration::from_millis(5));
        }

        token.cancel();
        let start = std::time::Instant::now();
        for worker in workers {
            let err = worker.join().expect("subprocess thread joins").unwrap_err();
            assert!(matches!(err, BigSubprocessError::Cancelled), "{err:?}");
        }
        let elapsed = start.elapsed();
        std::fs::remove_dir_all(&dir).expect("remove marker directory");
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn deadline_cancel_kills_subprocess() {
        cancel_started_sleeps(&["only"]);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn deadline_cancel_kills_every_subprocess() {
        cancel_started_sleeps(&["first", "second"]);
    }

    /// A completed task can never hard-cancel, so a late run would be
    /// unkillable until its own timeout.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn completed_signal_starts_nothing() {
        let token = BigTaskToken::new(Duration::from_millis(20));
        token.complete();
        let start = std::time::Instant::now();
        let err = token
            .signal()
            .run_subprocess(
                &BigSubprocessSpec::builder()
                    .program("sleep")
                    .arg("5")
                    .build(),
            )
            .unwrap_err();
        assert!(matches!(err, BigSubprocessError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    /// A task cancelled before its program starts does not wait out the
    /// grace a running one gets.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn cancelled_signal_starts_nothing() {
        let token = BigTaskToken::new(Duration::from_secs(5));
        token.cancel();
        let start = std::time::Instant::now();
        let err = token
            .signal()
            .run_subprocess(
                &BigSubprocessSpec::builder()
                    .program("sleep")
                    .arg("5")
                    .build(),
            )
            .unwrap_err();
        assert!(matches!(err, BigSubprocessError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
