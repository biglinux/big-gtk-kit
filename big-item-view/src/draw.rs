// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! One item, drawn into a render node of its own.
//!
//! The node is kept and appended again, unchanged, on every frame the item looks
//! the same: GSK's diff of the new frame against the last then sees the same node
//! and damages nothing there. Only an item whose content, state or theme changed
//! is drawn again.

use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, pango};

use crate::palette::{Palette, Role};
use crate::{
    Badge, ColumnShows, Composition, Corner, Geometry, Glyph, IconFrame, ItemContent, TickMode,
};

/// What decides how an item looks, beyond its content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct State {
    pub(crate) hovered: bool,
    pub(crate) selected: bool,
    pub(crate) cursor: bool,
    pub(crate) focus_visible: bool,
    pub(crate) view_focused: bool,
    pub(crate) drop_target: bool,
    /// An editor is laid over the name, which is therefore not drawn.
    pub(crate) editing: bool,
    /// In a tree: the row's depth, and whether it is expanded when it can be.
    pub(crate) tree: Option<(u16, Option<bool>)>,
}

/// The picture, resolved for drawing.
pub(crate) enum Picture<'a> {
    None,
    Texture(&'a gdk::Texture),
    Paintable(&'a gtk::IconPaintable),
    Placeholder,
}

/// Everything drawing one item needs.
pub(crate) struct Item<'a> {
    pub(crate) size: (f32, f32),
    /// Where the picture and the name go: across the whole item, or in the
    /// primary column of a row ([`primary_span`]), past a tree row's expander.
    pub(crate) primary: (f32, (f32, f32)),
    /// The primary span before a tree row's indent: where its expander goes.
    pub(crate) outer: (f32, (f32, f32)),
    /// The other columns' words, one per column (`None` for the primary one).
    pub(crate) cells: &'a [Option<pango::Layout>],
    pub(crate) geometry: &'a Geometry,
    pub(crate) content: &'a ItemContent,
    pub(crate) title: &'a pango::Layout,
    pub(crate) subtitle: Option<&'a pango::Layout>,
    pub(crate) picture: Picture<'a>,
    pub(crate) palette: &'a Palette,
    pub(crate) state: State,
    pub(crate) rtl: bool,
    pub(crate) display: &'a gdk::Display,
    pub(crate) scale: i32,
}

/// The item's background, inside its margins.
pub(crate) fn background(geometry: &Geometry, size: (f32, f32)) -> graphene::Rect {
    let (my, mx) = (geometry.margin.0 as f32, geometry.margin.1 as f32);
    graphene::Rect::new(
        mx,
        my,
        (size.0 - 2.0 * mx).max(0.0),
        (size.1 - 2.0 * my).max(0.0),
    )
}

/// What an item's tint and rings cover when only what it draws is the item
/// ([`Geometry::hit_drawn_only`]): its picture and its words, the way Dolphin
/// and Nautilus mark a selected file, and not the empty rest of its cell. In
/// the icon grid a card as wide as the cell and as tall as the lines the name
/// takes; in a row, from the picture to the end of the longest line.
///
/// The same rectangle is what a press has to land in (`ItemView::drawn_areas`).
pub(crate) fn card(
    geometry: &Geometry,
    size: (f32, f32),
    rtl: bool,
    title: &pango::Layout,
    subtitle: Option<&pango::Layout>,
) -> graphene::Rect {
    let bg = background(geometry, size);
    let pad = geometry.padding as f32;
    // Worked out left to right and mirrored once at the end.
    let words = text_box(geometry, size, false);
    match geometry.composition {
        Composition::Stacked => {
            // A caption sits right under the name (`draw_words`).
            let text = title.pixel_extents().1.height() as f32
                + subtitle.map_or(0.0, |line| line.pixel_extents().1.height() as f32);
            let bottom = (words.y() + text + pad).min(bg.y() + bg.height());
            graphene::Rect::new(bg.x(), bg.y(), bg.width(), (bottom - bg.y()).max(0.0))
        }
        Composition::Inline => {
            let widest = |layout: &pango::Layout| {
                layout
                    .lines_readonly()
                    .iter()
                    .map(|line| line.pixel_extents().1.width())
                    .max()
                    .unwrap_or(0) as f32
            };
            let text = widest(title)
                .max(subtitle.map_or(0.0, widest))
                .min(words.width());
            let end = words.x() + text + pad;
            let rect = graphene::Rect::new(
                bg.x(),
                bg.y(),
                (end - bg.x()).clamp(0.0, bg.width()),
                bg.height(),
            );
            mirror(rect, size.0, rtl)
        }
    }
}

