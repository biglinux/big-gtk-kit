#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Find the GTK4 code shapes that leaked on the BigLinux desktop, in Rust sources.

    tools/static/leak_lint.py [--json] [--no-icons] [--errors-only] PATH...

Each rule is one leak that was measured, not a style preference. Errors are shapes
that are wrong whenever they appear; warnings are heuristics that need a person to
read the code (and then either fix it or say why it is fine).

Suppress one finding with a comment on its line or the line above:
    // leak-guard: allow(<rule>) <why this one is fine>
A suppression without a reason is itself reported.

Exit status: 0 no errors, 1 errors found, 2 usage error.
"""

from __future__ import annotations

import argparse
import itertools
import json
import os
import re
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

ALLOW = re.compile(r"//\s*leak-guard:\s*allow\(([\w-]+)\)\s*(.*)")

# Calls that put a child inside a container: the child is then kept by it.
PLACE = (
    r"(add_suffix|add_prefix|append|prepend|set_child|add|add_row|set_content|pack_start|"
    r"pack_end|set_title_widget|set_header_suffix|add_top_bar|add_bottom_bar|"
    r"insert_child_after|set_activatable_widget|set_extra_child|put|attach|add_overlay|"
    r"set_popover|set_start_widget|set_end_widget|set_center_widget|set_label_widget|"
    r"add_named|add_titled|add_child|set_bottom_bar|set_top_bar|set_start_child|"
    r"set_end_child|add_controller)"
)
# Builder setters that put a child inside the object being built.
BUILDER_PLACE = re.compile(
    r"\.\s*(?:child|content|titlebar|title_widget|popover|start_widget|end_widget|"
    r"center_widget|label_widget|start_child|end_child)\s*\(\s*(?:Some\(\s*)?&\s*(\w+)"
)
FN_START = re.compile(
    r"^\s*(pub(\([^)]*\))?\s+)?(const\s+)?(async\s+)?(unsafe\s+)?fn\s"
)


@dataclass
class Finding:
    path: str
    line: int
    severity: str
    rule: str
    message: str


def strip_comments(line: str) -> str:
    """The code part of one line (good enough for these shapes: no multi-line strings)."""
    if "//" not in line and '"' not in line:
        return line  # most lines: skip the character walk
    out, i, quote = [], 0, None
    while i < len(line):
        c = line[i]
        if quote:
            out.append(c)
            if c == "\\":
                out.append(line[i + 1 : i + 2])
                i += 2
                continue
            if c == quote:
                quote = None
        elif line.startswith("'\"'", i) or line.startswith("'\\\"'", i):
            end = line.index("'", i + 1) + 1
            out.append(line[i:end])
            i = end
            continue
        elif c == '"':
            quote = c
            out.append(c)
        elif line.startswith("//", i):
            break
        else:
            out.append(c)
        i += 1
    return "".join(out)


def call_argument(text: str, open_paren: int) -> str:
    """Everything between `(` at `open_paren` and its matching `)`."""
    depth, i, quote = 0, open_paren, None
    while i < len(text):
        c = text[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if c == quote:
                quote = None
        elif text.startswith("'\"'", i) or text.startswith("'\\\"'", i):
            i = text.index("'", i + 1) + 1
            continue
        elif c == '"':
            quote = c
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return text[open_paren + 1 : i]
        i += 1
    return text[open_paren + 1 :]


class Source:
    def __init__(self, path: Path):
        self.path = path
        self.raw = path.read_text(errors="replace").splitlines()
        self.code = [strip_comments(line) for line in self.raw]
        self.text = "\n".join(self.code)
        # offset of the start of each line in `text`
        self.starts = []
        pos = 0
        for line in self.code:
            self.starts.append(pos)
            pos += len(line) + 1

    def line_of(self, offset: int) -> int:
        lo, hi = 0, len(self.starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if self.starts[mid] <= offset:
                lo = mid
            else:
                hi = mid - 1
        return lo + 1

    def functions(self):
        """(first line index, last line index exclusive) of each fn, crudely."""
        starts = [i for i, line in enumerate(self.code) if FN_START.match(line)]
        starts.append(len(self.code))
        return list(itertools.pairwise(starts))

    def is_test_code(self, line_index: int) -> bool:
        """Inside a `#[cfg(test)]` item or a `#[test]` fn: a leak there costs nothing."""
        if not hasattr(self, "_test_lines"):
            self._test_lines = set()
            for m in re.finditer(r"#\[(cfg\(test\)|test)\]", self.text):
                brace = self.text.find("{", m.end())
                semi = self.text.find(";", m.end())
                if brace < 0 or (0 <= semi < brace):
                    continue  # `#[cfg(test)] mod tests;` or `use`: the body is elsewhere
                end = brace + len(call_argument(self.text, brace)) + 1
                self._test_lines.update(
                    range(self.line_of(m.start()) - 1, self.line_of(end))
                )
        return line_index in self._test_lines


def rule_closed_unparent(src: Source):
    """A popover unparented inside its own `closed` stays alive with everything in it."""
    for m in re.finditer(r"\.connect_closed\s*\(", src.text):
        arg = call_argument(src.text, m.end() - 1)
        if "unparent" in arg and not re.search(
            r"idle_add|timeout_add|spawn_local", arg
        ):
            yield (
                src.line_of(m.start()),
                (
                    "`unparent()` inside the popover's own `closed`: GTK keeps the popover, and "
                    "everything in it, when a child still holds focus. Unparent from an idle "
                    "(`glib::idle_add_local_once`)."
                ),
            )


def rule_window_close_by_destroy(src: Source):
    """`destroy` on a window only arrives once nothing holds it: a close handler there never runs."""
    for m in re.finditer(
        r"([\w.()]*window\w*(\(\))?)\s*\.\s*connect_destroy\s*\(",
        src.text,
        re.IGNORECASE,
    ):
        arg = call_argument(src.text, m.end() - 1)
        if re.search(r"\.send\(|\.emit\(|\bremove", arg):
            yield (
                src.line_of(m.start()),
                (
                    "a window's close detected with `connect_destroy`: GTK emits `destroy` from "
                    "dispose, which waits for the last reference, often held by the very controller "
                    "this handler would release. Detect the close with `connect_application_notify` "
                    "(`application().is_none()`) or `connect_close_request`."
                ),
            )


def rule_set_parent_without_unparent(src: Source):
    """A `set_parent` child is never detached by GTK when its parent goes."""
    file_detaches = re.search(
        r"\.unparent\(\)|unparent_popover_on_parent_teardown|\bimpl\s+Drop\s+for\b",
        src.text,
    )
    if file_detaches:
        return
    for m in re.finditer(r"\.\s*set_parent\s*\(", src.text):
        line = src.line_of(m.start())
        end = next(
            (last for first, last in src.functions() if first < line <= last),
            len(src.code),
        )
        after = "\n".join(src.code[line - 1 : end])
        if re.search(r"\b\w*(unparent|teardown|autodestroy)\w*\s*\(", after):
            continue  # handed to a helper that detaches it (defined elsewhere)
        yield (
            line,
            (
                "`set_parent` with no `unparent` anywhere in this file: GTK does not detach it when "
                'the parent is destroyed, and the child leaks ("Finalizing …, but it still has '
                "children left\"). Unparent on the parent's destroy, or allow it if the parent is a "
                "gtk::Box/Overlay, whose dispose detaches every child."
            ),
        )


def rule_teardown_reads_child(src: Source):
    """GTK clears a list item's child before emitting `teardown`."""
    for m in re.finditer(r"\.connect_teardown\s*\(", src.text):
        arg = call_argument(src.text, m.end() - 1)
        if re.search(r"\bchild\s*\(\s*\)|ListItem::child|::child\b", arg):
            yield (
                src.line_of(m.start()),
                (
                    "`teardown` reads the list item's child, which GTK has already cleared "
                    "(gtksignallistitemfactory.c: default teardown, then the signal). Anything "
                    "keyed by the child is never removed; key it by the ListItem."
                ),
            )


