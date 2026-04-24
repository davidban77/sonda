#!/usr/bin/env python3
"""Docs-drift catcher for sonda CLI commands referenced in user-facing documentation.

This script walks ``docs/site/docs/**/*.md``, extracts fenced ``bash`` code blocks,
finds ``sonda <subcommand>`` invocations, and verifies two properties:

1. **File existence** — when a command passes ``--scenario <path>`` pointing at a
   filesystem path (not a ``@builtin-name``), the path must exist on disk.
2. **Validity** — when the subcommand supports ``--dry-run``, the script invokes
   ``sonda`` with ``--dry-run`` appended and fails the check on a non-zero exit.

Runs as a zero-dependency, stdlib-only one-file drop-in. Intended to be invoked
from CI and locally via ``python3 scripts/validate_docs_commands.py`` from the
repo root.

Contributor notes
-----------------
- Motivation: PR #235 retroactively patched ``examples/alertmanager/alerting-scenario.yaml``
  after docs pointed users at a v1 YAML file that no longer compiled under v2. A CI
  step that greps runnable ``sonda ... --scenario <path>`` commands from markdown and
  dry-runs each one would have caught it automatically.
- Scope: this tool validates user-visible docs under ``docs/site/docs/``. Built-in
  catalog names (``@foo``) are left to sonda's runtime catalog probe — double-validating
  here is scope creep.
- Self-test: ``python3 scripts/validate_docs_commands.py --self-test`` runs unit tests
  on the parser logic without needing a ``sonda`` binary.
"""

from __future__ import annotations

import argparse
import dataclasses
import re
import shlex
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Iterable, Sequence

# --- Configuration -----------------------------------------------------------

DOCS_GLOB_ROOT = Path("docs/site/docs")
"""Docs root relative to the repo root."""

DOCS_GLOB_EXCLUDE = (Path("docs/site/site"),)
"""Build output directories to skip. mkdocs emits ``site/`` under ``docs/site/``."""

KNOWN_SUBCOMMANDS: frozenset[str] = frozenset(
    {
        "metrics",
        "logs",
        "histogram",
        "summary",
        "run",
        "catalog",
        "scenarios",
        "packs",
        "import",
        "init",
    }
)
"""Top-level ``sonda`` subcommands the script recognises. Anything else in a
``sonda <word>`` position is ignored (e.g., ``sonda --version`` or prose like
``sonda-server`` / ``sonda_core`` — those never match our regex anyway)."""

DRY_RUNNABLE_SINGLE: frozenset[str] = frozenset(
    {"metrics", "logs", "histogram", "summary", "run"}
)
"""Signal subcommands that accept ``--dry-run``."""

DRY_RUNNABLE_WITH_ACTION: frozenset[tuple[str, str]] = frozenset(
    {
        ("catalog", "run"),
        ("scenarios", "run"),
        ("packs", "run"),
    }
)
"""``<cmd> <action>`` pairs that accept ``--dry-run`` on the action."""

DEFAULT_SONDA_BINARY = Path("target/release/sonda")
"""Default path to the built sonda binary, relative to the repo root."""

REPO_RELATIVE_PATH_ROOTS: frozenset[str] = frozenset(
    {"examples", "scenarios", "packs", "docs", "tests"}
)
"""Top-level directory names that make a filesystem path "repo-relative".

The file-exists validation only fires on paths that start with one of these
roots (e.g. ``examples/foo.yaml``). Bare filenames like ``my-scenario.yaml``
or ``data.csv`` in the docs are tutorial placeholders the reader creates
locally — they aren't meant to exist in the repo. Metavar placeholders
containing shell-special characters (``<FILE>``, ``<FILE | @name>``) are also
filtered out at the extraction layer.
"""

DEFAULT_SUBPROCESS_TIMEOUT_S = 30.0
"""Per-invocation timeout when the script shells out to ``sonda --dry-run``."""


# --- Data model --------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class ExtractedCommand:
    """A single ``sonda`` invocation extracted from a markdown code block.

    Attributes:
        file: Absolute path to the markdown file the command was pulled from.
        line: 1-based line number of the first line of the (joined) command in
            the source markdown file. Stable for reporting.
        argv: Tokenised argument vector, starting with ``sonda``. Shell prompts
            and env-var prefixes are already stripped.
        raw: Original joined command line (continuations resolved, prompt stripped,
            env-var prefix stripped). For error reporting only.
        tutorial_titles: Set of paths that appear as ``title="..."`` on any
            code fence in the source markdown file. Commands that reference
            one of these paths are treated as tutorial examples and skip the
            file-exists + dry-run checks.
        block_is_tutorial: True when the bash block that contained this command
            already referenced a tutorial title. In that case, every path in
            the block is treated as tutorial material. This mirrors how docs
            authors actually write: a single bash block commonly demonstrates
            operations on a cluster of tutorial files shown above.
    """

    file: Path
    line: int
    argv: tuple[str, ...]
    raw: str
    tutorial_titles: frozenset[str] = dataclasses.field(default_factory=frozenset)
    block_is_tutorial: bool = False

    @property
    def subcommand(self) -> str | None:
        """Return the first recognised subcommand token, or ``None``.

        ``sonda --dry-run metrics ...`` → ``metrics`` (global flags skipped).
        """
        # Skip leading global flags (things starting with ``-``) when finding
        # the positional subcommand.
        for token in self.argv[1:]:
            if token.startswith("-"):
                # ``--scenario foo`` passes "foo" through as the next token;
                # but those come AFTER the subcommand by construction. Here we
                # only ever hit global flags (``--dry-run``, ``--quiet``,
                # ``--verbose``, ``--pack-path``, ``--scenario-path``,
                # ``--format``, short equivalents). Those with VALUES are
                # ``--pack-path <dir>``, ``--scenario-path <dir>``, ``--format <fmt>``.
                continue
            # If the previous token consumed a value we still want to skip it.
            # But the token.startswith("-") filter alone is enough as long as
            # flags don't use ``=value`` for global flags we care about — they
            # don't in practice in our docs.
            return token if token in KNOWN_SUBCOMMANDS else None
        return None

    @property
    def action(self) -> str | None:
        """Return the action verb for subcommands that have one.

        For ``sonda catalog run foo``, returns ``"run"``. For ``sonda metrics``,
        returns ``None``. This lets the caller differentiate
        ``catalog list`` (no dry-run) from ``catalog run`` (dry-run).
        """
        sub = self.subcommand
        if sub is None or sub not in {"catalog", "scenarios", "packs"}:
            return None
        # Find the subcommand position, then walk forward to the first non-flag token.
        seen_subcommand = False
        skip_next_value = False
        for token in self.argv[1:]:
            if skip_next_value:
                skip_next_value = False
                continue
            if not seen_subcommand:
                if token == sub:
                    seen_subcommand = True
                    continue
                # Global flag before subcommand — may carry a value.
                if token in {"--pack-path", "--scenario-path", "--format"}:
                    skip_next_value = True
                continue
            # After subcommand, skip any flags and their values.
            if token.startswith("-"):
                # Conservative: assume no flags *before* the action token.
                # Docs follow ``sonda catalog <action>`` consistently.
                continue
            return token
        return None


