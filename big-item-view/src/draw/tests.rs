// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

use super::*;
use crate::{Column, ColumnShows};

fn stars() -> (Geometry, Column) {
    let geometry = Geometry::rows(16);
    let column = Column {
        id: 7,
        title: String::new(),
        shows: ColumnShows::Stars(0),
        width: 100,
        expand: false,
        xalign: 0.0,
        sort: None,
    };
    (geometry, column)
}

/// A child row is pushed in one step per level past its expander, in
/// either reading direction, and its expander sits at its own level.
#[test]
fn tree_rows_indent_by_depth() {
    let geometry = Geometry::rows(16);
    let outer = (0.0, (300.0, 30.0));
    assert_eq!(
        tree_span(outer, 0, false),
        (TREE_STEP, (300.0 - TREE_STEP, 30.0))
    );
    assert_eq!(
        tree_span(outer, 2, false),
        (3.0 * TREE_STEP, (300.0 - 3.0 * TREE_STEP, 30.0))
    );
    assert_eq!(
        tree_span(outer, 2, true),
        (0.0, (300.0 - 3.0 * TREE_STEP, 30.0))
    );
    let margin = geometry.margin.1 as f32;
    let left = expander_box(&geometry, outer, 1, false);
    assert_eq!((left.x(), left.y()), (margin + TREE_STEP, 7.0));
    let right = expander_box(&geometry, outer, 1, true);
    assert_eq!(right.x(), 300.0 - margin - 2.0 * TREE_STEP);
}

/// Each star answers for its own box; the first star is at the start of
/// the reading direction, and nothing answers beside the five.
#[test]
fn a_press_finds_the_star_under_it() {
    let (geometry, column) = stars();
    let span = (40.0, 100.0);
    let first = cell_icons(&geometry, &column, span, 30.0, 5, false).x();
    let first_rtl = cell_icons(&geometry, &column, span, 30.0, 5, true).x();
    for star in 0..5_u8 {
        let step = CELL_ICON * f32::from(star) + 1.0;
        assert_eq!(
            star_at(&geometry, &column, span, 30.0, first + step, false),
            Some(star + 1)
        );
        assert_eq!(
            star_at(&geometry, &column, span, 30.0, first_rtl + step, true),
            Some(5 - star)
        );
    }
    assert_eq!(
        star_at(&geometry, &column, span, 30.0, first - 1.0, false),
        None
    );
    assert_eq!(
        star_at(
            &geometry,
            &column,
            span,
            30.0,
            first + 5.0 * CELL_ICON + 1.0,
            false
        ),
        None
    );
}

/// The disc is round, centred on the picture and inside the item's
/// background, so a round tile's tint never spills into its neighbour.
#[test]
fn picture_disc_circles_the_picture() {
    let geometry = Geometry {
        picture_disc: true,
        ..Geometry::grid(120, 48)
    };
    let size = (120.0, 110.0);
    let icon = icon_box(&geometry, size, false);
    let disc = picture_disc(&geometry, size, false).expect("room for the disc");
    assert_eq!(disc.width(), disc.height());
    assert_eq!(disc.width(), 48.0 + 2.0 * geometry.padding as f32);
    assert_eq!(disc.center().x(), icon.center().x());
    assert_eq!(disc.center().y(), icon.center().y());
    let bg = background(&geometry, size);
    assert!(disc.x() >= bg.x() && disc.y() >= bg.y());
    assert!(disc.x() + disc.width() <= bg.x() + bg.width());
    assert!(disc.y() + disc.height() <= bg.y() + bg.height());
}

/// Without room for a disc around the whole picture (a narrow cell, a flat
/// picture) the item keeps its background's mark.
#[test]
fn no_disc_without_room_around_picture() {
    let narrow = Geometry {
        picture_disc: true,
        ..Geometry::grid(48, 48)
    };
    assert!(picture_disc(&narrow, (48.0, 110.0), false).is_none());
    let flat = Geometry {
        picture_disc: true,
        picture_height: Some(24),
        ..Geometry::grid(120, 48)
    };
    assert!(picture_disc(&flat, (120.0, 110.0), false).is_none());
}
