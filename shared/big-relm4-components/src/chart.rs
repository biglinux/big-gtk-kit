// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! A rolling history graph: the shape behind a number.
//!
//! A reading answers "how much now"; the question underneath is usually "is this
//! normal", and a minute of history drawn in a card's width answers it. One
//! widget for every graph of the suite, so a processor in the desktop's card and
//! in the System Monitor look like the same thing.
//!
//! Drawn only while mapped, from samples the caller already has: the graph never
//! reads anything or keeps a timer of its own.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};

/// What the vertical axis spans.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// `0..=max`, fixed: a percentage, where autoscaling would make a quiet
    /// processor look as busy as a loaded one.
    Fixed(f64),
    /// `0..=` a round number above the busiest sample, which moves only when the
    /// samples leave it far behind: a rate, whose ceiling nobody knows.
    Auto,
}

/// How one series is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// The person's accent, line over a fading fill: the series that matters.
    Accent,
    /// The text color, a thinner dashed line: the series beside it.
    Quiet,
    /// A share of time, 0–100, hatched up from the baseline in the text
    /// color whatever the graph's scale: the time spent waiting under what
    /// was done.
    Waiting,
}

struct State {
    series: Vec<(Ink, Vec<Option<f64>>)>,
    capacity: usize,
    scale: Scale,
    ceiling: f64,
    /// The accent series in the text color: a graph beside the one the
    /// person chose.
    neutral: bool,
    /// The legend's keys, drawn in the graph's colors.
    keys: Vec<glib::WeakRef<gtk::DrawingArea>>,
}

/// One graph and its samples.
#[derive(Clone)]
pub struct HistoryChart {
    area: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
}

impl HistoryChart {
    /// A graph of `inks.len()` series keeping `capacity` samples each, drawn
    /// `height` pixels tall.
    #[must_use]
    pub fn new(inks: &[Ink], capacity: usize, scale: Scale, height: i32) -> Self {
        let state = Rc::new(RefCell::new(State {
            series: inks.iter().map(|ink| (*ink, Vec::new())).collect(),
            capacity: capacity.max(2),
            scale,
            ceiling: match scale {
                Scale::Fixed(max) => max,
                Scale::Auto => 1.0,
            },
            neutral: false,
            keys: Vec::new(),
        }));
        let area = gtk::DrawingArea::builder()
            .content_height(height)
            .hexpand(true)
            .css_classes(["big-history-chart"])
            .build();
        // An image with a description the caller keeps current: the figures in
        // words, which is what the shape says.
        area.set_accessible_role(gtk::AccessibleRole::Img);
        {
            let state = Rc::downgrade(&state);
            area.set_draw_func(move |area, context, width, height| {
                if let Some(state) = state.upgrade() {
                    // The accent the person chose, never a color written here.
                    let accent = adw::StyleManager::default().accent_color_rgba();
                    let size = (f64::from(width), f64::from(height));
                    draw(context, size, &state.borrow(), area.color(), accent);
                }
            });
        }
        Self { area, state }
    }

    /// The widget that draws the graph.
    #[must_use]
    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Add one sample per series; `None` is a gap (a reading that failed), never
    /// a zero.
    pub fn push(&self, values: &[Option<f64>]) {
        {
            let mut state = self.state.borrow_mut();
            let capacity = state.capacity;
            for ((_, samples), value) in state.series.iter_mut().zip(values) {
                samples.push(*value);
                let overflow = samples.len().saturating_sub(capacity);
                samples.drain(..overflow);
            }
            rescale(&mut state);
        }
        self.redraw();
    }

    /// Break every series: nothing was read for a while, and the lines stop
    /// rather than join the readings either side.
    pub fn push_gap(&self) {
        let series = self.state.borrow().series.len();
        self.push(&vec![None; series]);
    }

    /// Replace every series at once: a graph built after its readings began
    /// opens full rather than empty.
    pub fn replace(&self, series: &[Vec<f64>]) {
        {
            let mut state = self.state.borrow_mut();
            let capacity = state.capacity;
            for ((_, samples), values) in state.series.iter_mut().zip(series) {
                let skip = values.len().saturating_sub(capacity);
                *samples = values[skip..].iter().copied().map(Some).collect();
            }
            rescale(&mut state);
        }
        self.redraw();
    }

    /// Replace every series with `series`, gaps included: a graph of slots
    /// of time, some of which nothing was read in.
    pub fn set_samples(&self, series: &[Vec<Option<f64>>]) {
        {
            let mut state = self.state.borrow_mut();
            let capacity = state.capacity;
            for ((_, samples), values) in state.series.iter_mut().zip(series) {
                let skip = values.len().saturating_sub(capacity);
                *samples = values[skip..].to_vec();
            }
            rescale(&mut state);
        }
        self.redraw();
    }

