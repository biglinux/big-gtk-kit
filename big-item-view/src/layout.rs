// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Where every item goes, as arithmetic.
//!
//! Every item of a view has the same size, so the place of item `n`, the item
//! under a point and the items on screen are each a few divisions: O(1) for a
//! folder of ten or ten thousand entries. Nothing here touches GTK, which is what
//! lets it be tested without a display.

use std::ops::Range;
use std::rc::Rc;

/// How items follow each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Left to right, then the next line down; scrolls vertically. The number of
    /// columns is as many cells as fit the width, within these bounds. One
    /// column makes a list of rows.
    RowMajor {
        /// Fewest columns, whatever the width.
        min_columns: u32,
        /// Most columns, whatever the width.
        max_columns: u32,
    },
    /// Top to bottom, then the next column to the right; scrolls horizontally.
    /// As many rows as fit the height.
    ColumnMajor,
}

/// A direction the keyboard cursor can move in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// One item back along the reading order of the line (Left in a grid).
    Previous,
    /// One item forward along the reading order of the line (Right in a grid).
    Next,
    /// The item one line up.
    Up,
    /// The item one line down.
    Down,
    /// One screenful back.
    PageBack,
    /// One screenful forward.
    PageForward,
    /// The first item.
    First,
    /// The last item.
    Last,
}

/// An axis-aligned rectangle in content coordinates (before scrolling).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// Whether `other` and this rectangle share any area.
    #[must_use]
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

/// The placement of `n` equal cells in a viewport.
///
/// A column flow may instead give each column its own width
/// ([`Self::fit_columns`]): then a column is as wide as its widest item, the way
/// Dolphin's compact view is, and the arithmetic along the scroll direction
/// becomes a search in the columns' starts. A grid may likewise give each row
/// the height of its tallest item ([`Self::fit_rows`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Metrics {
    flow: Flow,
    n: u32,
    /// Cells across the scroll direction: columns in a grid, rows in a column flow.
    lanes: u32,
    /// Distance between the starts of two neighbouring cells, horizontally.
    pitch_x: f32,
    /// Distance between the starts of two neighbouring cells, vertically.
    pitch_y: f32,
    viewport: (f32, f32),
    rtl: bool,
    /// Where each column of a column flow starts, then where the last one
    /// ends; `None` when every column is `pitch_x` wide.
    starts: Option<Rc<[f32]>>,
    /// Where each row of a grid starts, then where the last one ends; `None`
    /// when every row is `pitch_y` tall.
    row_starts: Option<Rc<[f32]>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::compute(
            Flow::RowMajor {
                min_columns: 1,
                max_columns: 1,
            },
            (1.0, 1.0),
            0,
            (0.0, 0.0),
            false,
        )
    }
}

impl Metrics {
    /// Place `n` cells of at least `cell` (width, height) in a viewport of
    /// `viewport` (width, height).
    ///
    /// The free room across the scroll direction is shared out between the cells,
    /// the way `GtkGridView` does it: the lanes fill the viewport edge to edge
    /// instead of leaving the rest in one strip at the end.
    #[must_use]
    pub fn compute(flow: Flow, cell: (f32, f32), n: u32, viewport: (f32, f32), rtl: bool) -> Self {
        let (cell_w, cell_h) = (cell.0.max(1.0), cell.1.max(1.0));
        let (width, height) = (viewport.0.max(0.0), viewport.1.max(0.0));
        let (lanes, pitch_x, pitch_y) = match flow {
            Flow::RowMajor {
                min_columns,
                max_columns,
            } => {
                let fits = (width / cell_w).floor() as u32;
                let lanes = fits.clamp(min_columns.max(1), max_columns.max(min_columns.max(1)));
                // One item per line may be wider than the room (a list with
                // columns): then the line is its own width and scrolls sideways.
                let pitch_x = if width <= 0.0 {
                    cell_w
                } else if lanes == 1 {
                    width.max(cell_w)
                } else {
                    (width / lanes as f32).max(cell_w.min(width))
                };
                (lanes, pitch_x, cell_h)
            }
            Flow::ColumnMajor => {
                let lanes = ((height / cell_h).floor() as u32).max(1);
                let pitch_y = if height > 0.0 {
                    (height / lanes as f32).max(cell_h.min(height))
                } else {
                    cell_h
                };
                (lanes, cell_w, pitch_y)
            }
        };
        Self {
            flow,
            n,
            lanes,
            pitch_x,
            pitch_y,
            viewport: (width, height),
            rtl,
            starts: None,
            row_starts: None,
        }
    }

