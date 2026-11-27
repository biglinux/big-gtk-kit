// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Teardown and leak assertions.
//!
//! [`WidgetFinalizeCensus`] observes finalization and needs a running GTK app;
//! [`RssSlope`] and [`Subscription`] are pure.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use glib::prelude::*;

/// Least-squares slope is only computed over samples past this warm-up count,
/// so first-run allocations (font/renderer/icon caches filling) do not
/// masquerade as a per-cycle leak.
const WARMUP: usize = 5;

// Exact finalization census.

/// Finalization census: log a `new` line when a widget is tracked and a `fin`
/// line when it is finalized, and remember which labels never finalized.
///
/// The decisive GTK leak check: a `GObject` only finalizes when no Rust
/// closure still holds it, so a label seen `new` but never `fin` names the
/// exact widget a reference cycle pinned. Output matches the `[mem_audit]
/// object new/fin <label>` format the census scripts grep for.
#[derive(Default, Clone)]
pub struct WidgetFinalizeCensus {
    live: Rc<RefCell<BTreeMap<String, i64>>>,
}

impl WidgetFinalizeCensus {
    /// New empty census.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Track `object`: emit `new` now and `fin` when it is finalized.
    ///
    /// # Panics
    ///
    /// Must be called on the initialized GTK main thread. Tracked objects must
    /// also finalize on that thread, as with local weak notifications.
    pub fn track(&self, label: impl Into<String>, object: &impl IsA<glib::Object>) {
        assert!(gtk::is_initialized_main_thread());
        let label = label.into();
        eprintln!("[mem_audit] object new    {label}");
        *self.live.borrow_mut().entry(label.clone()).or_insert(0) += 1;
        let live = self.live.clone();
        let watch = Subscription::new(move || {
            eprintln!("[mem_audit] object fin    {label}");
            let mut live = live.borrow_mut();
            if let Some(count) = live.get_mut(&label) {
                *count -= 1;
                if *count == 0 {
                    live.remove(&label);
                }
            }
        });
        let key = glib::Quark::from_str("br.com.biglinux.big-testkit.finalization-watches");
        // Qdata is destroyed at finalization; weak notifications fire at disposal,
        // which can leave strong references alive. Keep every census on one key.
        // SAFETY: this private key only ever stores this type, and ThreadGuard
        // keeps the Rc-based watches on the thread that installed them.
        unsafe {
            if let Some(watches) =
                object.qdata::<glib::thread_guard::ThreadGuard<RefCell<Vec<Subscription>>>>(key)
            {
                watches.as_ref().get_ref().borrow_mut().push(watch);
            } else {
                object.set_qdata(
                    key,
                    glib::thread_guard::ThreadGuard::new(RefCell::new(vec![watch])),
                );
            }
        }
    }

    /// Labels tracked `new` more often than they were finalized: the leaked
    /// widgets. Empty slice ⇒ clean teardown.
    #[must_use]
    pub fn survivors(&self) -> Vec<String> {
        self.live
            .borrow()
            .iter()
            .filter(|&(_, &count)| count > 0)
            .map(|(label, _)| label.clone())
            .collect()
    }
}

// RSS-slope verdict (pure).

/// Advisory verdict for a run of post-close RSS samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Fewer than three post-warm-up samples: cannot judge.
    Inconclusive,
    /// Slope within threshold: a flat plateau, clean.
    Pass,
    /// Growing but the late per-cycle delta is far below the early one:
    /// cache warming, not a steady leak.
    Decelerating,
    /// Steady growth above threshold: a leak is suspected.
    Leak,
}

impl Verdict {
    /// Only [`Verdict::Leak`] fails a gate; `Decelerating` / `Inconclusive`
    /// are advisory non-failures.
    #[must_use]
    pub fn is_leak(self) -> bool {
        matches!(self, Verdict::Leak)
    }
}

