// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Drawing the items and keeping their drawings.

use std::collections::HashMap;
use std::ops::Range;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use crate::draw;
use crate::icons::{IconKey, IconState, Icons};
use crate::layout::{self, Metrics};
use crate::{IconFrame, IconSource, ItemContent};

use super::{HEADING_BELOW, imp, tree_of};

/// Past this many items the drawing cache starts evicting the least recently
/// drawn; a screenful is a few hundred.
const MIN_CACHE: usize = 256;

/// One item's cached drawing.
pub(super) struct Entry {
    /// Held so the pointer the entry is keyed by cannot be reused meanwhile.
    _object: glib::Object,
    pub(super) content: ItemContent,
    icon: Option<IconKey>,
    pub(super) title: Option<pango::Layout>,
    pub(super) subtitle: Option<pango::Layout>,
    /// The other columns' words, one per column.
    cells: Vec<Option<pango::Layout>>,
    /// The font serials, text width and row width the layouts were made for.
    layouts_for: (u32, u32, i32, i32),
    node: Option<gsk::RenderNode>,
    /// State, palette revision, geometry revision and size the node was drawn for.
    node_for: (draw::State, u64, u64, (i32, i32)),
    used: u64,
}

#[derive(Default)]
pub(super) struct Cache {
    pub(super) entries: HashMap<usize, Entry>,
    tick: u64,
}

impl Cache {
    fn evict(&mut self, keep: usize) {
        if self.entries.len() <= keep {
            return;
        }
        let mut ages: Vec<(u64, usize)> = self.entries.iter().map(|(k, e)| (e.used, *k)).collect();
        ages.sort_unstable();
        for (_, key) in ages.into_iter().take(self.entries.len() - keep) {
            self.entries.remove(&key);
        }
    }
}

impl imp::ItemView {
    /// Draw heading `index`: the presenter's words for the run under it.
    pub(super) fn draw_heading(
        &self,
        snapshot: &gtk::Snapshot,
        sections: &layout::Sections,
        metrics: &Metrics,
        index: usize,
        scroll: (f32, f32),
    ) {
        let (Some(rect), Some(model), Some(presenter)) = (
            sections.heading(metrics, index),
            self.model(),
            self.presenter.borrow().clone(),
        ) else {
            return;
        };
        let start = sections.starts[index];
        let Some(title) = model
            .item(start)
            .and_then(|item| presenter.section_title(start, &crate::view::presented(&item)))
        else {
            return;
        };
        let probes = self.probes.borrow();
        let Some(probes) = probes.as_ref() else {
            return;
        };
        let layout = pango::Layout::new(&probes.title_context());
        let mut font = probes
            .title_context()
            .font_description()
            .unwrap_or_default();
        font.set_weight(pango::Weight::Bold);
        layout.set_font_description(Some(&font));
        layout.set_text(&title);
        let (_, logical) = layout.pixel_extents();
        let geometry = self.geometry.borrow();
        let x = (geometry.margin.1 + geometry.padding) as f32;
        snapshot.save();
        snapshot.translate(&graphene::Point::new(
            rect.x + x - scroll.0,
            rect.y + rect.height - logical.height() as f32 - HEADING_BELOW - scroll.1,
        ));
        snapshot.append_layout(
            &layout,
            &self.palette.borrow().get(crate::palette::Role::Text),
        );
        snapshot.restore();
    }

    pub(crate) fn state_of(&self, position: u32) -> draw::State {
        let obj = self.obj();
        let focused = obj.has_focus();
        draw::State {
            hovered: self.hover.get() == Some(position),
            selected: self
                .model()
                .is_some_and(|model| model.is_selected(position)),
            cursor: self.cursor.get() == Some(position),
            focus_visible: focused
                && (self.keyboard_cursor.get()
                    || obj.state_flags().contains(gtk::StateFlags::FOCUS_VISIBLE)),
            view_focused: focused,
            drop_target: self.drop_target.get() == Some(position),
            striped: self.stripes.get() && position % 2 == 1,
            editing: self
                .editing
                .borrow()
                .as_ref()
                .is_some_and(|(edited, _, _)| edited.get() == position),
            tree: self
                .model()
                .and_then(|model| model.item(position))
                .and_then(|item| tree_of(&item)),
        }
    }