    /// Give each row of a grid the height of its tallest item,
    /// `height(position)`, never more than the cell's own height: a row whose
    /// names all fit on one line does not keep room for three. A column flow
    /// is left as it is.
    #[must_use]
    pub fn fit_rows(mut self, height: impl Fn(u32) -> f32) -> Self {
        if !matches!(self.flow, Flow::RowMajor { .. }) {
            return self;
        }
        let lanes = self.lanes.max(1);
        let mut starts = Vec::with_capacity(self.lines() as usize + 1);
        let mut y = 0.0_f32;
        starts.push(y);
        for line in 0..self.lines() {
            let first = line * lanes;
            let last = (first + lanes).min(self.n);
            y += (first..last)
                .map(&height)
                .fold(1.0_f32, f32::max)
                .min(self.pitch_y);
            starts.push(y);
        }
        self.row_starts = Some(starts.into());
        self
    }

    /// Where row `line` of a grid starts and how tall it is.
    fn row(&self, line: u32) -> (f32, f32) {
        match &self.row_starts {
            Some(starts) => {
                let at = |index: u32| starts.get(index as usize).copied().unwrap_or(0.0);
                (at(line), at(line + 1) - at(line))
            }
            None => (line as f32 * self.pitch_y, self.pitch_y),
        }
    }

    /// The grid row that `y` falls in, which may be one past the last.
    fn row_at(&self, y: f32) -> u32 {
        match &self.row_starts {
            Some(starts) => starts
                .partition_point(|start| *start <= y)
                .saturating_sub(1) as u32,
            None => (y / self.pitch_y).floor() as u32,
        }
    }

    /// Give each column of a column flow the width of its widest item,
    /// `width(position)`, and never less than one pixel. A grid is left as it is.
    #[must_use]
    pub fn fit_columns(mut self, width: impl Fn(u32) -> f32) -> Self {
        if self.flow != Flow::ColumnMajor {
            return self;
        }
        let lanes = self.lanes.max(1);
        let mut starts = Vec::with_capacity(self.lines() as usize + 1);
        let mut x = 0.0_f32;
        starts.push(x);
        for line in 0..self.lines() {
            let first = line * lanes;
            let last = (first + lanes).min(self.n);
            x += (first..last).map(&width).fold(1.0_f32, f32::max);
            starts.push(x);
        }
        self.starts = Some(starts.into());
        self
    }

    /// Where column `line` starts and how wide it is.
    fn column(&self, line: u32) -> (f32, f32) {
        match &self.starts {
            Some(starts) => {
                let at = |index: u32| starts.get(index as usize).copied().unwrap_or(0.0);
                (at(line), at(line + 1) - at(line))
            }
            None => (line as f32 * self.pitch_x, self.pitch_x),
        }
    }

    /// The column that `x` falls in, which may be one past the last.
    fn column_at(&self, x: f32) -> u32 {
        match &self.starts {
            Some(starts) => starts
                .partition_point(|start| *start <= x)
                .saturating_sub(1) as u32,
            None => (x / self.pitch_x).floor() as u32,
        }
    }

    /// How many items there are.
    #[must_use]
    pub const fn n_items(&self) -> u32 {
        self.n
    }

    /// Cells across the scroll direction: the columns of a grid, the rows of a
    /// column flow. This is how far Up and Down move the cursor.
    #[must_use]
    pub const fn items_per_line(&self) -> u32 {
        self.lanes
    }

    /// The size one cell is drawn at.
    #[must_use]
    pub const fn cell_size(&self) -> (f32, f32) {
        (self.pitch_x, self.pitch_y)
    }

    fn lines(&self) -> u32 {
        self.n.div_ceil(self.lanes.max(1))
    }

    /// How tall the viewport is.
    #[must_use]
    pub const fn viewport_height(&self) -> f32 {
        self.viewport.1
    }

    /// The size of everything laid out, which is what the scrollbars measure.
    #[must_use]
    pub fn content_size(&self) -> (f32, f32) {
        let lines = self.lines() as f32;
        match self.flow {
            // A line may be wider than the room (a list with columns).
            Flow::RowMajor { .. } => (
                self.viewport.0.max(self.pitch_x * self.lanes as f32),
                self.row_starts
                    .as_ref()
                    .and_then(|starts| starts.last().copied())
                    .unwrap_or(lines * self.pitch_y),
            ),
            Flow::ColumnMajor => (
                self.starts
                    .as_ref()
                    .and_then(|starts| starts.last().copied())
                    .unwrap_or(lines * self.pitch_x),
                self.viewport.1,
            ),
        }
    }

