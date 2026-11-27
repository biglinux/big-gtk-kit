// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The chrome of a full-window media view: an `AdwHeaderBar` with a primary
//! menu and a bottom control bar, floating over the picture, and the controller
//! that hides them while the person watches.
//!
//! The program keeps its picture surface; [`BigMediaOverlayStack`] lays the
//! bars over it in a `gtk::Overlay` rather than an `AdwToolbarView`, which
//! would throttle a `GtkGraphicsOffload` subsurface under it to about 1 fps.
//!
//! The primary menu is a `gtk::Button` opening a popover of action rows, so
//! a screen reader names and activates the button and every action.
//!
//! - `chrome`: the header bar, its primary menu and the bottom bar.
//! - `immersion`: hides the chrome, and the pointer if asked, after a moment
//!   idle.
//! - `overlay`: the overlay that stacks the picture, adornments and chrome.

mod chrome;
mod immersion;
mod overlay;

pub use chrome::{BigMediaChrome, BigMediaMenuSpec};
pub use immersion::{BIG_MEDIA_CHROME_IDLE_CSS_CLASS, BigMediaImmersionController};
pub use overlay::{BigMediaOverlayLayerRole, BigMediaOverlayStack};
