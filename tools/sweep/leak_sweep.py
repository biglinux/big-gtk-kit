#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Open and close each surface of a GTK4 program many times, and report every type that grew.

    leak_sweep.py SURFACES.toml [--only NAME...] [--quick] [--cycles N] [--json OUT]
                  [--library PATH]

Run it inside a graphical session with an accessibility bus, such as
`tools/headless-gate.sh --accessible python3 tools/sweep/leak_sweep.py ...`.
It starts the program itself, preloaded with libgobject-census, and asks for a
census by signal, so `command` must exec the GTK program directly: a wrapper
script would receive the signal instead. For each surface: warm-up cycles,
census A, N cycles, census B. A type that gained at least one instance per cycle
is a leak, unless the surfaces file allows it with a per-cycle cap and a reason.
The process's threads and open file descriptors are counted with each census;
they are exact, so any growth that a second round of cycles repeats is a leak.

A surface whose open steps bring up no new window or dialog, or that does not
really close, is reported as such, not as clean or as a leak: windows stacking
up look exactly like one. A surface that changes in place says so with
`in_place = true`.

Exit status: 0 clean; 1 leaks, surfaces that did not open or close, or surfaces
whose steps failed; 2 setup error.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_LIBRARY = HERE.parent / "build" / "libgobject-census.so"
DISMISS_NAMES = ("Cancelar", "Fechar", "Cancel", "Close")


def parse_census(text: str) -> dict[str, int]:
    """`count TypeName` and `count texture WxH` lines into one mapping."""
    counts: dict[str, int] = {}
    for line in text.splitlines():
        parts = line.split(maxsplit=1)
        if len(parts) == 2 and parts[0].isdigit():
            counts[parts[1].strip()] = int(parts[0])
    return counts


def grown(before: dict[str, int], after: dict[str, int]) -> dict[str, int]:
    return {
        name: after.get(name, 0) - before.get(name, 0)
        for name in set(before) | set(after)
        if after.get(name, 0) > before.get(name, 0)
    }


def verdict(
    growth: dict[str, int], cycles: int, allow: dict[str, float]
) -> tuple[dict, dict]:
    """(leaks, tolerated): a type that gained >= one per cycle leaks unless allowed."""
    leaks, tolerated = {}, {}
    for name, amount in sorted(growth.items(), key=lambda item: -item[1]):
        if amount < cycles:
            continue
        cap = allow.get(name)
        if cap is not None and amount <= cap * cycles:
            tolerated[name] = amount
        else:
            leaks[name] = amount
    return leaks, tolerated


def process_counts(pid: int) -> dict[str, int]:
    """The threads and open file descriptors of `pid`: exact counts, unlike a census."""
    try:
        status = Path(f"/proc/{pid}/status").read_text()
        fds = len(os.listdir(f"/proc/{pid}/fd"))
    except OSError as error:
        raise RuntimeError(f"cannot read /proc/{pid}: {error}") from error
    threads = next(
        int(line.split()[1])
        for line in status.splitlines()
        if line.startswith("Threads:")
    )
    return {"process threads": threads, "process file descriptors": fds}


def confirmed_growth(
    first: dict[str, int], second: dict[str, int], third: dict[str, int]
) -> dict[str, int]:
    """Exact counters that grew over one round and again over the next, by the
    first round's growth: a pool filling up grows once, a leak every time."""
    again = grown(second, third)
    return {name: n for name, n in grown(first, second).items() if name in again}


def with_workdir(text: str, workdir) -> str:
    """`{workdir}` replaced, and nothing else: other braces are the program's own."""
    return text.replace("{workdir}", str(workdir))