    /// Where item `position` sits, in content coordinates.
    #[must_use]
    pub fn rect(&self, position: u32) -> Option<Rect> {
        if position >= self.n {
            return None;
        }
        let (line, lane) = (position / self.lanes, position % self.lanes);
        let (x, y, width, height) = match self.flow {
            Flow::RowMajor { .. } => {
                let (y, height) = self.row(line);
                (lane as f32 * self.pitch_x, y, self.pitch_x, height)
            }
            Flow::ColumnMajor => {
                let (x, width) = self.column(line);
                (x, lane as f32 * self.pitch_y, width, self.pitch_y)
            }
        };
        let x = if self.rtl {
            self.content_size().0 - x - width
        } else {
            x
        };
        Some(Rect {
            x,
            y,
            width,
            height,
        })
    }

    /// The item under `(x, y)`, in content coordinates, if any.
    #[must_use]
    pub fn position_at(&self, x: f32, y: f32) -> Option<u32> {
        let x = if self.rtl {
            self.content_size().0 - x
        } else {
            x
        };
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (col, row) = match self.flow {
            Flow::RowMajor { .. } => ((x / self.pitch_x).floor() as u32, self.row_at(y)),
            Flow::ColumnMajor => (self.column_at(x), (y / self.pitch_y).floor() as u32),
        };
        let position = match self.flow {
            Flow::RowMajor { .. } => {
                if col >= self.lanes {
                    return None;
                }
                row.checked_mul(self.lanes)?.checked_add(col)?
            }
            Flow::ColumnMajor => {
                if row >= self.lanes {
                    return None;
                }
                col.checked_mul(self.lanes)?.checked_add(row)?
            }
        };
        (position < self.n).then_some(position)
    }

    /// The items at least partly inside the viewport scrolled to `scroll`.
    #[must_use]
    pub fn visible_range(&self, scroll: (f32, f32)) -> Range<u32> {
        let (first, last) = match self.flow {
            Flow::RowMajor { .. } => {
                let along = scroll.1.max(0.0);
                let end = along + self.viewport.1;
                let last = self.row_at(end);
                // A row that ends exactly at the edge is not on screen.
                let last = if self.row(last).0 < end {
                    last + 1
                } else {
                    last
                };
                (self.row_at(along), last)
            }
            Flow::ColumnMajor => {
                let along = scroll.0.max(0.0);
                let end = along + self.viewport.0;
                let last = self.column_at(end);
                // A column that ends exactly at the edge is not on screen.
                let last = if self.column(last).0 < end {
                    last + 1
                } else {
                    last
                };
                (self.column_at(along), last)
            }
        };
        let start = first.saturating_mul(self.lanes).min(self.n);
        let end = last.saturating_mul(self.lanes).min(self.n);
        start..end
    }

    /// Every item whose cell meets `band`, a rectangle in content coordinates:
    /// what a rubber band drawn there selects. Only the lines the band crosses are
    /// looked at.
    #[must_use]
    pub fn positions_in(&self, band: &Rect) -> Vec<u32> {
        let mut found = Vec::new();
        let (first_line, last_line, lane_axis_start, lane_axis_end, lane_pitch) = match self.flow {
            Flow::RowMajor { .. } => (
                self.row_at(band.y) as f32,
                self.row_at(band.y + band.height) as f32 + 1.0,
                band.x,
                band.x + band.width,
                self.pitch_x,
            ),
            Flow::ColumnMajor => (
                self.column_at(band.x) as f32,
                self.column_at(band.x + band.width) as f32 + 1.0,
                band.y,
                band.y + band.height,
                self.pitch_y,
            ),
        };
        let first_line = first_line.max(0.0) as u32;
        let last_line = (last_line.max(0.0) as u32).min(self.lines());
        for line in first_line..last_line {
            for lane in 0..self.lanes {
                let position = line * self.lanes + lane;
                if position >= self.n {
                    break;
                }
                let Some(rect) = self.rect(position) else {
                    continue;
                };
                let lane_start = match self.flow {
                    Flow::RowMajor { .. } => rect.x,
                    Flow::ColumnMajor => rect.y,
                };
                if lane_start < lane_axis_end && lane_start + lane_pitch > lane_axis_start {
                    found.push(position);
                }
            }
        }
        found
    }

