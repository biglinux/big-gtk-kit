// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Floating media header and bottom control chrome.

use crate::layout::hamburger_menu::{BigMenuActionItem, build_flat_action_popover};
use adw::prelude::*;
use relm4::gtk;

const DEFAULT_MENU_POPOVER_MAX_HEIGHT_PX: i32 = 560;
const MIN_MENU_POPOVER_MAX_HEIGHT_PX: i32 = 240;
const MENU_POPOVER_VIEWPORT_MARGIN_PX: i32 = 120;
const MENU_POPOVER_VERTICAL_PADDING_PX: i32 = 12;
const MENU_POPOVER_ROW_HEIGHT_PX: i32 = 41;
const WINDOW_CONTROL_CLASS: &str = "big-media-window-control";
const WINDOW_MAXIMIZE_ICON: &str = "window-maximize-symbolic";
const WINDOW_RESTORE_ICON: &str = "window-restore-symbolic";

/// Which part of [`BigMediaChrome`] to add at the current overlay position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BigMediaChromePlacement {
    /// Add the header revealer only.
    Header,
    /// Add the bottom revealer only.
    Bottom,
}

/// Spec for the primary (hamburger) menu.
#[derive(Debug, Clone)]
pub struct BigMediaMenuSpec {
    /// Symbolic icon name, e.g. `"open-menu-symbolic"`.
    pub icon_name: String,
    /// Accessible label + tooltip, e.g. `"Main menu"`.
    pub label: String,
    /// Flat ordered menu items (`win.*`/`app.*` actions).
    pub items: Vec<BigMenuActionItem>,
}

/// Shared floating chrome (top `AdwHeaderBar` + bottom bar, each in a `Revealer`)
/// for media-viewer apps. Build it, fill the header slots + bottom child, then
/// install it into the app's `gtk::Overlay` through `BigMediaOverlayStack`
/// (after the app has added its media surfaces, so the bars stack on top).
#[derive(Debug, Clone)]
pub struct BigMediaChrome {
    header: adw::HeaderBar,
    header_start_slot: gtk::Box,
    header_end_actions: gtk::Box,
    header_revealer: gtk::Revealer,
    bottom_revealer: gtk::Revealer,
    menu_button: gtk::Button,
    menu_popover: gtk::Popover,
}

impl BigMediaChrome {
    /// Build the chrome. `reveal_initially` controls whether the bars start
    /// shown (e.g. `false` for a welcome/background state with no media yet).
    #[must_use]
    pub fn new(menu: BigMediaMenuSpec, reveal_initially: bool) -> Self {
        let header = crate::layout::window_handle::header_bar(adw::HeaderBar::builder());
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        let header_start_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let header_end_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let header_end_actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header_start_slot.set_valign(gtk::Align::Center);
        header_end_slot.set_valign(gtk::Align::Center);
        header_end_actions.set_valign(gtk::Align::Center);
        let menu_button = big_gtk_kit::tooltip::icon_button(&menu.icon_name, &menu.label, &[]);
        // A plain button that opens a menu: a screen reader learns it from this alone.
        menu_button.update_property(&[gtk::accessible::Property::HasPopup(true)]);
        let menu_popover = build_flat_action_popover(&menu_button, &menu.items);
        menu_popover.set_position(gtk::PositionType::Bottom);
        menu_popover.set_parent(&menu_button);
        crate::feedback::popover_lifecycle::unparent_popover_on_parent_teardown(
            menu_button.upcast_ref(),
            &menu_popover,
        );
        let menu_scroll = scroll_wrap_menu_popover(&menu_popover);
        menu_button.connect_clicked({
            let menu_popover = menu_popover.downgrade();
            let menu_scroll = menu_scroll.downgrade();
            move |button| {
                let Some(menu_popover) = menu_popover.upgrade() else {
                    return;
                };
                if menu_popover.parent().is_none() {
                    menu_popover.set_parent(button);
                }
                if menu_popover.is_visible() {
                    menu_popover.popdown();
                    return;
                }
                if let Some(menu_scroll) = menu_scroll.upgrade() {
                    apply_menu_popover_height(&menu_scroll, Some(button));
                }
                menu_popover.popup();
            }
        });
        let minimize_button = media_window_control_button("window-minimize-symbolic");
        minimize_button.connect_clicked(|button| {
            if let Some(window) = window_for_control(button) {
                window.minimize();
            }
        });
        let maximize_restore_button = media_window_control_button(WINDOW_MAXIMIZE_ICON);
        maximize_restore_button.connect_root_notify(|button| {
            let Some(window) = window_for_control(button) else {
                return;
            };
            update_maximize_restore_icon(button, window.is_maximized());
            let button = button.downgrade();
            window.connect_maximized_notify(move |window| {
                if let Some(button) = button.upgrade() {
                    update_maximize_restore_icon(&button, window.is_maximized());
                }
            });
        });
        maximize_restore_button.connect_clicked(|button| {
            let Some(window) = window_for_control(button) else {
                return;
            };
            if window.is_maximized() {
                window.unmaximize();
            } else {
                window.maximize();
            }
        });
        let close_button = media_window_control_button("window-close-symbolic");
        close_button.connect_clicked(|button| {
            if let Some(window) = window_for_control(button) {
                window.close();
            }
        });
        let window_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        window_controls.set_valign(gtk::Align::Center);
        window_controls.set_visible(false);
        window_controls.append(&minimize_button);
        window_controls.append(&maximize_restore_button);
        window_controls.append(&close_button);
        header_end_slot.append(&header_end_actions);
        header_end_slot.append(&menu_button);
        header_end_slot.append(&window_controls);
        header.pack_start(&header_start_slot);
        header.pack_end(&header_end_slot);

        // Semantic shared wrapper; `.osd-header` is the current appearance
        // hook without GTK's opinionated `.osd` class.
        let header_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        header_box.add_css_class("osd-header");
        header_box.add_css_class("big-media-top-chrome");
        header_box.append(&header);
        let header_revealer = gtk::Revealer::builder()
            .valign(gtk::Align::Start)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .reveal_child(reveal_initially)
            .child(&header_box)
            .build();

        let bottom_revealer = gtk::Revealer::builder()
            .valign(gtk::Align::End)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(reveal_initially)
            .build();
        bottom_revealer.add_css_class("big-media-bottom-chrome");

        Self {
            header,
            header_start_slot,
            header_end_actions,
            header_revealer,
            bottom_revealer,
            menu_button,
            menu_popover,
        }
    }

