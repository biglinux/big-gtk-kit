#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Who holds the references that keep a GObject alive, from a reftrace.c log.

    reftrace.py TRACE BINARY --objects            which traced objects are still alive
    reftrace.py TRACE BINARY [--object N]          sites that took more than they released
    reftrace.py TRACE BINARY --object N --all      every site, with its balance

TRACE is REFTRACE_OUT.<pid> (its .maps file beside it); BINARY is the traced
executable, built with debug info. Each event is attributed to the deepest frame
whose function matches --crate, with its file:line.

Reading the log: each line is `N|R|S|s|U address count frames…`, and `count` is
the reference count BEFORE the operation. `N` is the creation, `s` a sink of the
floating reference (it takes that reference over and adds none), and `U 1` is the
last release: the object was finalised there. An address can be reused by another object afterwards; a live
check has to confirm the type (the census does).
"""

import argparse
import collections
import os
import re
import subprocess


def load(trace):
    with open(trace) as handle:
        # A line cut short by a killed process has fewer than three fields.
        return [
            fields
            for fields in map(str.split, handle)
            if len(fields) >= 3 and fields[0] in ("N", "R", "S", "s", "U")
        ]


def objects_summary(events):
    order = list(dict.fromkeys(event[1] for event in events))
    count = collections.Counter(event[1] for event in events)
    last = {}
    for event in events:
        last[event[1]] = (event[0], int(event[2]))
    for index, address in enumerate(order):
        kind, before = last[address]
        state = (
            "freed"
            if kind == "U" and before == 1
            else f"alive ({before + {'U': -1, 's': 0}.get(kind, 1)} refs)"
        )
        print(f"{index:4} {address} {count[address]:5} events  {state}")


def resolver(trace, binary):
    base = []
    name = os.path.basename(binary)
    with open(trace + ".maps") as maps:
        for line in maps:
            fields = line.split()
            if len(fields) >= 6 and os.path.basename(fields[5]) == name:
                start, end = (int(v, 16) for v in fields[0].split("-"))
                base.append((start, end, int(fields[2], 16)))
    segments = []
    for line in subprocess.run(
        ["readelf", "-lW", binary], capture_output=True, text=True, check=True
    ).stdout.splitlines():
        fields = line.split()
        if fields and fields[0] == "LOAD":
            segments.append(
                (int(fields[1], 16), int(fields[2], 16), int(fields[4], 16))
            )

    def address(runtime):
        for start, end, offset in base:
            if start <= runtime < end:
                file_offset = runtime - start + offset
                for seg_offset, seg_address, size in segments:
                    if seg_offset <= file_offset < seg_offset + size:
                        return file_offset - seg_offset + seg_address - 1
        return None

    return address


def symbolise(binary, addresses):
    names = {}
    if not addresses:
        return names
    output = subprocess.run(
        ["addr2line", "-f", "-C", "-i", "-a", "-e", binary, *map(hex, addresses)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    current = None
    for line in output:
        if line.startswith("0x"):
            current = int(line, 16)
            names[current] = []
        elif current is not None and line.startswith("/") and names[current]:
            where = "/".join(line.rsplit("/", 2)[-2:])
            names[current][-1] += f"  @{where}"
        elif current is not None and not line.startswith("??") and line != "??":
            names[current].append(line)
    return names


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("trace")
    parser.add_argument("binary")
    parser.add_argument(
        "--objects",
        action="store_true",
        help="list traced objects and whether each was freed",
    )
    parser.add_argument(
        "--object",
        type=int,
        help="index from --objects (default: the one with most events)",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="print every site, not only those that kept references",
    )
    parser.add_argument(
        "--crate",
        default=r"<?(big[a-z_]*|relm4)[:<]",
        help="regex of your own functions",
    )
    options = parser.parse_args()

    events = load(options.trace)
    if not events:
        parser.error(f"{options.trace} has no events: did REFTRACE_TYPE match a type?")
    if options.objects:
        objects_summary(events)
        return
    order = list(dict.fromkeys(event[1] for event in events))
    counts = collections.Counter(event[1] for event in events)
    if options.object is not None and not 0 <= options.object < len(order):
        parser.error(f"--object must be 0..{len(order) - 1} (see --objects)")
    target = (
        order[options.object]
        if options.object is not None
        else max(counts, key=counts.get)
    )
    mine = [event for event in events if event[1] == target]

    address = resolver(options.trace, options.binary)
    wanted = sorted(
        {
            a
            for event in mine
            for a in (address(int(x, 16)) for x in event[3:])
            if a is not None
        }
    )
    names = symbolise(options.binary, wanted)
    crate = re.compile(options.crate)

    def site(event):
        for raw in event[3:]:
            for name in names.get(address(int(raw, 16)), []):
                if crate.match(name):
                    return name[:220]
        return "(no frame of your code)"

    taken, released = collections.Counter(), collections.Counter()
    for event in mine:
        if event[0] in "NRS":
            taken[site(event)] += 1
        elif event[0] == "U":
            released[site(event)] += 1
    kind, before = mine[-1][0], int(mine[-1][2])
    state = "freed" if kind == "U" and before == 1 else "still alive"
    print(f"object {target}: {len(mine)} events, {state}")
    for name in sorted(
        set(taken) | set(released), key=lambda n: released[n] - taken[n]
    ):
        balance = taken[name] - released[name]
        if balance > 0 or options.all:
            print(
                f"  {balance:+4}  taken {taken[name]:3}  released {released[name]:3}  {name}"
            )


if __name__ == "__main__":
    main()