/// Where the picture goes.
pub(crate) fn icon_box(geometry: &Geometry, size: (f32, f32), rtl: bool) -> graphene::Rect {
    let bg = background(geometry, size);
    let (w, h) = geometry.picture();
    let pad = geometry.padding as f32;
    let (x, y) = match geometry.composition {
        Composition::Stacked => (bg.x() + (bg.width() - w) / 2.0, bg.y() + pad),
        Composition::Inline => (bg.x() + pad, bg.y() + (bg.height() - h) / 2.0),
    };
    mirror(graphene::Rect::new(x, y, w, h), size.0, rtl)
}

/// Where the words go: their box's origin and width.
pub(crate) fn text_box(geometry: &Geometry, size: (f32, f32), rtl: bool) -> graphene::Rect {
    let bg = background(geometry, size);
    let pad = geometry.padding as f32;
    let (w, h) = geometry.picture();
    let rect = match geometry.composition {
        Composition::Stacked => graphene::Rect::new(
            bg.x() + pad,
            bg.y() + pad + h + geometry.gap as f32,
            (bg.width() - 2.0 * pad).max(1.0),
            (bg.height() - 2.0 * pad - h - geometry.gap as f32).max(1.0),
        ),
        Composition::Inline => {
            let x = bg.x() + pad + w + geometry.gap as f32;
            graphene::Rect::new(
                x,
                bg.y() + pad,
                (bg.x() + bg.width() - pad - x).max(1.0),
                (bg.height() - 2.0 * pad).max(1.0),
            )
        }
    };
    mirror(rect, size.0, rtl)
}

/// The tick's hotspot: the corner of the item it is drawn on.
pub(crate) fn tick_box(geometry: &Geometry, size: (f32, f32), rtl: bool) -> graphene::Rect {
    let bg = background(geometry, size);
    let side = 18.0;
    mirror(
        graphene::Rect::new(bg.x() + 3.0, bg.y() + 3.0, side, side),
        size.0,
        rtl,
    )
}

fn mirror(rect: graphene::Rect, width: f32, rtl: bool) -> graphene::Rect {
    if rtl {
        graphene::Rect::new(
            width - rect.x() - rect.width(),
            rect.y(),
            rect.width(),
            rect.height(),
        )
    } else {
        rect
    }
}

fn rounded(rect: &graphene::Rect, radius: f32) -> gsk::RoundedRect {
    let radius = radius
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0)
        .max(0.0);
    gsk::RoundedRect::from_rect(*rect, radius)
}

fn ring(
    snapshot: &gtk::Snapshot,
    rect: &graphene::Rect,
    radius: f32,
    width: f32,
    color: gdk::RGBA,
) {
    snapshot.append_border(
        &rounded(rect, radius),
        &[width, width, width, width],
        &[color, color, color, color],
    );
}

/// One level of a tree: how far a child row is pushed in, and the side of the
/// expander's box.
pub(crate) const TREE_STEP: f32 = 16.0;

/// A tree row's primary span, pushed in past its depth and its expander.
pub(crate) fn tree_span(primary: (f32, (f32, f32)), depth: u16, rtl: bool) -> (f32, (f32, f32)) {
    let inset = TREE_STEP * f32::from(depth.saturating_add(1));
    let (x, (width, height)) = primary;
    let x = if rtl { x } else { x + inset };
    (x, ((width - inset).max(0.0), height))
}