class Program:
    def __init__(self, spec: dict, library: Path, workdir: Path):
        self.spec = spec
        self.workdir = workdir
        self.prefix = workdir / "census"
        env = dict(os.environ)
        # `{workdir}` in the file stands for this run's scratch directory, so a
        # program can be given a throwaway configuration instead of the real one.
        env.update(
            {k: with_workdir(v, workdir) for k, v in spec.get("env", {}).items()}
        )
        for relative, content in spec.get("files", {}).items():
            target = workdir / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content)
        preload = env.get("LD_PRELOAD", "")
        env["LD_PRELOAD"] = f"{library}:{preload}" if preload else str(library)
        env["GOBJECT_CENSUS_OUT"] = str(self.prefix)
        # The log lives as long as the program.
        self.log = open(workdir / "program.log", "w")  # noqa: SIM115
        self.process = subprocess.Popen(
            [with_workdir(arg, workdir) for arg in spec["command"]],
            env=env,
            stdout=self.log,
            stderr=subprocess.STDOUT,
        )
        self.serial = 0

    def census(self, timeout: float = 30) -> dict[str, int]:
        self.serial += 1
        path = Path(f"{self.prefix}.{self.process.pid}.{self.serial}")
        os.kill(self.process.pid, signal.SIGUSR2)
        deadline = time.monotonic() + timeout
        while not path.exists():
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"the program exited ({self.process.returncode}) before a census"
                )
            if time.monotonic() > deadline:
                raise RuntimeError(
                    f"no census in {timeout} s: is the library loaded? ({path})"
                )
            time.sleep(0.1)
        return parse_census(path.read_text())

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(10)
            except subprocess.TimeoutExpired:
                self.process.kill()
        self.log.close()


def atspi():
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi

    return Atspi


def application(name: str):
    Atspi = atspi()
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        child = desktop.get_child_at_index(i)
        if child is not None and child.get_name() == name:
            return child
    return None


def walk(node, depth=0):
    if node is None or depth > 60:
        return
    yield node
    for i in range(node.get_child_count()):
        yield from walk(node.get_child_at_index(i), depth + 1)


def open_surfaces(app) -> int:
    """Top-level frames plus dialogs: what a closed surface must bring back down."""
    if app is None:
        return 0
    frames = sum(
        1
        for i in range(app.get_child_count())
        if (child := app.get_child_at_index(i)) is not None
        and child.get_role_name() == "frame"
    )
    dialogs = sum(
        1 for node in walk(app) if node.get_role_name() in ("dialog", "alert")
    )
    return frames + dialogs


def press(node) -> bool:
    action = node.get_action_iface()
    if action is None or node.get_n_actions() == 0:
        return False
    return bool(action.do_action(0))


def activate_accessible_action(app, name: str) -> bool:
    """Run the first accessible action called `name`; False when none is."""
    for node in walk(app):
        action = node.get_action_iface()
        if action is None:
            continue
        for index in range(action.get_n_actions()):
            if action.get_action_name(index) == name:
                if not action.do_action(index):
                    raise RuntimeError(f"the program refused the action {name!r}")
                return True
    return False


def find(app, name: str, roles=(), prefix=False):
    for node in walk(app):
        text = node.get_name() or ""
        if (text.startswith(name) if prefix else text == name) and (
            not roles or node.get_role_name() in roles
        ):
            return node
    return None


def dismiss(app, first_frame: int = 1) -> int:
    """Press Cancel/Close in every dialog and in every frame from `first_frame` on."""
    pressed = 0
    containers = [
        app.get_child_at_index(i) for i in range(first_frame, app.get_child_count())
    ]
    containers += [n for n in walk(app) if n.get_role_name() in ("dialog", "alert")]
    for container in containers:
        if container is None:
            continue
        for node in walk(container):
            if (
                node.get_role_name() in ("button", "push button")
                and node.get_name() in DISMISS_NAMES
            ):
                pressed += press(node)
                break
    return pressed


