# big-gtk-kit

GTK4 building blocks shared by the BigLinux Rust programs, the crates those
programs are built on (`shared/`), and the tools that check them. A piece of
the `big-gtk-kit` crate enters only when it is GTK-generic, used by more than
one program or backed by a measurement, and it depends on GTK, libadwaita and
a few small crates (gdk4-wayland, futures-channel, log), never on code of any
application.

When GTK or libadwaita gets something wrong, the kit works around it with a
different way of doing the same thing; its code never relies on a patched
library. Each workaround checks for the faulty state first, so a fixed library
leaves it idle. Fixes to send upstream are kept apart, in `upstream/`.

The `big-gtk-kit` crate needs GTK 4.22 and libadwaita 1.9 or newer, the
crates in `shared/` GTK 4.24; BigLinux ships GTK 4.24 and libadwaita 1.10.
Older libraries are not supported.

## Use

Rust, as a Cargo git dependency:

```toml
big-gtk-kit = { git = "https://github.com/biglinux/big-gtk-kit" }
```

`deny.toml` in each consumer allows git sources from the `biglinux` GitHub
organization only.

`big-item-view/` is a second crate in this workspace: one drawn, virtualized
widget for an icon grid, a column flow or rows with columns. Five thousand rows
with three columns and their headings, in a 1100×760 window, are 46 widgets for
the whole window, as are none; a `GtkColumnView` with the same columns makes
1 460, a row, three cells and three labels for each row it builds (GTK
4.22.5, counted by the `tools/` census). Colored icons come from the caller's
`IconLoader`, else GTK's icon theme draws them.

```toml
big-item-view = { git = "https://github.com/biglinux/big-gtk-kit" }
```

`shared/` holds the crates the BigLinux programs are built on, above the kit:
`big-os-kit` (GTK-free system calls, storage, processes), `big-image` (image
loading in glycin's sandbox), `big-relm4-components` (Relm4 and libadwaita
components), `big-app-kit` (the application shell) and `big-testkit` (the test
harness). Each is a git dependency the same way, at the same revision as the
kit. They need Rust 1.100. `big-image` calls what only this repository's copy
of glycin has (`third_party/glycin/BIGLINUX-PATCH.md`), and Cargo reads
`[patch]` only in the workspace it builds, so a program that uses it names the
copy in its own `Cargo.toml`:

```toml
[patch.crates-io]
glycin = { git = "https://github.com/biglinux/big-gtk-kit" }
```

`shared/clippy.toml` holds their bans and `rules/` their ast-grep rules: a
program copies the entries of the owners it calls.

The Python programs carry their own copy of the Python port (`big_gtk_kit`:
`install()` with the tooltip card, the dialog focus fix and the popup-parent
release), so they need nothing from this repository.

## Install

```rust
app.connect_startup(|_| big_gtk_kit::install());
```

This turns on what needs no change at the call sites:

- the tooltip card below;
- taking the focus out of every `AdwDialog` as it closes: otherwise a dialog
  opened over a window where nothing had the focus kept its whole widget tree
  (libadwaita 1.9 restores no focus, GTK 4.22 loses the reference it then
  takes), 22 of 25 measured, every `AdwShortcutsDialog` over 120 objects;
- releasing the parent surface of every Wayland popup: GDK never drops the
  reference a popup takes on it, so every window that showed a popover, menu or
  tooltip kept its surface after closing, 25 of 25 measured (GTK 4.22 through
  4.25.0, still so on `main`; it runs on those releases only, since on one that
  drops the reference itself it would unreference the surface twice, so any
  other release leaks; a test fails on a GTK that belongs on the other side of
  that list);
- under the Cairo renderer (`GSK_RENDERER=cairo`), libadwaita's window shadows
  restated so their fade to backdrop changes colors only: GTK keeps every
  blurred shadow corner it draws, keyed by blur and size, and each frame of
  libadwaita's fade has new ones, 2.2 MB a minute for a window losing the
  focus 70 times a minute without this; every state draws the same pixels
  (GTK 4.24.1, still so on `main`; libadwaita 1.10, left off under
  `GTK_THEME` or granite, where its stylesheet is not in use, and once that
  stylesheet changes these rules);
- turning a `GtkSwitch` at once, with animations on, when it is off screen or
  its window is suspended (screens off, covered): GTK changes a switch's state
  on the last frame of its slide, so a key or a screen reader's action on it
  waited until the window was seen again (GTK 4.22 through 4.25.0, still so on
  `main`).

