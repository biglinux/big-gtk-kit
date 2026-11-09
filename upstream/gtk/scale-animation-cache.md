# GPU renderer: cache entries for every frame of a scale animation

A GTK 4.24.1 program that draws with the GPU renderer caches glyphs, fills and
strokes at the exact scale they are drawn at. libadwaita's dialogs scale in, so
each frame of the opening draws at a new scale and adds a full set of entries.
With the scaled font leak fixed ([glyph-scaled-font-leak](glyph-scaled-font-leak.md)),
in the runs below (big-host 0.1.0-112), 200 openings of BigImages' About dialog
still cost 25 to 27 MiB of Pss_Anon and 43 to 46 MiB of VRAM over the program at
rest, against 6 and 9 MiB without animations in the runs of
glyph-scaled-font-leak (big-host 0.1.0-111); the cache held 16 061 to 17 590
glyphs, 2 045 to 2 053 fills and 864 to 866 strokes in 9 or 10 atlases of
1024×1024 pixels.

`gsk_gpu_node_processor_add_glyph_node()` (`gsk/gpu/gskgpunodeprocessor.c`)
looks glyphs up at `MAX (scale.width, scale.height)`, and
`determine_scale_and_subpixel_grid()` (`gsk/gpu/gskgpucachedfill.c`,
`gskgpucachedstroke.c`) renders masks at the exact scale on a 1/32 pixel grid.
Only under rotations and other transforms that are not a scale do they round
the scale, up to the next power of two.

`scale-animation-cache.patch` (5 files, +157 −12) applies on 4.24.1 and on main
b9cd9290d5, alone or after the other held patches. After
glyph-scaled-font-leak it needs a three-way apply (`git apply -3` or
`git am -3`, as the package uses), because both add the same include to
`testsuite/gsk/misc.c`.

## Change

Output scales are multiples of 1/120 (`wp_fractional_scale_v1`), as are the
usual static scales: 0.75, 0.9, 1.1, 1.25 and every other multiple of 0.05 or
1/8.

- A scale on a multiple of 1/120 (within 1/1000 of a step) is drawn as before.
- Between two multiples, glyphs are rasterized at the nearest one and drawn
  pixel for pixel: no resampling, so text stays as sharp as at rest. A glyph
  differs in size from the exact one by at most 1/240 of its size at scale 1,
  0.05 pixels for a 12 pixel glyph, at any scale.
- Between two multiples, fills and strokes are rendered at the next multiple up
  on a quarter pixel grid instead of a 1/32 one and resampled down, as the
  renderer already does under rotations. The frames of one animation, and the
  openings after it, then share entries.
- `testsuite/gsk/misc.c` gains `/renderer/gl/scaled-text`: "Abc" in the test
  suite's Cantarell, hinted as in the compare tests, renders the same pixels
  under a 1.003 scale as without it. It fails without the change. It opens a
  second display, as `/renderer/gl` does, because with Mesa a GL renderer on
  the default display aborts once another display that used GL was closed, and
  it loads the font file through fontconfig and FreeType. So it runs on Linux
  only (Wayland or X11) and would fail on macOS and Windows, where GDK opens one
  display; BigLinux's programs are Linux-only.

Which scales count as at rest is decided by the scale alone, not by watching it
change: the renderer draws only on damage, so the last frame of an animation
that ends at a scale between multiples would stay in a rounded rendering.

## Measurements

BigImages and BigFiles (big-host 0.1.0-112) in a headless big-compositor
session (1280x720, scale 1, GL on an NVIDIA GTX 1660 Ti), isolated HOME, GTK
4.24.1 with the three held patches without (before) and with this one (after),
loaded through `LD_LIBRARY_PATH` and checked in `/proc/PID/maps`.
`GSK_CACHE_TIMEOUT=60` as in the BigLinux session, `GSK_DEBUG=cache` and
`GDK_DEBUG=frames`. Memory from `smaps_rollup` in KiB, VRAM from `nvidia-smi`
in MiB, the cache at the last periodic collection.

