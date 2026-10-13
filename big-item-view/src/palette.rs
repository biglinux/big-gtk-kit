// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The theme's colours and fonts, read from CSS.
//!
//! GTK has no public call that answers "what is the selected background in this
//! theme"; the answer only exists as the computed style of a CSS node. So the
//! view keeps one tiny invisible child per colour it draws with, a *probe*, and
//! reads the probe's `color`. Every themable colour is written in CSS as a
//! `color:` value, which GTK resolves with `var()`, `alpha()` and `color-mix()`
//! like any other; a consumer restyles its view with ordinary selectors such as
//! `.bigfiles-icon-grid > probe.selected-fill { color: … }`.
//!
//! The probes are mapped and allocated 0×0, never drawn. Mapped matters: a theme,
//! accent or contrast change restyles them, their redraw marks the view as
//! needing one, and the next frame reads the new colours, with no style-manager
//! signal to connect and to disconnect again in a process that runs for months.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

/// Every colour the view draws with, in probe order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Text,
    TextDim,
    HoverFill,
    StripeFill,
    SelectedFill,
    SelectedFocusRing,
    FocusRing,
    CursorFill,
    DropFill,
    DropRing,
    CardFill,
    CardShadow,
    CardEdge,
    BadgeEdge,
    TickBg,
    TickBorder,
    TickOnBg,
    TickOnFg,
    RubberbandFill,
    RubberbandEdge,
    PlaceholderFill,
    Accent,
}

pub(crate) const ROLES: [Role; 22] = [
    Role::Text,
    Role::TextDim,
    Role::HoverFill,
    Role::StripeFill,
    Role::SelectedFill,
    Role::SelectedFocusRing,
    Role::FocusRing,
    Role::CursorFill,
    Role::DropFill,
    Role::DropRing,
    Role::CardFill,
    Role::CardShadow,
    Role::CardEdge,
    Role::BadgeEdge,
    Role::TickBg,
    Role::TickBorder,
    Role::TickOnBg,
    Role::TickOnFg,
    Role::RubberbandFill,
    Role::RubberbandEdge,
    Role::PlaceholderFill,
    Role::Accent,
];

impl Role {
    const fn class(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::TextDim => "text-dim",
            Self::HoverFill => "hover-fill",
            Self::StripeFill => "stripe-fill",
            Self::SelectedFill => "selected-fill",
            Self::SelectedFocusRing => "selected-focus-ring",
            Self::FocusRing => "focus-ring",
            Self::CursorFill => "cursor-fill",
            Self::DropFill => "drop-fill",
            Self::DropRing => "drop-ring",
            Self::CardFill => "card-fill",
            Self::CardShadow => "card-shadow",
            Self::CardEdge => "card-edge",
            Self::BadgeEdge => "badge-edge",
            Self::TickBg => "tick-bg",
            Self::TickBorder => "tick-border",
            Self::TickOnBg => "tick-on-bg",
            Self::TickOnFg => "tick-on-fg",
            Self::RubberbandFill => "rubberband-fill",
            Self::RubberbandEdge => "rubberband-edge",
            Self::PlaceholderFill => "placeholder-fill",
            Self::Accent => "accent",
        }
    }
}

/// The two fonts, read from probes of their own: the name, and the smaller line
/// under it. A personalised font reaches the view through them.
pub(crate) const TITLE_FONT: &str = "title";
pub(crate) const CAPTION_FONT: &str = "caption";

/// The defaults, below application priority so a consumer's CSS wins.
const DEFAULT_CSS: &str = "
itemview > probe { min-width: 0; min-height: 0; }
itemview > probe.text-dim { color: alpha(currentColor, 0.7); }
itemview > probe.hover-fill { color: alpha(currentColor, 0.06); }
itemview > probe.stripe-fill { color: alpha(currentColor, 0.04); }
itemview > probe.selected-fill { color: alpha(var(--accent-bg-color), 0.22); }
itemview > probe.selected-focus-ring { color: var(--accent-bg-color); }
itemview > probe.focus-ring { color: var(--accent-color); }
itemview > probe.cursor-fill { color: transparent; }
itemview > probe.drop-fill { color: alpha(var(--accent-bg-color), 0.18); }
itemview > probe.drop-ring { color: var(--accent-bg-color); }
itemview > probe.card-fill { color: var(--card-bg-color); }
itemview > probe.card-shadow { color: alpha(black, 0.22); }
itemview > probe.card-edge { color: alpha(currentColor, 0.08); }
itemview > probe.badge-edge { color: var(--view-bg-color); }
itemview > probe.tick-bg { color: var(--view-bg-color); }
itemview > probe.tick-border { color: alpha(currentColor, 0.45); }
itemview > probe.tick-on-bg { color: var(--accent-bg-color); }
itemview > probe.tick-on-fg { color: var(--accent-fg-color); }
itemview > probe.rubberband-fill { color: alpha(var(--accent-bg-color), 0.2); }
itemview > probe.rubberband-edge { color: var(--accent-bg-color); }
itemview > probe.placeholder-fill { color: alpha(currentColor, 0.08); }
itemview > probe.accent { color: var(--accent-color); }
itemview > probe.caption { font-size: smaller; }
itemheader > button { background-color: transparent; padding: 3px 6px; border-radius: 0; box-shadow: none; min-height: 0; }
itemheader > button > box { color: color-mix(in srgb, currentColor 40%, transparent); border-spacing: 6px; }
itemheader > button:hover > box { color: color-mix(in srgb, currentColor 70%, transparent); }
itemheader > button:active > box { color: currentColor; }
";

