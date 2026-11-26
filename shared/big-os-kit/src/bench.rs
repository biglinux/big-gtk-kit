// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Env-gated benchmark timestamps that can live in the hot path permanently.
//!
//! [`mark`] costs a single already-initialized atomic load when the
//! `BIG_BENCH_TIMING` environment variable is unset (the normal case), so
//! marks left in startup paths never need to be added and removed for a
//! measurement. Set `BIG_BENCH_TIMING=/path/to/log` before launching (or in the
//! session environment, to profile the resident desktop process) and every mark
//! appends `<ms>  <label>` to that file, `<ms>` read from `CLOCK_MONOTONIC` so
//! marks of a launcher and of the resident host it hands a window to share one
//! timeline. A [`span`] marks `<label> took <n> ms` when its scope ends, for the
//! phases of opening a window, and [`first_frame`] the first painted frame of a
//! product's window.

use std::io::Write as _;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

static SINK: LazyLock<Option<Mutex<std::fs::File>>> = LazyLock::new(|| {
    let path = std::env::var_os("BIG_BENCH_TIMING")?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(Mutex::new)
});

/// Milliseconds of `CLOCK_MONOTONIC`, the clock `Instant` and Python's
/// `time.monotonic()` read: one timeline for every process on the machine.
fn monotonic_ms() -> f64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec, and CLOCK_MONOTONIC exists on
    // every Linux kernel, so the call only writes it.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) };
    now.tv_sec as f64 * 1000.0 + now.tv_nsec as f64 / 1_000_000.0
}

/// Whether benchmark marks are being recorded this run.
#[must_use]
pub fn enabled() -> bool {
    SINK.as_ref().is_some()
}

/// Append one timestamped label to the benchmark log, if `BIG_BENCH_TIMING` is
/// set. A no-op otherwise.
pub fn mark(label: &str) {
    let Some(sink) = SINK.as_ref() else {
        return;
    };
    let ms = monotonic_ms();
    if let Ok(mut file) = sink.lock() {
        let _ = writeln!(file, "{ms:12.1}  {label}");
    }
}

/// Mark the first frame a window of the product `app_id` painted, as
/// `first-frame <app-id>`: where a start ends, for every product alike. Called
/// from the window's first after-paint.
pub fn first_frame(app_id: &str) {
    if enabled() {
        mark(&format!("first-frame {app_id}"));
    }
}

/// Logs how long it lived as `label` when dropped, with benchmarks recording.
pub struct Span {
    label: &'static str,
    began: Option<Instant>,
}

/// Time the rest of the enclosing scope.
#[must_use]
pub fn span(label: &'static str) -> Span {
    Span {
        label,
        began: enabled().then(Instant::now),
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(began) = self.began {
            mark(&format!(
                "{} took {:.1} ms",
                self.label,
                began.elapsed().as_secs_f64() * 1000.0
            ));
        }
    }
}

/// Run `body` inside a [`span`] named `label`.
pub fn timed<T>(label: &'static str, body: impl FnOnce() -> T) -> T {
    let _span = span(label);
    body()
}

#[cfg(test)]
mod tests {
    /// A mark is stamped on the clock a launcher in another process reads, so a
    /// start can be timed from the launch to a mark the resident host writes.
    #[test]
    fn marks_share_the_monotonic_clock() {
        if std::env::var_os("BIG_OS_KIT_CHILD_BENCH").is_some() {
            super::mark("probe");
            return;
        }
        let log = std::env::temp_dir().join(format!("big-os-kit-bench-{}", std::process::id()));
        let before = super::monotonic_ms();
        let child = crate::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "bench::tests::marks_share_the_monotonic_clock",
                "--quiet",
            ])
            .env("BIG_OS_KIT_CHILD_BENCH", "1")
            .env("BIG_BENCH_TIMING", &log)
            .build()
            .run()
            .expect("child run");
        let after = super::monotonic_ms();
        let text = std::fs::read_to_string(&log).expect("the child wrote its log");
        let _ = std::fs::remove_file(&log);
        assert!(child.status.success(), "{}", child.stdout_lossy());
        let (stamp, label) = text.trim().split_once("  ").expect("stamp and label");
        assert_eq!(label, "probe");
        let stamp: f64 = stamp.trim().parse().expect("a stamp in ms");
        assert!(
            (before..=after).contains(&stamp),
            "{before} <= {stamp} <= {after}"
        );
    }
}