/// Where a tree row's expander is, in the item's coordinates; `primary` is
/// the span before [`tree_span`].
pub(crate) fn expander_box(
    geometry: &Geometry,
    primary: (f32, (f32, f32)),
    depth: u16,
    rtl: bool,
) -> graphene::Rect {
    let (x, (width, height)) = primary;
    let level = geometry.margin.1 as f32 + TREE_STEP * f32::from(depth);
    let x = if rtl {
        x + width - level - TREE_STEP
    } else {
        x + level
    };
    graphene::Rect::new(x, (height - TREE_STEP) / 2.0, TREE_STEP, TREE_STEP)
}

/// The side of a star in a [`ColumnShows::Stars`] column, and of a
/// [`ColumnShows::Button`]'s icon.
pub(crate) const CELL_ICON: f32 = 14.0;

/// Where a column's icons go: `count` of them, side by side, placed in the
/// column `(x, width)` by its `xalign`; the first icon's box.
pub(crate) fn cell_icons(
    geometry: &Geometry,
    column: &crate::Column,
    (x, width): (f32, f32),
    height: f32,
    count: usize,
    rtl: bool,
) -> graphene::Rect {
    let pad = geometry.padding as f32;
    let row = CELL_ICON * count as f32;
    let slack = (width - 2.0 * pad - row).max(0.0);
    let align = if rtl {
        1.0 - column.xalign
    } else {
        column.xalign
    };
    graphene::Rect::new(
        x + pad + slack * align,
        (height - CELL_ICON) / 2.0,
        CELL_ICON,
        CELL_ICON,
    )
}

/// Which star, 1 to 5, is at `x` (in the item's coordinates) of a
/// [`ColumnShows::Stars`] column `(x, width)`; `None` between or beside them.
pub(crate) fn star_at(
    geometry: &Geometry,
    column: &crate::Column,
    span: (f32, f32),
    height: f32,
    x: f32,
    rtl: bool,
) -> Option<u8> {
    let first = cell_icons(geometry, column, span, height, 5, rtl);
    let slot = ((x - first.x()) / CELL_ICON).floor();
    if !(0.0..5.0).contains(&slot) {
        return None;
    }
    let slot = slot as u8;
    Some(if rtl { 5 - slot } else { slot + 1 })
}

/// How big a [`Corner::Trailing`] mark is; the name leaves room for it.
pub(crate) const TRAILING_BADGE: f32 = 16.0;

