# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""The parts of the sweep that decide, without a display."""

import contextlib
import io
import subprocess
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


# One cycle per line on stdin: `every` leaks a thread and a file descriptor each
# time, `once` only on the first; it answers when started and after each line.
FIXTURE = """
import os, sys, threading
hold = threading.Event()
print(flush=True)
for n, line in enumerate(sys.stdin):
    if line.strip() == "every" or n == 0:
        os.open(os.devnull, os.O_RDONLY)
        threading.Thread(target=hold.wait, daemon=True).start()
    print(flush=True)
"""


class ProcessCounters(unittest.TestCase):
    def rounds(self, mode: str, cycles: int = 3) -> dict[str, int]:
        child = subprocess.Popen(
            [sys.executable, "-I", "-c", FIXTURE],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(child.wait)
        self.addCleanup(child.kill)
        child.stdout.readline()  # started: its imports closed their files
        counts = [leak_sweep.process_counts(child.pid)]
        for _ in range(2):
            for _ in range(cycles):
                child.stdin.write(mode + "\n")
                child.stdin.flush()
                child.stdout.readline()
            counts.append(leak_sweep.process_counts(child.pid))
        return leak_sweep.confirmed_growth(*counts)

    def test_leak_per_cycle_is_confirmed(self):
        self.assertEqual(
            self.rounds("every"),
            {"process threads": 3, "process file descriptors": 3},
        )

    def test_growth_in_one_round_passes(self):
        self.assertEqual(self.rounds("once"), {})


PROGRAM = """
[program]
command = ["prog"]
atspi_name = "prog"
"""


class Steps(unittest.TestCase):
    def test_failing_command_is_an_error(self):
        with (
            tempfile.TemporaryDirectory() as tmp,
            self.assertRaises(subprocess.CalledProcessError),
        ):
            leak_sweep.run_step(
                {"do": "command", "argv": ["false"], "settle": 0}, {}, "", Path(tmp), 1
            )


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

    def test_malformed_tables_are_refused(self):
        for text in (
            PROGRAM + 'env = "x"\n',
            PROGRAM + 'files = "x"\n',
            'allow = ["x"]\n' + PROGRAM,
            'surface = ["x"]\n' + PROGRAM,
            PROGRAM + '[[surface]]\nname = "a"\nopen = []\nclose = "dismiss"\n',
            PROGRAM + '[[surface]]\nname = "a"\nopen = []\nallow = ["x"]\n',
        ):
            with self.subTest(text=text):
                self.refused(text)

    def test_accessible_action_step_loads(self):
        spec = leak_sweep.load_spec(
            self.write(
                PROGRAM
                + '[[surface]]\nname = "a"\n'
                + 'open = [{ do = "app-action", name = "win.open-disc" }]\n'
            )
        )
        self.assertEqual(spec["surface"][0]["open"][0]["do"], "app-action")

    def test_zero_cycles_is_refused(self):
        path = self.write(PROGRAM)
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            status = leak_sweep.main(
                [str(path), "--cycles", "0", "--library", str(path)]
            )
        self.assertEqual(status, 2)
        self.assertIn("cycles", error.getvalue())

    def test_workdir_is_the_only_placeholder(self):
        self.assertEqual(
            leak_sweep.with_workdir("{workdir}/a {} {x}", "/w"), "/w/a {} {x}"
        )


if __name__ == "__main__":
    unittest.main()