def embedded_image_icons(extra_roots=()) -> set[str]:
    """Names of themed icons whose SVG embeds a raster (`href="data:image/…"`)."""
    dirs = (os.environ.get("XDG_DATA_DIRS") or "/usr/local/share:/usr/share").split(":")
    dirs.append(os.path.expanduser("~/.local/share"))
    roots = (
        [Path(d) / "icons" for d in dirs]
        + [Path("/usr/share/pixmaps")]
        + list(extra_roots)
    )
    names = set()
    for root in roots:
        if not root.is_dir():
            continue
        for dirpath, _, files in os.walk(root):
            for name in files:
                if not name.endswith(".svg"):
                    continue
                try:
                    with open(os.path.join(dirpath, name), "rb") as handle:
                        if b"data:image/" in handle.read():
                            names.add(name[:-4])
                except OSError:
                    continue
    return names


def rule_embedded_image_icon(src: Source, names: set[str]):
    if not names:
        return
    for i, line in enumerate(src.code):
        if "icon" not in line.lower():
            continue
        for literal in re.findall(r'"([A-Za-z0-9_.+-]+)"', line):
            if literal in names:
                yield (
                    i + 1,
                    (
                        f"icon `{literal}` is an SVG with an embedded bitmap in an installed theme: "
                        "GTK 4.22 never frees those (Adwaita's preferences-desktop-appearance-symbolic "
                        "holds 29 MB of them). Pick another icon, or draw it without GTK's loader."
                    ),
                )


