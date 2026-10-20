// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

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
fn headings_push_runs_down() {
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

/// A photo library by month: every run starts a line of its own under its
/// heading, so a month's short last line leaves the rest of it empty.
#[test]
fn grid_runs_start_new_lines() {
    // 10 columns of 100, rows of 120, headings 40; runs of 12, 3 and 10.
    let metrics = grid(25);
    let runs = Sections {
        starts: vec![0, 12, 15],
        height: 40.0,
    };
    let at = |position| runs.rect(&metrics, position).map(|r| (r.x, r.y));
    assert_eq!(at(0), Some((0.0, 40.0)));
    assert_eq!(at(11), Some((100.0, 160.0)), "the run's second line");
    assert_eq!(at(12), Some((0.0, 320.0)), "under the second heading");
    assert_eq!(at(15), Some((0.0, 480.0)), "under the third heading");
    assert_eq!(at(24), Some((900.0, 480.0)));
    assert_eq!(runs.heading(&metrics, 1).map(|r| r.y), Some(280.0));
    assert_eq!(runs.content_size(&metrics), (1000.0, 600.0));
    assert_eq!(runs.position_at(&metrics, 150.0, 170.0), Some(11));
    assert_eq!(
        runs.position_at(&metrics, 350.0, 170.0),
        None,
        "the empty end of a short line"
    );
    assert_eq!(runs.position_at(&metrics, 150.0, 300.0), None, "a heading");
    assert_eq!(runs.position_at(&metrics, 250.0, 330.0), Some(14));
    // A viewport 600 tall shows everything; one scrolled to 300, the
    // second and third runs.
    assert_eq!(runs.visible_range(&metrics, (0.0, 0.0)), 0..25);
    assert_eq!(runs.visible_range(&metrics, (0.0, 300.0)), 12..25);
    assert_eq!(runs.visible_headings(&metrics, (0.0, 300.0)), 1..3);
    let band = Rect {
        x: 50.0,
        y: 150.0,
        width: 200.0,
        height: 200.0,
    };
    assert_eq!(
        runs.positions_in(&metrics, &band),
        vec![0, 1, 2, 10, 11, 12, 13, 14],
        "three lines, the heading between them skipped"
    );
    // Right to left mirrors the lanes, not the lines.
    let rtl = Metrics::compute(GRID, (100.0, 120.0), 25, (1000.0, 600.0), true);
    assert_eq!(
        runs.rect(&rtl, 12).map(|r| (r.x, r.y)),
        Some((900.0, 320.0))
    );
    assert_eq!(runs.position_at(&rtl, 950.0, 330.0), Some(12));
}

/// Up and Down keep the column across a heading, the way the items are
/// drawn, and a short line takes the cursor to its last item.
#[test]
fn keys_cross_headings_in_column() {
    let metrics = grid(25);
    let runs = Sections {
        starts: vec![0, 12, 15],
        height: 40.0,
    };
    let step = |from, step| runs.step(&metrics, from, step);
    assert_eq!(step(5, Step::Down), Some(11), "the run's short last line");
    assert_eq!(step(11, Step::Down), Some(13), "down across the heading");
    assert_eq!(step(13, Step::Up), Some(11), "up to the short line's end");
    assert_eq!(step(13, Step::Down), Some(16), "same column, next run");
    assert_eq!(step(16, Step::Up), Some(13));
    assert_eq!(step(2, Step::Up), None, "the top edge");
    assert_eq!(step(20, Step::Down), None, "the bottom edge");
    assert_eq!(step(11, Step::Next), Some(12), "reading order runs on");
    assert_eq!(step(0, Step::Previous), None);
    assert_eq!(step(3, Step::PageForward), Some(24), "five lines run off");
    assert_eq!(step(20, Step::PageBack), Some(0));
    assert_eq!(step(7, Step::Last), Some(24));
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
fn grid_places_row_by_row() {
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
fn fitted_columns_fit_widest_item() {
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
fn fitted_rows_fit_tallest_item() {
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
fn column_flow_scrolls_sideways() {
    let metrics = flow(45);
    assert_eq!(metrics.items_per_line(), 20);
    let r = metrics.rect(21).unwrap();
    assert_eq!((r.x, r.y), (200.0, 30.0));
    assert_eq!(metrics.content_size(), (600.0, 600.0));
}

#[test]
fn point_finds_item_or_gap() {
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
fn keys_follow_drawn_order() {
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
