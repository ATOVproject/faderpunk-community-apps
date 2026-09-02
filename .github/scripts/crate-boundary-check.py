#!/usr/bin/env python3
"""Flags any `crate::` path in a submitted app whose first segment isn't
`app` — the sanctioned facade per CONTRIBUTING.md ("everything ... goes
through use crate::app::{...}").

Handles two shapes a plain per-line regex can't: `use` items written as
nested brace-lists (`use crate::{ app::{...}, storage::{...} };` — the
prohibited segment isn't textually adjacent to `crate::`, it's a sibling
inside the same braces), and fully-qualified inline paths anywhere else
in the code (`crate::tasks::global_config::get_global_config()`).

Text-only: never executes or type-checks the input, just tokenizes it.

Usage: crate-boundary-check.py <path-to-added-lines.txt>
Exit 0 with no output = no violations. Exit 0 with one violation per
line on stdout = violations found (caller decides pass/fail).
"""

import re
import sys


def split_top_level_commas(s: str) -> list[str]:
    parts = []
    depth = 0
    cur = []
    for c in s:
        if c in "{(":
            depth += 1
            cur.append(c)
        elif c in "})":
            depth -= 1
            cur.append(c)
        elif c == "," and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(c)
    if cur:
        parts.append("".join(cur))
    return parts


def find_top_level_brace(s: str) -> int | None:
    depth = 0
    for i, c in enumerate(s):
        if c == "{":
            if depth == 0:
                return i
            depth += 1
        elif c == "}":
            depth -= 1
    return None


def parse_use_item(item: str) -> list[list[str]]:
    item = item.strip()
    if not item or item == "self":
        return []
    brace_idx = find_top_level_brace(item)
    if brace_idx is None:
        path = item.split(" as ")[0].strip()
        segs = [p.strip() for p in path.split("::") if p.strip() and p.strip() != "self"]
        return [segs] if segs else []

    prefix = item[:brace_idx].rstrip()
    prefix = prefix[:-2] if prefix.endswith("::") else prefix
    prefix_segs = [p.strip() for p in prefix.split("::") if p.strip()]

    close = item.rfind("}")
    inner = item[brace_idx + 1 : close] if close != -1 else item[brace_idx + 1 :]

    results = []
    for sub in split_top_level_commas(inner):
        for sub_path in parse_use_item(sub):
            results.append(prefix_segs + sub_path)
    return results


def strip_comments(text: str) -> str:
    """Blank out `//...` and `/* ... */` comments (doc comments included) so
    a crate::... path mentioned only in prose — e.g. an intra-doc link like
    [`crate::tasks::clock`] — isn't mistaken for real code. Simple regex
    pass; doesn't special-case `//`/`/*` inside string literals, which
    don't occur near the paths this script looks for in practice."""
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
    text = re.sub(r"//[^\n]*", "", text)
    return text


def flatten_use_statements(text: str) -> list[list[str]]:
    """Every `use crate::...;` in `text`, flattened to leaf segment lists."""
    paths = []
    for m in re.finditer(r"\buse\s+crate::(.*?);", text, re.DOTALL):
        paths.extend(parse_use_item(m.group(1)))
    return paths


def find_inline_qualified_paths(text: str) -> list[str]:
    """`crate::foo::bar(...)`-style fully-qualified references outside a
    `use` statement — e.g. a call made without importing the function
    first. Matches the leading `crate::<segment>::` prefix only."""
    # Strip `use crate::...;` statements first so their content (already
    # covered by flatten_use_statements) doesn't double-report here.
    without_use = re.sub(r"\buse\s+crate::.*?;", "", text, flags=re.DOTALL)
    return re.findall(r"\bcrate::[a-zA-Z_][a-zA-Z0-9_]*::[a-zA-Z_][a-zA-Z0-9_:]*", without_use)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: crate-boundary-check.py <added-lines-file>", file=sys.stderr)
        return 2

    with open(sys.argv[1], encoding="utf-8") as f:
        text = strip_comments(f.read())

    violations = []

    for segs in flatten_use_statements(text):
        if not segs:
            continue
        if segs[0] != "app":
            violations.append("crate::" + "::".join(segs))

    for path in find_inline_qualified_paths(text):
        first_seg = path.split("::")[1] if "::" in path else ""
        if first_seg != "app":
            violations.append(path)

    for v in sorted(set(violations)):
        print(v)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
