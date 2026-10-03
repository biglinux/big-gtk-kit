// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Icons drawn at the size they are shown, shared by every item that shows them.
//!
//! One texture per distinct icon: a folder of a thousand files has a few dozen
//! types, and GSK uploads a texture to the GPU once however many items draw it.
//! Coloured icons come from the view's [`crate::IconLoader`] when it has one;
//! symbolic icons, and every icon without a loader, stay GTK's own
//! `IconPaintable`, recoloured as they are drawn.

use std::collections::HashMap;

use gtk::prelude::*;
use gtk::{gdk, gio};

/// Where one icon is at.
#[derive(Clone)]
pub(crate) enum IconState {
    /// Asked for; a texture or a paintable arrives later.
    Pending,
    /// A picture of the icon at the size asked.
    Texture(gdk::Texture),
    /// A symbolic icon, or one the sandbox left to GTK: drawn by GTK.
    Paintable(gtk::IconPaintable),
}

/// An icon's name at one size and scale.
pub(crate) type IconKey = (String, i32, i32);

/// Past this many remembered icons the map starts over; the textures still on
/// screen are kept alive by the nodes that draw them and come back at once from
/// the loader's own caches.
const MAX_ICONS: usize = 2048;

#[derive(Default)]
pub(crate) struct Icons {
    states: HashMap<IconKey, IconState>,
}

impl Icons {
    pub(crate) fn key(gicon: &gio::Icon, size: i32, scale: i32) -> IconKey {
        let name = IconExt::to_string(gicon).map_or_else(String::new, |name| name.to_string());
        (name, size, scale)
    }

    pub(crate) fn get(&self, key: &IconKey) -> Option<IconState> {
        self.states.get(key).cloned()
    }

    /// Note that `key` has been asked for. Returns whether it was new.
    pub(crate) fn begin(&mut self, key: IconKey) -> bool {
        if self.states.contains_key(&key) {
            return false;
        }
        if self.states.len() >= MAX_ICONS {
            self.states.clear();
        }
        self.states.insert(key, IconState::Pending);
        true
    }

    pub(crate) fn settle(&mut self, key: IconKey, state: IconState) {
        self.states.insert(key, state);
    }

    pub(crate) fn clear(&mut self) {
        self.states.clear();
    }
}

/// GTK's own drawing of `gicon`, for the icons the sandbox does not draw.
pub(crate) fn paintable(
    display: &gdk::Display,
    gicon: &gio::Icon,
    size: i32,
    scale: i32,
    rtl: bool,
) -> gtk::IconPaintable {
    gtk::IconTheme::for_display(display).lookup_by_gicon(
        gicon,
        size,
        scale,
        if rtl {
            gtk::TextDirection::Rtl
        } else {
            gtk::TextDirection::Ltr
        },
        gtk::IconLookupFlags::empty(),
    )
}

/// A symbolic icon by name, for badges and the tick.
pub(crate) fn symbolic(
    display: &gdk::Display,
    name: &str,
    size: i32,
    scale: i32,
) -> gtk::IconPaintable {
    gtk::IconTheme::for_display(display).lookup_icon(
        name,
        &[],
        size,
        scale,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::FORCE_SYMBOLIC,
    )
}
