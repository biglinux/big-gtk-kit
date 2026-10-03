// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Where every item goes, as arithmetic.
//!
//! Every item of a view has the same size, so the place of item `n`, the item
//! under a point and the items on screen are each a few divisions — O(1) for a
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

/// Headings over runs of items, in a list of one item per line: each run's
/// first position, and how tall a heading is. Everything below a heading moves
/// down by it; with no runs this is [`Metrics`] unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sections {
    /// Where each run starts, ascending; the first is 0.
    pub starts: Vec<u32>,
    /// A heading's height.
    pub height: f32,
}

impl Sections {
    /// How many headings sit above `position`.
    fn above(&self, position: u32) -> usize {
        self.starts.partition_point(|start| *start <= position)
    }

    /// Where item `position` sits, headings included.
    #[must_use]
    pub fn rect(&self, metrics: &Metrics, position: u32) -> Option<Rect> {
        let mut rect = metrics.rect(position)?;
        rect.y += self.above(position) as f32 * self.height;
        Some(rect)
    }

    /// Where heading `index` sits.
    #[must_use]
    pub fn heading(&self, metrics: &Metrics, index: usize) -> Option<Rect> {
        let start = *self.starts.get(index)?;
        let (width, pitch) = (metrics.content_size().0, metrics.cell_size().1);
        Some(Rect {
            x: 0.0,
            y: start as f32 * pitch + index as f32 * self.height,
            width,
            height: self.height,
        })
    }

    /// The run that the content line at `y` belongs to, and where its rows begin.
    fn run_at(&self, metrics: &Metrics, y: f32) -> Option<(usize, f32)> {
        let pitch = metrics.cell_size().1;
        let index = self
            .starts
            .partition_point(|start| {
                let at = self.starts.partition_point(|other| other < start);
                *start as f32 * pitch + at as f32 * self.height <= y
            })
            .checked_sub(1)?;
        let top = self.starts[index] as f32 * pitch + (index + 1) as f32 * self.height;
        Some((index, top))
    }

    /// The item under `(x, y)`, headings included; `None` on a heading.
    #[must_use]
    pub fn position_at(&self, metrics: &Metrics, x: f32, y: f32) -> Option<u32> {
        if self.starts.is_empty() {
            return metrics.position_at(x, y);
        }
        let (index, top) = self.run_at(metrics, y)?;
        if y < top {
            return None;
        }
        let position = self.starts[index] + ((y - top) / metrics.cell_size().1).floor() as u32;
        let end = self
            .starts
            .get(index + 1)
            .copied()
            .unwrap_or(metrics.n_items());
        (position < end
            && metrics
                .rect(position)
                .is_some_and(|rect| x >= rect.x && x < rect.x + rect.width))
        .then_some(position)
    }

    /// The items a viewport scrolled by `scroll` shows, headings included.
    #[must_use]
    pub fn visible_range(&self, metrics: &Metrics, scroll: (f32, f32)) -> Range<u32> {
        if self.starts.is_empty() {
            return metrics.visible_range(scroll);
        }
        let n = metrics.n_items();
        let pitch = metrics.cell_size().1;
        let first = |y: f32| match self.run_at(metrics, y) {
            None => 0,
            Some((index, top)) => {
                let end = self.starts.get(index + 1).copied().unwrap_or(n);
                (self.starts[index] + ((y - top).max(0.0) / pitch).floor() as u32).min(end)
            }
        };
        let top = scroll.1.max(0.0);
        let start = first(top).min(n);
        let end = (first(top + metrics.viewport_height()) + 1).min(n);
        start..end.max(start)
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
        if self.starts.is_empty() {
            return metrics.positions_in(band);
        }
        let range = self.visible_range(metrics, (band.x, band.y));
        let bottom = self.visible_range(
            metrics,
            (band.x, band.y + band.height - metrics.viewport_height()),
        );
        (range.start..bottom.end.max(range.start))
            .filter(|position| {
                self.rect(metrics, *position).is_some_and(|rect| {
                    rect.y < band.y + band.height
                        && rect.y + rect.height > band.y
                        && rect.x < band.x + band.width
                        && rect.x + rect.width > band.x
                })
            })
            .collect()
    }

