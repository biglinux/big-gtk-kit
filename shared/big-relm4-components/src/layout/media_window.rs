// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Shared immersive chrome and toast-host policy for a full-window media view
//! (the image pane's immersive surface): a real `AdwHeaderBar` (top) with an
//! accessible primary menu, and a bottom control bar.
//!
//! Design: the app owns its media `gtk::Overlay`, its picture surface, and
//! any clipped offload surface. The framework owns the repeatable shell policy:
//! toast host creation, content mounting, header, primary menu and bottom
//! revealer. This keeps each app's media plumbing
//! untouched (a `GtkGraphicsOffload` subsurface, which an `AdwToolbarView`
//! would throttle to about 1 fps) while shared shell mechanics are not
//! re-derived.
//!
//! The primary menu is a real `GtkButton` on a real `AdwHeaderBar`, opening
//! explicit `MenuItem` rows so assistive technologies can name and activate
//! the trigger plus every action.

mod chrome;
mod immersion;
mod overlay;

pub use chrome::{BigMediaChrome, BigMediaMenuSpec};
pub use immersion::{BIG_MEDIA_CHROME_IDLE_CSS_CLASS, BigMediaImmersionController};
pub use overlay::{BigMediaOverlayLayerRole, BigMediaOverlayStack};
