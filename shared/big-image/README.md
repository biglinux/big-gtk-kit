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

The exact public API and safety requirements are in crate-level rustdoc. Do not
infer current methods from roadmap documents.

## Ownership and lifecycle

CPU/native decode work runs outside GTK callbacks; texture creation returns to the display owner.

For budgeted prefetch or zoom pixels, put `AccountedRgba` inside the shared `Arc`
before passing it to `rgba_to_shared_texture`. The reservation then follows the
allocation through caches and GTK textures until its last owner is dropped.
Ordinary `BigRgbaImage` callers remain supported without a reservation; this is
not an accounting limit for GPU memory, decoder scratch space, or the process.

## Native dependencies

glycin/system codecs, resvg, image libraries depending on feature/use.
`thumbnailer` and `office_page` run `bwrap`; `office_page` also needs
LibreOffice (`/usr/bin/soffice`) and keeps its profile template under
`$XDG_CACHE_HOME/big-image`.

The workspace feature gates and nearest product PKGBUILD define exact versions.
See [getting started](https://github.com/biglinux/big/blob/main/docs/handbook/getting-started.md).

## Example

Real adopter: `apps/bigimages`.

The complete learning map is [framework examples](https://github.com/biglinux/big/blob/main/docs/reference/framework-examples.md).

## Consumers

bigdesktop's [package catalog](https://github.com/biglinux/big/blob/main/docs/reference/packages.md) lists its packages that use this one.

## Validate

```bash
cargo fmt -p big-image -- --check
cargo clippy -p big-image --all-targets --all-features --locked -- -D warnings
cargo nextest run -p big-image --locked
cargo test -p big-image --doc --locked
```

Run graphical tests through `tools/headless-gate.sh` when the changed behavior
needs GTK. Follow [validation](https://github.com/biglinux/big/blob/main/docs/handbook/validation.md) for the full matrix.
