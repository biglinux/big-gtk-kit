# libadwaita: 28 strings in 8 files are looked up in the application's domain

The close button of every `AdwDialog` is announced as "Close", "Close the
window" to a Brazilian Portuguese user, though libadwaita's `pt_BR` catalog has
"Fechar", "Fecha a janela". The same goes for the toast announcement, the split
button's "More Options" tooltip, the avatar's label, the view switcher badge,
the shortcut label's L/R markers, "No Shortcut" and the inspector's adaptive
preview.

`src/adw-sheet-controls.c` and 7 other files translate with `<glib/gi18n.h>`,
whose `_()` and `C_()` call `gettext()` in the program's default text domain.
`adw_init()` binds the `libadwaita` domain (`src/adw-main.c`) but a library
never makes it the default, so these strings are translated only when the
application's own catalog has the same msgid. The other 13 files that translate
include `<glib/gi18n-lib.h>`, whose `_()` is `g_dgettext (GETTEXT_PACKAGE, …)`,
and `config.h` defines `GETTEXT_PACKAGE` as `libadwaita`.

## Change

`translation-domain.patch` replaces `<glib/gi18n.h>` with `<glib/gi18n-lib.h>`
in those 8 files, one line each. Each already includes `config.h` first, which
`gi18n-lib.h` needs. The 3 other files of `src/` that include `<glib/gi18n.h>`
translate nothing (`adw-shortcuts-section.c`, `adw-spinner.c`,
`adw-spinner-paintable.c`); the patch drops the include there. The commit
message is at the top of the patch.

It applies on 1.10.0 (the release BigLinux ships, `1:1.10.0-1`) and on main
295aa0e8.

## Tested

A Python program presents an `AdwDialog` with an `AdwHeaderBar` over an
`AdwApplicationWindow` and calls no `textdomain()`. It ran with
`LANG=pt_BR.UTF-8` under a headless compositor with an isolated HOME, and the
close button was read over AT-SPI:

| library | name | description |
|---|---|---|
| system 1.10.0, `/usr/lib/libadwaita-1.so.0` | Close | Close the window |
| patched, `LD_LIBRARY_PATH` to the package's `usr/lib` | Fechar | Fecha a janela |

`/proc/PID/maps` of each run named the library given. libadwaita's own tests
pass with the patch: 71 of 71 (`meson test`, headless weston, in the package's
`check()`).

## Before sending

- No test: libadwaita's test suite has no installed catalog to translate with.
