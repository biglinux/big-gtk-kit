# GPU renderer: a font kept for every scale step of an animation

A GTK 4.24.1 program that draws with the GPU renderer keeps one more font for
each scale step of a scale animation, until it exits. libadwaita's dialogs scale
in: BigImages' About dialog left 40 to 50 fonts per opening at first and 610 to
670 after 200 openings, BigFiles' Connect to Server 70 per opening and 980 after
100.

`gsk_gpu_node_processor_add_glyph_node()` (`gsk/gpu/gskgpunodeprocessor.c`)
draws glyphs under a scale or a translation at the exact scale of the frame, so
each frame of the animation has a new scale. `gsk_gpu_cached_glyph_lookup()`
(`gsk/gpu/gskgpucachedglyph.c`) asks `gsk_reload_font()` for the font at that
scale, and Pango's font map makes a new font. Since "gsk/gpu: avoid GPU draw
ops for zero-ink glyphs" (!10244, August 2026, in 4.24.1) a glyph without ink,
such as a space, returns before that font is dropped. Such a glyph gets no
cache entry, so each frame that draws it adds one more reference, and the font
stays with its cairo scaled font and glyph images. The count only stops growing
when later animations repeat the scales of earlier ones.

`glyph-scaled-font-leak.patch` (2 files, +65 −1) applies on 4.24.1 and on main
b9cd9290d5.

## Change

- Drop the font on the early return too.
- `testsuite/gsk/misc.c` gains `/renderer/gl/space-glyph`: it draws "a b" under
  a 1.5 scale with the GL renderer, then checks that the font reloaded at 1.5
  is freed when its last user drops it. It is skipped when GL is not available.
  It opens a second display, as `/renderer/gl` does, because with Mesa a GL
  renderer on the default display aborts once another display that used GL was
  closed. So it runs on Linux only (Wayland or X11) and would fail on macOS and
  Windows, where GDK opens one display; BigLinux's programs are Linux-only.

## Measurements

BigImages and BigFiles (big-host 0.1.0-111) in a headless big-compositor
session (1280x720, scale 1, GL on an NVIDIA GTX 1660 Ti), isolated HOME, GTK
4.24.1 built with the held patches and loaded through `LD_LIBRARY_PATH`, the
library checked in `/proc/PID/maps`. Fonts are live `PangoCairoFcFont`
instances counted by [gobject-census](../../tools/census/); memory from
`smaps_rollup`, KiB; VRAM is the process's line in `nvidia-smi`, MiB.

| journey | fonts | Pss_Anon | VRAM |
|---|---|---|---|
| About, before opening | 3 | 33 100 | 9 |
| About, 200 openings, 4.24.1 (2 runs) | 608, 669 | 71 284, 73 620 | 51, 55 |
| About, 200 openings, patched (2 runs) | 3, 3 | 64 472, 65 772 | 63, 75 |
| About, 200 openings and 60 s idle, 4.24.1 | 669 | 73 608 | 55 |
| About, 200 openings and 60 s idle, patched | 3 | 65 496 | 75 |
| About, 200 openings without animations, patched | 3 | 39 148 | 18 |
| Connect to Server, before opening | 12 | 47 704 | 21 |
| Connect to Server, 100 openings, 4.24.1 | 982 | 81 656 | 26 |
| Connect to Server, 100 openings, patched | 12 | 64 288 | 26 |

- The patch saves 6.7 to 7.7 MiB at 200 openings of About and 17 MiB at 100
  openings of Connect to Server.
- Frames are unchanged: 16.7 ms median and 33.3 ms 95th percentile between
  animation frames (`GDK_DEBUG=frames`), 4949 and 4943 frames over 200
  openings.
- A test program that draws "Connect to Server" at 200 scales from 1.001 to
  1.200 keeps 199 fonts with 4.24.1 and none with the patch.

## Same results

- The 200 renders of that program are byte-identical with and without the
  patch (200 distinct images).
- Screenshots of BigImages at rest and with About open are byte-identical; the
  same for BigFiles with Connect to Server open. BigFiles at rest differs in one
  160×1 row above the path bar, by at most 6/255; its glyphs are identical.
- GTK tests on main with the held patches (wayland setup, headless
  compositor): `misc` in `testsuite/gsk` passes, `/renderer/gl/space-glyph`
  included; without the fix that test fails ("'scaled' should be NULL").
  `compare gl` (2850 tests), two runs with the fix and two without: 43 fail in
  all four (`glyph-hinting-none`, `dmabuf-mipmap`,
  `big-checkerboard-scaled-down2`, `fill-subpixel-offsets-*-nocairo` and
  single ones), so they fail on the GTX 1660 Ti either way. 1 to 5 more fail in
  some runs only, with and without the fix, all in the `clip` variant:
  `blur-child-bounds-oversize`, `huge-width-hdr`,
  `texture-scale-magnify-rotate`, `glyph-hinting-none` and
  `fill-subpixel-offsets-horizontal-nocairo`. `testsuite/gtk`, run in the C
  locale: 93 of 95 pass, `accessor-apis` (in the `failing` suite) fails and one
  is skipped. `misc` also passes on 4.24.1, and on Mesa llvmpipe.

## What stays

In these runs (big-host 0.1.0-111), without animations About costs 6 MiB of
Pss_Anon and 9 MiB of VRAM; with them, patched, 31 to 32 MiB and 54 to 66 MiB.
A scale animation draws every glyph, fill and stroke again at the scale of each
frame, and the GPU cache keeps them in its atlases (at 200 openings, in the
runs of [scale-animation-cache](scale-animation-cache.md): 9 or 10 atlases of
4 MiB holding 16 061 to 17 590 glyphs, 2 045 to 2 053 fills and 864 to 866
strokes). Entries go stale after `GSK_CACHE_TIMEOUT` (15 s in GTK, 60 s in the
BigLinux session) and an atlas is freed only when all its entries are stale.
cairo keeps up to 256 scaled fonts and 512 glyph pages, and Pango keeps one
`FcPattern` per scale. This patch does not change any of that;
scale-animation-cache lets the frames share entries.

## Costs and risks

None found: one reference drop on a path that returns early.

## Before sending

- The VRAM of About with the patch is higher than with 4.24.1 in the three
  pairs of the table: 63/51 and 75/55 MiB at 200 openings, 75/55 after the
  60 s idle. Connect to Server has 26 MiB in both. Nothing in the patch
  touches the atlases, and the cause is not known.