/// Draw `item` and return its node.
pub(crate) fn item_node(item: &Item<'_>) -> Option<gsk::RenderNode> {
    let snapshot = gtk::Snapshot::new();
    let geometry = item.geometry;
    let palette = item.palette;
    let state = item.state;
    let bg = if geometry.hit_drawn_only && geometry.columns.is_empty() {
        card(
            geometry,
            item.primary.1,
            item.rtl,
            item.title,
            item.subtitle,
        )
        .offset_r(item.primary.0, 0.0)
    } else {
        background(geometry, item.size)
    };

    // The tint of the whole item: dropping onto it, selected, hovered. None
    // while its name is being edited: the editor is what the eye must find,
    // and a tinted, ringed card behind it only competes with it.
    let fill = if state.editing {
        None
    } else if state.drop_target {
        Some(palette.get(Role::DropFill))
    } else if state.selected {
        Some(palette.get(Role::SelectedFill))
    } else if state.cursor && state.focus_visible && palette.get(Role::CursorFill).alpha() > 0.0 {
        // Where the keyboard is, washed as well as ringed, for a consumer
        // whose items are never selected (a launcher).
        Some(palette.get(Role::CursorFill))
    } else if state.hovered {
        Some(palette.get(Role::HoverFill))
    } else {
        None
    };
    if let Some(fill) = fill {
        snapshot.push_rounded_clip(&rounded(&bg, geometry.radius));
        snapshot.append_color(&fill, &bg);
        snapshot.pop();
    }
    if state.editing {
    } else if state.drop_target {
        ring(
            &snapshot,
            &bg,
            geometry.radius,
            2.0,
            palette.get(Role::DropRing),
        );
    } else if state.cursor && state.focus_visible {
        ring(
            &snapshot,
            &bg,
            geometry.radius,
            2.0,
            palette.get(Role::FocusRing),
        );
    } else if state.selected
        && state.view_focused
        && item.content.frame != IconFrame::Cover
        && !geometry.hit_drawn_only
    {
        // (A card around what the item draws is its own mark, as in Dolphin.)
        // (A cover's own frame already marks it; a second ring is noise.)
        ring(
            &snapshot,
            &bg,
            geometry.radius,
            2.0,
            palette.get(Role::SelectedFocusRing),
        );
    }

    if let Some((depth, Some(expanded))) = state.tree {
        let spot = expander_box(geometry, item.outer, depth, item.rtl);
        let name = match (expanded, item.rtl) {
            (true, _) => "pan-down-symbolic",
            (false, false) => "pan-end-symbolic",
            (false, true) => "pan-start-symbolic",
        };
        symbolic_at(&snapshot, item, name, &spot, palette.get(Role::TextDim));
    }

    // The picture, the name and their marks, in the primary column of a row.
    snapshot.save();
    snapshot.translate(&graphene::Point::new(item.primary.0, 0.0));
    let icon = icon_box(geometry, item.primary.1, item.rtl);
    if item.content.dimmed {
        snapshot.push_opacity(0.55);
        snapshot.push_color_matrix(&grayscale(), &graphene::Vec4::zero());
    }
    draw_picture(&snapshot, item, &icon);
    if let Some(overlay) = &item.content.overlay {
        let scale = item.scale.max(1) as f32;
        let natural = (
            overlay.width() as f32 / scale,
            overlay.height() as f32 / scale,
        );
        append_texture(&snapshot, overlay, &fitted(natural, &icon), scale);
    }
    // A cover is a photograph, and a tint over it hides the thing being
    // chosen: the chosen one is framed as well, focused or not.
    if item.content.frame == IconFrame::Cover && state.selected {
        ring(
            &snapshot,
            &icon,
            8.0,
            3.0,
            palette.get(Role::SelectedFocusRing),
        );
    }
    if item.content.dimmed {
        snapshot.pop();
        snapshot.pop();
    }
    for badge in &item.content.badges {
        draw_badge(&snapshot, item, &icon, badge);
    }

    if !state.editing {
        draw_words(&snapshot, item);
    }

    match geometry.tick {
        TickMode::Selection
            if !state.editing
                && (state.hovered || (state.selected && state.cursor && state.focus_visible)) =>
        {
            draw_tick(&snapshot, item, state.selected);
        }
        TickMode::Check => draw_tick(&snapshot, item, item.content.checked.unwrap_or(false)),
        _ => {}
    }
    snapshot.restore();
    draw_cells(&snapshot, item);
    snapshot.to_node()
}

/// Where each column sits across a row `row_width` wide, as (x, width) in the
/// columns' order; mirrored in right-to-left. The first column that expands
/// takes what the others leave.
pub(crate) fn column_spans(geometry: &Geometry, row_width: f32, rtl: bool) -> Vec<(f32, f32)> {
    let mx = geometry.margin.1 as f32;
    let total: f32 = geometry
        .columns
        .iter()
        .map(|column| column.width as f32)
        .sum();
    let extra = (row_width - 2.0 * mx - total).max(0.0);
    let grows = geometry.columns.iter().position(|column| column.expand);
    let mut x = mx;
    geometry
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let width = column.width as f32 + if grows == Some(index) { extra } else { 0.0 };
            let span = if rtl {
                (row_width - x - width, width)
            } else {
                (x, width)
            };
            x += width;
            span
        })
        .collect()
}

/// Where the picture and the name are drawn in an item `size` big: the x they
/// start at, and the size they are laid out in as though that were the item.
/// The whole item, unless the geometry has columns.
pub(crate) fn primary_span(geometry: &Geometry, size: (f32, f32), rtl: bool) -> (f32, (f32, f32)) {
    let mx = geometry.margin.1 as f32;
    geometry
        .columns
        .iter()
        .zip(column_spans(geometry, size.0, rtl))
        .find(|(column, _)| column.shows == ColumnShows::Primary)
        .map_or((0.0, size), |(_, (x, width))| {
            (x - mx, (width + 2.0 * mx, size.1))
        })
}

