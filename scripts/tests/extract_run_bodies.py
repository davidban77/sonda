#!/usr/bin/env python3
"""Print the bodies of a workflow file's ``run:`` steps.

The action's safety properties live inside `run:` scripts, and checking
them against the whole file is how #554 round 1 shipped two holes: a
`set -f` needle satisfied by a comment that merely mentioned `set -f`, and
an interpolation check whose allow-list matched any colon. Both go away
once the checks read step bodies instead of the file.

Round 2 found the next layer of the same mistake: the extractor recognised
only ``run: |`` with exactly one space, so a single-line ``run: echo …``
and a re-indented ``run:  |`` were both invisible — and every check
underneath inherited that blindness while staying green. Hence
:func:`run_key_count`, which lets a caller assert the extractor accounted
for *every* ``run:`` in the file rather than merely finding *some*.

Modes:

* ``code`` — bodies with whole-line comments dropped. Use when asserting a
  construction is really there, so a comment quoting it cannot stand in.
* ``raw`` — bodies verbatim, comments included. Use when asserting
  something must be absent: GitHub substitutes ``${{ }}`` before bash
  parses the script, and it does that inside comments too.
"""

from __future__ import annotations

import re
import sys

# Any indentation, any spacing, and a block scalar with any chomping or
# indentation indicator (`|`, `|-`, `>+`, `|2`, …).
RUN_KEY = re.compile(r"^(\s*)run:(\s*)(.*)$")
BLOCK_SCALAR = re.compile(r"^[|>][+-]?\d*$")


def run_key_count(text: str) -> int:
    """How many ``run:`` step keys the file contains.

    Counted independently of the extraction walk, so a caller can compare
    the two and notice a body the walk failed to open.
    """
    return sum(1 for line in text.split("\n") if RUN_KEY.match(line))


def run_bodies(text: str, mode: str = "code") -> tuple[list[str], int]:
    """Lines belonging to `run:` step bodies, and how many keys were opened.

    Both block scalars (``run: |``) and single-line values (``run: echo x``)
    are returned: a single-line body is shell too, and an interpolation in
    one is just as executable.
    """
    out: list[str] = []
    opened = 0
    indent: int | None = None

    for line in text.split("\n"):
        key = RUN_KEY.match(line)

        # Inside a block scalar: it ends at the first non-blank line
        # indented no further than the key that introduced it.
        if indent is not None:
            if line.strip() == "":
                continue
            if len(line) - len(line.lstrip()) > indent:
                if not (mode == "code" and line.lstrip().startswith("#")):
                    out.append(line)
                continue
            indent = None  # fell out; reconsider this line as a key below

        if not key:
            continue
        rest = key.group(3).strip()
        if BLOCK_SCALAR.match(rest):
            indent = len(key.group(1))
            opened += 1
        elif rest:
            opened += 1
            if not (mode == "code" and rest.startswith("#")):
                out.append(line)
        # else: `run:` with the block scalar on the NEXT line. That is
        # valid YAML and this walk does not follow it, so it must NOT be
        # counted as opened — counting it is what keeps the completeness
        # check equal and therefore silent, which is the guard defeating
        # itself (#554 round 3, W3). Left uncounted, `run_key_count` sees
        # the key, `opened` does not, and the caller goes red.

    return out, opened


def run_blocks(text: str) -> list[str]:
    """Each block-scalar `run:` body as one dedented, executable script."""
    blocks: list[str] = []
    current: list[str] | None = None
    indent = 0
    for line in text.split("\n"):
        key = RUN_KEY.match(line)
        if current is not None:
            if line.strip() == "" or len(line) - len(line.lstrip()) > indent:
                current.append(line[indent + 2 :] if len(line) > indent else "")
                continue
            blocks.append("\n".join(current))
            current = None
        if key and BLOCK_SCALAR.match(key.group(3).strip()):
            indent = len(key.group(1))
            current = []
    if current is not None:
        blocks.append("\n".join(current))
    return blocks


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print("usage: extract_run_bodies.py <file> [code|raw|count|block:N]", file=sys.stderr)
        return 2
    mode = argv[2] if len(argv) > 2 else "code"
    if not (mode in ("code", "raw", "count") or mode.startswith("block:")):
        print(
            f"unknown mode {mode!r}: expected 'code', 'raw', 'count' or 'block:N'",
            file=sys.stderr,
        )
        return 2
    with open(argv[1], encoding="utf-8") as handle:
        text = handle.read()
    if mode == "count":
        # "<keys in file> <keys the walk opened>" — equal means the walk saw
        # everything; unequal means some body is invisible to every check.
        _, opened = run_bodies(text, "raw")
        print(f"{run_key_count(text)} {opened}")
        return 0
    if mode.startswith("block:"):
        # One step's body, dedented, ready to execute. This is what lets a
        # test drive PRODUCTION shell instead of a copy of it — the copy is
        # what diverged on exactly the line a bug was on, twice.
        wanted = int(mode.split(":", 1)[1])
        blocks = run_blocks(text)
        if wanted < 1 or wanted > len(blocks):
            print(
                f"block {wanted} out of range: file has {len(blocks)} run: blocks",
                file=sys.stderr,
            )
            return 2
        print(blocks[wanted - 1])
        return 0
    lines, _ = run_bodies(text, mode)
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
