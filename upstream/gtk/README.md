# GTK fixes held for upstream

Fixes to GTK itself, found while building the BigLinux desktop. They are kept
here until it is decided which to send upstream; none has been sent. Each has a
note with the cause, the change and how it was tested, with a patch where there
is one; the patches apply on the GTK release BigLinux ships and on GTK main.

| fix | effect | state |
|---|---|---|
| [icon-cache-lookup](icon-cache-lookup.md) | a GTK 4 start with a large icon theme takes half the time (papient: 112 → 56 ms, 346 → 165 ms on an Intel HD 3000) | ready, test included |
| [image-missing-recursion](image-missing-recursion.md) | a missing icon with no `image-missing` in any theme falls back to GTK's own picture instead of overflowing the stack | ready, test included |
| [fonts-at-startup](fonts-at-startup.md) | where the font cost of a start goes; cuts for Pango and fontconfig | finding only, no patch |

`package/` builds Arch's `gtk4` 4.24.1 with both patches for testing (same
split packages and file lists as Arch's). The `.patch` links point at the
patches above.

```sh
cd package && makepkg -s
```

To compare with Arch's own `1:4.24.1-1`, build with the compiler flags it was
built with instead of the host's:

```sh
cd package && makepkg --config makepkg-arch-flags.conf -s
```

The patches are under GTK's license, LGPL-2.1-or-later. `package/` is derived
from Arch's packaging and is under its license, 0BSD.
