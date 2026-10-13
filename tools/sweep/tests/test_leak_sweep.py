# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""The parts of the sweep that decide, without a display."""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import leak_sweep


class Census(unittest.TestCase):
    def test_types_and_textures_are_read(self):
        counts = leak_sweep.parse_census("12 GtkLabel\n3 texture 1024x800\nnoise\n")
        self.assertEqual(counts, {"GtkLabel": 12, "texture 1024x800": 3})

    def test_only_growth_is_reported(self):
        self.assertEqual(
            leak_sweep.grown({"A": 5, "B": 3}, {"A": 11, "B": 2, "C": 4}),
            {"A": 6, "C": 4},
        )


class Verdict(unittest.TestCase):
    def test_one_per_cycle_is_a_leak(self):
        leaks, tolerated = leak_sweep.verdict({"GtkLabel": 6}, 6, {})
        self.assertEqual(leaks, {"GtkLabel": 6})
        self.assertEqual(tolerated, {})

    def test_slower_growth_is_warming_up(self):
        self.assertEqual(leak_sweep.verdict({"GtkCssStaticStyle": 5}, 6, {}), ({}, {}))

    def test_allowed_type_tolerated_to_cap(self):
        allow = {"AdwBreakpoint": 3}
        self.assertEqual(
            leak_sweep.verdict({"AdwBreakpoint": 18}, 6, allow),
            ({}, {"AdwBreakpoint": 18}),
        )
        self.assertEqual(
            leak_sweep.verdict({"AdwBreakpoint": 24}, 6, allow),
            ({"AdwBreakpoint": 24}, {}),
        )


PROGRAM = """
[program]
command = ["prog"]
atspi_name = "prog"
"""


class Spec(unittest.TestCase):
    def write(self, text):
        with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as handle:
            handle.write(text)
        path = Path(handle.name)
        self.addCleanup(path.unlink)
        return path

    def refused(self, text):
        with self.assertRaises((ValueError, TypeError)):
            leak_sweep.load_spec(self.write(text))

    def test_a_complete_file_loads(self):
        spec = leak_sweep.load_spec(
            self.write(
                """
                cycles = 6
                [program]
                command = ["prog"]
                atspi_name = "prog"
                dbus_name = "org.example.Prog"
                window_path = "/org/example/Prog/window/1"
                [[surface]]
                name = "about"
                open = [{ do = "action", name = "about" }]
                """
            )
        )
        self.assertEqual(spec["surface"][0]["name"], "about")

    def test_a_program_without_a_command_is_refused(self):
        with self.assertRaises(ValueError):
            leak_sweep.load_spec(self.write('[program]\natspi_name = "x"\n'))

    def test_string_command_is_refused(self):
        self.refused('[program]\ncommand = "prog --flag"\natspi_name = "x"\n')

    def test_a_seed_outside_the_workdir_is_refused(self):
        for name in ("/etc/passwd", "../outside"):
            self.refused(PROGRAM + f'[program.files]\n"{name}" = "x"\n')

    def test_bare_allow_is_refused(self):
        self.refused(PROGRAM + '[[allow]]\ntype = "AdwBreakpoint"\nper_cycle = 3\n')
        self.refused(
            PROGRAM + '[[allow]]\ntype = "AdwBreakpoint"\nreason = "upstream"\n'
        )

    def test_incomplete_action_is_refused(self):
        self.refused(
            PROGRAM
            + '[[surface]]\nname = "a"\nopen = [{ do = "action", name = "a" }]\n'
        )

    def test_an_unknown_step_is_refused(self):
        self.refused(PROGRAM + '[[surface]]\nname = "a"\nopen = [{ do = "press" }]\n')

    def test_workdir_is_the_only_placeholder(self):
        self.assertEqual(
            leak_sweep.with_workdir("{workdir}/a {} {x}", "/w"), "/w/a {} {x}"
        )


if __name__ == "__main__":
    unittest.main()