/// Between the settings (400) and the application (600).
const DEFAULT_PRIORITY: u32 = gtk::STYLE_PROVIDER_PRIORITY_APPLICATION - 100;

thread_local! {
    static STYLED: RefCell<Vec<glib::WeakRef<gdk::Display>>> = const { RefCell::new(Vec::new()) };
}

/// Give `display` the defaults, once.
pub(crate) fn install_defaults(display: &gdk::Display) {
    let already = STYLED.with(|styled| {
        let mut styled = styled.borrow_mut();
        styled.retain(|weak| weak.upgrade().is_some());
        if styled
            .iter()
            .any(|weak| weak.upgrade().as_ref() == Some(display))
        {
            true
        } else {
            styled.push(display.downgrade());
            false
        }
    });
    if already {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(DEFAULT_CSS);
    gtk::style_context_add_provider_for_display(display, &provider, DEFAULT_PRIORITY);
}

/// The colours read on one frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Palette {
    colors: Vec<gdk::RGBA>,
}

impl Palette {
    pub(crate) fn get(&self, role: Role) -> gdk::RGBA {
        self.colors
            .get(
                ROLES
                    .iter()
                    .position(|candidate| *candidate == role)
                    .unwrap_or(0),
            )
            .copied()
            .unwrap_or(gdk::RGBA::BLACK)
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            colors: vec![gdk::RGBA::BLACK; ROLES.len()],
        }
    }
}

/// The probes of one view.
pub(crate) struct Probes {
    colors: Vec<Probe>,
    title: Probe,
    caption: Probe,
}

impl Probes {
    /// Make the probes and parent them to `view`.
    pub(crate) fn new(view: &impl IsA<gtk::Widget>) -> Self {
        let colors: Vec<Probe> = ROLES.iter().map(|role| Probe::new(role.class())).collect();
        let title = Probe::new(TITLE_FONT);
        let caption = Probe::new(CAPTION_FONT);
        for probe in colors.iter().chain([&title, &caption]) {
            probe.set_parent(view);
        }
        Self {
            colors,
            title,
            caption,
        }
    }

    pub(crate) fn read(&self) -> Palette {
        Palette {
            colors: self.colors.iter().map(WidgetExt::color).collect(),
        }
    }

    pub(crate) fn title_context(&self) -> gtk::pango::Context {
        self.title.pango_context()
    }

    pub(crate) fn caption_context(&self) -> gtk::pango::Context {
        self.caption.pango_context()
    }

    /// Snapshot every probe: nothing is drawn, but it is what clears GTK's
    /// `draw_needed` on them. `gtk_widget_queue_draw` stops climbing at the first
    /// widget that already needs a draw, so a probe never snapshotted keeps that
    /// flag forever and its restyle (a theme, accent or contrast change) never
    /// reaches the view: the colours change and nothing is redrawn.
    pub(crate) fn snapshot(&self, parent: &impl IsA<gtk::Widget>, snapshot: &gtk::Snapshot) {
        for probe in self.colors.iter().chain([&self.title, &self.caption]) {
            parent.snapshot_child(probe, snapshot);
        }
    }

    /// Give every probe a size, so it stays mapped: zero, where nothing sees it.
    pub(crate) fn allocate(&self) {
        for probe in self.colors.iter().chain([&self.title, &self.caption]) {
            probe.allocate(0, 0, -1, None);
        }
    }

    pub(crate) fn unparent(&self) {
        for probe in self.colors.iter().chain([&self.title, &self.caption]) {
            probe.unparent();
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Probe;

    #[glib::object_subclass]
    impl ObjectSubclass for Probe {
        const NAME: &'static str = "BigItemViewProbe";
        // One per module copy of this crate (see `ItemView`).
        const ALLOW_NAME_CONFLICT: bool = true;
        type Type = super::Probe;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("probe");
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl ObjectImpl for Probe {}
    impl WidgetImpl for Probe {}
}

glib::wrapper! {
    /// An invisible widget whose only job is to have a computed style.
    pub struct Probe(ObjectSubclass<imp::Probe>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Probe {
    fn new(class: &str) -> Self {
        let probe: Self = glib::Object::new();
        probe.add_css_class(class);
        probe.set_can_target(false);
        probe.set_can_focus(false);
        probe
    }
}