    /// Add only the header bar to the app's overlay.
    pub(crate) fn install_header_into(&self, overlay: &gtk::Overlay) {
        overlay.add_overlay(&self.header_revealer);
        overlay.set_measure_overlay(&self.header_revealer, false);
    }

    /// Add only the bottom bar to the app's overlay.
    pub(crate) fn install_bottom_into(&self, overlay: &gtk::Overlay) {
        overlay.add_overlay(&self.bottom_revealer);
        overlay.set_measure_overlay(&self.bottom_revealer, false);
    }

    /// Set the bottom bar's content (the app's transport / capture controls).
    pub fn set_bottom_child(&self, child: &impl IsA<gtk::Widget>) {
        self.bottom_revealer.set_child(Some(child));
    }

    /// Add an app action to the header start (left of the title).
    pub fn add_header_start(&self, widget: &impl IsA<gtk::Widget>) {
        self.header_start_slot.append(widget);
    }

    /// Add an app action to the header END (right, before the primary menu).
    pub fn add_header_end(&self, widget: &impl IsA<gtk::Widget>) {
        self.header_end_actions.append(widget);
    }

    /// The real `AdwHeaderBar` (for direct tweaks; prefer the slot helpers).
    #[must_use]
    pub fn header(&self) -> &adw::HeaderBar {
        &self.header
    }

    /// The primary menu button.
    #[must_use]
    pub fn menu_button(&self) -> &gtk::Button {
        &self.menu_button
    }

    /// The primary menu popover.
    #[must_use]
    pub fn menu_popover(&self) -> &gtk::Popover {
        &self.menu_popover
    }

    /// The header reveal widget (apps wire auto-hide / OSD onto it).
    #[must_use]
    pub fn header_revealer(&self) -> &gtk::Revealer {
        &self.header_revealer
    }

    /// The bottom reveal widget (apps wire auto-hide / OSD onto it).
    #[must_use]
    pub fn bottom_revealer(&self) -> &gtk::Revealer {
        &self.bottom_revealer
    }
}

fn media_window_control_button(icon_name: &str) -> gtk::Button {
    gtk::Button::builder()
        .child(&gtk::Image::from_icon_name(icon_name))
        .css_classes([WINDOW_CONTROL_CLASS])
        .has_frame(false)
        .valign(gtk::Align::Center)
        .build()
}

fn window_for_control(control: &gtk::Button) -> Option<gtk::Window> {
    control.root()?.downcast::<gtk::Window>().ok()
}

fn update_maximize_restore_icon(button: &gtk::Button, is_maximized: bool) {
    let Some(image) = button.child().and_downcast::<gtk::Image>() else {
        return;
    };
    image.set_icon_name(Some(if is_maximized {
        WINDOW_RESTORE_ICON
    } else {
        WINDOW_MAXIMIZE_ICON
    }));
}

fn scroll_wrap_menu_popover(popover: &gtk::Popover) -> gtk::ScrolledWindow {
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .max_content_height(menu_popover_max_height(0))
        .build();
    if let Some(menu_child) = popover.child() {
        popover.set_child(None::<&gtk::Widget>);
        scroller.set_child(Some(&menu_child));
        popover.set_child(Some(&scroller));
    }
    scroller
}

fn apply_menu_popover_height(scroller: &gtk::ScrolledWindow, button: Option<&gtk::Button>) {
    let viewport_height = button
        .and_then(|button| button.root())
        .and_then(|root| root.downcast::<gtk::Window>().ok())
        .map_or(0, |window| window.height());
    scroller.set_max_content_height(menu_popover_max_height(viewport_height));
}