CLOSURE = re.compile(r"(move\s*)?\|([^|]*)\|")
# the source may be a path (`self.networks.downgrade()`): its last segment is the name
LET_CLONE = r"let\s+(?:mut\s+)?(\w+)\s*=\s*(?:((?:\w+\.)*\w+)\s*\.\s*clone\(\)|Rc::clone\(&\s*((?:\w+\.)*\w+)\s*\))\s*;"
LET_WEAK = r"let\s+(?:mut\s+)?(\w+)\s*=\s*(?:((?:\w+\.)*\w+)\s*\.\s*downgrade\(\)|Rc::downgrade\(&\s*((?:\w+\.)*\w+)\s*\))\s*;"


def source_name(path: str) -> str:
    """`self.x` is `x`; any other path (`properties.group`) names nothing local."""
    return path.removeprefix("self.")


def in_scope_at_end(text: str) -> list[bool]:
    """For each offset: whether the block open there is still open at the end of `text`."""
    masked = re.sub(r'"(?:\\.|[^"\\])*"', lambda m: " " * len(m.group()), text)
    depth, depths = 0, []
    for c in masked:
        if c in "{":
            depth += 1
        elif c in "}":
            depth -= 1
        depths.append(depth)
    open_at_end, lowest = [False] * len(depths), depth
    for i in range(len(depths) - 1, -1, -1):
        lowest = min(lowest, depths[i])
        open_at_end[i] = lowest >= depths[i]
    return open_at_end


TUPLE_CLONE = re.compile(r"let\s+\(([^)]*)\)\s*=\s*\(([^;]*)\)\s*;")


def bindings(text: str) -> tuple[dict[str, str], dict[str, str]]:
    """alias -> source for `let a = s.clone()` and `let a = s.downgrade()` (tuples too),
    read in order: a clone of a weak reference is weak, and a later binding wins."""
    events = []
    for kind, pattern in (("strong", LET_CLONE), ("weak", LET_WEAK)):
        for m in re.finditer(pattern, text):
            events.append(
                (m.start(), m.group(1), kind, source_name(m.group(2) or m.group(3)))
            )
    for m in TUPLE_CLONE.finditer(text):
        for alias, value in zip(
            [n.strip() for n in m.group(1).split(",")],
            [v.strip() for v in m.group(2).split(",")],
        ):
            for kind, method in (("strong", "clone"), ("weak", "downgrade")):
                found = re.fullmatch(
                    rf"((?:\w+\.)*\w+)\s*\.\s*{method}\(\)|Rc::{method}\(&\s*((?:\w+\.)*\w+)\s*\)",
                    value,
                )
                if found:
                    events.append(
                        (
                            m.start(),
                            alias,
                            kind,
                            source_name(found.group(1) or found.group(2)),
                        )
                    )
    # A binding made inside a block that has closed (`for row in … { let x = … }`)
    # is gone by the end of `text`.
    open_at_end = in_scope_at_end(text)
    state: dict[str, tuple[str, str]] = {}
    for position, alias, kind, source in sorted(events, key=lambda event: event[0]):
        if not open_at_end[position]:
            continue
        if source in state:  # a copy of an alias is a copy of what the alias holds
            source_kind, source = state[source]
            kind = "weak" if source_kind == "weak" else kind
        state[alias] = (kind, source)
    strong = {
        alias: source for alias, (kind, source) in state.items() if kind == "strong"
    }
    weak = {alias: source for alias, (kind, source) in state.items() if kind == "weak"}
    return strong, weak


