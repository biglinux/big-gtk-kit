# big-testkit

Dev-only test harness: finalization census in a headless KWin session, RSS-slope leak verdicts and the AT-SPI reader of the process for the big/ suite.

## Use when

Use this crate to write dev-only lifecycle, leak, and responsiveness tests shared by multiple packages.

## Do not use when

Do not use it in production dependencies or as a substitute for product assertions.

## Main entry points

GTK test runners, finalization census, RSS slope verdicts, the frame-interval probe (`frame_probe`: p50, p95, p99 and frames over a budget), key presses as GTK delivers them (`harness::press_key`) and the stylesheet check (`harness::assert_css_parses`).

The exact public API and safety requirements are in crate-level rustdoc. Do not
infer current methods from roadmap documents.

## Ownership and lifecycle

Test harnesses create and fully tear down their GTK/application state.

## Native dependencies

GTK/Weston/D-Bus for graphical tests; `/proc` for Linux memory probes.

The workspace feature gates and nearest product PKGBUILD define exact versions.

## Example

`cargo test -p big-testkit --all-targets --locked`.

## Validate

```bash
cargo fmt -p big-testkit -- --check
cargo clippy -p big-testkit --all-targets --all-features --locked -- -D warnings
cargo nextest run -p big-testkit --locked
cargo test -p big-testkit --doc --locked
```

Run graphical tests through `tools/headless-gate.sh` when the changed behavior
needs GTK.