@dataclasses.dataclass
class ValidationResult:
    """Outcome of validating one :class:`ExtractedCommand`.

    Attributes:
        command: The command that was checked.
        ok: Overall pass/fail.
        message: Human-readable failure summary (empty when ``ok``).
    """

    command: ExtractedCommand
    ok: bool
    message: str = ""


# --- Markdown extraction -----------------------------------------------------

# Matches a fenced code block opener. Captures the info string (language).
# Allows any leading whitespace so that mkdocs-material admonition-nested
# fences (4-space indented) are also picked up. ``text`` markdown's 3-space
# rule is irrelevant here — mkdocs-material doesn't follow it strictly.
_FENCE_OPEN_RE = re.compile(r"^(?P<indent>[ \t]*)```(?P<info>[^\s`]*)")
_FENCE_CLOSE_RE = re.compile(r"^(?P<indent>[ \t]*)```\s*$")

# Matches an mkdocs-material ``title="..."`` attribute on any fenced block.
# mkdocs-material uses these to label code samples with the path the reader
# should save them to. When a path appears in a ``title=``, the file is
# treated as tutorial material the reader creates — not as a real repo file.
_FENCE_TITLE_RE = re.compile(r'title\s*=\s*"([^"]+)"')

# Strip a leading env-var assignment prefix: ``FOO=bar BAZ=qux sonda ...`` → ``sonda ...``
# We only strip assignments that precede the FIRST non-assignment token. Matches NAME=value
# where NAME is a shell-valid identifier (underscore + uppercase + digits) and value is
# anything until whitespace.
_ENV_ASSIGN_RE = re.compile(r"^[A-Z_][A-Z0-9_]*=[^ \t]*\s+")


def iter_markdown_files(docs_root: Path) -> list[Path]:
    """Return a sorted list of markdown files under ``docs_root``.

    Filters out files under any path in :data:`DOCS_GLOB_EXCLUDE`.
    """
    excluded = tuple(docs_root.parent / ex for ex in DOCS_GLOB_EXCLUDE)
    out: list[Path] = []
    for md in docs_root.rglob("*.md"):
        if any(ex in md.parents for ex in excluded):
            continue
        out.append(md)
    out.sort()
    return out


def extract_tutorial_file_titles(markdown_text: str) -> set[str]:
    """Return the set of ``title="..."`` values from ALL fenced blocks.

    Any path the docs present as a labelled code sample is considered
    tutorial material: the reader is expected to save the contents to that
    path themselves, so the path isn't required to exist in the repo. This
    suppresses false positives on pedagogical examples like
    ``examples/ci-high-memory-alert.yaml`` while still catching real drift
    on paths that are referenced in commands but NEVER shown as a titled
    code sample (the PR #235 regression class).
    """
    titles: set[str] = set()
    for line in markdown_text.splitlines():
        if "```" not in line:
            continue
        # Only consider lines that open a fence. Closing fences shouldn't
        # carry titles, but this check is cheap safety.
        m = _FENCE_OPEN_RE.match(line)
        if not m:
            continue
        tail = line[m.end() :]
        for tm in _FENCE_TITLE_RE.finditer(tail):
            titles.add(tm.group(1))
    return titles


def extract_bash_blocks(markdown_text: str) -> list[tuple[int, str]]:
    """Return ``(line_number, block_body)`` tuples for ``bash`` fenced blocks.

    Only fences whose info string is exactly ``bash`` (case-insensitive) are
    returned. ``text``, ``yaml``, ``json``, ``toml``, ``shell``, etc. are
    ignored. ``bash title="..."`` IS accepted (mkdocs allows titled fences).

    The line number is 1-based and points at the first line of content INSIDE
    the fence (the line following the opener), so downstream reporting aligns
    with what the reader sees.
    """
    lines = markdown_text.splitlines()
    blocks: list[tuple[int, str]] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        m = _FENCE_OPEN_RE.match(line)
        if not m:
            i += 1
            continue
        info_raw = m.group("info") or ""
        # The info string may carry a language then trailing attributes like
        # ``bash title="foo"`` — we split on whitespace, but attributes land on
        # the same raw info token only when mkdocs uses pymdownx.superfences.
        # In practice docs use plain ``bash`` or ``bash title="..."``. The line
        # after the fence marker holds the attributes when present. So split
        # the whole fence line's remainder.
        fence_tail = line[m.end() :].strip()
        if info_raw.lower() != "bash":
            # Not a bash block. Find the closing fence and move on.
            i += 1
            while i < len(lines):
                if _FENCE_CLOSE_RE.match(lines[i]):
                    i += 1
                    break
                i += 1
            continue
        # bash block — collect body until the closing fence.
        _ = fence_tail  # unused: present for clarity, ignored content
        start_body_line = i + 2  # 1-based line number of first body line
        i += 1
        body: list[str] = []
        while i < len(lines):
            if _FENCE_CLOSE_RE.match(lines[i]):
                i += 1
                break
            body.append(lines[i])
            i += 1
        blocks.append((start_body_line, "\n".join(body)))
    return blocks


def join_continuations(block_body: str) -> list[tuple[int, str]]:
    """Join shell line continuations (``\\`` at end of line).

    Returns ``(relative_line_offset, joined_line)`` tuples where
    ``relative_line_offset`` is the 0-based offset of the FIRST physical line
    of the joined logical line, relative to the block body.
    """
    physical = block_body.splitlines()
    logical: list[tuple[int, str]] = []
    buf: list[str] = []
    buf_start: int | None = None
    for idx, line in enumerate(physical):
        if not buf:
            buf_start = idx
        stripped_end = line.rstrip()
        if stripped_end.endswith("\\"):
            # Drop the trailing backslash and keep accumulating.
            buf.append(stripped_end[:-1])
            continue
        buf.append(line)
        logical.append((buf_start or 0, " ".join(s.strip() for s in buf).strip()))
        buf = []
        buf_start = None
    if buf:
        # Unterminated continuation — keep what we have.
        logical.append((buf_start or 0, " ".join(s.strip() for s in buf).strip()))
    return logical