/// A column's words: one line, cut to the column.
pub(crate) fn cell_layout(
    context: &pango::Context,
    geometry: &Geometry,
    text: &str,
    width: f32,
) -> pango::Layout {
    let layout = pango::Layout::new(context);
    layout.set_text(text);
    layout.set_ellipsize(pango::EllipsizeMode::End);
    let inner = (width - 2.0 * geometry.padding as f32).max(1.0);
    layout.set_width((inner * pango::SCALE as f32) as i32);
    layout
}

fn draw_cells(snapshot: &gtk::Snapshot, item: &Item<'_>) {
    let geometry = item.geometry;
    if geometry.columns.is_empty() {
        return;
    }
    let mut color = item.palette.get(Role::Text);
    if item.content.dimmed {
        color.set_alpha(color.alpha() * 0.7);
    }
    let pad = geometry.padding as f32;
    let spans = column_spans(geometry, item.size.0, item.rtl);
    for ((column, (x, width)), layout) in geometry.columns.iter().zip(spans).zip(item.cells) {
        match column.shows {
            ColumnShows::Stars(index) => {
                let rating = item.content.ratings.get(index).copied().unwrap_or(0);
                let first = cell_icons(geometry, column, (x, width), item.size.1, 5, item.rtl);
                for star in 0..5_u8 {
                    // The first star at the start of the reading direction.
                    let slot = if item.rtl { 4 - star } else { star };
                    let spot = first.offset_r(CELL_ICON * f32::from(slot), 0.0);
                    let (name, role) = if star < rating {
                        ("starred-symbolic", Role::Accent)
                    } else {
                        ("non-starred-symbolic", Role::TextDim)
                    };
                    symbolic_at(snapshot, item, name, &spot, item.palette.get(role));
                }
                continue;
            }
            ColumnShows::Button(name) => {
                let state = item.state;
                if !item.content.hides_button
                    && (state.hovered || (state.cursor && state.focus_visible))
                {
                    let spot = cell_icons(geometry, column, (x, width), item.size.1, 1, item.rtl);
                    symbolic_at(snapshot, item, name, &spot, item.palette.get(Role::TextDim));
                }
                continue;
            }
            ColumnShows::Primary | ColumnShows::Cell(_) => {}
        }
        let Some(layout) = layout else {
            continue;
        };
        let (_, logical) = layout.pixel_extents();
        let inner = (width - 2.0 * pad).max(0.0);
        let slack = (inner - logical.width() as f32).max(0.0);
        let align = if item.rtl {
            1.0 - column.xalign
        } else {
            column.xalign
        };
        snapshot.save();
        snapshot.push_clip(&graphene::Rect::new(x, 0.0, width, item.size.1));
        snapshot.translate(&graphene::Point::new(
            x + pad + slack * align,
            (item.size.1 - logical.height() as f32) / 2.0,
        ));
        snapshot.append_layout(layout, &color);
        snapshot.pop();
        snapshot.restore();
    }
}

fn grayscale() -> graphene::Matrix {
    // Rec. 709 luma into every channel, alpha untouched.
    let (r, g, b) = (0.2126, 0.7152, 0.0722);
    graphene::Matrix::from_float([
        r, r, r, 0.0, //
        g, g, g, 0.0, //
        b, b, b, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ])
}

/// `natural` fitted inside `bound`, centred.
fn fitted(natural: (f32, f32), bound: &graphene::Rect) -> graphene::Rect {
    let (w, h) = (natural.0.max(1.0), natural.1.max(1.0));
    let scale = (bound.width() / w).min(bound.height() / h).min(1.0);
    let (fw, fh) = (w * scale, h * scale);
    graphene::Rect::new(
        bound.x() + (bound.width() - fw) / 2.0,
        bound.y() + (bound.height() - fh) / 2.0,
        fw,
        fh,
    )
}

