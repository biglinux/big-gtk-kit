# GtkIconTheme: look up icons in icon-theme.cache instead of listing them

A large icon theme makes every GTK 4 program start slower. At the first lookup,
`theme_subdir_load()` (`gtk/gtkicontheme.c`) walks every directory of the theme
and of the themes it inherits, lists every icon of each directory from
`icon-theme.cache` (one pass over the cache per directory), interns every name
and fills per-size hash tables. A valid cache only replaces `readdir()` with the
cache's list; it does not avoid the listing. bigicons-papient has 38 directories
plus 38 scaled ones, 23 130 files and 24 627 links; Adwaita has 34 directories
and 804 files.

`icon-cache-lookup.patch` (6 files, +434 −58) applies on 4.24.1 and on main
b9cd9290d5. The commit message is at the top of the patch.

## Change

- A directory with a valid `icon-theme.cache` keeps only (cache, directory index
  in the cache). Lookups use the cache's own hash table, the hash of
  `gtk4-update-icon-cache`, as GTK 3 did: `gtk_icon_cache_find_icon()` finds the
  image list of `name` and `name.symbolic` once per cache and lookup, and
  `gtk_icon_cache_get_icon_flags()` gives the flags in the directory, with the
  same `.symbolic.png` conversion as the listing. Directories without a cache are
  listed as before.
- Order is kept: within a size, listed and cached directories stay in load order
  (`dir_index`) and the first that has the icon wins, as with the single table.
  `has_icon`, `has_gicon` and `get_icon_sizes` use the same lookup;
  `get_icon_names` lists the cached directories only when called.
- Listed directory names stay interned and compared by pointer. The fast refusal
  of an unknown name becomes "no cache has the name" (two hash lookups per
  cache). Every read happens under `gtk_icon_theme_lock`, including the threaded
  preload (`load_theme_thread`); the mapped cache is only read; the
  `GtkIconCache` reference count changes only at load and in `blow_themes`, under
  the same lock.
- History: "icon-cache: Add new function to list all the icons in a
  directory" and "IconTheme: Make icon lookups faster" (!1381, 2020) replaced
  GTK 3's per-directory lookups with per-size tables and a global name set, and
  "GtkIconCache: Remove unused code" in the same merge request removed the
  cache lookup; "icontheme: Optimize memory use and lookup speed by
  internalizing icon names" (!1390) interned the names; "icontheme: Keep a
  single string set" (!5423, 2023) merged the themes' sets. Related issues:
  #5362 (scanner running twice with Papirus), #5263 (slow GTK 4 startup).

## Test

The patch adds `icontheme-cached` to `testsuite/gtk/meson.build`: the
`icontheme` test run again on copies of its two test themes, to which
`cache-icon-themes.py` gives an `icon-theme.cache` made by
`gtk4-update-icon-cache`. The cache of the copied icons theme lists
`16x16/cache-only.png`, and the file is removed after the cache is written, so
only a lookup in the cache can find it. `/icontheme/cache` checks
`has_icon()`, `get_icon_names()` and the lookup of that name; on the uncached
themes it is skipped.

With this patch and image-missing-recursion, `meson test icontheme
icontheme-cached` (wayland setup, headless compositor) passes on 4.24.1 and on
main: `icontheme` 21 passed and `/icontheme/cache` skipped, `icontheme-cached`
22 passed. The cached run goes through the patch's lookup: under gdb,
`gtk_icon_cache_find_icon()` runs 156 times in `icontheme-cached` and never in
`icontheme`, and a hash that is off by one makes `icontheme-cached` fail
("simple" resolves to `image-missing`) while `icontheme` still passes. GTK
4.24.1 without the patch also passes `icontheme-cached`, through its listing of
the cache, so the test holds the behaviour of a cached theme rather than one
implementation.

## Measurements

GTK 4 window with 6 icons (32 px) and a label, from `main()` to the first painted
frame (`after-paint`), PSS read in the same frame. The theme comes from
`settings.ini` in a scratch `XDG_CONFIG_HOME`: changing it from code after
startup waits for the theme GTK already loads in a thread. 15 interleaved runs,
median (q1–q3), ms. The library was loaded through `LD_LIBRARY_PATH` and checked
in `/proc/PID/maps`.

Desktop (16 CPUs, NVIDIA, 1 323 font faces):

| case | 4.24.1 | patched | PSS MiB |
|---|---|---|---|
| Adwaita, cairo | 60 (56–61) | 61 (58–68) | 23.1 → 22.9 |
| bigicons-papient, cairo | 112 (106–122) | 56 (51–59) | 26.8 → 23.3 |
| Adwaita, GL | 168 (158–177) | 162 (150–171) | 71.6 → 71.4 |
| bigicons-papient, GL | 217 (213–229) | 162 (153–183) | 77.3 → 73.9 |

Intel HD 3000 laptop (4 CPUs, 825 faces):

| case | 4.24.1 | patched | PSS MiB |
|---|---|---|---|
| Adwaita, cairo | 169 (166–174) | 163 (153–167) | 24.3 → 24.2 |
| bigicons-papient, cairo | 346 (342–356) | 165 (158–174) | 28.5 → 24.9 |
| Adwaita, GL | 574 (568–593) | 566 (559–577) | 77.3 → 77.2 |
| bigicons-papient, GL | 767 (753–787) | 577 (574–587) | 81.7 → 78.2 |

Theme load alone (lookup program, no window): papient 55 → 2.9 ms, breeze
27 → 3.1, oxygen 37 → 3.3, Adwaita 4.7 → 3.2.

## Same results

- A lookup program run against both libraries, output compared byte for byte, on
  10 themes (papient and its dark and accent variants, Adwaita, AdwaitaLegacy,
  breeze, Pop, oxygen, hicolor, and a mixed theme with an uncached directory of
  PNG, SVG and `.symbolic.png` in front of a cached Adwaita). For every name of
  `get_icon_names` plus its `-symbolic`, `.symbolic` and missing variants:
  `has_icon`, `get_icon_sizes` and the resolved file at 5 sizes × scale 1 and 2,
  `FORCE_SYMBOLIC` and `FORCE_REGULAR`. 3.9 million identical lines.
- GTK tests: `icontheme` and `icontheme-cached` as above; `check-icon-names`
  67/67.
- valgrind memcheck on the lookup program (Adwaita): 0 errors.

## Costs and risks

- Each lookup is slightly slower: 57 → 85 µs per name in the lookup program
  (~56 calls per name), about +0.5 µs per call; a program makes a few hundred at
  startup.
- `gtk_icon_theme_get_icon_names()` goes from 2.3 to 45 ms on papient: the cost
  moves from every startup to the callers that want the whole list (icon
  browsers, the inspector).
- A corrupt or truncated `icon-theme.cache` can make a GTK program read past the
  end of the mapped file and crash: `gtkiconcache.c` does not check the offsets
  it reads against the file size, in the listing of 4.24.1 as in the lookups of
  the patch, and the validator only runs with `GTK_DEBUG=icontheme`. The patch
  only adds a refusal of an empty hash table.
- A stale cache follows the existing rule: used only when not older than the
  theme directory.

## Before sending

- Maintainers will want to discuss the slower `get_icon_names` and the per-lookup
  cost; a per-theme map from cache directory to size, built in one pass over the
  image list, would cut the second.
