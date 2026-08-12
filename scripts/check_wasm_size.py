#!/usr/bin/env python3
"""Size budget for the committed playground WebAssembly bundle.

The bundle is downloaded by every visitor who opens a page carrying a live
widget or the playground itself, so its size is a user-facing property and
deserves a gate rather than good intentions. This asserts the committed
``sonda_wasm_bg.wasm`` is no larger than :data:`BUDGET_BYTES`.

**The budget is a ratchet.** Lower it whenever a rebuild comes in smaller —
that is the point, and the script tells you when there is enough slack to be
worth doing. Raise it only with a written justification in the commit that
does so: a budget quietly raised to fit whatever landed is not a budget.

Stdlib-only, no arguments needed::

    python3 scripts/check_wasm_size.py

``--print`` reports the current size and exits 0, for use when re-basing the
ratchet after an intentional rebuild.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

BUNDLE = Path("docs/site/docs/javascripts/sonda_wasm_bg.wasm")

# Measured size of the committed bundle plus ~10% headroom, so ordinary code
# growth is not a merge-blocker but a step change is.
#
#   1,240,112  before WP5 (release profile, no wasm-opt)
#     771,488  now (wasm-release profile + wasm-opt -Oz)  -> 37.8% smaller
#
# Almost all of that came from wasm-opt, not from the cargo profile: the
# profile alone is worth ~3% after wasm-bindgen. If a rebuild lands near the
# old figure, the wasm-opt step was skipped — see `task site:wasm`.
BUDGET_BYTES = 850_000

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
            "The bundle ships to every visitor of a page with a live widget. "
            "Shrink it, or raise BUDGET_BYTES in scripts/check_wasm_size.py "
            "with the reason in the commit message.",
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