fn draw_picture(snapshot: &gtk::Snapshot, item: &Item<'_>, icon: &graphene::Rect) {
    let palette = item.palette;
    match item.picture {
        Picture::None => {}
        Picture::Placeholder => {
            snapshot.push_rounded_clip(&rounded(icon, 8.0));
            snapshot.append_color(&palette.get(Role::PlaceholderFill), icon);
            snapshot.pop();
        }
        Picture::Paintable(paintable) => {
            let icon = if item.content.frame == IconFrame::Small {
                let (w, h) = (icon.width() / 2.0, icon.height() / 2.0);
                graphene::Rect::new(icon.x() + w / 2.0, icon.y() + h / 2.0, w, h)
            } else {
                *icon
            };
            snapshot.save();
            snapshot.translate(&graphene::Point::new(icon.x(), icon.y()));
            let (w, h) = (f64::from(icon.width()), f64::from(icon.height()));
            // Since GTK 4.20 every icon paintable takes colours; a coloured
            // icon simply ignores them.
            let fg = palette.get(Role::Text);
            paintable.snapshot_symbolic(snapshot.upcast_ref::<gdk::Snapshot>(), w, h, &[fg]);
            snapshot.restore();
        }
        Picture::Texture(texture) if item.content.frame == IconFrame::Cover => {
            let scale = item.scale.max(1) as f32;
            let natural = (
                (texture.width() as f32).max(1.0),
                (texture.height() as f32).max(1.0),
            );
            // Grown until both sides cover the box, centred, and cut to it.
            let grow = (icon.width() / natural.0).max(icon.height() / natural.1);
            let (w, h) = (natural.0 * grow, natural.1 * grow);
            let cover = graphene::Rect::new(
                icon.x() + (icon.width() - w) / 2.0,
                icon.y() + (icon.height() - h) / 2.0,
                w,
                h,
            );
            snapshot.push_rounded_clip(&rounded(icon, 8.0));
            append_texture(snapshot, texture, &cover, scale);
            snapshot.pop();
        }
        Picture::Texture(texture) => {
            // Device pixels to logical: a texture is made at size × scale.
            let scale = item.scale.max(1) as f32;
            let natural = (
                texture.width() as f32 / scale,
                texture.height() as f32 / scale,
            );
            let rect = fitted(natural, icon);
            if item.content.frame == IconFrame::Card {
                let card = rounded(&rect, 8.0);
                snapshot.append_outset_shadow(
                    &card,
                    &palette.get(Role::CardShadow),
                    0.0,
                    1.0,
                    0.0,
                    3.0,
                );
                snapshot.push_rounded_clip(&card);
                snapshot.append_color(&palette.get(Role::CardFill), &rect);
                append_texture(snapshot, texture, &rect, scale);
                snapshot.pop();
                ring(snapshot, &rect, 8.0, 1.0, palette.get(Role::CardEdge));
            } else {
                append_texture(snapshot, texture, &rect, scale);
            }
        }
    }
}

/// A texture at `rect`, filtered only when it is being shrunk or grown.
fn append_texture(
    snapshot: &gtk::Snapshot,
    texture: &gdk::Texture,
    rect: &graphene::Rect,
    scale: f32,
) {
    let exact = (texture.width() as f32 - rect.width() * scale).abs() < 0.5
        && (texture.height() as f32 - rect.height() * scale).abs() < 0.5;
    if exact {
        snapshot.append_texture(texture, rect);
    } else {
        snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Trilinear, rect);
    }
}

