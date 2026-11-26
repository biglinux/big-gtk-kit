#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""The sweep's verdicts on a real GTK program, one surface per verdict. Needs a
session with an accessibility bus: `make check-sweep` runs it in a private KWin
session.
"""

import json
import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import leak_sweep

STUB = Path(__file__).resolve().parent / "stub_app.py"

SURFACES = f"""
cycles = 2
warm = 1

[program]
command = [{json.dumps(sys.executable)}, {json.dumps(str(STUB))}]
atspi_name = "stub_app.py"
dbus_name = "org.biglinux.SweepStub"
window_path = "/org/biglinux/SweepStub/window/1"
settle = 1

[[surface]]
name = "opens"
open = [{{ do = "action", name = "opens", settle = 0.3 }}]

[[surface]]
name = "opens-by-accessible-action"
open = [{{ do = "app-action", name = "win.opens", settle = 0.3 }}]

[[surface]]
name = "changes-in-place"
in_place = true
open = [{{ do = "action", name = "nothing", settle = 0.3 }}]

[[surface]]
name = "never-opens"
open_timeout = 1
open = [{{ do = "action", name = "nothing", settle = 0.3 }}]

[[surface]]
name = "failing-command"
open = [{{ do = "command", argv = ["false"] }}]

[[surface]]
name = "wrong-dbus-method"
open = [{{ do = "dbus", dest = "org.biglinux.SweepStub", path = "/org/biglinux/SweepStub", interface = "org.freedesktop.DBus.Peer", method = "Missing" }}]

[[surface]]
name = "stays-open"
close_timeout = 1
open = [{{ do = "action", name = "stays", settle = 0.3 }}]
"""

EXPECTED = {
    "opens": "ok",
    "opens-by-accessible-action": "ok",
    "changes-in-place": "ok",
    "never-opens": "did not open",
    "failing-command": "error",
    "wrong-dbus-method": "error",
    "stays-open": "did not close",
}


def outcome(result: dict) -> str:
    if "error" in result:
        return "error"
    if result["did_not_open"]:
        return "did not open"
    if result["did_not_close"]:
        return "did not close"
    return "leak" if result["leaks"] else "ok"


def main() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        surfaces, report = Path(tmp, "stub.toml"), Path(tmp, "results.json")
        surfaces.write_text(SURFACES)
        os.environ["LEAK_SWEEP_DIR"] = tmp
        status = leak_sweep.main([str(surfaces), "--json", str(report)])
        if not report.exists():
            print(f"FAIL the sweep did not run (exit {status})")
            return 1
        results = {r["surface"]: r for r in json.loads(report.read_text())}
    failures = [
        f"{name}: expected {wanted}, got {outcome(results[name]) if name in results else 'nothing'}"
        for name, wanted in EXPECTED.items()
        if name not in results or outcome(results[name]) != wanted
    ]
    # Windows left open grow like a leak: listed, never judged as one.
    if "GtkWindow" not in results.get("stays-open", {}).get("unjudged", {}):
        failures.append("stays-open: the stacked windows are not listed as unjudged")
    if status != 1:
        failures.append(f"exit status {status}, expected 1")
    for failure in failures:
        print(f"FAIL {failure}")
    print("FAIL" if failures else "PASS")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
