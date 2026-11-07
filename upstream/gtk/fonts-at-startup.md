# Fonts at GTK 4 startup

No patch: where the font cost of a GTK 4 start goes, and two cuts that would
not change any text.

With the icon-cache patch applied, the number of installed faces costs little on
the critical path: 1 323 vs 2 faces, 53 vs 50 ms on the desktop; 825 vs 2 faces,
159 vs 150 ms on the Intel HD 3000 laptop (window and method as in
[icon-cache-lookup](icon-cache-lookup.md)). Without the patch the font thread
competes with the icon theme load: 119 vs 104 ms and 344 vs 338 ms. These are
runs of their own, not those of the icon-cache-lookup tables; their icon theme
and renderer were not recorded, and the times are closest to its
bigicons-papient, cairo rows (56 and 112 ms, 165 and 346 ms).

Where the time goes (perf, 20 starts):

- `FcInit` runs on Pango 1.58's "[pango] fontconfig" thread
  (`pangofc-fontmap.c`), started early from `gtkmain.c:453`; the main thread
  waits for it only in `wait_for_fc_init`. About 15 ms of CPU per start on that
  thread with 1 323 faces, 9 ms with 2. 85 % of the samples are expat:
  fontconfig parses `conf.d` (58 files) and, at every `FcInit`, also parses the
  whole `conf.avail` (81 files, 468 KB) only to list descriptions
  (`fcinit.c:117`, `FcConfigParseOnly (FC_TEMPLATEDIR)`), about 3 ms.
- On the main thread, creating the first font calls
  `pango_fc_font_map_get_face` → `ensure_families`, which lists every face into
  families (`FcFontSetList`, 1 323 faces in 315 families): about 1.3 ms per
  start, 0 with 2 faces.
- GTK lists families at startup only for the font chooser and
  `GtkFontDialogButton`.

Possible cuts:

- Pango: have `get_face` look up only the font's family instead of building all
  of them (1–3 ms on the main thread, growing with the number of faces).
- fontconfig: skip parsing `conf.avail` in `FcInit` unless the list of
  configuration files is asked for (about 3 ms on the font thread).