    /// Each series' samples, oldest first, `None` for a gap: what a table
    /// of the graph lists.
    #[must_use]
    pub fn samples(&self) -> Vec<Vec<Option<f64>>> {
        self.state
            .borrow()
            .series
            .iter()
            .map(|(_, samples)| samples.clone())
            .collect()
    }

    /// The value the top edge stands for, for a label beside the graph.
    #[must_use]
    pub fn ceiling(&self) -> f64 {
        self.state.borrow().ceiling
    }

    /// Draw the accent series in the text color, for a graph beside the one
    /// that matters, or in the accent again.
    pub fn set_neutral(&self, neutral: bool) {
        let changed = std::mem::replace(&mut self.state.borrow_mut().neutral, neutral) != neutral;
        if changed {
            self.redraw();
            for key in self
                .state
                .borrow()
                .keys
                .iter()
                .filter_map(glib::WeakRef::upgrade)
            {
                key.queue_draw();
            }
        }
    }

    /// A key for a legend beside the words of the series drawn in `ink`,
    /// which turns to the text color with the graph.
    #[must_use]
    pub fn key(&self, ink: Ink) -> gtk::DrawingArea {
        let state = Rc::downgrade(&self.state);
        let area = key_area(ink, move || {
            state.upgrade().is_some_and(|state| state.borrow().neutral)
        });
        self.state.borrow_mut().keys.push(area.downgrade());
        area
    }

    /// What a screen reader says for the graph.
    pub fn set_description(&self, text: &str) {
        self.area
            .update_property(&[gtk::accessible::Property::Label(text)]);
    }

    fn redraw(&self) {
        if self.area.is_mapped() {
            self.area.queue_draw();
        }
    }
}

/// Keep an automatic ceiling round, and still: it grows at once when a sample
/// passes it and shrinks only when everything fits in a quarter of it, so the
/// graph does not jump with every reading.
fn rescale(state: &mut State) {
    let Scale::Auto = state.scale else { return };
    let busiest = state
        .series
        .iter()
        .filter(|(ink, _)| *ink != Ink::Waiting)
        .flat_map(|(_, samples)| samples.iter().flatten())
        .copied()
        .fold(0.0_f64, f64::max);
    if busiest > state.ceiling || busiest < state.ceiling / 4.0 {
        state.ceiling = round_ceiling(busiest);
    }
}

/// The smallest 1, 2 or 5 times a power of ten of a binary unit (B, KiB, MiB
/// ...) above `value` (at least 1): the rate labels print binary units, so
/// the ceiling reads as a whole number of the unit it is labeled in.
#[must_use]
pub fn round_ceiling(value: f64) -> f64 {
    if !value.is_finite() || value <= 1.0 {
        return 1.0;
    }
    let unit = 1024_f64.powf((value.log2() / 10.0).floor());
    [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1024.0]
        .into_iter()
        .map(|step| step * unit)
        .find(|candidate| *candidate >= value)
        .unwrap_or(1024.0 * unit)
}

