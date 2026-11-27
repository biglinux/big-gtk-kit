#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""A GTK program for the sweep to drive: its window actions open a window that
closes, open a window without a way to close it, or open nothing."""

import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gio, Gtk


def present(parent: Gtk.Window, closable: bool):
    window = Gtk.Window(transient_for=parent, title="opened")
    if closable:
        close = Gtk.Button(label="Close")
        # The button finds its window when pressed: a handler holding the window
        # would keep it alive, a leak of the stub's own.
        close.connect("clicked", lambda button: button.get_root().destroy())
        window.set_child(close)
    window.present()


def on_activate(app: Gtk.Application):
    window = Gtk.ApplicationWindow(application=app, title="stub")
    for name, handler in (
        ("opens", lambda *_: present(window, closable=True)),
        ("stays", lambda *_: present(window, closable=False)),
        ("nothing", lambda *_: None),
    ):
        action = Gio.SimpleAction.new(name, None)
        action.connect("activate", handler)
        window.add_action(action)
    window.present()


app = Gtk.Application(application_id="br.com.biglinux.SweepStub")
app.connect("activate", on_activate)
sys.exit(app.run(sys.argv[:1]))