def top_level_arguments(arguments: str) -> list[str]:
    """`a, f(b, c), d` -> [`a`, `f(b, c)`, `d`]."""
    parts, depth, current = [], 0, []
    for c in arguments:
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        if c == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(c)
    parts.append("".join(current))
    return parts


def statement_after(text: str, start: int) -> str:
    """From `start` to the `;` that ends the statement, skipping nested brackets."""
    depth, i = 0, start
    while i < len(text):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == ";" and depth <= 0:
            break
        i += 1
    return text[start:i]


CLOSURE_VALUE = re.compile(r"\s*(?:(?:std::rc::)?Rc::new\()?\s*(?:\{|move\s*\||\|)")


def closure_values(text: str) -> dict[str, set[str]]:
    """What each local closure keeps alive: `let f = move || …`, `let f = { …; move || … }`,
    and `Rc::new` of either or of an earlier one. A handler that captures `f` keeps
    all of it: a closure shared through an `Rc` is still owned by every handler holding it."""
    values: dict[str, set[str]] = {}
    for m in re.finditer(r"\blet\s+(?:mut\s+)?(\w+)\s*(?::[^=;]*)?=(?!=)", text):
        value = statement_after(text, m.end())
        wrapped = re.fullmatch(r"\s*(?:std::rc::)?Rc::new\(\s*(\w+)\s*\)\s*", value)
        if wrapped:
            if wrapped.group(1) in values:
                values[m.group(1)] = values[wrapped.group(1)]
            continue
        if CLOSURE_VALUE.match(value):
            start = (
                value.find("(") + 1
                if value.lstrip().startswith(("Rc::", "std::"))
                else 0
            )
            values[m.group(1)] = closure_captures(
                value[start:], text[: m.start()], values
            )
    return values


def handler_captures(
    body_text: str,
    connect_end: int,
    before: str,
    values: dict[str, set[str]] | None = None,
) -> set[str]:
    """Names the handler whose `(` ends at `connect_end` keeps alive, by their source name.

    Only what the closure captures counts: a clone made inside its body runs when
    the handler runs and is dropped with it; the handler's parameters are its own."""
    return closure_captures(
        call_argument(body_text, connect_end - 1), before, values or {}
    )