def strip_prompt(line: str) -> str:
    """Strip a leading ``$ `` shell prompt, if present."""
    if line.startswith("$ "):
        return line[2:]
    return line


def strip_env_prefix(line: str) -> str:
    """Strip leading ``VAR=value ...`` env-var assignments from a command line.

    Iteratively removes assignment prefixes until the first token is a command.
    Handles multiple (``FOO=bar BAZ=qux sonda ...``).
    """
    while True:
        m = _ENV_ASSIGN_RE.match(line)
        if not m:
            return line
        line = line[m.end() :]


def _contains_cli_placeholder_token(tokens: Iterable[str]) -> bool:
    """Return True if any token is a CLI-usage placeholder like ``[OPTIONS]``.

    The usage syntax ``sonda metrics [OPTIONS]`` shows the invocation shape,
    not a runnable command. Detected by brackets or angle-brackets on either
    end of a token.
    """
    for tok in tokens:
        if tok.startswith(("<", "[")) or tok.endswith((">", "]")):
            return True
    return False


def _trim_shell_trailers(line: str) -> str:
    """Return the portion of the line before shell redirects, pipes to other
    commands, backgrounding, and trailing inline comments.

    Truncates at the first unescaped ``>``, ``<`` (as a redirect, not a lead
    token), ``&`` (as a backgrounder or ``&&``), or ``#`` preceded by whitespace
    (inline comment). Does NOT split on ``|`` — the caller handles pipelines
    separately because a sonda invocation may legitimately sit on either side
    of a pipe.
    """
    out: list[str] = []
    i = 0
    in_single = False
    in_double = False
    while i < len(line):
        ch = line[i]
        if ch == "'" and not in_double:
            in_single = not in_single
            out.append(ch)
            i += 1
            continue
        if ch == '"' and not in_single:
            in_double = not in_double
            out.append(ch)
            i += 1
            continue
        if in_single or in_double:
            out.append(ch)
            i += 1
            continue
        if ch == "\\" and i + 1 < len(line):
            # Escaped next char — keep both verbatim.
            out.append(ch)
            out.append(line[i + 1])
            i += 2
            continue
        if ch == "#" and (i == 0 or line[i - 1].isspace()):
            break
        if ch in (">", "<"):
            # Potential redirect. A free-standing ``>`` / ``<`` with whitespace
            # on BOTH sides (or ending the line) is a shell redirect and
            # truncates the command. A ``<`` followed immediately by a
            # non-space character is a metavar like ``<FILE>`` and stays in
            # the token. ``>`` after a token character stays (e.g. in a
            # ``--label`` value); after whitespace, it's a redirect.
            prev_is_space = i == 0 or line[i - 1].isspace()
            next_is_space = i + 1 >= len(line) or line[i + 1].isspace()
            if prev_is_space and (next_is_space or ch == ">"):
                break
            # Keep as part of current token.
            out.append(ch)
            i += 1
            continue
        if ch == "&":
            # ``&&`` or lone ``&`` → trim here.
            break
        out.append(ch)
        i += 1
    return "".join(out).rstrip()


def extract_sonda_commands(
    md_file: Path, markdown_text: str
) -> list[ExtractedCommand]:
    """Extract every ``sonda <known-subcommand> ...`` invocation from a markdown file.

    Returns a list sorted by file, then by line number. Only recognised subcommands
    from :data:`KNOWN_SUBCOMMANDS` qualify — bare ``sonda`` mentions, ``sonda-server``,
    ``sonda_core``, or ``sonda --version`` don't match and are excluded.
    """
    commands: list[ExtractedCommand] = []
    tutorial_titles = frozenset(extract_tutorial_file_titles(markdown_text))
    for block_line, block_body in extract_bash_blocks(markdown_text):
        # A block is "tutorial" if any command inside it references a path
        # shown as a code-fence title elsewhere in the file. Compute this once
        # per block so every command in the block gets the same verdict.
        block_is_tutorial = any(
            title in block_body for title in tutorial_titles
        )
        for rel_offset, raw_line in join_continuations(block_body):
            stripped = raw_line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            cleaned = strip_prompt(stripped)
            cleaned = strip_env_prefix(cleaned)
            # Fast filter: must have "sonda" in it somewhere.
            if "sonda" not in cleaned:
                continue
            # Handle pipelines / command chains: split on `|`, `&&`, `||`, `;`
            # so that `sonda metrics ... | curl ...` still parses the sonda part.
            # This is conservative and may over-split — but for validation we
            # only care about the leading sonda invocation.
            segments = re.split(r"\s*(?:\|\||&&|;|\|)\s*", cleaned)
            for seg in segments:
                seg = seg.strip()
                if not seg.startswith("sonda "):
                    # Must start with ``sonda `` — this filters out things like
                    # ``cargo run -- sonda metrics ...`` (we don't validate those),
                    # ``# sonda init --help`` (comments already filtered),
                    # ``sonda-server``, ``sonda_core`` (they don't have a space).
                    continue
                # Trim shell redirects, backgrounding, and inline comments
                # before tokenising — they'd fail clap parsing if passed
                # through verbatim.
                seg = _trim_shell_trailers(seg)
                if not seg:
                    continue
                try:
                    tokens = tuple(shlex.split(seg, comments=True))
                except ValueError:
                    # Unbalanced quotes or other shell-parse error. Skip rather
                    # than crash — this is a docs tool, not a shell emulator.
                    continue
                if not tokens or tokens[0] != "sonda":
                    continue
                # Skip CLI-usage syntax examples like
                # ``sonda metrics [OPTIONS]`` or ``sonda run --scenario <FILE>``.
                # These are reference documentation, not runnable commands.
                if _contains_cli_placeholder_token(tokens):
                    continue
                cmd = ExtractedCommand(
                    file=md_file,
                    line=block_line + rel_offset,
                    argv=tokens,
                    raw=seg,
                    tutorial_titles=tutorial_titles,
                    block_is_tutorial=block_is_tutorial,
                )
                # Only count commands with a recognised subcommand.
                if cmd.subcommand is None:
                    continue
                commands.append(cmd)
    return commands


# --- Validation --------------------------------------------------------------


def is_metavar_placeholder(path: str) -> bool:
    """Return True for CLI-reference metavar placeholders like ``<FILE>``.

    Placeholders in docs aren't real filesystem paths — they're usage syntax.
    Detected by the presence of an unescaped ``<`` or ``>``.
    """
    return "<" in path or ">" in path


