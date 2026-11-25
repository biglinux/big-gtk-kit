// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Ordered overlay layer policy for full-bleed media scenes.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::gtk;

use super::chrome::{BigMediaChrome, BigMediaChromePlacement};

/// Semantic role for one media overlay child.
///
/// The role names the user's visible layer purpose and carries its alignment
/// and stacking order. Consumers add role layers in stacking order: viewer
/// adornment overlays first, then chrome through [`BigMediaChrome`]. Every
/// layer floats over the scene, neither clipped to it nor counted in its size
/// request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BigMediaOverlayLayerRole {
    /// Viewer-specific adornment such as OSD labels, histograms, or strips,
    /// placed by its own alignment.
    ViewerAdornmentOverlay,
    /// Floating top chrome, normally installed through [`BigMediaChrome`].
    TopChrome,
    /// Floating bottom chrome, normally installed through [`BigMediaChrome`].
    BottomChrome,
}

impl BigMediaOverlayLayerRole {
    /// The edge this role holds its layer to, as horizontal and vertical
    /// alignment; `None` keeps the widget's own alignment.
    #[must_use]
    const fn alignment(self) -> Option<(gtk::Align, gtk::Align)> {
        match self {
            Self::ViewerAdornmentOverlay => None,
            Self::TopChrome => Some((gtk::Align::Fill, gtk::Align::Start)),
            Self::BottomChrome => Some((gtk::Align::Fill, gtk::Align::End)),
        }
    }

    /// Relative order used by consumers when assembling a media scene.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_relm4_components::layout::media_window::BigMediaOverlayLayerRole;
    ///
    /// assert!(
    ///     BigMediaOverlayLayerRole::ViewerAdornmentOverlay.stacking_order()
    ///         < BigMediaOverlayLayerRole::TopChrome.stacking_order()
    /// );
    /// ```
    #[must_use]
    pub const fn stacking_order(self) -> u8 {
        match self {
            Self::ViewerAdornmentOverlay => 40,
            Self::TopChrome | Self::BottomChrome => 70,
        }
    }

    /// Chrome placement represented by this role, when the role is backed by
    /// [`BigMediaChrome`].
    #[must_use]
    pub(super) const fn chrome_placement(self) -> Option<BigMediaChromePlacement> {
        match self {
            Self::TopChrome => Some(BigMediaChromePlacement::Header),
            Self::BottomChrome => Some(BigMediaChromePlacement::Bottom),
            Self::ViewerAdornmentOverlay => None,
        }
    }
}

/// Shared `GtkOverlay` stack for full-bleed media scenes.
///
/// The app still owns its renderer widgets and domain state. This wrapper owns
/// only the repeatable GTK policy around the scene: one background child plus
/// ordered floating overlay layers.
#[derive(Debug, Clone)]
pub struct BigMediaOverlayStack {
    overlay: gtk::Overlay,
    last_semantic_layer_order: Rc<Cell<u8>>,
}

impl BigMediaOverlayStack {
    /// Create a media overlay stack around the background widget.
    #[must_use]
    pub fn new(background: &impl IsA<gtk::Widget>) -> Self {
        let overlay = gtk::Overlay::builder()
            .hexpand(true)
            .vexpand(true)
            .child(background)
            .build();
        Self {
            overlay,
            last_semantic_layer_order: Rc::new(Cell::new(0)),
        }
    }

    /// The assembled `GtkOverlay`.
    #[must_use]
    pub fn overlay(&self) -> &gtk::Overlay {
        &self.overlay
    }

    /// Set the overflow behavior on the underlying overlay.
    pub fn set_overflow(&self, overflow: gtk::Overflow) {
        self.overlay.set_overflow(overflow);
    }

    /// Add a layer using the shared semantic role policy.
    fn add_layer_with_role(
        &self,
        layer: &impl IsA<gtk::Widget>,
        layer_role: BigMediaOverlayLayerRole,
    ) {
        let previous_order = self.last_semantic_layer_order.get();
        let next_order = layer_role.stacking_order();
        debug_assert!(
            next_order >= previous_order,
            "media overlay layer {layer_role:?} was added after a later layer order {previous_order}"
        );
        self.last_semantic_layer_order
            .set(previous_order.max(next_order));
        if let Some((horizontal, vertical)) = layer_role.alignment() {
            layer.set_halign(horizontal);
            layer.set_valign(vertical);
        }
        // GtkOverlay neither clips nor measures an overlay child by default.
        self.overlay.add_overlay(layer);
    }

    /// Install a widget under a semantic media-surface role.
    ///
    /// This is the role-first entry point used by shared immersive surfaces:
    /// the role supplies the stacking and alignment policy,
    /// while the caller keeps ownership of the widget itself.
    ///
    /// # Examples
    ///
    /// ```
    /// use big_relm4_components::layout::media_window::{
    ///     BigMediaOverlayLayerRole, BigMediaOverlayStack,
    /// };
    /// use gtk::prelude::*;
    ///
    /// if gtk::init().is_ok() {
    ///     let background = gtk::Box::new(gtk::Orientation::Vertical, 0);
    ///     let stack = BigMediaOverlayStack::new(&background);
    ///     let osd = gtk::Box::new(gtk::Orientation::Vertical, 0);
    ///
    ///     stack.install_role(BigMediaOverlayLayerRole::ViewerAdornmentOverlay, &osd);
    ///
    ///     assert!(osd.parent().is_some());
    /// }
    /// ```
    pub fn install_role(
        &self,
        layer_role: BigMediaOverlayLayerRole,
        layer: &impl IsA<gtk::Widget>,
    ) {
        self.add_layer_with_role(layer, layer_role);
    }