fn draw_words(snapshot: &gtk::Snapshot, item: &Item<'_>) {
    let geometry = item.geometry;
    let palette = item.palette;
    let words = text_box(geometry, item.primary.1, item.rtl);
    let dim = |mut color: gdk::RGBA| {
        if item.content.dimmed {
            color.set_alpha(color.alpha() * 0.7);
        }
        color
    };
    let (_, title) = item.title.pixel_extents();
    let title_h = title.height() as f32;
    let title_color = if item.content.emphasized {
        palette.get(Role::Accent)
    } else {
        palette.get(Role::Text)
    };
    let subtitle_h = item
        .subtitle
        .map_or(0.0, |layout| layout.pixel_extents().1.height() as f32);
    let top = match geometry.composition {
        Composition::Stacked => words.y(),
        // A row centres its words; a line that is not there leaves no gap.
        Composition::Inline => words.y() + ((words.height() - title_h - subtitle_h) / 2.0).max(0.0),
    };
    snapshot.save();
    snapshot.translate(&graphene::Point::new(words.x(), top));
    snapshot.append_layout(item.title, &dim(title_color));
    snapshot.restore();
    if let Some(subtitle) = item.subtitle {
        // Right under the name, whatever lines it took: a grid row is as tall
        // as its longest name (`Metrics::fit_rows`), and a caption kept under
        // lines a short name does not have would fall outside it.
        let below = title_h;
        snapshot.save();
        snapshot.translate(&graphene::Point::new(words.x(), top + below));
        snapshot.append_layout(subtitle, &dim(palette.get(Role::TextDim)));
        snapshot.restore();
    }
}

fn draw_badge(snapshot: &gtk::Snapshot, item: &Item<'_>, icon: &graphene::Rect, badge: &Badge) {
    let d = if badge.corner == Corner::Trailing {
        TRAILING_BADGE
    } else {
        (icon.width() * 0.36).clamp(11.0, 22.0)
    };
    let inset = d * 0.25;
    let start = !item.rtl;
    let left = |at_start: bool| {
        if at_start == start {
            icon.x() - inset
        } else {
            icon.x() + icon.width() - d + inset
        }
    };
    let (x, y) = match badge.corner {
        Corner::TopStart => (left(true), icon.y() - inset),
        Corner::TopEnd => (left(false), icon.y() - inset),
        Corner::BottomStart => (left(true), icon.y() + icon.height() - d + inset),
        Corner::BottomEnd => (left(false), icon.y() + icon.height() - d + inset),
        Corner::Trailing => {
            let words = text_box(item.geometry, item.primary.1, item.rtl);
            let x = if item.rtl {
                words.x()
            } else {
                words.x() + words.width() - d
            };
            (x, words.y() + (words.height() - d) / 2.0)
        }
    };
    let disc = graphene::Rect::new(x, y, d, d);
    let round = rounded(&disc, d / 2.0);
    snapshot.push_rounded_clip(&round);
    snapshot.append_color(&badge.fill, &disc);
    snapshot.pop();
    ring(
        snapshot,
        &disc,
        d / 2.0,
        1.5,
        item.palette.get(Role::BadgeEdge),
    );
    match &badge.glyph {
        Glyph::Text(text) => {
            let layout = pango::Layout::new(&item.title.context());
            let mut font = item.title.context().font_description().unwrap_or_default();
            font.set_weight(pango::Weight::Bold);
            font.set_absolute_size(f64::from(d * 0.62) * f64::from(pango::SCALE));
            layout.set_font_description(Some(&font));
            layout.set_text(text);
            let (_, logical) = layout.pixel_extents();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                disc.x() + (d - logical.width() as f32) / 2.0,
                disc.y() + (d - logical.height() as f32) / 2.0,
            ));
            snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
            snapshot.restore();
        }
        Glyph::Icon(name) => {
            let side = (d * 0.7).round();
            let paintable = crate::icons::symbolic(item.display, name, side as i32, item.scale);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                disc.x() + (d - side) / 2.0,
                disc.y() + (d - side) / 2.0,
            ));
            paintable.snapshot_symbolic(
                snapshot.upcast_ref::<gdk::Snapshot>(),
                f64::from(side),
                f64::from(side),
                &[gdk::RGBA::WHITE],
            );
            snapshot.restore();
        }
    }
}

/// A symbolic icon filling `spot`, in `color`.
fn symbolic_at(
    snapshot: &gtk::Snapshot,
    item: &Item<'_>,
    name: &str,
    spot: &graphene::Rect,
    color: gdk::RGBA,
) {
    let paintable = crate::icons::symbolic(item.display, name, spot.width() as i32, item.scale);
    snapshot.save();
    snapshot.translate(&graphene::Point::new(spot.x(), spot.y()));
    paintable.snapshot_symbolic(
        snapshot.upcast_ref::<gdk::Snapshot>(),
        f64::from(spot.width()),
        f64::from(spot.height()),
        &[color],
    );
    snapshot.restore();
}