def is_repo_relative_path(path: str) -> bool:
    """Return True when ``path`` looks like a repo-relative file reference.

    Only paths whose first segment is in :data:`REPO_RELATIVE_PATH_ROOTS` qualify
    for file-exists validation. Bare filenames, absolute paths, ``~/`` paths,
    and metavar placeholders all return False.
    """
    if is_metavar_placeholder(path):
        return False
    if path.startswith(("/", "~", "@")):
        return False
    # Split on both POSIX and Windows separators to be safe; docs are POSIX but
    # this is cheap insurance against odd path styles sneaking in.
    first, sep, _rest = path.partition("/")
    if not sep:
        return False
    return first in REPO_RELATIVE_PATH_ROOTS


def extract_scenario_path(argv: Sequence[str]) -> str | None:
    """Return the ``--scenario`` path value, or ``None`` if absent.

    Handles both ``--scenario foo`` and ``--scenario=foo`` forms. ``@name``
    values are returned as-is; the caller decides whether to skip them.
    """
    for idx, tok in enumerate(argv):
        if tok == "--scenario":
            if idx + 1 < len(argv):
                return argv[idx + 1]
            return None
        if tok.startswith("--scenario="):
            return tok[len("--scenario=") :]
    return None


def extract_import_or_from_file(argv: Sequence[str]) -> str | None:
    """Return a referenced file path for ``import <file>`` and ``init --from <val>``.

    For ``import <file>``, returns the positional file arg (skipping flags).
    For ``init --from <val>``, returns the value only if it doesn't start with ``@``.
    Otherwise returns ``None``.
    """
    if len(argv) < 2:
        return None
    sub = None
    # Find the first known subcommand token, skipping global flags.
    for tok in argv[1:]:
        if tok.startswith("-"):
            continue
        sub = tok
        break
    if sub == "import":
        # First positional after "import" that isn't a flag or a flag value.
        skip_next = False
        seen_import = False
        for tok in argv[1:]:
            if skip_next:
                skip_next = False
                continue
            if not seen_import:
                if tok == "import":
                    seen_import = True
                continue
            if tok.startswith("-"):
                # Flags that take values on import: -o, --output, --columns,
                # --rate, --duration.
                if tok in {"-o", "--output", "--columns", "--rate", "--duration"}:
                    skip_next = True
                elif "=" in tok:
                    # ``-o=foo`` style — nothing to skip.
                    pass
                continue
            return tok
        return None
    if sub == "init":
        for idx, tok in enumerate(argv):
            if tok == "--from":
                if idx + 1 < len(argv):
                    val = argv[idx + 1]
                    if val.startswith("@"):
                        return None
                    return val
                return None
            if tok.startswith("--from="):
                val = tok[len("--from=") :]
                if val.startswith("@"):
                    return None
                return val
        return None
    return None


def supports_dry_run(cmd: ExtractedCommand) -> bool:
    """Return True when the command's subcommand (and action, if any) supports ``--dry-run``.

    Special case: ``sonda import ... --run`` accepts ``--dry-run`` too. Plain
    ``sonda import --analyze`` or ``sonda import -o foo.yaml`` do not.
    """
    sub = cmd.subcommand
    if sub is None:
        return False
    if sub in DRY_RUNNABLE_SINGLE:
        return True
    action = cmd.action
    if action is not None and (sub, action) in DRY_RUNNABLE_WITH_ACTION:
        return True
    if sub == "import" and "--run" in cmd.argv:
        return True
    return False


def validate_command(
    cmd: ExtractedCommand,
    repo_root: Path,
    sonda_bin: Path | None,
    subprocess_timeout: float = DEFAULT_SUBPROCESS_TIMEOUT_S,
) -> ValidationResult:
    """Run the file-exists + dry-run checks on a single extracted command."""
    scenario_path = extract_scenario_path(cmd.argv)
    scenario_is_repo_path = (
        scenario_path is not None
        and not scenario_path.startswith("@")
        and is_repo_relative_path(scenario_path)
        and scenario_path not in cmd.tutorial_titles
        and not cmd.block_is_tutorial
    )
    if scenario_is_repo_path:
        target = (repo_root / scenario_path).resolve()  # type: ignore[arg-type]
        if not target.exists():
            return ValidationResult(
                command=cmd,
                ok=False,
                message=(
                    f"--scenario path does not exist: {scenario_path} "
                    f"(resolved to {target})"
                ),
            )

    referenced_file = extract_import_or_from_file(cmd.argv)
    if (
        referenced_file is not None
        and is_repo_relative_path(referenced_file)
        and referenced_file not in cmd.tutorial_titles
        and not cmd.block_is_tutorial
    ):
        target = (repo_root / referenced_file).resolve()
        if not target.exists():
            return ValidationResult(
                command=cmd,
                ok=False,
                message=(
                    f"referenced file does not exist: {referenced_file} "
                    f"(resolved to {target})"
                ),
            )

    if not supports_dry_run(cmd):
        return ValidationResult(command=cmd, ok=True)

    # Only dry-run when the command's file references resolve. A tutorial-style
    # command like ``sonda run --scenario my-scenario.yaml`` would fail dry-run
    # with a "file not found" that isn't docs drift — it's the reader's TODO.
    if scenario_path is not None and not scenario_path.startswith("@"):
        if not scenario_is_repo_path:
            return ValidationResult(command=cmd, ok=True)
    # ``@name`` refs defer to sonda's catalog probe — per the brief, validating
    # catalog entry names against the actual catalog is out of scope. Skip.
    if scenario_path is not None and scenario_path.startswith("@"):
        return ValidationResult(command=cmd, ok=True)
    # ``sonda catalog run <name>``, ``scenarios run <name>``, ``packs run <name>``
    # — the ``<name>`` arg is a catalog lookup same as ``@name``. Out of scope.
    if cmd.action == "run" and cmd.subcommand in {"catalog", "scenarios", "packs"}:
        return ValidationResult(command=cmd, ok=True)
    # Tutorial blocks: reader is expected to create the referenced files.
    if cmd.block_is_tutorial:
        return ValidationResult(command=cmd, ok=True)
    # ``sonda import <file>`` / ``sonda init --from <file>`` with a bare,
    # non-repo-relative filename — tutorial placeholder the reader provides.
    if referenced_file is not None and not is_repo_relative_path(referenced_file):
        return ValidationResult(command=cmd, ok=True)

    if sonda_bin is None:
        # File-exists check already happened; no binary available to dry-run.
        return ValidationResult(command=cmd, ok=True)

    dry_run_argv = _build_dry_run_argv(cmd, sonda_bin)
    try:
        proc = subprocess.run(
            dry_run_argv,
            cwd=str(repo_root),
            capture_output=True,
            timeout=subprocess_timeout,
            check=False,
            text=True,
        )
    except subprocess.TimeoutExpired:
        return ValidationResult(
            command=cmd,
            ok=False,
            message=f"dry-run timed out after {subprocess_timeout:.0f}s: "
            f"{' '.join(dry_run_argv)}",
        )
    except FileNotFoundError:
        return ValidationResult(
            command=cmd,
            ok=False,
            message=f"sonda binary not found: {sonda_bin}",
        )

    if proc.returncode == 0:
        return ValidationResult(command=cmd, ok=True)
    stderr = proc.stderr.strip()
    # Trim stderr to the first ~20 lines to keep error output scannable.
    stderr_lines = stderr.splitlines()
    if len(stderr_lines) > 20:
        stderr = "\n".join(stderr_lines[:20]) + "\n    ..."
    return ValidationResult(
        command=cmd,
        ok=False,
        message=(
            f"dry-run exited {proc.returncode}: {' '.join(dry_run_argv)}\n"
            f"    stderr: {stderr}"
        ),
    )