## Workarounds

Clippy enforces the ones that stand in for a single call: copy the entries of
this repository's `clippy.toml` into the program's own. `dialog::present_kept`,
`popover` and `timer` change how a call is used, which a lint cannot see; the
ast-grep rules under `rules/` find the hand-made copies of the last two.

| Use | Instead of | Why (measured) |
|---|---|---|
| `accessible::relate` | `update_relation` | gtk-rs 0.11 leaks the target list on every call |
| `alert::choose`, `choose_future` | `AlertDialogExtManual::choose` | libadwaita-rs 0.9 keeps every dialog asked that way |
| `alert::Alert` | `adw::AlertDialog::new`, `builder` | libadwaita 1.9 leaves three breakpoints of every dialog shown as a sheet behind, about 1.8 KB an opening; an `Alert` made for a parent in an `AdwWindow` asks again with a dialog already made; add responses, connect `closed` and downgrade through it (a `WeakAlert` from an earlier question stops upgrading) |
| `breakpoint::add` | `add_breakpoint` | libadwaita 1.9 never releases a window's breakpoints |
| `css::add_provider_for_widget_lifetime` | `style_context_add_provider_for_display` per widget | the display keeps the provider and its rules |
| `css::install`, `css::DisplaySheet` | `style_context_add_provider_for_display` | the display keeps every provider it is given: a sheet added again restyles every window for nothing |
| `css::provider` | `CssProvider::new` | a program's provider answers `prefers-color-scheme`, `prefers-contrast` and `prefers-reduced-motion` from its own properties, light and no preference, whatever the system says |
| `dialog::Dialog` | `adw::Dialog::new`, `builder` | the same three breakpoints per opening; a `Dialog` shows one already made, as `Alert` does; one kept and presented again stays an `adw::Dialog` |
| `dialog::present_kept` | building `AdwShortcutsDialog` per opening | its sections and models outlive it (libadwaita 1.9) |
| `file_dialog::present`, `present_multiple` | `gtk::FileDialog::new`, `builder` | GTK asks the desktop's own picker through the portal only for sandboxed programs; these ask the portal first, so GNOME, KDE and BigLinux each show theirs |
| `directory::unwatched` | `gtk::DirectoryList::new`, `builder` | GTK 4.22 to 4.24 (still so on `main`) frees a watched list's queued change while its file lookup runs, and the answer reads freed memory: a crash when lists are rebuilt while files arrive; the list does not follow its folder, so make it again when the folder changes |
| `menu::append_shared_section`, `append_refilled_section` (items vary per build) | `gio::Menu::append_section` | GTK 4.22's menu tracker keeps every section it shows |
| `password::password_row` | `PasswordEntryRow::new`, `build()` | libadwaita 1.9 keeps the buffer, and the password |
| `popover::auto_unparent_on_close` | a popover left on `set_parent` | GTK pins it, and its tree, to the widget forever |
| `slider::with_range`, `slider::build` | `Scale::new`, `with_range`, `ScaleBuilder::build` | a `GtkScale` takes every wheel turn over it, so scrolling a page moves the slider under the pointer |
| `source::remove` | `glib::SourceId::remove` | panics, and so aborts the program, when the source already ran |
| `switch::set_active` | `Switch::set_active` | GTK 4.22 cancels a click's slide on an unchanged value |
| `timer::OnceTimer` | a `timeout_add_local_once` id kept to cancel | GLib gives the id of a timer that ran to another source, which the cancel then removes |
| `view_switcher::toggles_for` | `AdwViewStack`, `AdwInlineViewSwitcher` | leak their animation and a tree per page |

`Alert` and `Dialog` take the parent when they are made, and
`present()`, `Alert::choose` and `Alert::choose_future` show them on it. A
parent inside an `AdwWindow` or `AdwApplicationWindow` gets a dialog already
made, shown as a sheet, which goes back once it has closed. No parent, or one
in any other window, gets a dialog of its own that is never reused:
libadwaita 1.9 cannot show a dialog that was once a sheet in a window of its
own, and such a dialog leaves no breakpoints behind anyway. Make them once the
parent is in its window.

Not reachable from a program, so left as they are: the three breakpoints of a
dialog made outside `Alert` and `Dialog` (an `AdwPreferencesDialog`, an
`AdwAboutDialog`, or one that must stay its own object, such as a Relm4 root
built per opening), breakpoints declared in `.ui` files, and GtkSourceView
5.20's completion (3 `GSignalGroup` and 4 `GtkAdjustment` per view with its
map).