    /// Where the keyboard cursor goes from `from` when moved by `step`.
    ///
    /// In a grid Previous/Next walk the row and Up/Down jump a row; in a column
    /// flow the same keys follow the column instead, so Up and Down stay "the
    /// neighbour above and below" on screen. Right-to-left mirrors the horizontal
    /// keys, which the caller does by swapping Previous and Next.
    #[must_use]
    pub fn step(&self, from: u32, step: Step) -> Option<u32> {
        if self.n == 0 {
            return None;
        }
        let last = self.n - 1;
        let from = from.min(last);
        let lanes = self.lanes.max(1);
        let page_lines = match self.flow {
            Flow::RowMajor { .. } => (self.viewport.1 / self.pitch_y).floor().max(1.0) as u32,
            Flow::ColumnMajor => (self.viewport.0 / self.pitch_x).floor().max(1.0) as u32,
        };
        let (along_line, across_line) = match self.flow {
            // Along the line = the horizontal neighbours in a grid.
            Flow::RowMajor { .. } => (1, lanes),
            // In a column flow the vertical neighbours are one position apart.
            Flow::ColumnMajor => (lanes, 1),
        };
        // A list of one item per line has nothing beside an item: Left and
        // Right are its edges, for the keyboard to leave by.
        if matches!(self.flow, Flow::RowMajor { .. })
            && lanes == 1
            && matches!(step, Step::Previous | Step::Next)
        {
            return None;
        }
        let (back, forward) = match step {
            Step::Previous => (along_line, 0),
            Step::Next => (0, along_line),
            Step::Up => (across_line, 0),
            Step::Down => (0, across_line),
            Step::PageBack => (page_lines * lanes, 0),
            Step::PageForward => (0, page_lines * lanes),
            Step::First => return Some(0),
            Step::Last => return Some(last),
        };
        let target = if back > 0 {
            from.checked_sub(back)
        } else {
            // Down into a shorter last line lands on its last item; Down from
            // the last line is the edge, not a step along it.
            from.checked_add(forward).and_then(|target| {
                if target <= last {
                    Some(target)
                } else if matches!(step, Step::PageForward) || last / lanes > from / lanes {
                    Some(last)
                } else {
                    None
                }
            })
        };
        // A page jump that runs off either end lands on the end; a single step
        // that would leave the view stays where it is.
        match (target, step) {
            (None, Step::PageBack) => Some(0),
            (Some(target), _) if target != from || matches!(step, Step::PageForward) => {
                Some(target)
            }
            _ => None,
        }
    }
}

/// Headings over runs of items: each run's first position, and how tall a
/// heading is. A run starts on a line of its own under its heading, in a list
/// and in a grid alike, so a grid's last line in a run may be short. Lines are
/// all `metrics.cell_size().1` tall here: a run's rows are not fitted. With no
/// runs, or in a column flow, this is [`Metrics`] unchanged.
///
/// Runs are few (the months of a photo library, the days of Recent), so each
/// question walks them rather than keeping a table that would have to follow
/// every resize.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sections {
    /// Where each run starts, ascending; the first is 0.
    pub starts: Vec<u32>,
    /// A heading's height.
    pub height: f32,
}

/// Where a content line falls among the runs.
enum Line {
    /// On a heading.
    Heading,
    /// On line `line` of run `index`.
    Items { index: usize, line: u32 },
    /// Below everything.
    Past,
}

impl Sections {
    /// Whether the runs change anything: they do in a list or grid with runs.
    fn applies(&self, metrics: &Metrics) -> bool {
        !self.starts.is_empty() && matches!(metrics.flow, Flow::RowMajor { .. })
    }

    fn lanes(metrics: &Metrics) -> u32 {
        metrics.items_per_line().max(1)
    }

    /// The run `position` belongs to.
    fn run_of(&self, position: u32) -> usize {
        self.starts
            .partition_point(|start| *start <= position)
            .saturating_sub(1)
    }

    /// Where run `index` ends (exclusive).
    fn end_of(&self, metrics: &Metrics, index: usize) -> u32 {
        self.starts
            .get(index + 1)
            .copied()
            .unwrap_or(metrics.n_items())
            .min(metrics.n_items())
    }

    /// How many lines run `index` takes.
    fn lines_of(&self, metrics: &Metrics, index: usize) -> u32 {
        (self.end_of(metrics, index) - self.starts[index].min(metrics.n_items()))
            .div_ceil(Self::lanes(metrics))
    }