/// Draw `state` at `size` in the text color `ink` and the person's `accent`.
fn draw(
    context: &cairo::Context,
    (width, height): (f64, f64),
    state: &State,
    ink: gdk::RGBA,
    accent: gdk::RGBA,
) {
    let accent = if state.neutral {
        neutral_ink(ink)
    } else {
        accent
    };
    // The baseline is always there, so an empty graph reads as "nothing yet"
    // rather than as a card that failed to load.
    set(context, ink, 0.16);
    context.set_line_width(1.0);
    context.move_to(0.0, height - 0.5);
    context.line_to(width, height - 0.5);
    let _ = context.stroke();

    let step = width / (state.capacity - 1) as f64;
    // Each series as the points of its unbroken stretches, the waiting first:
    // it goes under the lines it explains.
    let mut traced: Vec<(Ink, Vec<Vec<(f64, f64)>>)> = state
        .series
        .iter()
        .map(|(series_ink, samples)| {
            // Right-aligned: the newest sample is always at the right edge.
            let first = state.capacity.saturating_sub(samples.len());
            let ceiling = if *series_ink == Ink::Waiting {
                100.0
            } else {
                state.ceiling
            };
            let point = |index: usize, value: f64| {
                (
                    (first + index) as f64 * step,
                    height - (value / ceiling).clamp(0.0, 1.0) * (height - 2.0) - 1.0,
                )
            };
            let stretches = runs(samples)
                .into_iter()
                .filter(|run| run.len() >= 2)
                .map(|run| {
                    run.iter()
                        .map(|(index, value)| point(*index, *value))
                        .collect()
                })
                .collect();
            (*series_ink, stretches)
        })
        .collect();
    traced.sort_by_key(|(series_ink, _)| *series_ink != Ink::Waiting);
    let (waiting, lines) =
        traced.split_at(traced.partition_point(|(series_ink, _)| *series_ink == Ink::Waiting));
    for points in waiting.iter().flat_map(|(_, stretches)| stretches) {
        trace(context, points);
        hatch_under(context, points, (width, height), ink);
    }
    if !waiting.is_empty() {
        // A gap along every line cut out of the hatching, drawn in the same
        // text color, which would otherwise swallow a quiet line; all of them
        // first, so no gap crosses a line already drawn.
        for (series_ink, stretches) in lines {
            for points in stretches {
                let _ = context.save();
                context.set_operator(cairo::Operator::Clear);
                pen(context, *series_ink, ink, accent);
                context.set_line_width(context.line_width() + 2.0);
                context.set_dash(&[], 0.0);
                trace(context, points);
                let _ = context.stroke();
                let _ = context.restore();
            }
        }
    }
    for (series_ink, stretches) in lines {
        for points in stretches {
            trace(context, points);
            // The accent fills under its line.
            if *series_ink == Ink::Accent {
                let path = context.copy_path();
                if let (Ok(path), Some(&(first_x, _)), Some(&(last_x, _))) =
                    (path, points.first(), points.last())
                {
                    context.line_to(last_x, height);
                    context.line_to(first_x, height);
                    context.close_path();
                    let fade = cairo::LinearGradient::new(0.0, 0.0, 0.0, height);
                    fade.add_color_stop_rgba(
                        0.0,
                        f64::from(accent.red()),
                        f64::from(accent.green()),
                        f64::from(accent.blue()),
                        0.28,
                    );
                    fade.add_color_stop_rgba(
                        1.0,
                        f64::from(accent.red()),
                        f64::from(accent.green()),
                        f64::from(accent.blue()),
                        0.02,
                    );
                    let _ = context.set_source(&fade);
                    let _ = context.fill();
                    context.append_path(&path);
                }
            }
            pen(context, *series_ink, ink, accent);
            let _ = context.stroke();
        }
    }
    context.set_dash(&[], 0.0);
}

/// The unbroken stretches of a series, with each sample's position.
fn runs(samples: &[Option<f64>]) -> Vec<Vec<(usize, f64)>> {
    let mut runs = vec![Vec::new()];
    for (index, sample) in samples.iter().enumerate() {
        match sample {
            Some(value) => runs.last_mut().map_or((), |run| run.push((index, *value))),
            None => runs.push(Vec::new()),
        }
    }
    runs
}

/// A smooth line through `points`: quadratic curves between the midpoints,
/// which never rise above the highest sample the way a spline can.
fn trace(context: &cairo::Context, points: &[(f64, f64)]) {
    let Some(&(x, y)) = points.first() else {
        return;
    };
    context.move_to(x, y);
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (cx, cy) = context.current_point().unwrap_or((x0, y0));
        // Quadratic as cubic: control points two thirds of the way to (x0, y0).
        context.curve_to(
            cx + 2.0 / 3.0 * (x0 - cx),
            cy + 2.0 / 3.0 * (y0 - cy),
            mx + 2.0 / 3.0 * (x0 - mx),
            my + 2.0 / 3.0 * (y0 - my),
            mx,
            my,
        );
    }
    if let Some(&(x, y)) = points.last() {
        context.line_to(x, y);
    }
}

/// Hatch the area between the traced line through `points` and the
/// baseline, then stroke the line thin: the hatching's slant tells it from
/// the fills of the other series without color.
fn hatch_under(
    context: &cairo::Context,
    points: &[(f64, f64)],
    (width, height): (f64, f64),
    ink: gdk::RGBA,
) {
    let (Ok(line), Some(&(first_x, _)), Some(&(last_x, _))) =
        (context.copy_path(), points.first(), points.last())
    else {
        context.new_path();
        return;
    };
    context.line_to(last_x, height);
    context.line_to(first_x, height);
    context.close_path();
    let _ = context.save();
    context.clip();
    hatch(context, (width, height), ink);
    let _ = context.restore();
    context.append_path(&line);
    pen(context, Ink::Waiting, ink, ink);
    let _ = context.stroke();
}

