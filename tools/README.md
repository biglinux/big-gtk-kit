# big-gtk-kit leak tools

Automated memory leak checks for GTK4 programs. Three pieces, from the cheapest
to the most precise; each answers what the one before cannot. The static
checker reads Rust only; the sweep, the census and `reftrace` work on any GLib
process (Rust, C, Python, Vala).

| Piece | Question | Needs a display |
|---|---|---|
| `static/leak_lint.py` | does the code have a shape that has leaked before? | no |
| `sweep/leak_sweep.py` + `build/libgobject-census.so` | which object types does opening and closing this screen leave behind? | yes (isolated session with AT-SPI) |
| `build/reftrace.so` + `reftrace/reftrace.py` | who holds the object that was left? | yes |

Requirements: Python ≥ 3.11 (standard library only; the driver uses `gi` with
Atspi), GLib/GObject and a C compiler. `make` builds both libraries;
`make check` runs the census control test and the Python tool tests;
`make check-gtk` runs the signal-triggered census on a real GTK program inside
`headless-gate.sh` (needs `kwin_wayland`).

`headless-gate.sh [--accessible] COMMAND...` runs the command in a private
virtual KWin session with its own D-Bus bus; `--accessible` also starts the
accessibility bus and a throwaway HOME, which is what the sweep needs.

## 1. Static checker

    static/leak_lint.py [--json] [--no-icons] [--errors-only] PATH...

Errors (exit 1): `unparent()` inside the popover's own `closed` handler,
closing a window by watching `destroy`, `set_parent` without `unparent`, a
`teardown` that reads the list item's child, an icon with an embedded image (in
the themes installed on the machine running the checker). Warnings: a handler
that holds its own widget or an ancestor (also through a closure kept in an
`Rc`, a child set by a builder, or a controller), siblings that hold each
other, follower lists pruned too late, a handler on an object that lives as
long as the process (the display clipboard, `VolumeMonitor`, the application
seen from a component) with its id thrown away. Reads `.rs` files only; test
code is ignored and an unreadable file is skipped with a warning.

A finding is waived with a reason; a waiver without one is an error:

    // leak-guard: allow(set-parent-without-unparent) the parent is a GtkOverlay, which undoes it on dispose

## 2. Per-type sweep

    headless-gate.sh --accessible python3 sweep/leak_sweep.py SURFACES.toml \
        [--only NAME...] [--quick] [--cycles N] [--json OUT] [--library PATH]

The driver starts the program with the census preloaded, waits for the app on
the accessibility bus and, for each surface: warms up, takes census A, opens
and closes it N times, takes census B and confirms with a second round. A type
that gained at least one instance per cycle is a leak, unless an `[[allow]]`
with a per-cycle ceiling and a reason covers it (in the file or in the
surface). A surface that does not really close is reported as such. Exit 0
clean, 1 leak, 2 did not run (incomplete file, missing program or `gdbus`);
`LEAK_SWEEP_DIR` chooses where the censuses and the program log go.

`command` must execute the GTK program itself: the driver asks the process it
started for a census by signal, and an intermediate script would receive the
signal instead. `{workdir}` is the only placeholder substituted; the seeds of
`[program.files]` always live inside it.

Surface file format:

```toml
cycles = 6

[program]
command = ["target/debug/app", "{workdir}/folder"]  # {workdir}: throwaway directory
atspi_name = "app"
dbus_name = "org.example.App"
window_path = "/org/example/App/window/1"

[program.env]
XDG_CONFIG_HOME = "{workdir}/config"

[program.files]                                     # seeds inside {workdir}
"folder/note.txt" = "one\n"

[[allow]]
type = "AdwBreakpoint"
per_cycle = 3
reason = "libadwaita 1.9.3: AdwBreakpointBin never releases its breakpoints"

[[surface]]
name = "preferences"
quick = true
open = [{ do = "action", name = "preferences", settle = 1.0 }]
close = [{ do = "dismiss" }]
```

