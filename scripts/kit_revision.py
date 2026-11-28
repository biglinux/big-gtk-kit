#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Check that a program builds against one big-gtk-kit revision.

    kit_revision.py --check [ROOT]

ROOT is the program's repository, the current directory when left out.

Every tracked Cargo.toml that names the kit's git source pins it by a `rev`,
every tracked Cargo.lock resolves its kit crates to that same source and
revision, and every tracked fuzz workspace's Cargo.lock is current
(`cargo metadata --locked`). A moved pin that misses one manifest, or a
`cargo update -p big-gtk-kit` that leaves the other kit crates behind, fails
here instead of in a fuzz build or a package.

Exit status: 0 one revision and current lockfiles; 1 otherwise; 2 usage error.
"""

import re
import subprocess
import sys
import tomllib
from pathlib import Path

REV = re.compile(r"[0-9a-f]{40}")
# `cargo metadata` may fetch the kit's git source before it answers.
METADATA_TIMEOUT_S = 300


def kit_source(url: str) -> str | None:
    """`url` without a `.git` suffix when it names the kit's repository, else None."""
    url = url.removesuffix("/").removesuffix(".git")
    return url if url.endswith("/big-gtk-kit") else None


def manifest_pins(text: str) -> list[tuple[str, str | None]]:
    """The kit sources a manifest names, each with its `rev`, or None without one."""
    pins = []

    def walk(node: object) -> None:
        if isinstance(node, dict):
            git = node.get("git")
            source = kit_source(git) if isinstance(git, str) else None
            if source:
                rev = node.get("rev")
                pins.append(
                    (
                        source,
                        rev if isinstance(rev, str) and REV.fullmatch(rev) else None,
                    )
                )
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for value in node:
                walk(value)

    walk(tomllib.loads(text))
    return pins


def lock_pins(text: str) -> list[tuple[str, str | None]]:
    """The kit sources a lockfile resolves, each with its `rev`, or None for a branch or tag."""
    pins = []
    for package in tomllib.loads(text).get("package", []):
        source = package.get("source", "")
        if not source.startswith("git+"):
            continue
        url, _, query = source.removeprefix("git+").partition("#")[0].partition("?")
        kit = kit_source(url)
        if kit is None:
            continue
        fields = dict(pair.partition("=")[::2] for pair in query.split("&") if pair)
        rev = fields.get("rev")
        pins.append((kit, rev if rev and REV.fullmatch(rev) else None))
    return pins


def tracked(root: Path, name: str) -> list[Path]:
    """The files git tracks under `root` whose name is `name`."""
    listed = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
        encoding="utf-8",
    ).stdout
    return [
        root / path for path in listed.split("\0") if path and Path(path).name == name
    ]


def check(root: Path) -> list[str]:
    problems = []
    pins: dict[tuple[str, str], list[str]] = {}
    files = {"manifests": 0, "lockfiles": 0}
    for kind, pattern, read in (
        ("manifests", "Cargo.toml", manifest_pins),
        ("lockfiles", "Cargo.lock", lock_pins),
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
    if files["manifests"] == 0:
        problems.append("no tracked Cargo.toml names the kit's git source")
    elif files["lockfiles"] == 0:
        problems.append("no tracked Cargo.lock resolves the kit")
    if len(pins) > 1:
        for (source, rev), names in sorted(pins.items()):
            problems.append(f"{source} {rev[:7]}: {', '.join(sorted(set(names)))}")
    fuzz = [
        manifest
        for manifest in tracked(root, "Cargo.toml")
        if manifest.parent.name == "fuzz"
    ]
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
            encoding="utf-8",
            timeout=METADATA_TIMEOUT_S,
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
