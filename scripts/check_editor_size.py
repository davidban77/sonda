#!/usr/bin/env python3
"""Size budget for the committed CodeMirror editor bundle.

The playground downloads this on every visit, so its size is a user-facing
property — the same argument :mod:`check_wasm_size` makes about the wasm
bundle, and this script deliberately mirrors it.

It exists because of review #548 M2. WP11 PR3 added schema-driven completion
and the bundle grew 505,984 -> 549,276 bytes, +8.5%, entirely from pulling in
``@codemirror/autocomplete``. That growth was real, justified, and recorded
only in a commit message — and a number in a commit message is the one place
nobody looks twice. The wasm bundle has had a ratchet since WP5 for exactly
this reason; the editor bundle had a byte-exact DRIFT gate, which catches "you
forgot to rebuild" and says nothing at all about weight.

**The budget is a ratchet.** Lower it whenever a rebuild comes in smaller.
Raise it only with a written justification in the commit that does so: a
budget quietly raised to fit whatever landed is not a budget.

Stdlib-only, no arguments needed::

    python3 scripts/check_editor_size.py

``--print`` reports the current size and exits 0, for use when re-basing the
ratchet after an intentional rebuild.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

BUNDLE = Path("docs/site/docs/javascripts/sonda-editor.js")

# Measured size of the committed bundle plus ~10% headroom, so ordinary code
# growth is not a merge-blocker but a step change is.
#
#     505,984  before WP11 PR3
#     549,276  now (+8.5%, @codemirror/autocomplete)
#
# The dependency is what dominates: the completion SOURCE is a few dozen lines
# and the two pure helpers it calls tree-shake in at negligible cost. So a
# future jump of this magnitude almost certainly means another CodeMirror
# package arrived, which is a decision worth making on purpose.
BUDGET_BYTES = 605_000

# When the bundle is this far under budget, the ratchet has gone slack and is
# no longer measuring anything. Reported, not failed: tightening it is a
# deliberate act that belongs in a commit message, not a surprise in CI.
SLACK_WARN_RATIO = 0.15


def find_repo_root(start: Path) -> Path:
    """Walk up from ``start`` until a directory with a ``Cargo.toml`` is found."""
    current = start.resolve()
    while True:
        if (current / "Cargo.toml").is_file():
            return current
        if current.parent == current:
            raise RuntimeError(f"could not locate repo root above {start}")
        current = current.parent


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--print",
        action="store_true",
        dest="print_only",
        help="Report the current size and exit 0 (for re-basing the ratchet).",
    )
    args = parser.parse_args(argv)

    repo_root = find_repo_root(Path(__file__).parent)
    bundle = repo_root / BUNDLE
    if not bundle.is_file():
        print(f"bundle not found at {bundle}", file=sys.stderr)
        return 2

    actual = bundle.stat().st_size
    if args.print_only:
        print(f"{actual} bytes ({actual / 1024:.1f} KiB)")
        return 0

    headroom = BUDGET_BYTES - actual
    summary = (
        f"{BUNDLE.name}: {actual:,} bytes "
        f"({actual / 1024:.1f} KiB), budget {BUDGET_BYTES:,}"
    )

    if actual > BUDGET_BYTES:
        over = actual - BUDGET_BYTES
        print(
            f"::error::{summary} — OVER by {over:,} bytes ({over / BUDGET_BYTES:.1%}).\n"
            "The playground downloads this bundle on every visit. Shrink it, or "
            "raise BUDGET_BYTES in scripts/check_editor_size.py with the reason "
            "in the commit message.",
            file=sys.stderr,
        )
        return 1

    print(f"OK: {summary}, {headroom:,} bytes to spare")
    if headroom > BUDGET_BYTES * SLACK_WARN_RATIO:
        print(
            f"::warning::the budget has {headroom / BUDGET_BYTES:.0%} slack — "
            "the ratchet should be lowered toward the current size so it keeps "
            "measuring something."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
