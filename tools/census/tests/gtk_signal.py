#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""The census through its signal, in a real GTK 4 program: 200 labels shown, then
destroyed. Needs a display: `make check-gtk` runs it preloaded in a private
KWin session.
"""

import gc
import os
import signal
import sys
import time

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk

out = sys.argv[1]


def census(serial):
    os.kill(os.getpid(), signal.SIGUSR2)
    path = f"{out}/c.{os.getpid()}.{serial}"
    deadline = time.monotonic() + 10
    while not os.path.exists(path) and time.monotonic() < deadline:
        GLib.MainContext.default().iteration(False)
    counts = {}
    with open(path) as handle:
        for line in handle:
            if line.startswith("#"):
                continue
            n, name = line.split(maxsplit=1)
            counts[name.strip()] = int(n)
    return counts.get("GtkLabel", 0)


def pump():
    for _ in range(200):
        GLib.MainContext.default().iteration(False)


if not Gtk.init_check():
    print("SKIP: no display")
    sys.exit(77)
window = Gtk.Window()
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
window.set_child(box)
window.present()
pump()
base = census(1)
for i in range(200):
    box.append(Gtk.Label(label=f"label {i}"))
pump()
shown = census(2)
window.destroy()
del box, window
gc.collect()
pump()
after = census(3)
print(f"base={base} shown={shown} after={after}")
ok = shown - base == 200 and after <= base
print("PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