def closure_captures(arg: str, before: str, values: dict[str, set[str]]) -> set[str]:
    """What the closure in `arg` (a closure, or a block ending in one) keeps alive."""
    macro = re.search(r"\bclone!\s*\(", arg)
    if macro and macro.start() < (
        CLOSURE.search(arg).start() if CLOSURE.search(arg) else len(arg)
    ):
        # glib::clone!: only `#[strong]` (and a bare name, which clones it) is kept alive
        inner = call_argument(arg, macro.end() - 1)
        closure = CLOSURE.search(inner)
        head = inner[: closure.start()] if closure else inner
        held = set(re.findall(r"#\[strong\]\s*(\w+)", head))
        attributed = set(re.findall(r"#\[\w+(?:\([^)]*\))?\]\s*(\w+)", head))
        bare = set(re.findall(r"(?:^|,)\s*(\w+)\s*(?=,)", head)) - attributed
        return held | bare
    closure = CLOSURE.search(arg)
    if not closure:
        return set()  # a function path: captures nothing
    pre, body = arg[: closure.start()], arg[closure.end() :]
    body = re.sub(r'"(?:\\.|[^"\\])*"', '""', body)  # words in strings are not names
    params = set(re.findall(r"\b([a-z_]\w*)\b", closure.group(2)))
    # Where each name is first bound inside the body (let, for, match arm, inner closure).
    bound_at: dict[str, int] = {}
    binders = [
        r"\blet\s+([^=;]+?)\s*(?::[^=;]*)?=(?!=)",
        r"\bfor\s+(.+?)\s+in\b",
        r"([^{},;]*?)=>",
        r"\|([^|]*)\|",
    ]
    for binder in binders:
        for m in re.finditer(binder, body):
            # a `let` binds after its statement: its right-hand side still sees the outer name
            where = m.start(1)
            if binder is binders[0]:
                conditional = re.search(r"\b(if|while)\s*$|&&\s*$", body[: m.start()])
                if conditional:  # `if let`, `while let`, and a let chain's `&& let`
                    ends = [
                        e
                        for e in (body.find("{", m.end()), body.find("&&", m.end()))
                        if e >= 0
                    ]
                    end = min(ends) if ends else -1
                else:
                    end = body.find(";", m.end())
                where = end if end >= 0 else len(body)
            for name in re.findall(r"\b([a-z_]\w*)\b", m.group(1)):
                bound_at[name] = min(bound_at.get(name, len(body)), where)
    pattern_spans = [
        (m.start(1), m.end(1)) for binder in binders for m in re.finditer(binder, body)
    ]
    used = set()
    # variables only: not a field or method (`.x`), a path (`x::`, `::x`), a call, a macro,
    # or a field name in a struct literal (`x: value`)
    for m in re.finditer(r"(?<![.\w:])([a-z_]\w*)\b(?!\s*(?:\(|!|::|:(?!:)))", body):
        name = m.group(1)
        if any(lo <= m.start() < hi for lo, hi in pattern_spans):
            continue  # the binding itself
        if m.start() < bound_at.get(name, len(body)):
            used.add(name)  # used before any inner binding: the outer one, captured
    # a local closure is used by calling it
    calls = set(re.findall(r"(?<![.\w:])([a-z_]\w*)\s*\(", body)) - params
    used -= params
    used -= {
        "self",
        "move",
        "let",
        "mut",
        "if",
        "else",
        "match",
        "return",
        "true",
        "false",
        "as",
        "in",
        "for",
        "while",
        "loop",
        "break",
        "continue",
        "ref",
        "async",
        "await",
        "_",
    }
    strong, weak = bindings(before + "\n" + pre)
    held = {strong[a] for a in used | calls if a in strong}
    if closure.group(1):  # `move`: a bare outer name used in the body is moved in
        for name in used | (calls & set(values)):
            if name not in strong and name not in weak:
                held.add(name)
    pending = [name for name in held if name in values]
    while pending:
        for name in values.get(pending.pop(), set()) - held:
            held.add(name)
            pending.append(name)
    return held