def run_step(
    step: dict, program_spec: dict, app_name: str, workdir: Path, frames_at_open: int
):
    """One step; `frames_at_open` is how many top-level windows the program had
    when the cycle began."""
    kind = step["do"]
    if kind == "action":
        target = step.get("path", program_spec.get("window_path"))
        subprocess.run(
            [
                "gdbus",
                "call",
                "--session",
                "--dest",
                program_spec["dbus_name"],
                "--object-path",
                target,
                "--method",
                "org.gtk.Actions.Activate",
                step["name"],
                "[]",
                "{}",
            ],
            check=True,
            capture_output=True,
        )
    elif kind == "dbus":
        subprocess.run(
            [
                "dbus-send",
                "--session",
                # Waits for the reply, so a missing method or object is an error.
                "--print-reply",
                f"--dest={step['dest']}",
                step["path"],
                f"{step['interface']}.{step['method']}",
            ],
            check=True,
            capture_output=True,
        )
    elif kind == "command":
        argv = [with_workdir(arg, workdir) for arg in step["argv"]]
        subprocess.run(argv, check=True, capture_output=True, timeout=30)
    elif kind == "app-action":
        # GTK lists a window's actions (`win.…`) among its frame's accessible
        # actions: the way in for a program that exports none on D-Bus.
        if not activate_accessible_action(application(app_name), step["name"]):
            raise RuntimeError(f"no accessible action named {step['name']!r}")
    elif kind in ("toggle", "click"):
        app = application(app_name)
        roles = ("toggle button",) if kind == "toggle" else ()
        node = find(
            app, step["name"], roles=roles, prefix=step.get("prefix", kind == "toggle")
        )
        if node is None or not press(node):
            raise RuntimeError(f"nothing to {kind} named {step['name']!r}")
    elif kind == "dismiss":
        app = application(app_name)
        if app is not None:
            dismiss(app)
    elif kind == "close-new":
        # Only what this cycle opened: in a program whose first windows are not
        # the main one (a desktop shell hosting apps), "every frame after the
        # first" would close the desktop itself.
        app = application(app_name)
        if app is not None:
            dismiss(app, first_frame=frames_at_open)
    elif kind == "wait":
        pass
    else:
        raise ValueError(f"unknown step {kind!r}")
    time.sleep(float(step.get("settle", 1.0)))


def reaches(app_name: str, wanted, timeout: float) -> bool:
    """Whether the count of open surfaces satisfies `wanted` within `timeout` s."""
    deadline = time.monotonic() + timeout
    while not wanted(open_surfaces(application(app_name))):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.2)
    return True


def cycle(surface: dict, program_spec: dict, app_name: str, workdir: Path) -> str:
    """Open and close once: "ok", "did not open" or "did not close"."""
    app = application(app_name)
    frames = app.get_child_count() if app is not None else 1
    before = open_surfaces(app)
    for step in surface["open"]:
        run_step(step, program_spec, app_name, workdir, frames)
    opened = surface.get("in_place", False) or reaches(
        app_name, lambda count: count > before, float(surface.get("open_timeout", 5))
    )
    for step in surface.get("close", [{"do": "dismiss"}]):
        run_step(step, program_spec, app_name, workdir, frames)
    if not reaches(
        app_name, lambda count: count <= before, float(surface.get("close_timeout", 5))
    ):
        return "did not close"
    return "ok" if opened else "did not open"


def sweep_surface(
    program: Program, spec: dict, surface: dict, cycles: int, allow: dict
) -> dict:
    app_name = spec["atspi_name"]
    warm = int(surface.get("warm", spec.get("warm", 2)))
    outcomes = collections.Counter()
    for _ in range(warm):
        outcomes[cycle(surface, spec, app_name, program.workdir)] += 1
    time.sleep(1)
    pid = program.process.pid
    before, process_before = program.census(), process_counts(pid)
    for _ in range(cycles):
        outcomes[cycle(surface, spec, app_name, program.workdir)] += 1
    time.sleep(float(surface.get("after", 1.5)))
    after, process_after = program.census(), process_counts(pid)
    local_allow = {
        **allow,
        **{a["type"]: float(a["per_cycle"]) for a in surface.get("allow", [])},
    }
    leaks, tolerated = verdict(grown(before, after), cycles, local_allow)
    if outcomes["ok"] == warm + cycles and (
        leaks or grown(process_before, process_after)
    ):
        # Confirm: a leak grows again over a second round; a banner that happened to
        # appear, or a cache filling once, does not.
        for _ in range(cycles):
            outcomes[cycle(surface, spec, app_name, program.workdir)] += 1
        time.sleep(float(surface.get("after", 1.5)))
        again, _ = verdict(grown(after, program.census()), cycles, local_allow)
        leaks = {name: amount for name, amount in leaks.items() if name in again}
        leaks |= confirmed_growth(process_before, process_after, process_counts(pid))
    unjudged = {}
    if outcomes["did not open"] or outcomes["did not close"]:
        # Windows left open grow like a leak, and a surface never shown says
        # nothing about one: the growth is listed, not judged.
        unjudged, leaks = leaks, {}
    return {
        "surface": surface["name"],
        "cycles": cycles,
        "did_not_open": outcomes["did not open"],
        "did_not_close": outcomes["did not close"],
        "leaks": leaks,
        "unjudged": unjudged,
        "tolerated": tolerated,
    }


