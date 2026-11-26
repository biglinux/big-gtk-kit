# big-relm4-components

Reusable Relm4/libadwaita components for BigLinux applications.

## Use when

Use this crate to reuse a stateful or behavior-rich Relm4/libadwaita component already shared by applications.

## Do not use when

Do not use it for one fixed label/box or product-specific policy.

## Main entry points

modules `chart`, `css`, `desktop_entry`, `feedback`, `input`, `layout`, `list`, `qr`,
`renderer`, `shape`, `task`, `templates`, `text`, `time` and `wayland`.

The exact public API and safety requirements are in crate-level rustdoc. Do not
infer current methods from roadmap documents.

## Ownership and lifecycle

Parents retain every child controller or subscription for as long as its widget is used.

## Stylesheets

Make every `gtk::CssProvider` with `css::provider()`: one made by
`gtk::CssProvider::new()` answers `prefers-color-scheme`, `prefers-contrast` and
`prefers-reduced-motion` as light and no preference whatever the system says, so
its dark, high-contrast and reduced-motion rules never apply. Clippy refuses the
plain constructor.

## Navigation trails

Use `layout::breadcrumb::BigBreadcrumbBar` for a path that must fit inside a
window or split pane. Pass translated menu labels and your own buttons through
`set_items`; the shared owner handles separators and overflow.
`breadcrumb_button` supplies the standard icon, ellipsis and spacing.

Navigation stays in the product: Bigfiles retains location editing and middle
click, while the terminal retains its linked-directory behavior. Overflow entries invoke
the original button after closing the menu. Capture product owners weakly in
the button callbacks, as with other GTK children.

`input::permissions::BigPermissionEditor` supplies owner/group/others access rows
for BigFiles and BigTerminal. Add `rows()` to a
preferences group, read `mode()` when applying, and capture widgets weakly in
`connect_changed`. Consumers own identity checks, filesystem writes and errors;
the editor changes only its nine ordinary access choices.

## Menus

`layout::hamburger_menu` draws a `gio::Menu` as named rows with icons,
one-line explanations, shortcuts and submenus that open beside their row,
plus a strip of side-by-side buttons, a switch row and a slider
(`described_item`, `append_strip_section`, `switch_item`, `scale_item`,
`mark_destructive`).
Context menus use `BigContextMenu`: one popover parented to its host for the
host's life and filled again at every opening. `attach_context_menu` opens it
on a secondary click, a long press, the Menu key and Shift+F10; a surface whose
menu is something else hears the same requests through
`connect_context_menu_request`.

## Native dependencies

GTK 4.24, libadwaita 1.10, Relm4 0.11.

The workspace feature gates and nearest product PKGBUILD define exact versions.

## Example

`cargo run -p big-relm4-components --example shortcut_editor`.

## Validate

```bash
cargo fmt -p big-relm4-components -- --check
cargo clippy -p big-relm4-components --all-targets --all-features --locked -- -D warnings
cargo nextest run -p big-relm4-components --locked
cargo test -p big-relm4-components --doc --locked
```

Run graphical tests through `tools/headless-gate.sh` when the changed behavior
needs GTK.

The tests that read this process's AT-SPI bus (the links, a field's refusal, the
notice strip and the shortcut capture) are ignored by default and need the real
bridge:

```bash
tools/headless-gate.sh --accessible cargo test -p big-relm4-components --locked -- --ignored
```

`input::dropdown::choice_row(title, labels)` builds the native libadwaita choice
row used by settings and terminal rule forms. Pass translated plain-text labels;
use the returned `AdwComboRow` directly for selection and change notifications.
Its explicit combo-box role exposes the selector to AT-SPI. The existing gallery
`settings-rows` example uses it through `big-settings-rows`. Every choice row and
drop-down of the suite is built by `input::dropdown::build`,
`build_drop_down` or `from_strings`, which name each choice for a screen reader;
`clippy.toml` bans the stock constructors.