## Tooltips

After `install()`, every `tooltip-text` and `tooltip-markup` of the program shows as
the BigLinux card, whether set in code, builders, relm4 `view!` or `.ui`
files, in light, dark and high contrast, with no change at the call sites. GTK keeps
deriving the accessible name and description from the tooltip.

- One card per window or popover, made on first hover; widgets carry nothing.
- 150 ms before the first card; moving on to a neighbor shows the next at once.
- A click, key, scroll or touch closes it; it stays closed on that widget.
- `tooltip::set_icon(&widget, Some("icon-name"))` adds an icon tile.
- `tooltip::set_show_when_inactive(true)` for panels, which are never active.
- `tooltip::set_follow_pointer(true)` anchors cards at the pointer's column.
- `tooltip::hide_all()` closes the card when a popup or preview is about to
  take its place without an input event.
- `tooltip::set_area_tips(&view, |view, x, y| ...)` gives a widget that draws
  its own items one tip per item, in place of answering GTK's `query-tooltip`.
- `tooltip::name_control(&widget, label)` gives a control one name: the tooltip
  a person reads and the label a screen reader announces. `icon_button`,
  `icon_toggle_button`, `icon_menu_button`, `custom_menu_button` and
  `labeled_icon_button` build named buttons.
- In a program made of modules, each linking its own copy of the kit, these
  settings and `hide_all` reach the copy that installed the cards: they live
  on the display, so call them once it is open.

`cargo run --example tooltip_demo` shows every case (`BIG_DEMO_DARK=1` for the
dark card, `BIG_DEMO_FOLLOW=1` to follow the pointer).

## Option cards

A row that shows a setting's value opens a card of choices beside it, on the
window's overlay, joined to the row by a bridge. Two ways to open one:

- `side_card::SideCards`: a click or Enter opens it, with the focus on the
  current choice. Nothing opens on hover, so the pointer can cross the rows on
  its way elsewhere; settings pages and editors use it.
- `hover_card::HoverCards`: the pointer resting on a row opens its card, and
  the pointer may travel into it; a click or Enter opens it for the keyboard,
  which the pointer then never replaces. A short sidebar of options uses it.

Both place the card on the row's end side, or the other side when only that
has room, and report what happens as events the owner passes back, so no
handler holds a widget.

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --config 'build.warnings="deny"'
tools/headless-gate.sh cargo nextest run --workspace
cargo nextest run -p glycin --lib
tools/headless-gate.sh cargo nextest run --workspace --run-ignored only -j 1 \
    -E 'not test(/::(mount_change_is_seen|closed_about_keeps_cache|removed_link_label_keeps_cache|first_refusal_is_announced|work_start_is_announced)$/)'
tools/headless-gate.sh --accessible cargo nextest run --workspace --run-ignored only \
    -E 'test(/::(closed_about_keeps_cache|removed_link_label_keeps_cache|first_refusal_is_announced|work_start_is_announced)$/)'
tools/headless-gate.sh cargo test --workspace --doc
ast-grep scan
cargo deny --workspace check
ruff check . && ruff format --check .
```

The patched glycin copy is outside the workspace, so `--workspace` skips its
tests; `-p glycin` runs them (`third_party/glycin/BIGLINUX-PATCH.md` says what
they check).

`tools/headless-gate.sh` runs the tests in a private KWin session with a home
of its own. The ignored tests take the activation, so they run one at a time;
four read the AT-SPI bridge, and `mount_change_is_seen` needs a private mount
namespace. rustdoc runs the doc tests from `TMPDIR`, which must allow
execution.

Three tests of big-os-kit's document storage need the machine to offer what
they check is kept, and fail saying what is missing: the user in a second group
(`replaced_document_keeps_group`), a temporary directory on a file system that
takes access ACLs (`replaced_document_keeps_access_acl`) and seccomp filters on a
thread (`creation_without_renameat2_links`, which refuses `renameat2` there).

## tools/

Leak detection for GTK4 programs: a static checker for Rust sources, plus a
per-type sweep over open/close cycles with a GObject census and a reference
tracer that work on any GLib process. See `tools/README.md`.

## upstream/

Fixes to the libraries themselves, held until they are sent upstream: a note
for each fix, with a patch where there is one, and one test package per
library.
