// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! How big the items are: the cell, and the rows and columns fitted to
//! their names.

use std::rc::Rc;

use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::draw;
use crate::layout::{Flow, Metrics};
use crate::palette::Probes;
use crate::{Composition, Geometry};

use super::imp;

/// How far a summed width ([`crate::text_width::CharWidths`]) may fall
/// short of the laid-out one: a name within this of its box is laid out to
/// tell whether it fits on one line.
const SUMMED_SHORT_BY: f32 = 2.0;

impl imp::ItemView {
    /// Drops what the items measured of themselves; it is stale.
    pub(super) fn forget_measures(&self) {
        self.natural_widths.replace(None);
        self.titles.replace(None);
        self.title_lines.replace(None);
    }

    /// `metrics` with each row as tall as its tallest item, in an icon
    /// grid whose names may take several lines.
    fn fitted_rows(&self, metrics: Metrics) -> Metrics {
        let geometry = self.geometry.borrow().clone();
        // Under headings every line is the cell's height: the runs are
        // laid out by `layout::Sections`, which does not fit rows.
        if !matches!(geometry.flow, Flow::RowMajor { .. })
            || geometry.composition != Composition::Stacked
            || geometry.title_lines < 2
            || !self.sections.borrow().starts.is_empty()
        {
            return metrics;
        }
        let (pitch_x, pitch_y) = metrics.cell_size();
        let words = draw::text_box(&geometry, (pitch_x, pitch_y), false).width();
        let Some(lines) = self.lines_at(&geometry, words) else {
            return metrics;
        };
        let (title, caption) = self.line_heights.get();
        // The height of each count of lines, worked out once.
        let heights: Vec<f32> = (1..=geometry.title_lines)
            .map(|count| {
                let mut shape = geometry.clone();
                shape.title_lines = count;
                draw::cell_height(&shape, title, caption)
            })
            .collect();
        metrics.fit_rows(|position| {
            let count = lines
                .get(position as usize)
                .copied()
                .unwrap_or(geometry.title_lines);
            heights[usize::from(count.clamp(1, geometry.title_lines)) - 1]
        })
    }

    /// How many lines each name takes in words `width` pixels wide.
    ///
    /// A name narrower than the width on one line is one line, which is
    /// most names; only the others are laid out at the width, so a window
    /// being resized pays for its long names rather than for all of them.
    fn lines_at(&self, geometry: &Geometry, width: f32) -> Option<Rc<[u8]>> {
        let key = width.round() as i32;
        if let Some((at, lines)) = self.title_lines.borrow().as_ref()
            && *at == key
        {
            return Some(lines.clone());
        }
        let titles = self.titles()?;
        let (model, presenter) = (self.model()?, self.presenter.borrow().clone()?);
        let context = self
            .probes
            .borrow()
            .as_ref()
            .map_or_else(|| self.obj().pango_context(), Probes::title_context);
        let lines: Rc<[u8]> = (0_u32..)
            .zip(titles.iter())
            .map(|(position, single)| {
                if *single + SUMMED_SHORT_BY <= width {
                    return 1;
                }
                let Some(item) = model.item(position) else {
                    return 1;
                };
                let title = presenter.title(position, &crate::view::presented(&item));
                let layout = draw::title_layout(&context, geometry, &title, width);
                u8::try_from(layout.line_count()).unwrap_or(u8::MAX)
            })
            .collect();
        self.title_lines.replace(Some((key, lines.clone())));
        Some(lines)
    }

    /// How wide each item's name is on one line.
    fn titles(&self) -> Option<Rc<[f32]>> {
        if let Some(titles) = self.titles.borrow().clone() {
            return Some(titles);
        }
        let (model, presenter) = (self.model()?, self.presenter.borrow().clone()?);
        let context = self
            .probes
            .borrow()
            .as_ref()
            .map_or_else(|| self.obj().pango_context(), Probes::title_context);
        let layout = pango::Layout::new(&context);
        let mut widths = self.title_chars.borrow_mut();
        let titles: Rc<[f32]> = (0..model.n_items())
            .map(|position| {
                model.item(position).map_or(0.0, |item| {
                    widths.measure(
                        &layout,
                        &presenter.title(position, &crate::view::presented(&item)),
                    )
                })
            })
            .collect();
        drop(widths);
        self.titles.replace(Some(titles.clone()));
        Some(titles)
    }

    /// `metrics` with each column as wide as its widest item, in a column
    /// flow; as it is otherwise. Never wider than the viewport, where a
    /// name that long is ellipsized anyway.
    pub(super) fn fitted(&self, metrics: Metrics) -> Metrics {
        let metrics = self.fitted_rows(metrics);
        if self.geometry.borrow().flow != Flow::ColumnMajor {
            return metrics;
        }
        let Some(widths) = self.natural_widths() else {
            return metrics;
        };
        let widest = self.obj().width() as f32;
        metrics.fit_columns(|position| {
            let width = widths.get(position as usize).copied().unwrap_or(1.0);
            if widest > 0.0 {
                width.min(widest)
            } else {
                width
            }
        })
    }

    /// How wide each item would be with its whole name on one line.
    fn natural_widths(&self) -> Option<Rc<[f32]>> {
        if let Some(widths) = self.natural_widths.borrow().clone() {
            return Some(widths);
        }
        let (model, presenter) = (self.model()?, self.presenter.borrow().clone()?);
        let probes = self.probes.borrow();
        let (title_context, caption_context) = match probes.as_ref() {
            Some(probes) => (probes.title_context(), probes.caption_context()),
            None => (self.obj().pango_context(), self.obj().pango_context()),
        };
        drop(probes);
        let geometry = self.geometry.borrow().clone();
        let (picture, _) = geometry.picture();
        // The words' box starts after the margin, the padding, the picture
        // and the gap, and ends a padding and a margin before the cell does
        // (`draw::text_box`); a pixel more for the rounding of each.
        let around = 2.0 * geometry.margin.1 as f32
            + 2.0 * geometry.padding as f32
            + picture
            + geometry.gap as f32
            + 2.0;
        let title_layout = pango::Layout::new(&title_context);
        let caption_layout = pango::Layout::new(&caption_context);
        let mut title_chars = self.title_chars.borrow_mut();
        let mut caption_chars = self.caption_chars.borrow_mut();
        let widths: Rc<[f32]> = (0..model.n_items())
            .map(|position| {
                let Some(item) = model.item(position) else {
                    return 1.0;
                };
                let item = crate::view::presented(&item);
                let title = title_chars.measure(&title_layout, &presenter.title(position, &item));
                let subtitle = if geometry.subtitle {
                    presenter
                        .subtitle(position, &item)
                        .map_or(0.0, |text| caption_chars.measure(&caption_layout, &text))
                } else {
                    0.0
                };
                around + title.max(subtitle)
            })
            .collect();
        drop((title_chars, caption_chars));
        self.natural_widths.replace(Some(widths.clone()));
        Some(widths)
    }

    pub(crate) fn cell_size(&self) -> (f32, f32) {
        let geometry = self.geometry.borrow();
        let (title, caption) = self.line_heights.get();
        // A list with columns is as wide as its columns.
        let width = if geometry.columns.is_empty() {
            geometry.cell_width
        } else {
            geometry
                .columns
                .iter()
                .map(|column| column.width)
                .sum::<i32>()
                + 2 * geometry.margin.1
        };
        (
            width.max(1) as f32,
            draw::cell_height(&geometry, title, caption),
        )
    }
}