def rule_handler_holds_owner(src: Source):
    """A handler that holds the object it hangs off, or one of that object's ancestors."""
    for first, last in src.functions():
        body = src.code[first:last]
        text = "\n".join(body)
        starts, pos = [], 0
        for line in body:
            starts.append(pos)
            pos += len(line) + 1
        placed: dict[str, set[str]] = {}
        for m in re.finditer(r"\b(\w+)\s*\.\s*" + PLACE + r"\s*\(", text):
            # Only a whole argument is a child: `list.append(&row(&labels))`
            # places the row it makes, not `labels`.
            for argument in top_level_arguments(call_argument(text, m.end() - 1)):
                child = re.fullmatch(r"\s*(?:Some\(\s*)?&\s*(\w+)\s*\)?\s*", argument)
                if m.group(2) == "add_controller":  # takes the controller by value
                    child = re.fullmatch(r"\s*&?\s*(\w+)\s*", argument)
                if child:
                    placed.setdefault(child.group(1), set()).add(m.group(1))
        for m in re.finditer(
            r"\blet\s+(?:mut\s+)?(\w+)\s*=\s*[\w:]+::builder\(\)", text
        ):
            for child in BUILDER_PLACE.findall(statement_after(text, m.end())):
                placed.setdefault(child, set()).add(m.group(1))
        values = closure_values(text)
        changed = True
        while changed:  # an ancestor's ancestor is an ancestor
            changed = False
            for child, parents in placed.items():
                more = (
                    set().union(*(placed.get(p, set()) for p in parents))
                    - parents
                    - {child}
                )
                if more:
                    parents |= more
                    changed = True
        for m in re.finditer(r"\b(\w+)\s*\.\s*connect_\w+\s*\(", text):
            target = m.group(1)
            index = max(i for i, st in enumerate(starts) if st <= m.start())
            strong = handler_captures(text, m.end(), text[: m.start()], values)
            if target in strong:
                yield (
                    first + index + 1,
                    (
                        f"the `{target}` handler holds `{target}` itself: the object owns the handler, "
                        "so neither is ever freed. Use the handler's argument or a weak reference."
                    ),
                )
            for owner in sorted(placed.get(target, set()) & strong):
                if owner != target:
                    yield (
                        first + index + 1,
                        (
                            f"the `{target}` handler holds `{owner}` strongly, and `{target}` sits inside "
                            f"`{owner}`: a cycle through the handler. Hold `{owner}` with a weak reference."
                        ),
                    )


def rule_mutual_siblings(src: Source):
    """Two widgets whose handlers each hold the other outlive the container they are in."""
    for first, last in src.functions():
        body = src.code[first:last]
        text = "\n".join(body)
        starts, pos = [], 0
        for line in body:
            starts.append(pos)
            pos += len(line) + 1
        holds: dict[str, tuple[set[str], int]] = {}
        for m in re.finditer(r"\b(\w+)\s*\.\s*connect_\w+\s*\(", text):
            index = max(i for i, st in enumerate(starts) if st <= m.start())
            before = "\n".join(body[max(0, index - 12) : index + 1])
            strong = handler_captures(text, m.end(), before)
            entry = holds.setdefault(m.group(1), (set(), first + index + 1))
            entry[0].update(strong)
        for a, (held_a, line_a) in holds.items():
            for b in held_a:
                if b != a and b in holds and a in holds[b][0] and a < b:
                    yield (
                        line_a,
                        (
                            f"the handlers of `{a}` and `{b}` each hold the other strongly: two "
                            "siblings keeping each other alive outlive the container they sit in. "
                            "Make one side (or both) a weak reference."
                        ),
                    )


def rule_lazy_followers(src: Source):
    """A long-lived list of (weak widget, closure) pruned only when the service next has news."""
    for m in re.finditer(
        r"\(\s*(?:gtk::)?(?:glib::)?WeakRef<[^>]+>\s*,\s*(?:Box|Rc)<dyn\s+Fn",
        src.text,
    ):
        yield (
            src.line_of(m.start()),
            (
                "followers kept as (weak anchor, strong closure): the closure, and every widget it "
                "holds, lives until the list is next pruned, which for a quiet service is hours. "
                "Keep the closure on the anchor and only a weak handle in the list."
            ),
        )


# Objects that live as long as the process: a handler on one outlives every
# window that connected it.
PROCESS_OBJECT = (
    r"(?:\.clipboard\(\)|\.primary_clipboard\(\)|VolumeMonitor::get\(\)|"
    r"StyleManager::default\(\)|gtk::Settings::default\(\)(?:\.\w+\([^)]*\))?|"
    r"NetworkMonitor::default\(\)|IconTheme::for_display\([^)]*\)|"
    # the application itself only when a component reaches it (`init.app`,
    # `self.app`): a program's own setup in `main` connects once
    r"\binit\.app\.|\bself\.app\.)"
)


def rule_process_object_handler(src: Source):
    """A handler on a process-lifetime object whose id is thrown away."""
    for m in re.finditer(
        r"(?m)^[ \t]*(?!let\b)[^\n;=]*?"
        + PROCESS_OBJECT
        + r"\s*\.?\s*connect_(\w+)\s*\(",
        src.text,
    ):
        yield (
            src.line_of(m.start()),
            (
                f"`connect_{m.group(1)}` on an object that lives as long as the process, with the "
                "handler id discarded: every window or component that runs this leaves one more "
                "handler (and all it captures) behind. Keep the id and disconnect it when the owner "
                "goes, or run it once per process."
            ),
        )