    /// Where the first line of run `index` starts, under its heading.
    fn top_of(&self, metrics: &Metrics, index: usize) -> f32 {
        let pitch = metrics.cell_size().1;
        (0..index)
            .map(|run| self.height + self.lines_of(metrics, run) as f32 * pitch)
            .sum::<f32>()
            + self.height
    }

    /// What the content line at `y` is.
    fn line_at(&self, metrics: &Metrics, y: f32) -> Line {
        let pitch = metrics.cell_size().1;
        let mut top = 0.0_f32;
        for index in 0..self.starts.len() {
            if y < top + self.height {
                return Line::Heading;
            }
            top += self.height;
            let lines = self.lines_of(metrics, index);
            if y < top + lines as f32 * pitch {
                let line = ((y - top) / pitch).floor() as u32;
                return Line::Items { index, line };
            }
            top += lines as f32 * pitch;
        }
        Line::Past
    }

    /// The position at `line` and `lane` of run `index`, the run's last item
    /// when that line is short.
    fn at(&self, metrics: &Metrics, index: usize, line: u32, lane: u32) -> u32 {
        let lanes = Self::lanes(metrics);
        (self.starts[index] + line * lanes + lane).min(self.end_of(metrics, index) - 1)
    }

    /// Where item `position` sits, headings included.
    #[must_use]
    pub fn rect(&self, metrics: &Metrics, position: u32) -> Option<Rect> {
        if !self.applies(metrics) {
            return metrics.rect(position);
        }
        if position >= metrics.n_items() {
            return None;
        }
        let lanes = Self::lanes(metrics);
        let index = self.run_of(position);
        let offset = position - self.starts[index];
        // A lane's place across is the same on every line; line 0 of the
        // whole grid already knows it, right to left included.
        let lane = metrics.rect(offset % lanes)?;
        Some(Rect {
            y: self.top_of(metrics, index) + (offset / lanes) as f32 * metrics.cell_size().1,
            height: metrics.cell_size().1,
            ..lane
        })
    }

    /// Where heading `index` sits.
    #[must_use]
    pub fn heading(&self, metrics: &Metrics, index: usize) -> Option<Rect> {
        if !self.applies(metrics) || index >= self.starts.len() {
            return None;
        }
        Some(Rect {
            x: 0.0,
            y: self.top_of(metrics, index) - self.height,
            width: metrics.content_size().0,
            height: self.height,
        })
    }

    /// The item under `(x, y)`, headings included; `None` on a heading.
    #[must_use]
    pub fn position_at(&self, metrics: &Metrics, x: f32, y: f32) -> Option<u32> {
        if !self.applies(metrics) {
            return metrics.position_at(x, y);
        }
        let Line::Items { index, line } = self.line_at(metrics, y) else {
            return None;
        };
        // Line 0 of the whole grid tells the lane under `x`.
        let lane = metrics.position_at(x, 0.0)?;
        let position = self.starts[index] + line * Self::lanes(metrics) + lane;
        (position < self.end_of(metrics, index)).then_some(position)
    }

    /// The items whose lines meet the content band from `top` to `bottom`.
    fn between(&self, metrics: &Metrics, top: f32, bottom: f32) -> Range<u32> {
        let n = metrics.n_items();
        let lanes = Self::lanes(metrics);
        // The first item on a line that reaches below `top`.
        let first = match self.line_at(metrics, top.max(0.0)) {
            Line::Heading => {
                let index = self
                    .starts
                    .iter()
                    .enumerate()
                    .find(|(index, _)| self.top_of(metrics, *index) > top)
                    .map_or(self.starts.len(), |(index, _)| index);
                self.starts.get(index).copied().unwrap_or(n)
            }
            Line::Items { index, line } => self.starts[index] + line * lanes,
            Line::Past => n,
        };
        // One past the last item on a line that starts above `bottom`.
        let end = match self.line_at(metrics, bottom.max(0.0)) {
            // Everything before this heading's run.
            Line::Heading => {
                let index = (0..self.starts.len())
                    .rev()
                    .find(|index| self.top_of(metrics, *index) - self.height <= bottom)
                    .unwrap_or(0);
                self.starts[index]
            }
            Line::Items { index, line } => {
                (self.starts[index] + (line + 1) * lanes).min(self.end_of(metrics, index))
            }
            Line::Past => n,
        };
        let first = first.min(n);
        first..end.min(n).max(first)
    }

