// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A drawn, virtualized view of many items: an icon grid, a column flow, rows.
//!
//! **Why not `GtkGridView`.** A list view keeps one widget tree per item it has
//! built, and it builds `max-columns × 30` of them: a file manager's compact view
//! of a home folder built a cell for each of its 1129 entries, 82 GTK objects
//! apiece, and adding each one walked every earlier sibling for the
//! accessibility bus (O(n²)). A new tab cost 1.4 s of CPU and 50 MiB.
//!
//! Here there are no widgets per item. [`ItemView`] lays items out by arithmetic
//! ([`layout`]), draws the ones on screen into render nodes it keeps and hands
//! back to GSK unchanged frame after frame (so the renderer's diff damages only
//! what changed), reads its colours and fonts from CSS through a few invisible
//! probe widgets, and exposes the items to assistive technology as lightweight
//! accessible objects over the visible window. What an item shows comes from the
//! consumer's [`ItemPresenter`].

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod a11y;
mod draw;
mod header;
mod icons;
mod input;
pub mod layout;
mod palette;
mod text_width;
mod view;

use gtk::{gdk, gio, glib};

pub use header::ColumnHeader;
pub use layout::{Flow, Sections, Step};
pub use view::ItemView;

/// Draws `icon` at `size` and `scale` for the display, and hands the picture to
/// `deliver` on the main thread, at once or later; `None` lets GTK's icon theme
/// draw it.
pub type IconLoader =
    fn(&gdk::Display, &gio::Icon, i32, i32, Box<dyn FnOnce(Option<gdk::Texture>)>);

/// What a consumer tells the view about its items.
///
/// Held by the view for its lifetime: capture consumer state weakly, the way
/// every closure owned by a widget has to, or the view keeps its owner alive.
pub trait ItemPresenter: 'static {
    /// Describe the item at `position` into `out`, which arrives cleared.
    fn content(&self, position: u32, item: &glib::Object, out: &mut ItemContent);

    /// The item's name, as [`Self::content`] would give it. Asked of every item
    /// when the rows or columns are fitted to their names; a presenter whose
    /// content is costly to describe answers this one alone.
    fn title(&self, position: u32, item: &glib::Object) -> String {
        let mut out = ItemContent::default();
        self.content(position, item, &mut out);
        out.title
    }

    /// The line under the item's name, as [`Self::content`] would give it.
    /// Asked of every item when the columns are fitted to their words, and
    /// only where the geometry keeps that line.
    fn subtitle(&self, position: u32, item: &glib::Object) -> Option<String> {
        let mut out = ItemContent::default();
        self.content(position, item, &mut out);
        out.subtitle
    }

    /// The tooltip for the item at `position`, if it has one.
    fn tooltip(&self, _position: u32, _item: &glib::Object) -> Option<String> {
        None
    }

    /// What dragging `positions` carries; `None` refuses the drag.
    fn drag_content(&self, _positions: &[u32]) -> Option<gdk::ContentProvider> {
        None
    }

    /// Whether a drop may land on the item at `target`, or on the view itself
    /// when `target` is `None`.
    fn accepts_drop(&self, _target: Option<u32>) -> bool {
        false
    }

    /// The heading over the run of items starting at `position`, in a view
    /// with [`ItemView::set_sections`] on and a model that has sections (a
    /// `GtkSortListModel` with a section sorter).
    fn section_title(&self, _position: u32, _item: &glib::Object) -> Option<String> {
        None
    }

    /// Whether resting a drag on the item at `target` opens it (emits
    /// `activate`), the way a folder springs open in a file manager.
    fn springs_open(&self, _target: u32) -> bool {
        false
    }

    /// Take a drop on `target` (the view itself when `None`). `want_move` is
    /// Shift held. Returns whether the drop was taken.
    fn receive_drop(&self, _target: Option<u32>, _value: &glib::Value, _want_move: bool) -> bool {
        false
    }
}