    /// The item's node, from the cache or drawn now.
    pub(super) fn item_node(&self, position: u32, size: (f32, f32)) -> Option<gsk::RenderNode> {
        let model = self.model()?;
        let item = model.item(position)?;
        let key = item.as_ptr() as usize;
        let state = self.state_of(position);
        let size_key = (size.0.round() as i32, size.1.round() as i32);
        let node_for = (
            state,
            self.palette_rev.get(),
            self.geometry_rev.get(),
            size_key,
        );

        // 1. Content, asked once per item and kept.
        let icon_source = {
            let mut cache = self.cache.borrow_mut();
            cache.tick += 1;
            let tick = cache.tick;
            let entry = match cache.entries.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    let presenter = self.presenter.borrow().clone()?;
                    let mut scratch = self.scratch.borrow_mut();
                    *scratch = ItemContent::default();
                    presenter.content(position, &crate::view::presented(&item), &mut scratch);
                    vacant.insert(Entry {
                        _object: item.clone(),
                        content: scratch.clone(),
                        icon: None,
                        title: None,
                        subtitle: None,
                        cells: Vec::new(),
                        layouts_for: (0, 0, 0, 0),
                        node: None,
                        node_for,
                        used: tick,
                    })
                }
            };
            entry.used = tick;
            if entry.node_for == node_for && entry.node.is_some() {
                return entry.node.clone();
            }
            entry.content.icon.clone()
        };

        // 2. The picture: may arrive right now, and it re-enters `icon_arrived`.
        let obj = self.obj();
        let scale = obj.scale_factor().max(1);
        let icon_size = self.geometry.borrow().icon_size;
        let (icon_key, icon_state) = match &icon_source {
            IconSource::Gicon(gicon) => {
                let key = Icons::key(gicon, icon_size, scale);
                (Some(key.clone()), self.icon(gicon, key))
            }
            _ => (None, None),
        };

        // 3. Words and drawing.
        let mut cache = self.cache.borrow_mut();
        let entry = cache.entries.get_mut(&key)?;
        entry.icon = icon_key;
        let geometry = self.geometry.borrow();
        let rtl = obj.direction() == gtk::TextDirection::Rtl;
        let outer = draw::primary_span(&geometry, size, rtl);
        let primary = match state.tree {
            Some((depth, _)) => draw::tree_span(outer, depth, rtl),
            None => outer,
        };
        let words = draw::text_box(&geometry, primary.1, rtl);
        let fonts = self.fonts.get();
        let layouts_for = (
            fonts.0,
            fonts.1,
            words.width().round() as i32,
            size.0.round() as i32,
        );
        if entry.layouts_for != layouts_for || entry.title.is_none() {
            let probes = self.probes.borrow();
            let probes = probes.as_ref()?;
            // A mark at the end of the name keeps its room.
            let trailing = entry
                .content
                .badges
                .iter()
                .any(|badge| badge.corner == crate::Corner::Trailing);
            let title_width = if trailing {
                (words.width() - draw::TRAILING_BADGE - geometry.gap as f32).max(1.0)
            } else {
                words.width()
            };
            entry.title = Some(draw::title_layout(
                &probes.title_context(),
                &geometry,
                &entry.content.title,
                title_width,
            ));
            entry.cells = geometry
                .columns
                .iter()
                .zip(draw::column_spans(&geometry, size.0, rtl))
                .map(|(column, (_, width))| match column.shows {
                    crate::ColumnShows::Primary
                    | crate::ColumnShows::Stars(_)
                    | crate::ColumnShows::Button(_) => None,
                    crate::ColumnShows::Cell(index) => Some(draw::cell_layout(
                        &probes.title_context(),
                        &geometry,
                        entry.content.cells.get(index).map_or("", String::as_str),
                        width,
                    )),
                })
                .collect();
            // Only where the shape keeps a line for it: a caption drawn in
            // a cell with no room for one lands on the next row.
            entry.subtitle = entry
                .content
                .subtitle
                .as_deref()
                .filter(|_| geometry.subtitle)
                .map(|text| {
                    draw::subtitle_layout(&probes.caption_context(), &geometry, text, words.width())
                });
            entry.layouts_for = layouts_for;
        }
        let picture = match (&entry.content.icon, &icon_state) {
            (IconSource::Texture(texture), _) => draw::Picture::Texture(texture),
            (_, Some(IconState::Texture(texture))) => draw::Picture::Texture(texture),
            (_, Some(IconState::Paintable(paintable))) => draw::Picture::Paintable(paintable),
            (IconSource::Gicon(_), _) => draw::Picture::Placeholder,
            (IconSource::None, _) if entry.content.frame == IconFrame::Cover => {
                draw::Picture::Placeholder
            }
            _ => draw::Picture::None,
        };
        let palette = self.palette.borrow();
        let display = obj.display();
        let node = draw::item_node(&draw::Item {
            size,
            primary,
            outer,
            cells: &entry.cells,
            geometry: &geometry,
            content: &entry.content,
            title: entry.title.as_ref()?,
            subtitle: entry.subtitle.as_ref(),
            picture,
            palette: &palette,
            state,
            rtl,
            display: &display,
            scale,
        });
        entry.node = node.clone();
        entry.node_for = node_for;
        drop(geometry);
        drop(palette);
        let keep = (self
            .last_range
            .get()
            .1
            .saturating_sub(self.last_range.get().0) as usize
            * 2)
        .max(MIN_CACHE);
        cache.evict(keep);
        node
    }

    /// What is known of `gicon` at this size, asking for it the first time.
    fn icon(&self, gicon: &gtk::gio::Icon, key: IconKey) -> Option<IconState> {
        if let Some(state) = self.icons.borrow().get(&key) {
            return Some(state);
        }
        if !self.icons.borrow_mut().begin(key.clone()) {
            return self.icons.borrow().get(&key);
        }
        let obj = self.obj();
        let (size, scale) = (key.1, key.2);
        let view = obj.downgrade();
        let wanted = key.clone();
        let fallback = gicon.clone();
        let deliver = move |texture: Option<gdk::Texture>| {
            let Some(view) = view.upgrade() else {
                return;
            };
            let state = match texture {
                Some(texture) => IconState::Texture(texture),
                None => IconState::Paintable(crate::icons::paintable(
                    &view.display(),
                    &fallback,
                    size,
                    scale,
                    view.direction() == gtk::TextDirection::Rtl,
                )),
            };
            view.imp().icon_arrived(wanted, state);
        };
        match self.icon_loader.get() {
            Some(load) => load(&obj.display(), gicon, size, scale, Box::new(deliver)),
            None => deliver(None),
        }
        self.icons.borrow().get(&key)
    }

    /// Whether `item`, as last drawn, has no button. Only drawn items
    /// are pressed.
    pub(crate) fn hides_button(&self, item: &glib::Object) -> bool {
        self.cache
            .borrow()
            .entries
            .get(&(item.as_ptr() as usize))
            .is_some_and(|entry| entry.content.hides_button)
    }

    fn icon_arrived(&self, key: IconKey, state: IconState) {
        self.icons.borrow_mut().settle(key.clone(), state);
        // Only the items drawing this icon are drawn again. The cache may be
        // borrowed when a cached icon answers at once, inside `item_node`;
        // that item picks the icon up itself.
        if let Ok(mut cache) = self.cache.try_borrow_mut() {
            for entry in cache.entries.values_mut() {
                if entry.icon.as_ref() == Some(&key) {
                    entry.node = None;
                }
            }
        }
        self.obj().queue_draw();
    }

    /// Forget what `positions` showed, so the presenter is asked again.
    pub(crate) fn refresh(&self, positions: Range<u32>) {
        let Some(model) = self.model() else {
            return;
        };
        {
            let mut cache = self.cache.borrow_mut();
            for position in positions.clone() {
                if let Some(item) = model.item(position) {
                    cache.entries.remove(&(item.as_ptr() as usize));
                }
            }
        }
        let window = self.a11y.borrow().range();
        if positions.start < window.end && window.start < positions.end {
            self.describe_window();
        }
        self.obj().queue_draw();
    }

    pub(crate) fn refresh_all(&self) {
        self.cache.borrow_mut().entries.clear();
        self.describe_window();
        self.obj().queue_draw();
    }
}
