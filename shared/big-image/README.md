# big-image

One image loader for the suite: it decodes at the requested size, through the system's
codecs and off the main loop, and renders SVG at the target size instead of
rescaling a bitmap.

## Use when

Use this crate to decode and transform images with explicit size and safety policy outside product UI.

## Do not use when

Do not use it for image-viewer state, window actions, or workspace panes.

## Main entry points

`BigImageSpec`, bounded decode/load/rotate/thumbnail functions.

The exact public API and safety requirements are in crate-level rustdoc.

## Ownership and lifecycle

CPU/native decode work runs outside GTK callbacks; texture creation returns to the display owner.

For budgeted prefetch or zoom pixels, put `AccountedRgba` inside the shared `Arc`
before passing it to `rgba_to_shared_texture`. The reservation then follows the
allocation through caches and GTK textures until its last owner is dropped.
Ordinary `BigRgbaImage` callers remain supported without a reservation; this is
not an accounting limit for GPU memory, decoder scratch space, or the process.

## Native dependencies

glycin and the loaders it runs in its sandbox, and gdk-pixbuf's writers for
saving. `thumbnailer`, `video_thumbnail` and `office_page` run `bwrap`;
`video_thumbnail` also needs FFmpeg (`/usr/bin/ffmpeg`, `/usr/bin/ffprobe`),
`office_page` needs LibreOffice (`/usr/bin/soffice`) and keeps its profile
template under `$XDG_CACHE_HOME/big-image`, and saving an animation needs
ImageMagick (`/usr/bin/magick`).

## Example

`cargo run -p big-image --example loader_frame -- - <input> <output.png>` decodes
one picture through glycin and writes the frame it hands over as a PNG.

## Validate

```bash
cargo fmt -p big-image -- --check
cargo clippy -p big-image --all-targets --all-features --locked -- -D warnings
cargo nextest run -p big-image --locked
cargo test -p big-image --doc --locked
```

Run graphical tests through `tools/headless-gate.sh` when the changed behavior
needs GTK.