def _build_dry_run_argv(
    cmd: ExtractedCommand, sonda_bin: Path
) -> list[str]:
    """Build the argv the script actually executes for a dry-run.

    Strategy: replace ``sonda`` with the binary path, strip trailing shell
    operators (already split upstream), and inject ``--dry-run`` as a global
    flag right after the binary. If ``--dry-run`` is already present we leave
    it alone.
    """
    argv = list(cmd.argv)
    argv[0] = str(sonda_bin)
    if "--dry-run" not in argv:
        argv.insert(1, "--dry-run")
    return argv


# --- Orchestration -----------------------------------------------------------


def find_repo_root(start: Path) -> Path:
    """Walk up from ``start`` until a directory with a ``Cargo.toml`` is found."""
    current = start.resolve()
    while True:
        if (current / "Cargo.toml").is_file():
            return current
        if current.parent == current:
            raise RuntimeError(
                "could not locate repo root: no Cargo.toml in any parent of "
                f"{start}"
            )
        current = current.parent


def run_validation(
    repo_root: Path,
    sonda_bin: Path | None,
    subprocess_timeout: float = DEFAULT_SUBPROCESS_TIMEOUT_S,
    skip_files: Iterable[str] = (),
) -> tuple[int, list[ValidationResult]]:
    """Run the full validation pass. Returns ``(checked_count, failures)``.

    Args:
        repo_root: Path to the repository root (where ``Cargo.toml`` lives).
        sonda_bin: Path to a built sonda binary, or ``None`` to skip dry-runs.
        subprocess_timeout: Per-invocation timeout for dry-run calls.
        skip_files: Iterable of repo-relative markdown paths to exclude entirely
            (e.g., ``"docs/site/docs/guides/e2e-testing.md"``). Useful when a
            doc is known-broken and a fix is tracked in a separate follow-up.
    """
    docs_root = repo_root / DOCS_GLOB_ROOT
    if not docs_root.is_dir():
        raise RuntimeError(f"docs root not found: {docs_root}")

    skip_set = {str(s) for s in skip_files}
    md_files = iter_markdown_files(docs_root)
    all_commands: list[ExtractedCommand] = []
    for md in md_files:
        rel = str(md.relative_to(repo_root)) if md.is_absolute() else str(md)
        if rel in skip_set:
            continue
        text = md.read_text(encoding="utf-8")
        all_commands.extend(extract_sonda_commands(md, text))

    failures: list[ValidationResult] = []
    for cmd in all_commands:
        result = validate_command(
            cmd, repo_root=repo_root, sonda_bin=sonda_bin,
            subprocess_timeout=subprocess_timeout,
        )
        if not result.ok:
            failures.append(result)
    return len(all_commands), failures