fn draw_tick(snapshot: &gtk::Snapshot, item: &Item<'_>, on: bool) {
    let palette = item.palette;
    let spot = tick_box(item.geometry, item.primary.1, item.rtl);
    let d = 16.0;
    let disc = graphene::Rect::new(spot.x() + 1.0, spot.y() + 1.0, d, d);
    let round = rounded(&disc, d / 2.0);
    snapshot.push_rounded_clip(&round);
    snapshot.append_color(
        &palette.get(if on { Role::TickOnBg } else { Role::TickBg }),
        &disc,
    );
    snapshot.pop();
    if !on {
        ring(snapshot, &disc, d / 2.0, 1.0, palette.get(Role::TickBorder));
    }
    let side = 12.0;
    let paintable = crate::icons::symbolic(
        item.display,
        "object-select-symbolic",
        side as i32,
        item.scale,
    );
    let fg = if on {
        palette.get(Role::TickOnFg)
    } else {
        palette.get(Role::TickBorder)
    };
    snapshot.save();
    snapshot.translate(&graphene::Point::new(
        disc.x() + (d - side) / 2.0,
        disc.y() + (d - side) / 2.0,
    ));
    paintable.snapshot_symbolic(
        snapshot.upcast_ref::<gdk::Snapshot>(),
        f64::from(side),
        f64::from(side),
        &[fg],
    );
    snapshot.restore();
}

/// A layout for the item's name in `width` pixels.
pub(crate) fn title_layout(
    context: &pango::Context,
    geometry: &Geometry,
    text: &str,
    width: f32,
) -> pango::Layout {
    let layout = pango::Layout::new(context);
    layout.set_text(text);
    layout.set_width((width.max(1.0) * pango::SCALE as f32) as i32);
    layout.set_ellipsize(pango::EllipsizeMode::End);
    if geometry.title_lines > 1 {
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_height(-i32::from(geometry.title_lines));
    }
    layout.set_alignment(match geometry.composition {
        Composition::Stacked => pango::Alignment::Center,
        Composition::Inline => pango::Alignment::Left,
    });
    layout.set_auto_dir(true);
    layout
}

/// A layout for the line under the name.
pub(crate) fn subtitle_layout(
    context: &pango::Context,
    geometry: &Geometry,
    text: &str,
    width: f32,
) -> pango::Layout {
    let layout = pango::Layout::new(context);
    layout.set_text(text);
    layout.set_width((width.max(1.0) * pango::SCALE as f32) as i32);
    layout.set_ellipsize(geometry.subtitle_ellipsize);
    layout.set_alignment(match geometry.composition {
        Composition::Stacked => pango::Alignment::Center,
        Composition::Inline => pango::Alignment::Left,
    });
    layout.set_auto_dir(true);
    layout
}

/// The height of one line of `context`'s font.
pub(crate) fn line_height(context: &pango::Context) -> f32 {
    let metrics = context.metrics(None, None);
    ((metrics.ascent() + metrics.descent()) as f32 / pango::SCALE as f32).ceil()
}

/// How tall a cell of `geometry` is with lines `title_line` and `caption_line` high.
pub(crate) fn cell_height(geometry: &Geometry, title_line: f32, caption_line: f32) -> f32 {
    let (my, pad) = (geometry.margin.0 as f32, geometry.padding as f32);
    let text = title_line * f32::from(geometry.title_lines.max(1))
        + if geometry.subtitle { caption_line } else { 0.0 };
    let body = match geometry.composition {
        Composition::Stacked => geometry.picture().1 + geometry.gap as f32 + text,
        Composition::Inline => geometry.picture().1.max(text),
    };
    (2.0 * my + 2.0 * pad + body).ceil()
}

#[cfg(test)]
mod tests {
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
    fn tree_rows_are_pushed_in_by_their_depth() {
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
}