/// The strength of the waiting's hatching and edge, of the text color's.
const WAITING_INK: f64 = 0.8;

/// Lines slanting up to the right, 4 pixels apart, over the whole area.
fn hatch(context: &cairo::Context, (width, height): (f64, f64), ink: gdk::RGBA) {
    const HATCH: f64 = 4.0;
    // A slanted line covers no pixel whole: at this strength it still keeps
    // 3:1 on a light card.
    set(context, ink, WAITING_INK);
    context.set_line_width(1.0);
    context.set_dash(&[], 0.0);
    let mut x = -height;
    while x < width {
        context.move_to(x, height);
        context.line_to(x + height, 0.0);
        x += HATCH;
    }
    let _ = context.stroke();
}

/// The line `series` is drawn with, in `ink` (the text color) or `accent`.
fn pen(context: &cairo::Context, series: Ink, ink: gdk::RGBA, accent: gdk::RGBA) {
    match series {
        Ink::Waiting => {
            set(context, ink, WAITING_INK);
            context.set_line_width(1.0);
            context.set_dash(&[], 0.0);
        }
        Ink::Accent => {
            set(context, accent, 1.0);
            context.set_line_width(1.5);
            context.set_dash(&[], 0.0);
        }
        Ink::Quiet => {
            set(context, ink, 0.6);
            context.set_line_width(1.25);
            context.set_dash(&[3.0, 2.0], 0.0);
        }
    }
}

/// A short stretch of `ink`'s line, or a hatched patch for the waiting, for
/// a legend beside the words of its series.
#[must_use]
pub fn key(ink: Ink) -> gtk::DrawingArea {
    key_area(ink, || false)
}

/// [`key`], in the text color while `neutral` says so.
fn key_area(ink: Ink, neutral: impl Fn() -> bool + 'static) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(8)
        .valign(gtk::Align::Center)
        .build();
    area.set_accessible_role(gtk::AccessibleRole::Presentation);
    area.set_draw_func(move |area, context, width, height| {
        if ink == Ink::Waiting {
            hatch(context, (f64::from(width), f64::from(height)), area.color());
            return;
        }
        // On a pixel row's center, so the thin line covers one row whole
        // rather than two at half strength.
        let middle = (f64::from(height) / 2.0).floor() + 0.5;
        context.move_to(0.0, middle);
        context.line_to(f64::from(width), middle);
        let text = area.color();
        let accent = if neutral() {
            neutral_ink(text)
        } else {
            adw::StyleManager::default().accent_color_rgba()
        };
        pen(context, ink, text, accent);
        let _ = context.stroke();
    });
    area
}

/// The accent series' color in a neutral graph: over half the text's
/// strength, so the line keeps 3:1 on the window.
fn neutral_ink(ink: gdk::RGBA) -> gdk::RGBA {
    gdk::RGBA::new(ink.red(), ink.green(), ink.blue(), ink.alpha() * 0.55)
}

