// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Frame-clock intervals of a window, which show main-loop stalls.
//!
//! A [`FrameProbe`] records the time between the ticks of a widget's frame
//! clock; [`FrameStats`] reads p50, p95, p99 and the ticks that took longer
//! than a refresh budget. A tick callback keeps the clock running every refresh
//! whether or not the window redraws, so an interval above one refresh means the
//! main loop was held, and an idle window under the probe is never idle: do not
//! attach it while measuring idle commits or wakeups. It needs a GTK display.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

/// Intervals of one recording, in the order the frames came.
#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    intervals: Vec<Duration>,
}

impl FrameStats {
    /// Statistics over `intervals`.
    #[must_use]
    pub fn new(intervals: Vec<Duration>) -> Self {
        Self { intervals }
    }

    /// Number of intervals recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.intervals.len()
    }

    /// Whether no frame was drawn while recording.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    /// The interval that `share` (0 to 1) of the frames did not exceed, by
    /// nearest rank; `None` with no frames.
    #[must_use]
    pub fn percentile(&self, share: f64) -> Option<Duration> {
        let mut sorted = self.intervals.clone();
        sorted.sort_unstable();
        let last = sorted.len().checked_sub(1)?;
        // `ceil(share * n)` is the 1-based rank; the cast stays inside `0..=n`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "rank of a short list, clamped to its bounds"
        )]
        let rank = (share.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
        Some(sorted[rank.saturating_sub(1).min(last)])
    }

    /// Frames that took longer than `budget`.
    #[must_use]
    pub fn over(&self, budget: Duration) -> usize {
        self.intervals.iter().filter(|&&each| each > budget).count()
    }
}

/// Records the frame intervals of a widget's window until [`Self::stop`].
pub struct FrameProbe {
    tick: Option<gtk::TickCallbackId>,
    intervals: Rc<RefCell<Vec<Duration>>>,
}

impl FrameProbe {
    /// Start recording the frames `widget`'s window draws.
    #[must_use]
    pub fn start(widget: &impl IsA<gtk::Widget>) -> Self {
        let intervals = Rc::new(RefCell::new(Vec::new()));
        let previous = std::cell::Cell::new(None::<i64>);
        let tick = widget.add_tick_callback({
            let intervals = Rc::clone(&intervals);
            move |_, clock| {
                let now = clock.frame_time();
                if let Some(before) = previous.get() {
                    let micros = u64::try_from(now - before).unwrap_or(0);
                    intervals.borrow_mut().push(Duration::from_micros(micros));
                }
                previous.set(Some(now));
                glib::ControlFlow::Continue
            }
        });
        Self {
            tick: Some(tick),
            intervals,
        }
    }

    /// Stop recording and return what was seen.
    #[must_use]
    pub fn stop(mut self) -> FrameStats {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
        FrameStats::new(self.intervals.take())
    }
}

impl Drop for FrameProbe {
    fn drop(&mut self) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(values: &[u64]) -> FrameStats {
        FrameStats::new(values.iter().map(|&v| Duration::from_millis(v)).collect())
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let stats = ms(&[
            17, 16, 16, 16, 16, 16, 16, 16, 16, 40, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
        ]);
        assert_eq!(stats.percentile(0.5), Some(Duration::from_millis(16)));
        assert_eq!(stats.percentile(0.95), Some(Duration::from_millis(17)));
        assert_eq!(stats.percentile(1.0), Some(Duration::from_millis(40)));
        assert_eq!(FrameStats::default().percentile(0.95), None);
    }

    #[test]
    fn over_counts_slow_frames() {
        assert_eq!(ms(&[16, 17, 40]).over(Duration::from_micros(16_700)), 2);
    }

    /// An unattended window still ticks: the probe sees every refresh.
    #[test]
    fn probe_records_clock_ticks() {
        crate::harness::require_gtk_test(|| {
            let window = gtk::Window::builder().build();
            window.present();
            assert!(crate::harness::pump_frame(&window));
            let probe = FrameProbe::start(&window);
            let ticked = crate::harness::pump_until(|| probe.intervals.borrow().len() > 5);
            let stats = probe.stop();
            window.destroy();
            assert!(ticked, "{} intervals", stats.len());
        });
    }

    /// A held main loop shows as one interval as long as the hold.
    #[test]
    fn held_loop_is_one_long_interval() {
        crate::harness::require_gtk_test(|| {
            let window = gtk::Window::builder().build();
            window.present();
            assert!(crate::harness::pump_frame(&window));
            let probe = FrameProbe::start(&window);
            let intervals = || probe.intervals.borrow().len();
            // A tick before the hold, so the one after it has a predecessor.
            assert!(crate::harness::pump_until(|| intervals() > 0));
            let before = intervals();
            std::thread::sleep(Duration::from_millis(150));
            assert!(crate::harness::pump_until(|| intervals() > before));
            let stats = probe.stop();
            window.destroy();
            let longest = stats.percentile(1.0);
            assert!(longest >= Some(Duration::from_millis(150)), "{longest:?}");
        });
    }
}