def format_failure(result: ValidationResult, repo_root: Path) -> str:
    """Render one failure as a multi-line string suitable for CI logs."""
    try:
        rel = result.command.file.relative_to(repo_root)
    except ValueError:
        rel = result.command.file
    return (
        f"FAIL {rel}:{result.command.line}\n"
        f"    {result.command.raw}\n"
        f"    {result.message}"
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Validate that sonda CLI commands in user-facing docs still work.",
    )
    parser.add_argument(
        "--sonda",
        type=Path,
        default=None,
        help=(
            "Path to the sonda binary. Defaults to "
            f"{DEFAULT_SONDA_BINARY} relative to the repo root. "
            "Pass --no-binary to skip dry-run execution entirely."
        ),
    )
    parser.add_argument(
        "--no-binary",
        action="store_true",
        help="Skip all dry-run invocations; file-exists checks still run.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_SUBPROCESS_TIMEOUT_S,
        help="Per-invocation timeout in seconds for dry-run commands.",
    )
    parser.add_argument(
        "--skip-file",
        action="append",
        default=[],
        metavar="PATH",
        help=(
            "Repo-relative markdown path to skip entirely. Repeatable. "
            "Intended as a temporary escape hatch while a docs-drift fix "
            "is tracked in a separate PR — prefer fixing the docs."
        ),
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run the script's inline unit tests and exit.",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return _run_self_tests()

    repo_root = find_repo_root(Path(__file__).parent)

    sonda_bin: Path | None
    if args.no_binary:
        sonda_bin = None
    else:
        raw_bin = args.sonda or (repo_root / DEFAULT_SONDA_BINARY)
        raw_bin = raw_bin if raw_bin.is_absolute() else (repo_root / raw_bin)
        if not raw_bin.is_file():
            print(
                f"sonda binary not found at {raw_bin}. "
                "Build it first (cargo build --release -p sonda) or pass "
                "--no-binary to skip dry-run execution.",
                file=sys.stderr,
            )
            return 2
        sonda_bin = raw_bin

    checked, failures = run_validation(
        repo_root=repo_root, sonda_bin=sonda_bin,
        subprocess_timeout=args.timeout,
        skip_files=args.skip_file,
    )
    for failure in failures:
        print(format_failure(failure, repo_root), file=sys.stderr)
    print(
        f"{checked} commands checked, {len(failures)} failed",
        file=sys.stderr,
    )
    return 0 if not failures else 1


# --- Self-tests --------------------------------------------------------------


class _ExtractBashBlocksTests(unittest.TestCase):
    def test_extracts_only_bash_blocks(self) -> None:
        md = (
            "intro\n"
            "```yaml\n"
            "not: this\n"
            "```\n"
            "\n"
            "```bash\n"
            "sonda metrics --name up --rate 1 --duration 5s\n"
            "```\n"
            "\n"
            "```text\n"
            "nor this\n"
            "```\n"
        )
        blocks = extract_bash_blocks(md)
        self.assertEqual(len(blocks), 1)
        self.assertIn("sonda metrics", blocks[0][1])

    def test_indented_bash_block_in_admonition(self) -> None:
        md = (
            "!!! tip\n"
            "    ```bash\n"
            "    sonda --dry-run metrics --name up --rate 1 --duration 5s\n"
            "    ```\n"
        )
        blocks = extract_bash_blocks(md)
        self.assertEqual(len(blocks), 1)
        self.assertIn("sonda --dry-run metrics", blocks[0][1])

    def test_empty_fence_info_is_not_bash(self) -> None:
        md = "```\nsonda metrics --rate 1\n```\n"
        self.assertEqual(extract_bash_blocks(md), [])

    def test_line_number_points_at_first_body_line(self) -> None:
        md = "line 1\nline 2\n```bash\nsonda metrics\n```\n"
        blocks = extract_bash_blocks(md)
        self.assertEqual(len(blocks), 1)
        self.assertEqual(blocks[0][0], 4)


class _JoinContinuationsTests(unittest.TestCase):
    def test_joins_backslash_continuation(self) -> None:
        body = "sonda metrics \\\n  --name up --rate 1 --duration 5s"
        out = join_continuations(body)
        self.assertEqual(len(out), 1)
        self.assertEqual(
            out[0][1], "sonda metrics --name up --rate 1 --duration 5s"
        )
        self.assertEqual(out[0][0], 0)

    def test_two_separate_lines_stay_separate(self) -> None:
        body = "sonda metrics --rate 1\nsonda logs --rate 5"
        out = join_continuations(body)
        self.assertEqual(len(out), 2)
        self.assertEqual(out[1][0], 1)

    def test_empty_body(self) -> None:
        self.assertEqual(join_continuations(""), [])


class _StripPromptAndEnvTests(unittest.TestCase):
    def test_strips_dollar_prompt(self) -> None:
        self.assertEqual(strip_prompt("$ sonda metrics"), "sonda metrics")

    def test_no_prompt_passthrough(self) -> None:
        self.assertEqual(strip_prompt("sonda metrics"), "sonda metrics")

    def test_strips_single_env_var(self) -> None:
        self.assertEqual(
            strip_env_prefix("RUST_LOG=debug sonda metrics"),
            "sonda metrics",
        )

    def test_strips_multiple_env_vars(self) -> None:
        self.assertEqual(
            strip_env_prefix("RUST_LOG=debug SONDA_FOO=bar sonda metrics"),
            "sonda metrics",
        )

    def test_env_prefix_not_stripped_from_middle(self) -> None:
        # An env var in the middle is NOT an assignment prefix.
        self.assertEqual(
            strip_env_prefix("sonda metrics RUST_LOG=debug"),
            "sonda metrics RUST_LOG=debug",
        )


class _TrimShellTrailersTests(unittest.TestCase):
    def test_trims_redirect(self) -> None:
        self.assertEqual(
            _trim_shell_trailers("sonda metrics --rate 1 > /tmp/out.txt"),
            "sonda metrics --rate 1",
        )

    def test_trims_background(self) -> None:
        self.assertEqual(
            _trim_shell_trailers("sonda metrics --rate 1 &"),
            "sonda metrics --rate 1",
        )

    def test_trims_inline_comment(self) -> None:
        self.assertEqual(
            _trim_shell_trailers("sonda metrics --rate 1   # comment"),
            "sonda metrics --rate 1",
        )

    def test_preserves_hash_inside_token(self) -> None:
        # ``--label x=foo#bar`` — hash inside a token (no leading whitespace)
        # must survive.
        self.assertEqual(
            _trim_shell_trailers("sonda metrics --label x=foo#bar"),
            "sonda metrics --label x=foo#bar",
        )

    def test_preserves_less_than_inside_token(self) -> None:
        # Our metavar detection strips ``<FILE>`` tokens AFTER shlex, so
        # trim-shell-trailers must NOT eat the ``<`` when it's embedded in
        # a token. Leading whitespace before ``<`` DOES signal a redirect.
        self.assertEqual(
            _trim_shell_trailers("sonda metrics --rate=<FOO>"),
            "sonda metrics --rate=<FOO>",
        )
        self.assertEqual(
            _trim_shell_trailers("sonda metrics < /tmp/in"),
            "sonda metrics",
        )

    def test_preserves_inside_double_quotes(self) -> None:
        # Shell operators inside quoted strings are literal.
        self.assertEqual(
            _trim_shell_trailers('sonda metrics --label "a>b&c"'),
            'sonda metrics --label "a>b&c"',
        )


class _CliPlaceholderTokenTests(unittest.TestCase):
    def test_brackets_are_placeholder(self) -> None:
        self.assertTrue(
            _contains_cli_placeholder_token(("sonda", "metrics", "[OPTIONS]"))
        )

    def test_angle_brackets_are_placeholder(self) -> None:
        self.assertTrue(
            _contains_cli_placeholder_token(("sonda", "run", "--scenario", "<FILE>"))
        )

    def test_normal_command_is_not(self) -> None:
        self.assertFalse(
            _contains_cli_placeholder_token(("sonda", "metrics", "--rate", "1"))
        )


class _ExtractSondaCommandsTests(unittest.TestCase):
    def _extract(self, md: str) -> list[ExtractedCommand]:
        return extract_sonda_commands(Path("/tmp/doc.md"), md)

    def test_finds_basic_invocation(self) -> None:
        md = "```bash\nsonda metrics --name up --rate 1 --duration 5s\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].subcommand, "metrics")

    def test_ignores_non_bash_fences(self) -> None:
        md = "```text\nsonda metrics --rate 1\n```\n"
        self.assertEqual(self._extract(md), [])

    def test_ignores_yaml_fences_with_sonda_comments(self) -> None:
        md = "```yaml\n# sonda run --scenario foo.yaml\nversion: 2\n```\n"
        self.assertEqual(self._extract(md), [])

    def test_ignores_json_fences(self) -> None:
        md = '```json\n{"sonda": "metrics"}\n```\n'
        self.assertEqual(self._extract(md), [])

    def test_ignores_bare_sonda_in_prose(self) -> None:
        md = "Sonda has a `sonda` binary. Also sonda-server."
        self.assertEqual(self._extract(md), [])

    def test_ignores_sonda_server_and_sonda_core(self) -> None:
        md = "```bash\nsonda-server --port 8080\ncargo run -p sonda_core\n```\n"
        self.assertEqual(self._extract(md), [])

    def test_ignores_sonda_version_flag_only(self) -> None:
        # ``sonda --version`` has no known subcommand → excluded.
        md = "```bash\nsonda --version\n```\n"
        self.assertEqual(self._extract(md), [])

    def test_ignores_commented_sonda_line(self) -> None:
        md = "```bash\n# sonda init --help\nsonda metrics --name up --rate 1 --duration 5s\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].subcommand, "metrics")

    def test_line_continuation_joined(self) -> None:
        md = (
            "```bash\n"
            "sonda metrics \\\n"
            "  --name up --rate 1 --duration 5s\n"
            "```\n"
        )
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertIn("--duration", " ".join(out[0].argv))

    def test_strips_prompt_and_env_prefix(self) -> None:
        md = "```bash\n$ RUST_LOG=debug sonda metrics --rate 1\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].argv[0], "sonda")
        self.assertEqual(out[0].subcommand, "metrics")

    def test_at_name_scenario_passes_through(self) -> None:
        md = "```bash\nsonda metrics --scenario @cpu-spike --rate 5\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(extract_scenario_path(out[0].argv), "@cpu-spike")

    def test_pipeline_first_sonda_segment_parsed(self) -> None:
        md = (
            "```bash\n"
            "sonda metrics --rate 1 | curl -s --data-binary @- http://x\n"
            "```\n"
        )
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].subcommand, "metrics")

    def test_ignores_cli_syntax_placeholder(self) -> None:
        md = "```bash\nsonda metrics [OPTIONS]\n```\n"
        self.assertEqual(self._extract(md), [])

    def test_ignores_cli_angle_bracket_placeholder(self) -> None:
        md = "```bash\nsonda run --scenario <FILE>\n```\n"
        self.assertEqual(self._extract(md), [])

    def test_strips_shell_redirect(self) -> None:
        md = "```bash\nsonda metrics --rate 1 > /tmp/out.txt\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertNotIn(">", out[0].argv)

    def test_strips_background_ampersand(self) -> None:
        md = "```bash\nsonda metrics --rate 1 &\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertNotIn("&", out[0].argv)

    def test_strips_inline_comment(self) -> None:
        md = "```bash\nsonda metrics --rate 1  # inline comment\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].argv[-1], "1")

    def test_dry_run_global_flag_recognised_as_metrics_subcommand(self) -> None:
        md = "```bash\nsonda --dry-run metrics --name up --rate 1\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].subcommand, "metrics")

    def test_catalog_run_action_detected(self) -> None:
        md = "```bash\nsonda --dry-run catalog run cpu-spike\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].subcommand, "catalog")
        self.assertEqual(out[0].action, "run")

    def test_catalog_list_has_no_action_dry_run(self) -> None:
        md = "```bash\nsonda catalog list\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].subcommand, "catalog")
        self.assertEqual(out[0].action, "list")
        self.assertFalse(supports_dry_run(out[0]))

    def test_scenarios_run_action_dry_run(self) -> None:
        md = "```bash\nsonda scenarios run cpu-spike\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertTrue(supports_dry_run(out[0]))

    def test_init_subcommand_no_dry_run(self) -> None:
        md = "```bash\nsonda init --help\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0].subcommand, "init")
        self.assertFalse(supports_dry_run(out[0]))

    def test_import_analyze_no_dry_run(self) -> None:
        md = "```bash\nsonda import data.csv --analyze\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertFalse(supports_dry_run(out[0]))

    def test_import_run_has_dry_run(self) -> None:
        md = "```bash\nsonda import data.csv --run --duration 30s\n```\n"
        out = self._extract(md)
        self.assertEqual(len(out), 1)
        self.assertTrue(supports_dry_run(out[0]))


