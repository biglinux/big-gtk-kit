# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Run under ``tools/headless-gate.sh``: surfaces need a display."""

import gc
import time

from big_gtk_kit import surface
from gi.repository import GLib, Gtk


def pump():
    context = GLib.MainContext.default()
    for _ in range(50):
        while context.iteration(False):
            pass
        time.sleep(0.002)


def test_windows_that_showed_a_popover_release_their_surface():
    surface.install()
    freed = {"surfaces": 0}

    def bump(*_args):
        freed["surfaces"] += 1

    cycles = 25
    for _ in range(cycles):
        popover = Gtk.Popover(child=Gtk.Label(label="x"))
        button = Gtk.MenuButton(popover=popover)
        window = Gtk.Window(child=button)
        window.present()
        pump()
        window.get_surface().weak_ref(bump)
        button.popup()
        pump()
        button.popdown()
        pump()
        window.destroy()
        del popover, button, window
        gc.collect()
        pump()
    # GDK holds the last closed window's surface until another window comes.
    last = Gtk.Window()
    last.present()
    pump()
    last.destroy()
    pump()
    assert freed["surfaces"] == cycles
