# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""What kit_revision.py reads as a pin, from manifests and lockfiles Cargo writes."""

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "kit_revision", Path(__file__).resolve().parent.parent / "kit_revision.py"
)
kit_revision = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(kit_revision)

KIT = "https://github.com/biglinux/big-gtk-kit"
REV = "a" * 40


class ManifestPins(unittest.TestCase):
    def test_table(self):
        for manifest, expected in [
            (f'[dependencies]\nkit = {{ git = "{KIT}", rev = "{REV}" }}', [(KIT, REV)]),
            (f'[dependencies.kit]\ngit = "{KIT}.git"\nrev = "{REV}"', [(KIT, REV)]),
            (
                f'[workspace.dependencies]\nkit = {{ git = "{KIT}" }} # rev = "{REV}"',
                [(KIT, None)],
            ),
            (
                f'[patch.crates-io]\nglycin = {{ git = "{KIT}", branch = "main" }}',
                [(KIT, None)],
            ),
            (f'[dependencies]\nkit = {{ git = "{KIT}", rev = "abc" }}', [(KIT, None)]),
            (
                '[dependencies]\nother = { git = "https://github.com/x/big-gtk-kit-fork" }',
                [],
            ),
            ("[package]\nname = 'x'", []),
        ]:
            with self.subTest(manifest=manifest):
                self.assertEqual(kit_revision.manifest_pins(manifest), expected)


class LockPins(unittest.TestCase):
    def test_table(self):
        for source, expected in [
            (f"git+{KIT}?rev={REV}#{REV[:8]}", [(KIT, REV)]),
            (f"git+{KIT}.git?rev={REV}#{REV[:8]}", [(KIT, REV)]),
            (f"git+{KIT}#{REV}", [(KIT, None)]),
            (f"git+{KIT}?branch=main#{REV}", [(KIT, None)]),
            ("registry+https://github.com/rust-lang/crates.io-index", []),
        ]:
            lock = f'version = 4\n\n[[package]]\nname = "kit"\nversion = "0.1.0"\nsource = "{source}"\n'
            with self.subTest(source=source):
                self.assertEqual(kit_revision.lock_pins(lock), expected)


if __name__ == "__main__":
    unittest.main()