def strings(value) -> bool:
    return isinstance(value, list) and all(isinstance(v, str) for v in value)


def tables(value) -> bool:
    return isinstance(value, list) and all(isinstance(v, dict) for v in value)


def check_allow(entries, where: str):
    if not tables(entries):
        raise TypeError(f"{where}: every [[allow]] must be a table")
    for entry in entries:
        if not (
            isinstance(entry.get("type"), str)
            and isinstance(entry.get("per_cycle"), int | float)
            and str(entry.get("reason", "")).strip()
        ):
            raise ValueError(
                f"{where}: every [[allow]] needs `type`, `per_cycle` and `reason`"
            )


STEP_KEYS = {
    "action": ("name",),
    "app-action": ("name",),
    "dbus": ("dest", "path", "interface", "method"),
    "toggle": ("name",),
    "click": ("name",),
    "dismiss": (),
    "close-new": (),
    "wait": (),
}


def check_step(step, where: str, program: dict):
    """Refuse a step of a surface `where` names that `program` cannot run."""
    if not isinstance(step, dict) or not isinstance(step.get("settle", 0), int | float):
        raise TypeError(f"{where}: a step is a table whose `settle` is a number")
    kind = step.get("do")
    if kind == "command":
        if not step.get("argv") or not strings(step["argv"]):
            raise ValueError(
                f"{where}: a `command` step needs `argv`, a list of strings"
            )
        return
    if kind not in STEP_KEYS:
        raise ValueError(f"{where}: unknown step {kind!r}")
    missing = [key for key in STEP_KEYS[kind] if key not in step]
    if kind == "action":
        missing += [] if "dbus_name" in program else ["[program] dbus_name"]
        if "path" not in step and "window_path" not in program:
            missing.append("`path` or [program] window_path")
    if missing:
        raise ValueError(f"{where}: a {kind!r} step needs {', '.join(missing)}")


