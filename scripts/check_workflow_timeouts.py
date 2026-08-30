#!/usr/bin/env python3
"""Every workflow job must declare `timeout-minutes`.

A job without one inherits GitHub's 6-hour default. A hung step then burns
a runner for six hours before anyone sees a red check, which is how a
stuck job reads as a slow one.

Reusable-workflow callers (`uses:` at job level) are exempt: the field is
not valid there, and the timeout belongs to the called workflow's own jobs.
That exemption is structural, not a name list — there is nothing to keep
in sync.

Exit 1 and name every offending job, or exit 0.
"""

from __future__ import annotations

import sys
from pathlib import Path

import yaml

WORKFLOW_DIR = Path(__file__).resolve().parent.parent / ".github" / "workflows"


def main() -> int:
    if not WORKFLOW_DIR.is_dir():
        print(f"error: no workflow directory at {WORKFLOW_DIR}", file=sys.stderr)
        return 1

    files = sorted(WORKFLOW_DIR.glob("*.yml")) + sorted(WORKFLOW_DIR.glob("*.yaml"))
    if not files:
        print(f"error: no workflows found in {WORKFLOW_DIR}", file=sys.stderr)
        return 1

    missing: list[str] = []
    checked = 0

    for path in files:
        try:
            doc = yaml.safe_load(path.read_text())
        except yaml.YAMLError as exc:
            print(f"error: {path.name} is not valid YAML: {exc}", file=sys.stderr)
            return 1

        if not isinstance(doc, dict):
            print(f"error: {path.name} is not a mapping", file=sys.stderr)
            return 1

        jobs = doc.get("jobs")
        if not isinstance(jobs, dict) or not jobs:
            print(f"error: {path.name} declares no jobs", file=sys.stderr)
            return 1

        for name, job in jobs.items():
            if not isinstance(job, dict):
                print(f"error: {path.name}: job {name} is not a mapping", file=sys.stderr)
                return 1
            if "uses" in job:
                continue
            checked += 1
            if "timeout-minutes" not in job:
                missing.append(f"{path.name}: {name}")

    if missing:
        print("jobs missing `timeout-minutes`:", file=sys.stderr)
        for entry in missing:
            print(f"  {entry}", file=sys.stderr)
        print(
            "\nAdd `timeout-minutes: <n>` beside `runs-on:`, sized to the job's "
            "real work. Without it the job inherits GitHub's 6-hour default.",
            file=sys.stderr,
        )
        return 1

    print(f"OK: {checked} jobs across {len(files)} workflows declare timeout-minutes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