fn menu_popover_max_height(viewport_height: i32) -> i32 {
    let candidate_height = if viewport_height <= 0 {
        DEFAULT_MENU_POPOVER_MAX_HEIGHT_PX
    } else {
        viewport_height - MENU_POPOVER_VIEWPORT_MARGIN_PX
    };
    let bounded_height = candidate_height.clamp(
        MIN_MENU_POPOVER_MAX_HEIGHT_PX,
        DEFAULT_MENU_POPOVER_MAX_HEIGHT_PX,
    );
    snap_menu_popover_height_to_rows(bounded_height)
}

fn snap_menu_popover_height_to_rows(max_height: i32) -> i32 {
    if max_height <= MIN_MENU_POPOVER_MAX_HEIGHT_PX {
        return MIN_MENU_POPOVER_MAX_HEIGHT_PX;
    }

    let row_area_height = max_height - MENU_POPOVER_VERTICAL_PADDING_PX;
    let full_row_count = (row_area_height / MENU_POPOVER_ROW_HEIGHT_PX).max(1);
    let snapped_height =
        MENU_POPOVER_VERTICAL_PADDING_PX + (full_row_count * MENU_POPOVER_ROW_HEIGHT_PX);

    snapped_height.clamp(MIN_MENU_POPOVER_MAX_HEIGHT_PX, max_height)
}

#[cfg(test)]
mod tests {
    use super::{
        BigMediaChrome, BigMediaMenuSpec, DEFAULT_MENU_POPOVER_MAX_HEIGHT_PX,
        MENU_POPOVER_ROW_HEIGHT_PX, MENU_POPOVER_VERTICAL_PADDING_PX,
        MIN_MENU_POPOVER_MAX_HEIGHT_PX, menu_popover_max_height, snap_menu_popover_height_to_rows,
    };
    use adw::prelude::*;

    #[test]
    fn menu_popover_height_fits_720p_viewports() {
        assert_eq!(menu_popover_max_height(720), 545);
    }

    #[test]
    fn popover_height_leaves_margin() {
        assert_eq!(menu_popover_max_height(600), 463);
    }

    #[test]
    fn popover_height_keeps_minimum() {
        assert_eq!(menu_popover_max_height(300), MIN_MENU_POPOVER_MAX_HEIGHT_PX);
    }

    #[test]
    fn popover_height_default_before_realize() {
        assert_eq!(menu_popover_max_height(0), 545);
    }

    #[test]
    fn popover_height_snaps_to_rows() {
        let snapped_height = snap_menu_popover_height_to_rows(DEFAULT_MENU_POPOVER_MAX_HEIGHT_PX);

        assert_eq!(
            (snapped_height - MENU_POPOVER_VERTICAL_PADDING_PX) % MENU_POPOVER_ROW_HEIGHT_PX,
            0,
            "menu viewport should not clip the final visible row"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn menu_reopens_after_remount() {
        big_testkit::harness::require_gtk_test(|| {
            let chrome = BigMediaChrome::new(
                BigMediaMenuSpec {
                    icon_name: "open-menu-symbolic".to_owned(),
                    label: "Main Menu".to_owned(),
                    items: Vec::new(),
                },
                true,
            );
            let window = gtk::Window::new();
            window.set_child(Some(&chrome.header_revealer));
            window.present();
            big_testkit::harness::pump();
            window.set_child(gtk::Widget::NONE);
            assert!(chrome.menu_popover.parent().is_none());
            window.set_child(Some(&chrome.header_revealer));
            big_testkit::harness::pump();
            chrome.menu_button.emit_clicked();
            assert_eq!(
                chrome.menu_popover.parent(),
                Some(chrome.menu_button.clone().upcast())
            );
            assert!(chrome.menu_popover.is_visible());
            window.destroy();
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn maximize_glyph_follows_window_state() {
        big_testkit::harness::require_gtk_test(|| {
            let button = super::media_window_control_button(super::WINDOW_MAXIMIZE_ICON);
            super::update_maximize_restore_icon(&button, true);
            let image = button
                .child()
                .and_downcast::<gtk::Image>()
                .expect("window control owns an image");
            assert_eq!(
                image.icon_name().as_deref(),
                Some(super::WINDOW_RESTORE_ICON)
            );

            super::update_maximize_restore_icon(&button, false);
            assert_eq!(
                image.icon_name().as_deref(),
                Some(super::WINDOW_MAXIMIZE_ICON)
            );
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn menu_button_declares_its_popup() {
        big_testkit::harness::require_gtk_test(|| {
            let chrome = BigMediaChrome::new(
                BigMediaMenuSpec {
                    icon_name: "open-menu-symbolic".to_owned(),
                    label: "Main Menu".to_owned(),
                    items: Vec::new(),
                },
                true,
            );
            assert!(gtk::test_accessible_has_property(
                chrome.menu_button(),
                gtk::AccessibleProperty::HasPopup
            ));
        });
    }
}