/// Everything one item shows.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ItemContent {
    /// The picture: a themed or file icon, or a texture already made.
    pub icon: IconSource,
    /// How the picture is framed.
    pub frame: IconFrame,
    /// A picture laid over the icon and fitted to its box, transparent where
    /// the icon shows through: small pictures of what a folder holds, drawn on
    /// the folder's own icon.
    pub overlay: Option<gdk::Texture>,
    /// The name, wrapped and ellipsized to the geometry's line count.
    pub title: String,
    /// A dimmer line under the name.
    pub subtitle: Option<String>,
    /// Small marks on the corners of the picture.
    pub badges: Vec<Badge>,
    /// The item's own check, in [`TickMode::Check`].
    pub checked: Option<bool>,
    /// Drawn faded and grey, e.g. a file marked to be moved.
    pub dimmed: bool,
    /// What assistive technology reads as the item's name; the title when unset.
    pub accessible_label: Option<String>,
    /// What assistive technology reads after the name.
    pub accessible_description: Option<String>,
    /// Drawn in the accent colour: the item that is playing, or current.
    pub emphasized: bool,
    /// Ratings out of five, by [`ColumnShows::Stars`] index.
    pub ratings: Vec<u8>,
    /// The item has no [`ColumnShows::Button`]: nothing is drawn or pressed
    /// there (a song among albums that open).
    pub hides_button: bool,
    /// The words of the other columns, by [`ColumnShows::Cell`] index, in a
    /// list with columns.
    pub cells: Vec<String>,
}

/// Where an item's picture comes from.
#[derive(Clone, Debug, Default)]
pub enum IconSource {
    /// No picture.
    #[default]
    None,
    /// A themed, file or MIME-type icon, drawn at the geometry's icon size.
    Gicon(gio::Icon),
    /// A picture already decoded, e.g. a thumbnail.
    Texture(gdk::Texture),
}

/// How a picture is framed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconFrame {
    /// As it is.
    #[default]
    Plain,
    /// On a card with rounded corners and a shadow: how a thumbnail is told
    /// apart from a type icon.
    Card,
    /// Filling the whole picture box, cropped to it, with rounded corners: a
    /// photograph shown as a cover. A soft placeholder holds its place while
    /// the item has no picture yet.
    Cover,
    /// Half the picture box, centred: a symbolic mark of a kind of item in
    /// rows whose other items show covers.
    Small,
}

/// A small mark on a corner of an item's picture.
#[derive(Clone, Debug)]
pub struct Badge {
    /// Which corner.
    pub corner: Corner,
    /// The disc's colour.
    pub fill: gdk::RGBA,
    /// What is drawn on the disc.
    pub glyph: Glyph,
}

/// What a badge shows.
#[derive(Clone, Debug)]
pub enum Glyph {
    /// A character or two.
    Text(String),
    /// A symbolic icon name, drawn in white.
    Icon(String),
}

/// A corner of the picture, in reading direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    /// Top left in left-to-right.
    TopStart,
    /// Top right in left-to-right.
    TopEnd,
    /// Bottom left in left-to-right.
    BottomStart,
    /// Bottom right in left-to-right.
    BottomEnd,
    /// Not on the picture: at the end of the name, in a row. A row's picture is
    /// too small for a mark on its corner to say which mark it is, and marks at
    /// the end of every name line up in one column.
    Trailing,
}

/// How the picture and the words of an item are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Composition {
    /// Picture above, words centred under it: an icon grid.
    Stacked,
    /// Picture at the start, words beside it: a list row.
    Inline,
}

/// The tick an item carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickMode {
    /// None.
    None,
    /// Shown while the pointer is on the item; pressing it adds the item to the
    /// selection or takes it out, without touching the rest.
    Selection,
    /// The item's own check ([`ItemContent::checked`]); pressing it emits
    /// `check-toggled`.
    Check,
}

