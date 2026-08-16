#!/usr/bin/env python3
"""Print the bodies of a workflow file's ``run:`` blocks.

The action's safety properties live inside `run:` scripts, and checking
them against the whole file is how #554 round 1 shipped two holes: a
`set -f` needle satisfied by a comment that merely mentioned `set -f`, and
an interpolation check whose allow-list matched any colon. Both go away
once the checks read the block bodies instead of the file.

Modes:

* ``code`` — block bodies with whole-line comments dropped. Use when
  asserting that a construction is *really there*, so a comment quoting it
  cannot stand in for it.
* ``raw`` — block bodies verbatim, comments included. Use when asserting
  something must be *absent*: GitHub substitutes ``${{ }}`` before bash
  parses the script, and it does that inside comments too.
"""

from __future__ import annotations

import re
import sys

BLOCK = re.compile(r"^(\s*)run: [|>]")


def run_bodies(text: str, mode: str) -> list[str]:
    """Lines belonging to `run:` block scalars, in file order."""
    out: list[str] = []
    indent: int | None = None
    for line in text.split("\n"):
        opened = BLOCK.match(line)
        if indent is None:
            if opened:
                indent = len(opened.group(1))
            continue
        if line.strip() == "":
            continue
        # A block scalar ends at the first non-blank line indented no
        # further than the key that introduced it.
        if len(line) - len(line.lstrip()) <= indent:
            indent = len(opened.group(1)) if opened else None
            continue
        if mode == "code" and line.lstrip().startswith("#"):
            continue
        out.append(line)
    return out


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print("usage: extract_run_bodies.py <file> [code|raw]", file=sys.stderr)
        return 2
    mode = argv[2] if len(argv) > 2 else "code"
    if mode not in ("code", "raw"):
        print(f"unknown mode {mode!r}: expected 'code' or 'raw'", file=sys.stderr)
        return 2
    with open(argv[1], encoding="utf-8") as handle:
        print("\n".join(run_bodies(handle.read(), mode)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