class _ExtractScenarioPathTests(unittest.TestCase):
    def test_separate_value(self) -> None:
        argv = ("sonda", "metrics", "--scenario", "foo.yaml")
        self.assertEqual(extract_scenario_path(argv), "foo.yaml")

    def test_equals_value(self) -> None:
        argv = ("sonda", "metrics", "--scenario=foo.yaml")
        self.assertEqual(extract_scenario_path(argv), "foo.yaml")

    def test_at_name(self) -> None:
        argv = ("sonda", "metrics", "--scenario", "@cpu-spike")
        self.assertEqual(extract_scenario_path(argv), "@cpu-spike")

    def test_absent(self) -> None:
        argv = ("sonda", "metrics", "--rate", "1")
        self.assertIsNone(extract_scenario_path(argv))


class _ExtractImportOrFromFileTests(unittest.TestCase):
    def test_import_positional(self) -> None:
        argv = ("sonda", "import", "data.csv", "--analyze")
        self.assertEqual(extract_import_or_from_file(argv), "data.csv")

    def test_import_with_output_flag_before_positional(self) -> None:
        # Unusual but valid: -o consumes next arg, then positional follows.
        argv = ("sonda", "import", "-o", "out.yaml", "data.csv")
        self.assertEqual(extract_import_or_from_file(argv), "data.csv")

    def test_init_from_path(self) -> None:
        argv = ("sonda", "init", "--from", "data.csv")
        self.assertEqual(extract_import_or_from_file(argv), "data.csv")

    def test_init_from_at_name_returns_none(self) -> None:
        argv = ("sonda", "init", "--from", "@cpu-spike")
        self.assertIsNone(extract_import_or_from_file(argv))

    def test_init_from_equals_path(self) -> None:
        argv = ("sonda", "init", "--from=data.csv")
        self.assertEqual(extract_import_or_from_file(argv), "data.csv")

    def test_metrics_has_no_import_file(self) -> None:
        argv = ("sonda", "metrics", "--rate", "1")
        self.assertIsNone(extract_import_or_from_file(argv))