    /// The items a viewport scrolled by `scroll` shows, headings included.
    #[must_use]
    pub fn visible_range(&self, metrics: &Metrics, scroll: (f32, f32)) -> Range<u32> {
        if !self.applies(metrics) {
            return metrics.visible_range(scroll);
        }
        let top = scroll.1.max(0.0);
        self.between(metrics, top, top + metrics.viewport_height())
    }

    /// The headings a viewport scrolled by `scroll` shows.
    #[must_use]
    pub fn visible_headings(&self, metrics: &Metrics, scroll: (f32, f32)) -> Range<usize> {
        let (top, bottom) = (scroll.1, scroll.1 + metrics.viewport_height());
        let shown = |index: &usize| {
            self.heading(metrics, *index)
                .is_some_and(|rect| rect.y + rect.height > top && rect.y < bottom)
        };
        let first = (0..self.starts.len())
            .find(shown)
            .unwrap_or(self.starts.len());
        let last = (first..self.starts.len())
            .take_while(shown)
            .last()
            .map_or(first, |last| last + 1);
        first..last
    }

    /// Every item whose cell meets `band`, headings included.
    #[must_use]
    pub fn positions_in(&self, metrics: &Metrics, band: &Rect) -> Vec<u32> {
        if !self.applies(metrics) {
            return metrics.positions_in(band);
        }
        self.between(metrics, band.y, band.y + band.height)
            .filter(|position| {
                self.rect(metrics, *position)
                    .is_some_and(|rect| rect.intersects(band))
            })
            .collect()
    }

    /// The size of everything laid out, headings included.
    #[must_use]
    pub fn content_size(&self, metrics: &Metrics) -> (f32, f32) {
        if !self.applies(metrics) {
            return metrics.content_size();
        }
        let height = self.top_of(metrics, self.starts.len()) - self.height;
        (metrics.content_size().0, height)
    }

    /// Where the keyboard cursor goes from `from` when moved by `step`, the way
    /// [`Metrics::step`] does it, with every run starting a line of its own:
    /// Up and Down keep the lane across a heading, and land on a short line's
    /// last item.
    #[must_use]
    pub fn step(&self, metrics: &Metrics, from: u32, step: Step) -> Option<u32> {
        if !self.applies(metrics) {
            return metrics.step(from, step);
        }
        let n = metrics.n_items();
        if n == 0 {
            return None;
        }
        let last = n - 1;
        let from = from.min(last);
        let lanes = Self::lanes(metrics);
        let line_step = |at: u32, down: bool| -> Option<u32> {
            let index = self.run_of(at);
            let offset = at - self.starts[index];
            let (line, lane) = (offset / lanes, offset % lanes);
            if down {
                if line + 1 < self.lines_of(metrics, index) {
                    Some(self.at(metrics, index, line + 1, lane))
                } else {
                    (index + 1 < self.starts.len()).then(|| self.at(metrics, index + 1, 0, lane))
                }
            } else if line > 0 {
                Some(self.at(metrics, index, line - 1, lane))
            } else {
                index
                    .checked_sub(1)
                    .map(|above| self.at(metrics, above, self.lines_of(metrics, above) - 1, lane))
            }
        };
        let page = |down: bool| -> u32 {
            let lines = (metrics.viewport_height() / metrics.cell_size().1)
                .floor()
                .max(1.0) as u32;
            let mut at = from;
            for _ in 0..lines {
                match line_step(at, down) {
                    Some(next) => at = next,
                    // Running off either end lands on the end.
                    None => return if down { last } else { 0 },
                }
            }
            at
        };
        match step {
            // A list has nothing beside an item: Left and Right are its edges.
            Step::Previous | Step::Next if lanes == 1 => None,
            Step::Previous => from.checked_sub(1),
            Step::Next => (from < last).then_some(from + 1),
            Step::Up => line_step(from, false),
            Step::Down => line_step(from, true),
            Step::PageBack => Some(page(false)),
            Step::PageForward => Some(page(true)),
            Step::First => Some(0),
            Step::Last => Some(last),
        }
    }
}

/// Where a position that was valid before a model change is afterwards.
///
/// `items-changed(position, removed, added)`: before the changed range nothing
/// moved; after it everything shifted by `added - removed`; inside it the item is
/// gone, and `None` says the caller has to look for it (a re-sort re-adds the
/// same objects in a new order).
#[must_use]
pub fn remap(old: u32, position: u32, removed: u32, added: u32) -> Option<u32> {
    if old < position {
        Some(old)
    } else if old >= position.saturating_add(removed) {
        Some(old - removed + added)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
