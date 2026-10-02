# big-gtk-kit

GTK4 building blocks shared by the BigLinux programs, in Rust (`src/`) and
Python (`python/`), and the tools that check them. A piece enters only when it
is GTK-generic, used by more than one program or backed by a measurement, and
it depends on nothing but GTK and libadwaita: no code of any application.

When GTK or libadwaita gets something wrong, the fix lives here, as a different
way of doing the same thing — never as a patch to the library. Each workaround
checks for the faulty state first, so a fixed library leaves it idle.

## Use

Rust, as a Cargo git dependency:

```toml
big-gtk-kit = { git = "https://github.com/biglinux/big-gtk-kit" }
```

`deny.toml` in each consumer allows git sources from the `biglinux` GitHub
organisation only.

Python (PyGObject): the `python-big-gtk-kit` package (`pkgbuild/PKGBUILD`) in
the program's `depends`.

## Install

```rust
app.connect_startup(|_| big_gtk_kit::install());
```

```python
import big_gtk_kit

app.connect("startup", lambda _app: big_gtk_kit.install())
```

This turns on what needs no change at the call sites:

- the tooltip card below;
- taking the focus out of every `AdwDialog` as it closes: otherwise a dialog
  opened over a window where nothing had the focus kept its whole widget tree
  (libadwaita 1.9 restores no focus, GTK 4.22 loses the reference it then
  takes), 22 of 25 measured, every `AdwShortcutsDialog` over 120 objects;
- releasing the parent surface of every Wayland popup: GDK never drops the
  reference a popup takes on it, so every window that showed a popover, menu or
  tooltip kept its surface after closing, 25 of 25 measured (GTK 4.22.4, still
  so on `main`; limited to those versions).

## Workarounds

Clippy enforces them: copy the entries of this repository's `clippy.toml` into
the program's own.

| Use | Instead of | Why (measured) |
|---|---|---|
| `accessible::relate` | `update_relation` | gtk-rs 0.11 leaks the target list on every call |
| `alert::choose`, `choose_future` | `AlertDialogExtManual::choose` | libadwaita-rs 0.9 keeps every dialog asked that way |
| `breakpoint::add` | `add_breakpoint` | libadwaita 1.9 never releases a window's breakpoints |
| `css::add_provider_for_widget_lifetime` | `style_context_add_provider_for_display` per widget | the display keeps the provider and its rules |
| `dialog::present_kept` | building `AdwShortcutsDialog` per opening | its sections and models outlive it (libadwaita 1.9) |
| `menu::append_shared_section` | `gio::Menu::append_section` | GTK 4.22's menu tracker keeps every section it shows |
| `password::password_row` | `PasswordEntryRow::new`, `build()` | libadwaita 1.9 keeps the buffer, and the password |
| `popover::auto_unparent_on_close` | a popover left on `set_parent` | GTK pins it, and its tree, to the widget forever |
| `switch::set_active` | `Switch::set_active` | GTK 4.22 cancels a click's slide on an unchanged value |
| `view_switcher::toggles_for` | `AdwViewStack`, `AdwInlineViewSwitcher` | leak their animation and a tree per page |

Not reachable from a program, so left as they are: the three breakpoints an
adaptive `AdwDialog` makes for itself (about 0.5 KB per opening), breakpoints
declared in `.ui` files, and GtkSourceView 5.20's completion (3 `GSignalGroup`
and 4 `GtkAdjustment` per view with its map).

## Tooltips

After `install()`, every `tooltip-text` and `tooltip-markup` of the program shows as
the BigLinux card — set in code, builders, relm4 `view!` or `.ui` files alike —
in light, dark and high contrast, with no change at the call sites. GTK keeps
deriving the accessible name and description from the tooltip.

- One card per window or popover, made on first hover; widgets carry nothing.
- 150 ms before the first card; moving on to a neighbour shows the next at once.
- A click, key, scroll or touch closes it; it stays closed on that widget.
- `tooltip::set_icon(&widget, Some("icon-name"))` adds an icon tile.
- `tooltip::set_show_when_inactive(true)` for panels, which are never active.
- `tooltip::set_follow_pointer(true)` anchors cards at the pointer's column.
- `tooltip::hide_all()` closes the card when a popup or preview is about to
  take its place without an input event.
- `tooltip::set_area_tips(&view, |view, x, y| ...)` gives a widget that draws
  its own items one tip per item, in place of answering GTK's `query-tooltip`.

Both draw the card from the same `tooltip.css`. Python has `install()` only;
the functions above, and the workarounds, stay Rust-only until a Python program
needs one.

`cargo run --example tooltip_demo` shows every case (`BIG_DEMO_DARK=1` for the
dark card, `BIG_DEMO_FOLLOW=1` to follow the pointer).

## Checks

```sh
cargo clippy --all-targets -- -D warnings
tools/headless-gate.sh cargo test -- --include-ignored
ruff check . && ruff format --check .
tools/headless-gate.sh python -m pytest python/tests
```

The ignored tests need a display; `tools/headless-gate.sh` runs them in a
private KWin session.

## tools/

Leak detection for GTK4 programs (formerly `gtk4-leak-guard`): a static
checker for Rust sources, plus a per-type sweep over open/close cycles with a
GObject census and a reference tracer that work on any GLib process. See
`tools/README.md` (Portuguese).
