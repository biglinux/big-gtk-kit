// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Which GSK renderer the program really draws with.

use gtk::prelude::*;

/// Log which GSK renderer the display actually chose, once per process, when
/// `window` maps.
///
/// The renderer decides whether every window's buffer reaches the compositor as
/// a GPU dmabuf (`gl`/`vulkan`, imported zero-copy) or as a per-frame software
/// upload (`cairo`). GTK falls back silently when a pinned `GSK_RENDERER`
/// cannot be created, and a fall to `cairo` on an old GPU is the dominant
/// per-frame CPU cost there; this one line makes it show up in the log.
pub fn log_active_renderer_once(window: &impl IsA<gtk::Window>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static LOGGED: AtomicBool = AtomicBool::new(false);
    window.as_ref().connect_map(move |window| {
        if LOGGED.swap(true, Ordering::Relaxed) {
            return;
        }
        let renderer = window
            .renderer()
            .map_or_else(|| "unknown".to_owned(), |r| r.type_().name().to_string());
        let pinned =
            std::env::var("GSK_RENDERER").unwrap_or_else(|_| "unset (GTK default)".to_owned());
        log::info!("GSK renderer in use: {renderer} (GSK_RENDERER={pinned})");
    });
}