/// Post-close RSS samples plus the leak verdict math (warm-up drop,
/// least-squares slope, deceleration heuristic). Feed it sampled KiB; it needs
/// no display.
#[derive(Debug, Default, Clone)]
pub struct RssSlope {
    samples: Vec<u64>,
}

impl RssSlope {
    /// Empty sampler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one post-close RSS reading (KiB).
    pub fn push(&mut self, rss_kib: u64) {
        self.samples.push(rss_kib);
    }

    /// Every recorded sample, in cycle order.
    #[must_use]
    pub fn samples(&self) -> &[u64] {
        &self.samples
    }

    /// Samples past the warm-up window; falls back to all samples when the
    /// series is too short to drop a warm-up.
    fn post_warmup(&self) -> &[u64] {
        if self.samples.len() > WARMUP + 2 {
            &self.samples[WARMUP..]
        } else {
            &self.samples
        }
    }

    /// Least-squares slope in KiB/cycle over the post-warm-up samples (`0.0`
    /// with fewer than two).
    #[must_use]
    pub fn slope_kib_per_cycle(&self) -> f64 {
        least_squares_slope(self.post_warmup())
    }

    /// Classify the series: slope within `threshold_kib` ⇒ [`Verdict::Pass`];
    /// else a late/early delta ratio below `decel_ratio` ⇒
    /// [`Verdict::Decelerating`] (cache warming); else [`Verdict::Leak`].
    /// Fewer than three post-warm-up samples ⇒ [`Verdict::Inconclusive`].
    #[must_use]
    pub fn verdict(&self, threshold_kib: f64, decel_ratio: f64) -> Verdict {
        let series = self.post_warmup();
        let sample_count = series.len();
        if sample_count < 3 {
            return Verdict::Inconclusive;
        }
        let slope = least_squares_slope(series);
        if slope <= threshold_kib {
            return Verdict::Pass;
        }
        let (early_delta, late_delta) = split_half_deltas(series);
        if late_delta < early_delta * decel_ratio {
            Verdict::Decelerating
        } else {
            Verdict::Leak
        }
    }
}

/// Least-squares slope of `series` against `x = 1..=n` (`0.0` if `n < 2`).
fn least_squares_slope(series: &[u64]) -> f64 {
    let sample_count = series.len();
    if sample_count < 2 {
        return 0.0;
    }
    let count = sample_count as f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_xy = 0.0;
    for (index, &sample) in series.iter().enumerate() {
        let x = (index + 1) as f64;
        let y = sample as f64;
        sum_x += x;
        sum_y += y;
        sum_xx += x * x;
        sum_xy += x * y;
    }
    let denominator = count * sum_xx - sum_x * sum_x;
    if denominator == 0.0 {
        0.0
    } else {
        (count * sum_xy - sum_x * sum_y) / denominator
    }
}

/// Average per-cycle delta over the first vs the second half of `series`.
fn split_half_deltas(series: &[u64]) -> (f64, f64) {
    let sample_count = series.len();
    let half = sample_count / 2;
    let delta = |k: usize| series[k] as f64 - series[k - 1] as f64;

    let mut early = 0.0;
    for k in 1..half {
        early += delta(k);
    }
    if half > 1 {
        early /= (half - 1) as f64;
    }

    let mut late = 0.0;
    for k in half..sample_count {
        late += delta(k);
    }
    if sample_count - half > 0 {
        late /= (sample_count - half) as f64;
    }

    (early, late)
}

/// Parallel slope series for one coherent `/proc/<pid>/smaps_rollup` sample per
/// cycle.
///
/// Keeping the fields together prevents a census from accidentally comparing
/// PSS captured at one instant with private-dirty memory captured at another.
/// New retention gates should normally use [`Self::pss_anon`] as the headline
/// and [`Self::private_dirty`] as supporting evidence; RSS is not tracked
/// because file-backed/shared caches can legitimately move it.
#[derive(Debug, Default, Clone)]
pub struct SmapsRollupSlopes {
    pss: RssSlope,
    pss_anon: RssSlope,
    private_dirty: RssSlope,
    anonymous: RssSlope,
}

