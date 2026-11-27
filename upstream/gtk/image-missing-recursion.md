# GtkIconTheme: endless recursion in real_choose_icon() when no theme has image-missing

Looking up an icon that does not exist crashes with a stack overflow when no
theme in the chain has `image-missing`. When nothing matches,
`real_choose_icon()` looks up `image-missing` by calling itself
(`gtk/gtkicontheme.c:2143`, same line in 4.24.1 and main). When that lookup
fails too, it calls itself again with the same arguments, until the stack
runs out.

GTK's own `image-missing.png` is only found as part of hicolor
(`theme_subdir_load()`, `gtk/gtkicontheme.c:3235`): GTK looks for it under
`/org/gtk/libgtk/icons/` only in the directories listed by the hicolor
`index.theme` that is found first. So any of these leaves nothing to fall back
to, and every lookup of a missing icon crashes:

- a hicolor `index.theme` earlier in the search path (for example in
  `~/.local/share/icons/hicolor/`) that does not list `16x16/status`;
- a resource path set with `gtk_icon_theme_set_resource_path()` that does not
  include `/org/gtk/libgtk/icons/`;

when the selected theme and the themes it inherits have no `image-missing`
either (hicolor alone, a theme that is not installed, or a theme without it).

## Steps to reproduce

1. Create a hicolor index that lists one directory:

   ```sh
   dir=$(mktemp -d)
   mkdir -p "$dir/icons/hicolor"
   printf '[Icon Theme]\nName=Hicolor\nDirectories=48x48/apps\n\n[48x48/apps]\nSize=48\nType=Threshold\n' \
     > "$dir/icons/hicolor/index.theme"
   ```

2. Build this program (`gcc -o repro repro.c $(pkg-config --cflags --libs gtk4)`):

   ```c
   #include <gtk/gtk.h>

   int
   main (void)
   {
     GtkIconTheme *theme;
     GtkIconPaintable *icon;

     gtk_init ();

     theme = gtk_icon_theme_new ();
     gtk_icon_theme_set_theme_name (theme, "No-Such-Theme");
     icon = gtk_icon_theme_lookup_icon (theme, "no-such-icon", NULL, 32, 1,
                                        GTK_TEXT_DIR_NONE, 0);
     g_print ("%s\n", gtk_icon_paintable_get_icon_name (icon));

     return 0;
   }
   ```

3. Run it with `XDG_DATA_HOME=$dir ./repro` in the same shell.

The same happens without the hicolor file when the program calls
`gtk_icon_theme_set_resource_path (theme, (const char *[]) { NULL })` and
uses a theme without `image-missing`.

## Current behavior

The program dies with SIGSEGV after about 52 000 nested `real_choose_icon()`
calls. With `gtk_icon_theme_set_theme_name (theme, "Adwaita")`, which has
`image-missing`, it prints `image-missing`.

## Expected outcome

The lookup returns the `image-missing` fallback, as it does for every other
theme.

## Version information

- GTK 4.24.1 (Arch Linux package 1:4.24.1-1), GLib 2.90.1, Wayland.
- Also main at b9cd9290d5: `real_choose_icon()` has not changed since 4.24.1.

## Warnings

None.

## Backtrace

System GTK 4.24.1, symbols from debuginfod:

```
Thread 1 "repro" received signal SIGSEGV, Segmentation fault.
#0  g_get_monotonic_time_ns () at ../glib/glib/gmain.c:3339
#1  g_get_monotonic_time () at ../glib/glib/gmain.c:3351
#2  ensure_valid_themes (self=0x5555555b8820 [GtkIconTheme]) at ../gtk/gtk/gtkicontheme.c:1957
#3  real_choose_icon (self=self@entry=0x5555555b8820 [GtkIconTheme], icon_names=icon_names@entry=0x7fffff7ff200, size=size@entry=32, scale=scale@entry=1, flags=flags@entry=GTK_ICON_LOOKUP_NONE) at ../gtk/gtk/gtkicontheme.c:2013
#4  real_choose_icon (self=self@entry=0x5555555b8820 [GtkIconTheme], icon_names=icon_names@entry=0x7fffff7ff2a0, size=size@entry=32, scale=scale@entry=1, flags=flags@entry=GTK_ICON_LOOKUP_NONE) at ../gtk/gtk/gtkicontheme.c:2143
[... the same frame at gtkicontheme.c:2143, 52 372 more times ...]
#52377 real_choose_icon (self=self@entry=0x5555555b8820 [GtkIconTheme], icon_names=icon_names@entry=0x7fffffffd0d0, size=size@entry=32, scale=scale@entry=1, flags=flags@entry=GTK_ICON_LOOKUP_NONE) at ../gtk/gtk/gtkicontheme.c:2143
#52378 choose_icon (self=self@entry=0x5555555b8820 [GtkIconTheme], icon_names=icon_names@entry=0x7fffffffd0d0, size=size@entry=32, scale=scale@entry=1, direction=direction@entry=GTK_TEXT_DIR_NONE, flags=flags@entry=GTK_ICON_LOOKUP_NONE) at ../gtk/gtk/gtkicontheme.c:2278
#52379 gtk_icon_theme_lookup_icon (self=0x5555555b8820 [GtkIconTheme], icon_name=0x555555556012 "no-such-icon", fallbacks=0x0, size=32, scale=1, direction=GTK_TEXT_DIR_NONE, flags=GTK_ICON_LOOKUP_NONE) at ../gtk/gtk/gtkicontheme.c:2369
#52380 0x00005555555551e4 in main () at repro.c:13
```

## Additional information

Two downstream reports show the same backtrace and are probably this bug:
[omarchy#14851](https://github.com/omacom/omarchy/issues/14851) (GTK 4.22.4,
an icon theme that is not installed; reproduced there with a hicolor
`index.theme` in `~/.local/share/icons` that lists only `48x48/apps`) and
[faderframe#25](https://github.com/BurningTreeC/faderframe/issues/25) (macOS
bundle without its icon themes).

### Proposed fix

`image-missing-recursion.patch`: when the `image-missing` lookup itself
fails, return GTK's own `image-missing.png` from the resources, as
`gtk_icon_theme_lookup_by_gicon()` already does for GIcon types it cannot
handle. The paintable is cached like any other lookup result.

The patch adds `/icontheme/missing-without-fallback` to
`testsuite/gtk/icontheme.c`: the test theme with an empty resource path, a
lookup of a missing name, and checks that the result is GTK's
`image-missing.png`. Run with `meson test icontheme` (wayland setup, headless
compositor):

- without the fix: SIGSEGV after the first 10 subtests;
- with the fix: 21 of 21 subtests pass.

With the fix, the program above prints `image-missing` and draws GTK's icon.