    /// The size of everything laid out, headings included.
    #[must_use]
    pub fn content_size(&self, metrics: &Metrics) -> (f32, f32) {
        let (width, height) = metrics.content_size();
        (width, height + self.starts.len() as f32 * self.height)
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
mod tests {
    use super::*;

    const GRID: Flow = Flow::RowMajor {
        min_columns: 1,
        max_columns: 24,
    };

    fn grid(n: u32) -> Metrics {
        // 10 columns of 100 in a viewport 1000 wide and 600 tall, rows of 120.
        Metrics::compute(GRID, (100.0, 120.0), n, (1000.0, 600.0), false)
    }

    fn flow(n: u32) -> Metrics {
        // Rows of 30 in 600 tall: 20 rows; columns 200 wide.
        Metrics::compute(Flow::ColumnMajor, (200.0, 30.0), n, (1000.0, 600.0), false)
    }

    #[test]
    fn headings_push_their_runs_down_and_are_not_items() {
        let rows = Metrics::compute(
            Flow::RowMajor {
                min_columns: 1,
                max_columns: 1,
            },
            (100.0, 20.0),
            6,
            (100.0, 50.0),
            false,
        );
        let runs = Sections {
            starts: vec![0, 3],
            height: 10.0,
        };
        assert_eq!(runs.rect(&rows, 0).map(|r| r.y), Some(10.0));
        assert_eq!(runs.rect(&rows, 2).map(|r| r.y), Some(50.0));
        assert_eq!(
            runs.rect(&rows, 3).map(|r| r.y),
            Some(80.0),
            "under the second heading"
        );
        assert_eq!(runs.position_at(&rows, 5.0, 5.0), None, "the first heading");
        assert_eq!(runs.position_at(&rows, 5.0, 15.0), Some(0));
        assert_eq!(
            runs.position_at(&rows, 5.0, 75.0),
            None,
            "the second heading"
        );
        assert_eq!(runs.position_at(&rows, 5.0, 85.0), Some(3));
        assert_eq!(runs.content_size(&rows).1, 140.0);
        assert_eq!(runs.visible_range(&rows, (0.0, 0.0)), 0..3);
        assert_eq!(runs.visible_headings(&rows, (0.0, 0.0)), 0..1);
        assert_eq!(
            runs.positions_in(
                &rows,
                &Rect {
                    x: 0.0,
                    y: 40.0,
                    width: 10.0,
                    height: 50.0
                }
            ),
            vec![1, 2, 3]
        );
        let none = Sections::default();
        assert_eq!(none.rect(&rows, 2), rows.rect(2), "no runs, no change");
    }

    #[test]
    fn columns_follow_the_width_within_their_bounds() {
        assert_eq!(grid(100).items_per_line(), 10);
        let capped = Metrics::compute(
            Flow::RowMajor {
                min_columns: 1,
                max_columns: 4,
            },
            (100.0, 120.0),
            100,
            (1000.0, 600.0),
            false,
        );
        assert_eq!(capped.items_per_line(), 4);
        assert_eq!(capped.cell_size().0, 250.0, "the room is shared out");
        let narrow = Metrics::compute(GRID, (100.0, 120.0), 100, (50.0, 600.0), false);
        assert_eq!(narrow.items_per_line(), 1, "never fewer than the minimum");
    }

    #[test]
    fn a_grid_places_row_by_row_and_measures_its_rows() {
        let metrics = grid(25);
        assert_eq!(metrics.rect(0).unwrap().x, 0.0);
        let r = metrics.rect(12).unwrap();
        assert_eq!((r.x, r.y), (200.0, 120.0));
        assert_eq!(metrics.content_size(), (1000.0, 360.0));
        assert!(metrics.rect(25).is_none());
    }

    /// Dolphin's compact view: a column is as wide as its widest name, so a
    /// column of short names takes little room and a long one is read whole.
    #[test]
    fn fitted_columns_are_as_wide_as_their_widest_item() {
        // 20 rows: items 0..20 in the first column, 20..40 in the second.
        let widths = |position: u32| if position == 25 { 300.0 } else { 80.0 };
        let fitted = flow(45).fit_columns(widths);
        assert_eq!(fitted.rect(0).map(|r| (r.x, r.width)), Some((0.0, 80.0)));
        assert_eq!(fitted.rect(21).map(|r| (r.x, r.width)), Some((80.0, 300.0)));
        assert_eq!(fitted.rect(40).map(|r| (r.x, r.width)), Some((380.0, 80.0)));
        assert_eq!(fitted.content_size().0, 460.0);
        assert_eq!(fitted.position_at(79.0, 0.0), Some(0));
        assert_eq!(fitted.position_at(81.0, 30.0), Some(21));
        assert_eq!(fitted.position_at(400.0, 0.0), Some(40));
        assert_eq!(fitted.position_at(470.0, 0.0), None);
        // A viewport 100 wide scrolled to 60 shows the first two columns.
        let narrow = Metrics::compute(Flow::ColumnMajor, (200.0, 30.0), 45, (100.0, 600.0), false)
            .fit_columns(widths);
        assert_eq!(narrow.visible_range((60.0, 0.0)), 0..40);
        assert_eq!(narrow.visible_range((380.0, 0.0)), 40..45);
        let band = Rect {
            x: 390.0,
            y: 0.0,
            width: 10.0,
            height: 50.0,
        };
        assert_eq!(fitted.positions_in(&band), vec![40, 41]);
        // Right to left mirrors each column at its own width.
        let rtl = Metrics::compute(Flow::ColumnMajor, (200.0, 30.0), 45, (1000.0, 600.0), true)
            .fit_columns(widths);
        assert_eq!(rtl.rect(0).map(|r| (r.x, r.width)), Some((380.0, 80.0)));
        assert_eq!(rtl.position_at(420.0, 0.0), Some(0));
    }

    /// A grid row as tall as its tallest item: a row of one-line names does
    /// not keep the room three lines take.
    #[test]
    fn fitted_rows_are_as_tall_as_their_tallest_item() {
        // 10 columns of 100 in 1000 wide, cells up to 120 tall.
        let heights = |position: u32| if position == 12 { 120.0 } else { 80.0 };
        let fitted = grid(25).fit_rows(heights);
        assert_eq!(fitted.rect(0).map(|r| (r.y, r.height)), Some((0.0, 80.0)));
        assert_eq!(
            fitted.rect(15).map(|r| (r.y, r.height)),
            Some((80.0, 120.0))
        );
        assert_eq!(
            fitted.rect(20).map(|r| (r.y, r.height)),
            Some((200.0, 80.0))
        );
        assert_eq!(fitted.content_size().1, 280.0);
        assert_eq!(fitted.position_at(50.0, 79.0), Some(0));
        assert_eq!(fitted.position_at(250.0, 81.0), Some(12));
        assert_eq!(fitted.position_at(50.0, 210.0), Some(20));
        assert_eq!(fitted.visible_range((0.0, 0.0)), 0..25);
        let short =
            Metrics::compute(GRID, (100.0, 120.0), 25, (1000.0, 100.0), false).fit_rows(heights);
        assert_eq!(short.visible_range((0.0, 90.0)), 10..20);
        assert_eq!(short.visible_range((0.0, 70.0)), 0..20);
        // Never taller than the cell the geometry allows.
        let capped = grid(5).fit_rows(|_| 500.0);
        assert_eq!(capped.rect(0).map(|r| r.height), Some(120.0));
    }

    #[test]
    fn a_column_flow_places_column_by_column_and_scrolls_sideways() {
        let metrics = flow(45);
        assert_eq!(metrics.items_per_line(), 20);
        let r = metrics.rect(21).unwrap();
        assert_eq!((r.x, r.y), (200.0, 30.0));
        assert_eq!(metrics.content_size(), (600.0, 600.0));
    }

    #[test]
    fn the_item_under_a_point_is_found_and_gaps_are_nothing() {
        let metrics = grid(25);
        assert_eq!(metrics.position_at(250.0, 130.0), Some(12));
        assert_eq!(
            metrics.position_at(550.0, 250.0),
            None,
            "past the last item"
        );
        assert_eq!(metrics.position_at(-1.0, 10.0), None);
        let flow = flow(45);
        assert_eq!(flow.position_at(210.0, 35.0), Some(21));
    }

    #[test]
    fn right_to_left_mirrors_the_columns() {
        let metrics = Metrics::compute(GRID, (100.0, 120.0), 25, (1000.0, 600.0), true);
        assert_eq!(metrics.rect(0).unwrap().x, 900.0);
        assert_eq!(metrics.position_at(950.0, 10.0), Some(0));
    }

    #[test]
    fn only_the_lines_on_screen_are_visible() {
        let metrics = grid(1000);
        assert_eq!(metrics.visible_range((0.0, 0.0)), 0..50, "5 rows of 10");
        assert_eq!(
            metrics.visible_range((0.0, 130.0)),
            10..70,
            "a row cut at each end"
        );
        let flow = flow(1129);
        assert_eq!(flow.visible_range((0.0, 0.0)), 0..100, "5 columns of 20");
        assert_eq!(grid(3).visible_range((0.0, 0.0)), 0..3);
    }

    #[test]
    fn a_band_selects_the_cells_it_meets() {
        let metrics = grid(25);
        let band = Rect {
            x: 150.0,
            y: 10.0,
            width: 100.0,
            height: 150.0,
        };
        assert_eq!(metrics.positions_in(&band), vec![1, 2, 11, 12]);
        let beyond = Rect {
            x: 0.0,
            y: 1000.0,
            width: 10.0,
            height: 10.0,
        };
        assert!(metrics.positions_in(&beyond).is_empty());
    }

    #[test]
    fn keys_move_the_cursor_the_way_the_items_are_drawn() {
        let metrics = grid(25);
        assert_eq!(metrics.step(12, Step::Next), Some(13));
        assert_eq!(metrics.step(12, Step::Down), Some(22));
        assert_eq!(
            metrics.step(17, Step::Down),
            Some(24),
            "the last row is short"
        );
        assert_eq!(
            metrics.step(22, Step::Down),
            None,
            "the last row is the edge"
        );
        assert_eq!(metrics.step(0, Step::Up), None);
        assert_eq!(metrics.step(3, Step::Last), Some(24));
        assert_eq!(metrics.step(24, Step::PageForward), Some(24));
        assert_eq!(metrics.step(3, Step::PageBack), Some(0));
        let flow = flow(45);
        assert_eq!(flow.step(5, Step::Down), Some(6), "down the column");
        assert_eq!(flow.step(5, Step::Next), Some(25), "the next column");
        assert_eq!(grid(0).step(0, Step::Next), None);
    }

    #[test]
    fn positions_follow_a_model_change() {
        // 3 removed and 5 added at 10.
        assert_eq!(remap(4, 10, 3, 5), Some(4));
        assert_eq!(remap(20, 10, 3, 5), Some(22));
        assert_eq!(remap(11, 10, 3, 5), None);
        assert_eq!(
            remap(0, 0, 1129, 1129),
            None,
            "a reload replaces everything"
        );
    }
}