/// The shape of every item of a view.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// How items follow each other.
    pub flow: Flow,
    /// How an item's parts are arranged.
    pub composition: Composition,
    /// The narrowest a cell may be, margins included.
    pub cell_width: i32,
    /// The picture's size: its width, and its height too unless
    /// [`Self::picture_height`] says otherwise.
    pub icon_size: i32,
    /// The picture's height when it is not square (a wallpaper is the shape
    /// of a screen).
    pub picture_height: Option<i32>,
    /// Lines the name may take; the cell is as tall as that many lines.
    pub title_lines: u8,
    /// Where a name too long for its lines loses characters. The middle keeps
    /// both ends, which is what tells apart names that share a long start
    /// (`IMG_20260912_…`, `WhatsApp Image 2026-08-17 at …`).
    pub title_ellipsize: gtk::pango::EllipsizeMode,
    /// Whether room is kept for a subtitle line under the name.
    pub subtitle: bool,
    /// Where a subtitle too long for its line loses characters.
    pub subtitle_ellipsize: gtk::pango::EllipsizeMode,
    /// Space around the item's background: (vertical, horizontal).
    pub margin: (i32, i32),
    /// Space inside the item's background.
    pub padding: i32,
    /// Corner radius of the item's background.
    pub radius: f32,
    /// Space between the picture and the words.
    pub gap: i32,
    /// The tick items carry.
    pub tick: TickMode,
    /// The columns of a list with columns ([`Flow::RowMajor`], one item per
    /// line), in the order they are shown; empty for every other shape.
    pub columns: Vec<Column>,
    /// Whether only what an item draws (its picture, its words, its tick)
    /// is the item, rather than its whole cell. The rest of the cell is
    /// background: a press there starts a rubber band, as in a file manager.
    pub hit_drawn_only: bool,
}

/// One column of a list with columns.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    /// The consumer's own name for it, handed back by [`ColumnHeader`]'s signals.
    pub id: u32,
    /// What its heading says.
    pub title: String,
    /// What the column shows.
    pub shows: ColumnShows,
    /// Its width; the column that expands takes at least this.
    pub width: i32,
    /// Whether it takes the room the others leave (the first that does).
    pub expand: bool,
    /// Where its words sit: 0 at the start, 1 at the end.
    pub xalign: f32,
    /// The arrow its heading shows when the list is sorted by it.
    pub sort: Option<gtk::SortType>,
}

/// What a column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnShows {
    /// The item's picture, name and marks.
    Primary,
    /// [`ItemContent::cells`]`[index]`.
    Cell(usize),
    /// [`ItemContent::ratings`]`[index]` as five stars. A press on the `n`th
    /// star emits `cell-pressed` with `n`.
    Stars(usize),
    /// A symbolic icon drawn on the hovered item and the keyboard's item. A
    /// press on it emits `cell-pressed` with 0. What it does must also be
    /// reachable without a pointer (a key, the context menu).
    Button(&'static str),
}

impl Geometry {
    /// The picture box: width and height.
    #[must_use]
    pub(crate) fn picture(&self) -> (f32, f32) {
        let width = self.icon_size as f32;
        (
            width,
            self.picture_height.map_or(width, |height| height as f32),
        )
    }

    /// A grid of `icon_size` pictures in cells at least `cell_width` wide.
    #[must_use]
    pub fn grid(cell_width: i32, icon_size: i32) -> Self {
        Self {
            flow: Flow::RowMajor {
                min_columns: 1,
                max_columns: 64,
            },
            composition: Composition::Stacked,
            cell_width,
            icon_size,
            picture_height: None,
            title_lines: 2,
            title_ellipsize: gtk::pango::EllipsizeMode::End,
            subtitle: false,
            subtitle_ellipsize: gtk::pango::EllipsizeMode::End,
            margin: (6, 3),
            padding: 6,
            radius: 10.0,
            gap: 4,
            tick: TickMode::None,
            columns: Vec::new(),
            hit_drawn_only: false,
        }
    }

    /// Rows of `icon_size` pictures with the name beside them, one per line.
    #[must_use]
    pub fn rows(icon_size: i32) -> Self {
        Self {
            flow: Flow::RowMajor {
                min_columns: 1,
                max_columns: 1,
            },
            composition: Composition::Inline,
            cell_width: icon_size + 120,
            icon_size,
            picture_height: None,
            title_lines: 1,
            title_ellipsize: gtk::pango::EllipsizeMode::End,
            subtitle: false,
            subtitle_ellipsize: gtk::pango::EllipsizeMode::End,
            margin: (0, 2),
            padding: 4,
            radius: 6.0,
            gap: 8,
            tick: TickMode::None,
            columns: Vec::new(),
            hit_drawn_only: false,
        }
    }
}