class _ExtractTutorialTitlesTests(unittest.TestCase):
    def test_collects_yaml_title(self) -> None:
        md = (
            '```yaml title="examples/foo.yaml"\n'
            "version: 2\n"
            "```\n"
        )
        self.assertEqual(
            extract_tutorial_file_titles(md), {"examples/foo.yaml"}
        )

    def test_collects_multiple_titles(self) -> None:
        md = (
            '```yaml title="examples/foo.yaml"\n'
            "version: 2\n"
            "```\n"
            '```bash title="run.sh"\n'
            "sonda metrics --rate 1\n"
            "```\n"
        )
        self.assertEqual(
            extract_tutorial_file_titles(md),
            {"examples/foo.yaml", "run.sh"},
        )

    def test_no_titles_returns_empty(self) -> None:
        md = "```yaml\nversion: 2\n```\n"
        self.assertEqual(extract_tutorial_file_titles(md), set())

    def test_extract_command_carries_titles(self) -> None:
        md = (
            '```yaml title="examples/tutorial.yaml"\n'
            "version: 2\n"
            "```\n"
            "```bash\n"
            "sonda metrics --scenario examples/tutorial.yaml\n"
            "```\n"
        )
        cmds = extract_sonda_commands(Path("/tmp/x.md"), md)
        self.assertEqual(len(cmds), 1)
        self.assertIn("examples/tutorial.yaml", cmds[0].tutorial_titles)

    def test_block_marked_tutorial_when_it_mentions_a_title(self) -> None:
        md = (
            '```yaml title="examples/rule-a.yaml"\n'
            "version: 2\n"
            "```\n"
            "```bash\n"
            "sonda metrics --scenario examples/rule-a.yaml\n"
            "sonda run --scenario examples/rule-cluster.yaml\n"
            "```\n"
        )
        cmds = extract_sonda_commands(Path("/tmp/x.md"), md)
        self.assertEqual(len(cmds), 2)
        # Both commands in the block are tagged tutorial because the block
        # references a path that's declared as a tutorial title.
        self.assertTrue(cmds[0].block_is_tutorial)
        self.assertTrue(cmds[1].block_is_tutorial)

    def test_block_not_tutorial_when_it_mentions_no_title(self) -> None:
        md = (
            "```bash\n"
            "sonda metrics --scenario examples/real-file.yaml\n"
            "```\n"
        )
        cmds = extract_sonda_commands(Path("/tmp/x.md"), md)
        self.assertEqual(len(cmds), 1)
        self.assertFalse(cmds[0].block_is_tutorial)


class _RepoRelativePathTests(unittest.TestCase):
    def test_examples_path_is_repo_relative(self) -> None:
        self.assertTrue(is_repo_relative_path("examples/foo.yaml"))

    def test_scenarios_path_is_repo_relative(self) -> None:
        self.assertTrue(is_repo_relative_path("scenarios/link-failover.yaml"))

    def test_tests_path_is_repo_relative(self) -> None:
        self.assertTrue(is_repo_relative_path("tests/alerts/high-cpu.yaml"))

    def test_bare_filename_is_not_repo_relative(self) -> None:
        self.assertFalse(is_repo_relative_path("my-scenario.yaml"))
        self.assertFalse(is_repo_relative_path("data.csv"))

    def test_absolute_path_is_not_repo_relative(self) -> None:
        self.assertFalse(is_repo_relative_path("/tmp/foo.yaml"))

    def test_home_path_is_not_repo_relative(self) -> None:
        self.assertFalse(is_repo_relative_path("~/foo.yaml"))

    def test_unknown_root_is_not_repo_relative(self) -> None:
        # ``mydir/foo.yaml`` — the first segment isn't a known root.
        self.assertFalse(is_repo_relative_path("mydir/foo.yaml"))

    def test_metavar_placeholder_is_not_repo_relative(self) -> None:
        self.assertFalse(is_repo_relative_path("<FILE>"))
        self.assertFalse(is_repo_relative_path("<FILE | @name>"))

    def test_at_name_is_not_repo_relative(self) -> None:
        self.assertFalse(is_repo_relative_path("@cpu-spike"))

    def test_metavar_placeholder_detection(self) -> None:
        self.assertTrue(is_metavar_placeholder("<FILE>"))
        self.assertTrue(is_metavar_placeholder("<FILE | @name>"))
        self.assertFalse(is_metavar_placeholder("examples/foo.yaml"))


class _SupportsDryRunTests(unittest.TestCase):
    def _cmd(self, raw: str) -> ExtractedCommand:
        return ExtractedCommand(
            file=Path("/tmp/x.md"),
            line=1,
            argv=tuple(shlex.split(raw)),
            raw=raw,
        )

    def test_metrics_yes(self) -> None:
        self.assertTrue(supports_dry_run(self._cmd("sonda metrics --rate 1")))

    def test_logs_yes(self) -> None:
        self.assertTrue(supports_dry_run(self._cmd("sonda logs --rate 1")))

    def test_histogram_yes(self) -> None:
        self.assertTrue(supports_dry_run(self._cmd("sonda histogram --scenario foo")))

    def test_summary_yes(self) -> None:
        self.assertTrue(supports_dry_run(self._cmd("sonda summary --scenario foo")))

    def test_run_yes(self) -> None:
        self.assertTrue(supports_dry_run(self._cmd("sonda run --scenario foo")))

    def test_catalog_run_yes(self) -> None:
        self.assertTrue(supports_dry_run(self._cmd("sonda catalog run foo")))

    def test_catalog_list_no(self) -> None:
        self.assertFalse(supports_dry_run(self._cmd("sonda catalog list")))

    def test_scenarios_list_no(self) -> None:
        self.assertFalse(supports_dry_run(self._cmd("sonda scenarios list")))

    def test_packs_run_yes(self) -> None:
        self.assertTrue(supports_dry_run(self._cmd("sonda packs run foo")))

    def test_init_no(self) -> None:
        self.assertFalse(supports_dry_run(self._cmd("sonda init --help")))

    def test_import_analyze_no(self) -> None:
        self.assertFalse(supports_dry_run(self._cmd("sonda import data.csv --analyze")))

    def test_import_with_output_no(self) -> None:
        self.assertFalse(
            supports_dry_run(self._cmd("sonda import data.csv -o out.yaml"))
        )

    def test_import_run_yes(self) -> None:
        self.assertTrue(
            supports_dry_run(self._cmd("sonda import data.csv --run --duration 30s"))
        )


def _run_self_tests() -> int:
    loader = unittest.TestLoader()
    suite = unittest.TestSuite()
    for cls in (
        _ExtractBashBlocksTests,
        _JoinContinuationsTests,
        _StripPromptAndEnvTests,
        _TrimShellTrailersTests,
        _CliPlaceholderTokenTests,
        _ExtractSondaCommandsTests,
        _ExtractScenarioPathTests,
        _ExtractImportOrFromFileTests,
        _ExtractTutorialTitlesTests,
        _RepoRelativePathTests,
        _SupportsDryRunTests,
    ):
        suite.addTests(loader.loadTestsFromTestCase(cls))
    runner = unittest.TextTestRunner(verbosity=2)
    result = runner.run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
