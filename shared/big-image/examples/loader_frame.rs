// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Decode an image through glycin at its full size and write the frame glycin
//! hands over (sRGB, ICC profile already applied) as an 8-bit RGBA PNG.
//! Run once with the `glycin-jpegturbo` loader and once without, it gives two
//! outputs to compare.
//!
//! ```text
//! loader_frame <glycin-jpegturbo binary | -> <input> <output.png>
//! ```

use std::io::BufWriter;
use std::path::Path;
use std::process::ExitCode;

use gtk::{gio, glib};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [loader, input, output] = args.as_slice() else {
        eprintln!("usage: loader_frame <glycin-jpegturbo binary | -> <input> <output.png>");
        return ExitCode::from(2);
    };
    if loader != "-" {
        // SAFETY: `main` is still on its only thread and GLib has not read its data dirs.
        unsafe { big_image::jpeg_loader::activate_with(Path::new(loader)) };
    }
    let decoded = glib::MainContext::new().block_on(async {
        let mut loader = glycin::Loader::new(gio::File::for_path(input));
        loader.accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8a8);
        let image = loader.load().await.map_err(|error| error.to_string())?;
        image.next_frame().await.map_err(|error| error.to_string())
    });
    let frame = match decoded {
        Ok(frame) => frame,
        Err(error) => {
            eprintln!("decode failed: {error}");
            return ExitCode::from(3);
        }
    };
    let written = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut encoder = png::Encoder::new(
            BufWriter::new(std::fs::File::create(output)?),
            frame.width(),
            frame.height(),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        let stride = frame.stride() as usize;
        let row = frame.width() as usize * 4;
        let rows: Vec<u8> = frame
            .buf_slice()
            .chunks(stride)
            .flat_map(|line| &line[..row])
            .copied()
            .collect();
        writer.write_image_data(&rows)?;
        Ok(())
    })();
    match written {
        Ok(()) => {
            println!("color_state {:?}", frame.color_state());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("write failed: {error}");
            ExitCode::from(4)
        }
    }
}