def load_spec(path: Path) -> dict:
    """The surfaces file, refused (ValueError, TypeError) before anything starts."""
    spec = tomllib.loads(path.read_text())
    program = spec.get("program", {})
    if not isinstance(program, dict):
        raise TypeError(f"{path}: [program] must be a table")
    if not program.get("command") or not strings(program["command"]):
        raise ValueError(f"{path}: [program] needs `command`, a list of strings")
    if not isinstance(program.get("atspi_name"), str):
        raise TypeError(f"{path}: [program] needs `atspi_name`")
    env, files = program.get("env", {}), program.get("files", {})
    if not isinstance(env, dict) or not isinstance(files, dict):
        raise TypeError(f"{path}: [program.env] and [program.files] must be tables")
    if not all(isinstance(v, str) for v in env.values()):
        raise ValueError(f"{path}: [program.env] values must be strings")
    for relative, content in files.items():
        # Seeds go inside the scratch directory, never over a real file.
        if Path(relative).is_absolute() or ".." in Path(relative).parts:
            raise ValueError(
                f"{path}: [program.files] {relative!r} must stay inside {{workdir}}"
            )
        if not isinstance(content, str):
            raise TypeError(f"{path}: [program.files] {relative!r} must be a string")
    check_allow(spec.get("allow", []), str(path))
    if not tables(spec.get("surface", [])):
        raise TypeError(f"{path}: every [[surface]] must be a table")
    for surface in spec.get("surface", []):
        if not isinstance(surface.get("name"), str) or not isinstance(
            surface.get("open"), list
        ):
            raise TypeError(f"{path}: every [[surface]] needs `name` and `open`")
        where = f"{path}: surface {surface['name']!r}"
        if not isinstance(surface.get("close", []), list):
            raise TypeError(f"{where}: `close` must be a list of steps")
        check_allow(surface.get("allow", []), where)
        for step in surface["open"] + surface.get("close", []):
            check_step(step, where, program)
    return spec


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("surfaces", type=Path)
    parser.add_argument("--only", nargs="*", help="surface names to run")
    parser.add_argument(
        "--quick", action="store_true", help="only the surfaces marked `quick = true`"
    )
    parser.add_argument("--cycles", type=int, help="override the file's cycle count")
    parser.add_argument("--json", type=Path, help="also write the results here")
    parser.add_argument("--library", type=Path, default=DEFAULT_LIBRARY)
    options = parser.parse_args(argv)

    try:
        spec = load_spec(options.surfaces)
    except (OSError, ValueError, TypeError, tomllib.TOMLDecodeError) as error:
        print(f"leak_sweep: {error}", file=sys.stderr)
        return 2
    if not options.library.exists():
        print(f"leak_sweep: {options.library} not built (run `make`)", file=sys.stderr)
        return 2
    program_spec = spec["program"]
    cycles = options.cycles if options.cycles is not None else spec.get("cycles", 6)
    if not isinstance(cycles, int) or cycles < 1:
        print("leak_sweep: cycles must be a positive integer", file=sys.stderr)
        return 2
    allow = {a["type"]: float(a["per_cycle"]) for a in spec.get("allow", [])}
    surfaces = [
        s
        for s in spec.get("surface", [])
        if (not options.only or s["name"] in options.only)
        and (not options.quick or s.get("quick"))
    ]
    if not surfaces:
        print("leak_sweep: no surfaces selected", file=sys.stderr)
        return 2

    workdir = Path(
        tempfile.mkdtemp(prefix="leak-sweep-", dir=os.environ.get("LEAK_SWEEP_DIR"))
    )
    try:
        program = Program(program_spec, options.library, workdir)
    except OSError as error:
        print(f"leak_sweep: could not start the program: {error}", file=sys.stderr)
        return 2
    results = []
    try:
        deadline = time.monotonic() + float(program_spec.get("start_timeout", 30))
        while application(program_spec["atspi_name"]) is None:
            if program.process.poll() is not None or time.monotonic() > deadline:
                print(
                    f"leak_sweep: {program_spec['atspi_name']} never appeared on the accessibility bus",
                    file=sys.stderr,
                )
                return 2
            time.sleep(0.5)
        time.sleep(float(program_spec.get("settle", 3)))
        for surface in surfaces:
            try:
                result = sweep_surface(program, program_spec, surface, cycles, allow)
            except (
                RuntimeError,
                subprocess.CalledProcessError,
                subprocess.TimeoutExpired,
            ) as error:
                detail = getattr(error, "stderr", None)
                if isinstance(detail, bytes):
                    detail = detail.decode(errors="replace")
                message = f"{error}: {detail.strip()}" if detail else str(error)
                result = {"surface": surface["name"], "error": message}
            results.append(result)
            print_result(result)
    except FileNotFoundError as error:
        # gdbus, dbus-send or a step's command is missing: nothing can be measured.
        print(f"leak_sweep: {error}", file=sys.stderr)
        return 2
    finally:
        program.stop()
    if options.json:
        options.json.write_text(json.dumps(results, indent=2))
    bad = [
        r
        for r in results
        if r.get("error")
        or r.get("leaks")
        or r.get("did_not_open")
        or r.get("did_not_close")
    ]
    print(
        f"leak_sweep: {len(results)} surfaces, {len(bad)} with problems (logs in {workdir})"
    )
    return 1 if bad else 0


def print_result(result: dict):
    name = result["surface"]
    if "error" in result:
        print(f"ERROR {name}: {result['error']}")
        return
    if result["did_not_open"]:
        state = "NOT OPENED "
    elif result["did_not_close"]:
        state = "NOT CLOSED "
    elif result["leaks"]:
        state = "LEAK "
    else:
        state = "ok   "
    extra = "".join(
        f", {what} {result[key]}x"
        for key, what in (
            ("did_not_open", "did not open"),
            ("did_not_close", "did not close"),
        )
        if result[key]
    )
    print(f"{state}{name} ({result['cycles']} cycles{extra})")
    for type_name, amount in result["leaks"].items():
        print(f"        +{amount:<5} {type_name}")
    for type_name, amount in result["unjudged"].items():
        print(f"        +{amount:<5} {type_name}  (not judged)")
    for type_name, amount in result["tolerated"].items():
        print(f"        +{amount:<5} {type_name}  (allowed)")


if __name__ == "__main__":
    sys.exit(main())
