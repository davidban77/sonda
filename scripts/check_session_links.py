#!/usr/bin/env python3
"""No Claude Code session link in a pull request or in a commit reaching main.

A session link names a private session; once it is in a commit message on
`main` it is in every clone and cannot be removed without rewriting history.
This check fails a pull request whose title, body or commit messages carry
one, and a push to `main` whose commits do — the second case is the squash
commit itself, whose message is written at merge time and is never part of
the pull request.

The match is exact: the string ``claude.ai/code/session``, case-insensitive
(``PATTERN``). There is no opt-out.

Inputs, from the environment:

    COMMIT_RANGE    required: ``<from>..<to>`` or a single commit, given to
                    ``git rev-list``
    PR_EVENT_PATH   pull requests only: the path of the event payload
                    (``github.event_path``); the title and body are read from
                    its ``pull_request`` object. Unset means a push.

The title and body are read from the payload file, never passed as
environment values: Actions prints a step's ``env:`` at the top of its log,
so a body passed that way would put the very link being caught into a public
log before this script runs. Reading the file also keeps pull-request text out
of any shell.

Vacuity: the range must contain at least one commit, and for a pull request
the payload must parse and carry a non-empty title — a pull request always has
both, so their absence means the wiring broke, not that the input was clean.
A null body is a pull request with no body; that is the one input this check
cannot vouch for.

Findings name where the link is and redact the session id, so the check's own
log does not repeat what it found.

Exit 1 with every finding, 2 on a vacuous or broken input, 0 when clean.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys

PATTERN = re.compile(r"claude\.ai/code/session", re.IGNORECASE)
SESSION_ID = re.compile(r"(session)[_/-]?[A-Za-z0-9]+", re.IGNORECASE)


def redact(line: str) -> str:
    """Return ``line`` with every session id after a session link hidden."""
    return SESSION_ID.sub(r"\1_<redacted>", line)


def findings_in(label: str, text: str) -> list[str]:
    """Every line of ``text`` matching ``PATTERN``, as ``label line N: <redacted line>``."""
    return [
        f"{label} line {n}: {redact(line.strip())}"
        for n, line in enumerate(text.splitlines(), start=1)
        if PATTERN.search(line)
    ]


def commits_in(commit_range: str) -> list[tuple[str, str]]:
    """``(sha, message)`` for every commit in ``commit_range``, oldest first."""
    args = ["git", "rev-list", "--reverse", commit_range]
    if ".." not in commit_range:
        args.insert(2, "--max-count=1")
    shas = subprocess.run(args, check=True, capture_output=True, text=True).stdout.split()
    out = []
    for sha in shas:
        message = subprocess.run(
            ["git", "show", "-s", "--format=%B", sha],
            check=True, capture_output=True, text=True,
        ).stdout
        out.append((sha, message))
    return out


def pull_request_text(event_path: str) -> tuple[str, str]:
    """``(title, body)`` from the ``pull_request`` object of the event payload at ``event_path``.

    Raises ``ValueError`` when the file is unreadable, is not a pull-request
    payload, or has an empty title. A null body is returned as ``""``.
    """
    try:
        with open(event_path, encoding="utf-8") as f:
            pr = json.load(f).get("pull_request")
    except (OSError, json.JSONDecodeError) as e:
        raise ValueError(f"cannot read the event payload {event_path!r}: {e}") from e
    if not isinstance(pr, dict):
        raise ValueError(f"{event_path!r} has no pull_request object")
    title = pr.get("title") or ""
    if not title.strip():
        raise ValueError("the pull request title is empty; a pull request always has a title")
    return title, pr.get("body") or ""


def main() -> int:
    commit_range = os.environ.get("COMMIT_RANGE", "").strip()
    event_path = os.environ.get("PR_EVENT_PATH", "").strip()
    pr_mode = bool(event_path)
    if not commit_range:
        print("error: COMMIT_RANGE is empty; nothing to check", file=sys.stderr)
        return 2
    try:
        commits = commits_in(commit_range)
    except subprocess.CalledProcessError as e:
        print(f"error: git could not read {commit_range!r}: {e.stderr.strip()}", file=sys.stderr)
        return 2
    if not commits:
        print(f"error: {commit_range!r} contains no commits; refusing to report clean", file=sys.stderr)
        return 2

    found: list[str] = []
    if pr_mode:
        try:
            title, body = pull_request_text(event_path)
        except ValueError as e:
            print(f"error: {e}", file=sys.stderr)
            return 2
        found += findings_in("pull request title", title)
        found += findings_in("pull request body", body)
    for sha, message in commits:
        found += findings_in(f"commit {sha[:12]}", message)

    scanned = f"{len(commits)} commit(s)" + (", the title and the body" if pr_mode else "")
    if found:
        print(f"Claude Code session link found ({scanned} scanned):")
        for f in found:
            print(f"  {f}")
        print("Remove the link: from the title or body by editing the pull request,"
              " from a commit message by rewording the commit.")
        return 1
    print(f"OK: no Claude Code session link in {scanned}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