| journey | Pss_Anon | VRAM | glyphs | fills | strokes | atlases |
|---|---|---|---|---|---|---|
| About, at rest | 33 084 – 33 144 | 9 | | | | |
| About, 200 openings, before (2 runs) | 61 168, 58 848 | 55, 52 | 17 590, 16 061 | 2 053, 2 045 | 866, 864 | 10, 9 |
| About, 200 openings, after | 50 092 | 27 | 2 782 | 867 | 336 | 3 |
| Connect to Server, at rest | 47 280 – 47 632 | 21 | | | | |
| Connect to Server, 100 openings, before | 64 732 | 26 | 22 653 | 1 673 | | 3 |
| Connect to Server, 100 openings, after | 59 044 | 19 | 4 183 | 745 | | 1 |

- About after 200 openings costs 17 MiB of Pss_Anon and 18 MiB of VRAM over
  rest instead of 25 to 27 and 43 to 46; VRAM stops growing after 12 openings
  (27 MiB at 12, 50, 100 and 200). Connect to Server: 11 MiB of Pss_Anon
  instead of 17, and VRAM back under its value at rest.
- Frames are unchanged: 16.7 ms median and 33.3 ms 95th percentile between
  animation frames in both, 4 971 and 4 901 / 4 948 frames over 200 openings;
  7.3 to 8.5 ms of CPU per frame after, 6.7 to 12.4 before (load 3 to 7 from
  other builds on the host).
- Rounding the scale alone, with masks still on the 1/32 grid, left 1 568 fills,
  631 strokes and 7 atlases at 200 openings (53 264 KiB, 43 MiB): the quarter
  pixel grid is what lets masks be shared.

## Same results

The same widget (a label and four symbolic icons) rendered by the GL renderer
under static scales:

- 0.75, 0.9, 1.0, 1.1, 1.25, 1.5 and 2.0: byte-identical with and without the
  change.
- 0.8333, 0.93, 1.01, 1.037 and 1.2345: 276 to 1 526 pixels differ, by up to
  68/255 over white, in the text (its glyphs come from the nearest step) and at
  the icons' edges (the quarter pixel grid). The text is as sharp: at 0.93 it
  has 794 fully inked and 1 082 partly covered pixels against 799 and 1 091
  before, at 1.037 1 058 and 1 185 against 1 042 and 1 190.
- Screenshots: BigImages at rest and with About open differs only in the large
  symbolic icon of its empty window (by up to 51/255 at its edges), drawn at a
  scale between multiples; the dialog's text and icons are identical. BigFiles
  with Connect to Server open differs in 107 pixels by at most 4/255.
- About caught 0.15 and 0.25 s into its opening, with and without the change:
  text and icons look the same in the scaled dialog, sharp in both.

## Tests

`/renderer/gl/scaled-text` passes with the change and fails without it
("assertion failed (plain_data == scaled_data)") on 4.24.1 and on main, with
this patch alone and with the other held patches, on the GTX 1660 Ti and on
Mesa llvmpipe (wayland setup, headless compositor). The rest of `misc` in
`testsuite/gsk` passes in all of these.

The GL compare tests on text, glyphs, fills, strokes, scales and transforms
(170: plain and flipped): 162 pass and the same 8 fail with and without the
change (`glyph-hinting-none`, `big-checkerboard-scaled-down2`,
`fill-subpixel-offsets-*-nocairo`), as they do on the GTX 1660 Ti with the
other held patches.

## Costs and risks

- Fills and strokes drawn at rest at a scale between multiples of 1/120 are
  rendered at a scale up to 1/120 higher, resampled down and placed on a
  quarter pixel grid, so their edges move by up to 1/8 pixel.
- Glyphs under such a scale are at most 0.05 pixels (for 12 pixels) off their
  exact size.
- A mask is snapped to the pixel grid of the scale that first cached it, as
  under rotations, so a shared mask depends on which frame made it: BigImages'
  empty-window icon differed by up to 5/255 between two runs with the change,
  and not at all between two runs without it.
