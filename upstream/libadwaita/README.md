# libadwaita fixes held for upstream

Fixes to libadwaita itself, found while building the BigLinux desktop. They are
kept here until it is decided which to send upstream; none has been sent. Each
has a note with the cause, the change and how it was tested, and a patch that
applies on the libadwaita release BigLinux ships and on libadwaita main.

| fix | effect | state |
|---|---|---|
| [translation-domain](translation-domain.md) | the dialog close button, the toast announcement and the other strings of 8 files (28 in all) follow libadwaita's translation instead of staying in English | ready, no test possible in libadwaita's suite |

`package/` builds Arch's `libadwaita` 1.10.0 with the patch for testing (same
split packages and file lists as Arch's). The `.patch` link points at the patch
above.

```sh
cd package && makepkg -s
```

The patch is under libadwaita's license, LGPL-2.1-or-later. `package/` is
derived from Arch's packaging and is under its license, 0BSD.