impl SmapsRollupSlopes {
    /// Empty set of parallel memory series.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one internally consistent kernel rollup sample.
    pub fn push(&mut self, sample: crate::harness::SmapsRollupKib) {
        self.pss.push(sample.pss);
        self.pss_anon.push(sample.pss_anon);
        self.private_dirty.push(sample.private_dirty);
        self.anonymous.push(sample.anonymous);
    }

    /// Proportional-set-size series.
    #[must_use]
    pub fn pss(&self) -> &RssSlope {
        &self.pss
    }

    /// Anonymous proportional-set-size series; preferred retention headline.
    #[must_use]
    pub fn pss_anon(&self) -> &RssSlope {
        &self.pss_anon
    }

    /// Private dirty pages; corroborates process-owned retention.
    #[must_use]
    pub fn private_dirty(&self) -> &RssSlope {
        &self.private_dirty
    }

    /// Anonymous resident-memory series retained for old-gate comparison.
    #[must_use]
    pub fn anonymous(&self) -> &RssSlope {
        &self.anonymous
    }
}

// Plain state / drop assertions (pure).

/// A `#[must_use]` RAII unsubscribe guard: dropping it runs the stored
/// unsubscribe closure exactly once.
///
/// A long-lived registry (`Vec<Box<dyn Fn>>`) plus a dialog-scoped
/// subscription accumulates leaked closures unless the subscriber holds a
/// token that unregisters on drop.
#[must_use = "dropping the Subscription runs its unsubscribe; bind it to keep the subscription alive"]
pub struct Subscription {
    unsubscribe: Option<Box<dyn FnOnce()>>,
}

impl Subscription {
    /// Wrap an unsubscribe action; it runs when the guard drops.
    pub fn new(unsubscribe: impl FnOnce() + 'static) -> Self {
        Self {
            unsubscribe: Some(Box::new(unsubscribe)),
        }
    }

    fn run(&mut self) {
        if let Some(unsubscribe) = self.unsubscribe.take() {
            unsubscribe();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.run();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn census_waits_for_last_reference() {
        crate::harness::require_gtk_test(|| {
            let census = WidgetFinalizeCensus::new();
            let window = gtk::Window::new();
            census.track("window", &window);
            let retained = window.clone();
            gtk::prelude::GtkWindowExt::destroy(&window);
            // SAFETY: This isolated window has been destroyed and is only dropped
            // after disposal; no widget operation uses its disposed state.
            unsafe { window.run_dispose() };
            drop(window);
            crate::harness::pump();
            assert_eq!(census.survivors(), vec!["window"]);
            drop(retained);
            crate::harness::pump();
            assert!(census.survivors().is_empty());
        });
    }

    #[test]
    fn censuses_share_repeated_labels() {
        crate::harness::require_gtk_test(|| {
            let census = WidgetFinalizeCensus::new();
            let other = WidgetFinalizeCensus::new();
            let first = glib::Object::new::<glib::Object>();
            let second = glib::Object::new::<glib::Object>();
            census.track("object", &first);
            census.track("object", &first);
            census.track("object", &second);
            other.track("other", &first);
            assert_eq!(census.live.borrow()["object"], 3);
            assert_eq!(other.survivors(), vec!["other"]);
            drop(first);
            assert_eq!(census.live.borrow()["object"], 1);
            assert!(other.live.borrow().is_empty());
            drop(second);
            assert!(census.live.borrow().is_empty());
        });
    }

    #[test]
    fn subscription_runs_unsub_on_drop() {
        let calls = Rc::new(Cell::new(0));
        let calls_for_unsub = calls.clone();
        let subscription =
            Subscription::new(move || calls_for_unsub.set(calls_for_unsub.get() + 1));
        assert_eq!(calls.get(), 0, "unsub must not run before drop");
        drop(subscription);
        assert_eq!(calls.get(), 1, "unsub runs exactly once on drop");
    }
}