fn set(context: &cairo::Context, color: gdk::RGBA, alpha: f64) {
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()) * alpha,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: f64 = 1024.0 * 1024.0;

    #[test]
    fn ceilings_are_round_in_binary_units() {
        assert_eq!(round_ceiling(0.0), 1.0);
        assert_eq!(round_ceiling(3.0), 5.0);
        assert_eq!(round_ceiling(120.0), 200.0);
        assert_eq!(round_ceiling(f64::NAN), 1.0);
        assert_eq!(round_ceiling(1_000_000.0), MIB);
        assert_eq!(round_ceiling(2_000_000.0), 2.0 * MIB);
        assert_eq!(round_ceiling(5_000_000.0), 5.0 * MIB);
        assert_eq!(round_ceiling(5.0 * MIB + 1.0), 10.0 * MIB);
        assert_eq!(round_ceiling(30_000.0), 50.0 * 1024.0);
    }

    #[test]
    fn failed_reading_is_a_gap() {
        let runs = runs(&[Some(1.0), Some(2.0), None, Some(3.0)]);
        assert_eq!(runs, vec![vec![(0, 1.0), (1, 2.0)], vec![(3, 3.0)]]);
    }

    #[test]
    fn neutral_graph_leaves_the_accent() {
        let state = State {
            series: vec![(Ink::Accent, vec![Some(50.0); 4])],
            capacity: 4,
            scale: Scale::Fixed(100.0),
            ceiling: 100.0,
            neutral: true,
            keys: Vec::new(),
        };
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 40, 20).unwrap();
        {
            let context = cairo::Context::new(&surface).unwrap();
            let black = gdk::RGBA::new(0.0, 0.0, 0.0, 1.0);
            let red = gdk::RGBA::new(1.0, 0.0, 0.0, 1.0);
            draw(&context, (40.0, 20.0), &state, black, red);
        }
        let pixels = surface.data().unwrap();
        // ARGB32 in memory order on little-endian: blue, green, red, alpha.
        let red = pixels
            .chunks(4)
            .filter(|pixel| pixel[3] > 0 && pixel[2] > pixel[1].saturating_add(16))
            .count();
        assert_eq!(red, 0, "no accent drawn");
        assert!(pixels.chunks(4).any(|pixel| pixel[3] > 0), "a line drawn");
    }

    /// A rate graph's waiting is drawn on its own 0–100 scale: a quarter of
    /// the time waiting hatches the bottom quarter, whatever the rates.
    #[test]
    fn waiting_keeps_its_own_scale() {
        let mut state = State {
            series: vec![
                (Ink::Accent, vec![Some(8.0); 4]),
                (Ink::Waiting, vec![Some(25.0); 4]),
            ],
            capacity: 4,
            scale: Scale::Auto,
            ceiling: 1.0,
            neutral: false,
            keys: Vec::new(),
        };
        rescale(&mut state);
        assert_eq!(state.ceiling, 10.0, "the waiting leaves the ceiling");
        state.series[0].1 = vec![None; 4];
        let (width, height) = (40, 40);
        let mut surface =
            cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).unwrap();
        {
            let context = cairo::Context::new(&surface).unwrap();
            let black = gdk::RGBA::new(0.0, 0.0, 0.0, 1.0);
            draw(&context, (40.0, 40.0), &state, black, black);
        }
        let stride = usize::try_from(surface.stride()).unwrap();
        let pixels = surface.data().unwrap();
        let inked = |row: usize| (0..40).any(|column| pixels[row * stride + column * 4 + 3] > 0);
        // 25 % of the 38 px drawn above the baseline's pixel: rows 29 and below.
        assert!(inked(32), "the bottom quarter hatched");
        assert!(!inked(20), "nothing at half height");
    }

    /// A quiet line below the waiting level runs through a gap in the
    /// hatching: between its dashes the band it runs in is clear.
    #[test]
    fn quiet_line_cuts_the_hatching() {
        let state = State {
            series: vec![
                (Ink::Quiet, vec![Some(17.0); 11]),
                (Ink::Waiting, vec![Some(50.0); 11]),
            ],
            capacity: 11,
            scale: Scale::Fixed(100.0),
            ceiling: 100.0,
            neutral: false,
            keys: Vec::new(),
        };
        let (width, height) = (200, 100);
        let mut surface =
            cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).unwrap();
        {
            let context = cairo::Context::new(&surface).unwrap();
            let black = gdk::RGBA::new(0.0, 0.0, 0.0, 1.0);
            draw(&context, (200.0, 100.0), &state, black, black);
        }
        let stride = usize::try_from(surface.stride()).unwrap();
        let pixels = surface.data().unwrap();
        // The line runs at 100 - 17 % of 98 - 1 = 82.3 and its gap 1.6 px
        // either side, over rows 81 and 82 whole; its dashes (3 on, 2 off from
        // the left edge) leave columns 3, 4, 8, 9, ... between them.
        let alpha = |row: usize, column: usize| pixels[row * stride + column * 4 + 3];
        let gaps = (0..200).filter(|column| column % 5 >= 3);
        let inked: Vec<(usize, usize)> = gaps
            .flat_map(|column| (81..=82).map(move |row| (row, column)))
            .filter(|&(row, column)| alpha(row, column) > 0)
            .collect();
        assert!(inked.is_empty(), "hatching along the line at {inked:?}");
        assert!(
            (0..200).any(|column| alpha(70, column) > 0),
            "the hatching drawn"
        );
    }

    #[test]
    fn automatic_ceiling_stays_steady() {
        let mut state = State {
            series: vec![(Ink::Accent, vec![Some(900.0)])],
            capacity: 4,
            scale: Scale::Auto,
            ceiling: 1.0,
            neutral: false,
            keys: Vec::new(),
        };
        rescale(&mut state);
        assert_eq!(state.ceiling, 1024.0);
        state.series[0].1 = vec![Some(400.0)];
        rescale(&mut state);
        assert_eq!(state.ceiling, 1024.0, "still within a quarter");
        state.series[0].1 = vec![Some(100.0)];
        rescale(&mut state);
        assert_eq!(state.ceiling, 100.0);
    }
}