    /// Install media chrome at the current overlay position.
    fn install_chrome(&self, chrome: &BigMediaChrome, placement: BigMediaChromePlacement) {
        match placement {
            BigMediaChromePlacement::Header => chrome.install_header_into(&self.overlay),
            BigMediaChromePlacement::Bottom => chrome.install_bottom_into(&self.overlay),
        }
    }

    /// Install media chrome through its overlay role.
    ///
    /// Top and bottom chrome delegate to the existing [`BigMediaChrome`]
    /// installation helpers. Non-chrome roles return `false` so callers can
    /// route ordinary widgets through [`install_role`](Self::install_role).
    ///
    /// # Examples
    ///
    /// ```
    /// use big_relm4_components::layout::{
    ///     hamburger_menu::BigMenuActionItem,
    ///     media_window::{
    ///         BigMediaChrome, BigMediaMenuSpec, BigMediaOverlayLayerRole,
    ///         BigMediaOverlayStack,
    ///     },
    /// };
    /// use gtk::prelude::*;
    ///
    /// if gtk::init().is_ok() {
    ///     let background = gtk::Box::new(gtk::Orientation::Vertical, 0);
    ///     let stack = BigMediaOverlayStack::new(&background);
    ///     let chrome = BigMediaChrome::new(
    ///         BigMediaMenuSpec {
    ///             icon_name: "open-menu-symbolic".into(),
    ///             label: "Main menu".into(),
    ///             items: Vec::<BigMenuActionItem>::new(),
    ///         },
    ///         true,
    ///     );
    ///
    ///     assert!(stack.install_chrome_role(BigMediaOverlayLayerRole::TopChrome, &chrome));
    ///     assert!(chrome.header_revealer().parent().is_some());
    /// }
    /// ```
    #[must_use]
    pub fn install_chrome_role(
        &self,
        layer_role: BigMediaOverlayLayerRole,
        chrome: &BigMediaChrome,
    ) -> bool {
        let Some(placement) = layer_role.chrome_placement() else {
            return false;
        };
        self.install_chrome(chrome, placement);
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::layout::hamburger_menu::BigMenuActionItem;
    use crate::layout::widget_container::children;

    use super::{BigMediaChrome, BigMediaOverlayLayerRole, BigMediaOverlayStack};
    use crate::layout::media_window::BigMediaMenuSpec;
    use gtk::prelude::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn roles_install_in_layer_order() {
        big_testkit::harness::require_gtk_test(|| {
            let background = named_box("background");
            let stack = BigMediaOverlayStack::new(&background);
            let adornment = named_box("adornment");
            adornment.set_valign(gtk::Align::End);
            let top = named_box("top");
            let bottom = named_box("bottom");

            stack.install_role(BigMediaOverlayLayerRole::ViewerAdornmentOverlay, &adornment);
            stack.install_role(BigMediaOverlayLayerRole::TopChrome, &top);
            stack.install_role(BigMediaOverlayLayerRole::BottomChrome, &bottom);

            assert_eq!(adornment.valign(), gtk::Align::End);
            assert_eq!(top.halign(), gtk::Align::Fill);
            assert_eq!(top.valign(), gtk::Align::Start);
            assert_eq!(bottom.valign(), gtk::Align::End);
            assert!(!stack.overlay().is_clip_overlay(&top));
            assert!(!stack.overlay().is_measure_overlay(&top));
            assert_eq!(
                overlay_child_names(stack.overlay()),
                ["background", "adornment", "top", "bottom"]
            );
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn chrome_role_delegates_to_media_chrome() {
        big_testkit::harness::require_gtk_test(|| {
            let background = named_box("background");
            let stack = BigMediaOverlayStack::new(&background);
            let chrome = BigMediaChrome::new(
                BigMediaMenuSpec {
                    icon_name: "open-menu-symbolic".into(),
                    label: "Main menu".into(),
                    items: Vec::<BigMenuActionItem>::new(),
                },
                true,
            );

            assert!(stack.install_chrome_role(BigMediaOverlayLayerRole::TopChrome, &chrome));
            assert!(stack.install_chrome_role(BigMediaOverlayLayerRole::BottomChrome, &chrome));
            assert!(
                !stack
                    .install_chrome_role(BigMediaOverlayLayerRole::ViewerAdornmentOverlay, &chrome)
            );
            assert!(chrome.header_revealer().parent().is_some());
            assert!(chrome.bottom_revealer().parent().is_some());
            assert_eq!(chrome.header_revealer().valign(), gtk::Align::Start);
            assert_eq!(chrome.bottom_revealer().valign(), gtk::Align::End);
        });
    }

    fn named_box(name: &str) -> gtk::Box {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_widget_name(name);
        widget
    }

    fn overlay_child_names(overlay: &gtk::Overlay) -> Vec<String> {
        let mut names = Vec::new();
        for widget in children(overlay) {
            names.push(widget.widget_name().to_string());
        }
        names
    }
}