Steps: `action` (org.gtk.Actions.Activate), `dbus`, `command`, `toggle` and
`click` (by accessible name), `dismiss` (Cancel/Close in dialogs and in windows
beyond the first), `close-new` (the same, only in the windows the cycle opened:
for a shell that hosts the apps), `wait`.

The census works on any GLib process, without the driver:

    GOBJECT_CENSUS_OUT=DIR/c LD_PRELOAD=build/libgobject-census.so program
    kill -USR2 PID        # writes DIR/c.PID.1, .2, ...

Each line is `count Type` (and `count texture WxH`). Counts are approximate:
only the difference between two censuses of the same process means something.
Only resident pages are read, so an object swapped out is not counted.

How it counts: distribution GLib is not built with `G_ENABLE_DEBUG`, so
`g_type_get_instance_count` returns 0. The census scans the process's own
anonymous memory for class pointers and counts one only when it has a
plausible reference count and, with glibc malloc, a live block of exactly the
private data plus instance size starting at the right place. With another
allocator (jemalloc, tcmalloc, mimalloc, ASan) that last test is unavailable:
the census starts with the line `# malloc layout unknown`, and copies of an
object's header count as objects. Differences still hold; absolute counts are
higher.

## 3. Who holds it

    LD_PRELOAD=build/reftrace.so REFTRACE_TYPE=AdwDialog REFTRACE_OUT=DIR/rt program
    reftrace/reftrace.py DIR/rt.PID BINARY --objects        # alive and released
    reftrace/reftrace.py DIR/rt.PID BINARY --object N       # call sites that took more than they gave back

Each event is `N|R|S|s|U address count frames…`, with the count BEFORE the
operation: `N` is creation (`g_object_new` called from outside libgobject),
`s` sinks the floating reference (takes it over, adds nothing), `U 1` is the
last release. A survivor whose traced references balance is held by something
that never called `g_object_ref` on it: GTK's window list (a window never
destroyed) or a Rust value never dropped.

## Known library leaks

Measured on GTK 4.22.4, libadwaita 1.9.3 and GtkSourceView 5.20.0. The right
column says what the kit works around; whatever remains becomes an `[[allow]]`
in the surface file of whoever meets it. Every exception carries a per-cycle
ceiling (for example `AdwBreakpoint ≤ 3 per cycle`): an exception without one
hides regressions.

| Type | Growth | Cause | In the kit |
|---|---|---|---|
| `AdwBreakpoint` | 1 per program breakpoint, per closed window or dialog | libadwaita 1.9.3 `adw_breakpoint_bin_init` (array without a free function) | `breakpoint::add` |
| `AdwBreakpoint` | 3 per adaptive `AdwDialog` opening | the same, in the breakpoints the dialog makes for itself | unreachable: `[[allow]]` with a ceiling of 3 per cycle |
| the dialog's whole tree | per dialog closed over a window without focus | libadwaita 1.9 restores no focus; GTK 4.22 loses the reference of the deferred focus | `install()` (focus taken out on `closed`) |
| `AdwShortcutsSection`, items, `GtkFilterListModel`, `GtkCustomFilter`, `AdwTimedAnimation` | 1 set per `AdwShortcutsDialog` opening | the dialog's teardown | `dialog::present_kept` |
| `GdkWaylandToplevel` | 1 per window that showed a popover, menu or tooltip | GDK 4.22.4 and 4.22.5 (and `main`): the Wayland popup takes its parent with `g_value_dup_object` and never drops it | `install()` (released when the popup finalizes) |
| `GMenu` | 1 per section shown, per menu built | GTK 4.22 `gtk_menu_tracker_add_items` (unmerged section) | `menu::append_shared_section`, `menu::append_refilled_section` |
| `GSignalGroup` ×3, `GtkAdjustment` ×4 | per `GtkSourceView` with a map | GtkSourceView 5.20: `GtkSourceCompletion` creates three groups at init and never frees them | unreachable |
| `GskVulkanImage`, `GtkCssStaticStyle`, `GtkIconPaintable`, `GskFillNode`/`GskColorNode` | a few per opening, then stable | GTK caches | — (a cache, not a leak: memory stable over 20 openings) |
