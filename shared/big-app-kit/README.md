# big-app-kit

BigLinux graphical AppShell: single-instance activation, gettext/CSS boot, and dark-mode delivery.

## Use when

Use this crate to boot a GTK/libadwaita application, handle single-instance activation, initialize gettext/CSS, and deliver later activations to one Relm4 root.

## Do not use when

Do not use it for window layout, settings schemas, or display-free services.

## Main entry points

`AppShell`, `AppShellSpec`, `AppShellComponent`, `GettextInit`, `SessionRuntime`,
`taskbar_progress::TaskbarProgress` (a task's progress on the product's taskbar
button), `BigWindowSet` (the primary window and every workspace window, and
which one is active), and the window actions every product spells the same:
`install_product_actions` (Help and About for a `BigProduct`),
`install_window_quit`, `install_fullscreen_action` and `install_input_action`.

The exact public API and safety requirements are in crate-level rustdoc. Do not
infer current methods from roadmap documents.

## Ownership and lifecycle

The product root owns the application window and retains all child controllers. `AppShell` owns process/application startup and shutdown.

`session_runtime::SessionRuntime` has current consumers in AppShell, shell and
Workbench. Acquire on the GTK owner thread; retain the Rc in the window, session
or pane keepalive. The weak registry does not own the lifetime. Linked users share
one pressure transport and the existing cooperative resource governor. Separate
cdylibs/processes do not share Rust statics. Use named cache hooks only for a
process-lifetime cache; per-window hooks retain weak owners. Neither cache
registration nor release drops user captures under the registry borrow.

## Native dependencies

GTK 4.24, libadwaita 1.10, GLib/GIO.

The workspace feature gates and nearest product PKGBUILD define exact versions.

## Example

`cargo run -p big-app-kit --example relm4_lifecycle`.

## Validate

```bash
cargo fmt -p big-app-kit -- --check
cargo clippy -p big-app-kit --all-targets --all-features --locked -- -D warnings
cargo nextest run -p big-app-kit --locked
cargo test -p big-app-kit --doc --locked
```

Run graphical tests through `tools/headless-gate.sh` when the changed behavior
needs GTK.