ERROR_RULES = {
    "closed-unparent": rule_closed_unparent,
    "window-close-by-destroy": rule_window_close_by_destroy,
    "set-parent-without-unparent": rule_set_parent_without_unparent,
    "teardown-reads-child": rule_teardown_reads_child,
}
WARNING_RULES = {
    "handler-holds-owner": rule_handler_holds_owner,
    "mutual-siblings": rule_mutual_siblings,
    "lazy-followers": rule_lazy_followers,
    "process-object-handler": rule_process_object_handler,
}


def allowed(src: Source, line: int, rule: str, findings: list[Finding]) -> bool:
    for index in (line - 1, line - 2):
        if 0 <= index < len(src.raw):
            m = ALLOW.search(src.raw[index])
            if m and m.group(1) == rule:
                if not m.group(2).strip():
                    findings.append(
                        Finding(
                            str(src.path),
                            index + 1,
                            "error",
                            "allow-without-reason",
                            f"`leak-guard: allow({rule})` needs a reason after it",
                        )
                    )
                return True
    return False


def check(paths: list[Path], icons: set[str]) -> list[Finding]:
    findings: list[Finding] = []
    for path in paths:
        try:
            src = Source(path)
        except OSError as error:
            # A broken link or an unreadable file is not a finding, nor a crash.
            print(f"leak_lint: skipped {path}: {error.strerror}", file=sys.stderr)
            continue
        rules = [("error", name, fn) for name, fn in ERROR_RULES.items()]
        rules += [("warning", name, fn) for name, fn in WARNING_RULES.items()]
        for severity, name, fn in rules:
            for line, message in fn(src):
                if src.is_test_code(line - 1) or allowed(src, line, name, findings):
                    continue
                findings.append(Finding(str(path), line, severity, name, message))
        for line, message in rule_embedded_image_icon(src, icons):
            if not src.is_test_code(line - 1) and not allowed(
                src, line, "embedded-image-icon", findings
            ):
                findings.append(
                    Finding(str(path), line, "error", "embedded-image-icon", message)
                )
    seen, unique = set(), []
    for f in findings:
        key = (f.path, f.line, f.rule)
        if key not in seen:
            seen.add(key)
            unique.append(f)
    return sorted(unique, key=lambda f: (f.path, f.line, f.rule))


def rust_files(paths: list[str]) -> list[Path]:
    out = []
    for raw in paths:
        p = Path(raw)
        if p.is_file() and p.suffix == ".rs":
            out.append(p)
        elif p.is_dir():
            out.extend(
                f
                for f in sorted(p.rglob("*.rs"))
                if not ({"target", ".git"} & set(f.relative_to(p).parts))
            )
    return out


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--json", action="store_true", help="print findings as JSON")
    parser.add_argument(
        "--no-icons", action="store_true", help="skip the icon theme scan"
    )
    parser.add_argument(
        "--errors-only", action="store_true", help="do not print warnings"
    )
    options = parser.parse_args(argv)
    files = rust_files(options.paths)
    if not files:
        print("leak_lint: no Rust sources under the given paths", file=sys.stderr)
        return 2
    icons = set() if options.no_icons else embedded_image_icons()
    findings = check(files, icons)
    if options.errors_only:
        findings = [f for f in findings if f.severity == "error"]
    if options.json:
        print(json.dumps([asdict(f) for f in findings], indent=2))
    else:
        for f in findings:
            print(f"{f.path}:{f.line}: {f.severity}: {f.rule}: {f.message}")
        errors = sum(f.severity == "error" for f in findings)
        warnings = len(findings) - errors
        print(
            f"leak_lint: {len(files)} files, {errors} errors, {warnings} warnings",
            file=sys.stderr,
        )
    return 1 if any(f.severity == "error" for f in findings) else 0


if __name__ == "__main__":
    sys.exit(main())
