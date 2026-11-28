#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Check that a program builds against one big-gtk-kit revision.

    kit_revision.py --check [ROOT]

ROOT is the program's repository, the current directory when left out.

Every tracked Cargo.toml that names the kit's git source pins it by `rev` on
the same line, every tracked Cargo.lock resolves its kit crates to that same
source and revision, and every tracked fuzz workspace's Cargo.lock is current
(`cargo metadata --locked`). A moved pin that misses one manifest, or a
`cargo update -p big-gtk-kit` that leaves the other kit crates behind, fails
here instead of in a fuzz build or a package weeks later.

Exit status: 0 one revision and current lockfiles; 1 otherwise; 2 usage error.
"""

import re
import subprocess
import sys
from pathlib import Path

MANIFEST_SOURCE = re.compile(r'\bgit\s*=\s*"([^"]*big-gtk-kit)"')
MANIFEST_REV = re.compile(r'\brev\s*=\s*"([0-9a-f]{40})"')
LOCK_SOURCE = re.compile(
    r'^source = "git\+([^"?#]*big-gtk-kit)\?([^"#]*)#', re.MULTILINE
)
LOCK_REV = re.compile(r"^rev=([0-9a-f]{40})$")


def manifest_pins(text: str) -> list[tuple[str, str | None]]:
    """The kit sources a manifest names, each with its `rev`, or None without one."""
    pins = []
    for line in text.splitlines():
        if line.lstrip().startswith("#"):
            continue
        source = MANIFEST_SOURCE.search(line)
        if source:
            rev = MANIFEST_REV.search(line)
            pins.append((source.group(1), rev.group(1) if rev else None))
    return pins


def lock_pins(text: str) -> list[tuple[str, str | None]]:
    """The kit sources a lockfile resolves, each with its `rev`, or None for a tag or branch."""
    pins = []
    for source, query in LOCK_SOURCE.findall(text):
        rev = LOCK_REV.match(query)
        pins.append((source, rev.group(1) if rev else None))
    return pins


def tracked(root: Path, pattern: str) -> list[Path]:
    listed = subprocess.run(
        ["git", "ls-files", "-z", "--", pattern],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return [root / name for name in listed.split("\0") if name]


def check(root: Path) -> list[str]:
    problems = []
    pins: dict[tuple[str, str], list[str]] = {}
    files = {"manifests": 0, "lockfiles": 0}
    for kind, pattern, read in (
        ("manifests", "*Cargo.toml", manifest_pins),
        ("lockfiles", "*Cargo.lock", lock_pins),
    ):
        for path in tracked(root, pattern):
            found = read(path.read_text(encoding="utf-8"))
            if found:
                files[kind] += 1
            for source, rev in found:
                name = str(path.relative_to(root))
                if rev is None:
                    problems.append(f"{name}: the kit is not pinned by a 40-hex rev")
                else:
                    pins.setdefault((source, rev), []).append(name)
    if len(pins) > 1:
        for (source, rev), names in sorted(pins.items()):
            problems.append(f"{source} {rev[:7]}: {', '.join(sorted(set(names)))}")
    fuzz = tracked(root, "*fuzz/Cargo.toml")
    for manifest in fuzz:
        resolved = subprocess.run(
            [
                "cargo",
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--manifest-path",
                manifest,
            ],
            cwd=root,
            check=False,
            capture_output=True,
            text=True,
        )
        if resolved.returncode != 0:
            said = next(
                (
                    line
                    for line in resolved.stderr.splitlines()
                    if line.startswith("error")
                ),
                f"cargo metadata exited {resolved.returncode}",
            )
            problems.append(
                f"{manifest.relative_to(root)}: Cargo.lock is not current: {said}"
            )
    print(
        f"kit revision: {files['manifests']} manifests, {files['lockfiles']} lockfiles, "
        f"{len(pins)} distinct pins, {len(fuzz)} fuzz workspaces resolved"
    )
    return problems


def main(argv: list[str]) -> int:
    if argv[:1] != ["--check"] or len(argv) > 2:
        print("usage: kit_revision.py --check [ROOT]", file=sys.stderr)
        return 2
    problems = check(Path(argv[1] if len(argv) == 2 else ".").resolve())
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
