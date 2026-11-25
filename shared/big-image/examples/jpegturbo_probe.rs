// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Decode a JPEG through the configured glycin loader at a bounded size and print
//! the frame delivered. Point `GLYCIN_DATA_DIR` at a directory whose
//! `glycin-loaders/2+/conf.d/` names `glycin-jpegturbo`:
//!
//! ```text
//! GLYCIN_DATA_DIR=~/.cache/glytest \
//!   cargo run -p big-image --example jpegturbo_probe -- /path/to/huge.jpg 2000
//! ```

#![expect(
    clippy::expect_used,
    reason = "a developer tool: a failure ends the run"
)]

use gtk::{gio, glib};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: jpegturbo_probe <path> [bound]");
    let bound: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(2000);

    glib::MainContext::new().block_on(async move {
        let mut loader = glycin::Loader::new(gio::File::for_path(&path));
        loader.accepted_memory_formats(
            glycin::MemoryFormatSelection::R8g8b8 | glycin::MemoryFormatSelection::R8g8b8a8,
        );
        let image = loader.load().await.expect("glycin load");
        let details = image.details();
        println!("source {}x{}", details.width(), details.height());
        let frame = image
            .specific_frame(glycin::FrameRequest::new().scale(bound, bound))
            .await
            .expect("glycin frame");
        println!(
            "frame {}x{} format {:?}",
            frame.width(),
            frame.height(),
            frame.memory_format()
        );
    });
}
