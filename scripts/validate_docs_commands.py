#!/usr/bin/env python3
"""Docs-drift catcher for sonda CLI commands referenced in user-facing documentation.

Walks ``docs/site/docs/**/*.md``, extracts fenced ``bash`` code blocks, finds
``sonda <subcommand>`` invocations, and verifies two properties:

1. **File existence** — when a command passes ``--scenario <path>`` pointing at a
   filesystem path (not a ``@builtin-name``), the path must exist on disk.
2. **Validity** — when the subcommand supports ``--dry-run``, the script invokes
   ``sonda`` with ``--dry-run`` appended and fails the check on a non-zero exit.

Stdlib-only. Run from the repo root via ``python3 scripts/validate_docs_commands.py``.
``--self-test`` runs the inline unit tests without needing a ``sonda`` binary.
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

# `mkdocs build` emits site output under `docs/site/site/`; skip it.
DOCS_GLOB_EXCLUDE = (Path("docs/site/site"),)

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

DRY_RUNNABLE_SINGLE: frozenset[str] = frozenset(
    {"metrics", "logs", "histogram", "summary", "run"}
)

DRY_RUNNABLE_WITH_ACTION: frozenset[tuple[str, str]] = frozenset(
    {
        ("catalog", "run"),
        ("scenarios", "run"),
        ("packs", "run"),
    }
)

DEFAULT_SONDA_BINARY = Path("target/release/sonda")

# File-exists validation only fires on paths that start with one of these
# roots. Bare filenames in docs (e.g. `my-scenario.yaml`) are tutorial
# placeholders the reader creates locally.
REPO_RELATIVE_PATH_ROOTS: frozenset[str] = frozenset(
    {"examples", "scenarios", "packs", "docs", "tests"}
)

DEFAULT_SUBPROCESS_TIMEOUT_S = 30.0


# --- Data model --------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class ExtractedCommand:
    """A single ``sonda`` invocation extracted from a markdown code block."""

    file: Path
    line: int
    argv: tuple[str, ...]
    raw: str
    # Paths shown as ``title="..."`` on any code fence in this file are treated
    # as tutorial material the reader creates locally — commands referencing
    # them skip the file-exists + dry-run checks.
    tutorial_titles: frozenset[str] = dataclasses.field(default_factory=frozenset)
    # When True, every path in this command's block inherits tutorial-skip,
    # because docs commonly demonstrate operations on titled files just above.
    block_is_tutorial: bool = False

    @property
    def subcommand(self) -> str | None:
        """Return the first recognised subcommand token, or ``None``.

        Global flags (``sonda --dry-run metrics ...``) are skipped.
        """
        for token in self.argv[1:]:
            if token.startswith("-"):
                continue
            return token if token in KNOWN_SUBCOMMANDS else None
        return None

    @property
    def action(self) -> str | None:
        """Return the action verb for ``catalog``/``scenarios``/``packs``,
        or ``None``. Lets the caller differentiate ``catalog list`` (no
        dry-run) from ``catalog run`` (dry-run)."""
        sub = self.subcommand
        if sub is None or sub not in {"catalog", "scenarios", "packs"}:
            return None
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
                if token in {"--pack-path", "--scenario-path", "--format"}:
                    skip_next_value = True
                continue
            if token.startswith("-"):
                continue
            return token
        return None


@dataclasses.dataclass
class ValidationResult:
    """Outcome of validating one :class:`ExtractedCommand`."""

    command: ExtractedCommand
    ok: bool
    message: str = ""


# --- Markdown extraction -----------------------------------------------------

# Leading whitespace allowed so admonition-nested fences (4-space indented) match.
_FENCE_OPEN_RE = re.compile(r"^(?P<indent>[ \t]*)```(?P<info>[^\s`]*)")
_FENCE_CLOSE_RE = re.compile(r"^(?P<indent>[ \t]*)```\s*$")

_FENCE_TITLE_RE = re.compile(r'title\s*=\s*"([^"]+)"')

# `FOO=bar BAZ=qux sonda ...` → `sonda ...`. Loops to strip multiple prefixes.
_ENV_ASSIGN_RE = re.compile(r"^[A-Z_][A-Z0-9_]*=[^ \t]*\s+")


def iter_markdown_files(docs_root: Path) -> list[Path]:
    """Return a sorted list of markdown files under ``docs_root``, excluding
    any path in :data:`DOCS_GLOB_EXCLUDE`."""
    excluded = tuple(docs_root.parent / ex for ex in DOCS_GLOB_EXCLUDE)
    out: list[Path] = []
    for md in docs_root.rglob("*.md"):
        if any(ex in md.parents for ex in excluded):
            continue
        out.append(md)
    out.sort()
    return out


def extract_tutorial_file_titles(markdown_text: str) -> set[str]:
    """Return ``title="..."`` values from every fenced block in the document.

    Paths shown as labelled code samples are treated as tutorial material —
    commands that reference them skip the file-exists + dry-run checks.
    """
    titles: set[str] = set()
    for line in markdown_text.splitlines():
        if "```" not in line:
            continue
        m = _FENCE_OPEN_RE.match(line)
        if not m:
            continue
        tail = line[m.end() :]
        for tm in _FENCE_TITLE_RE.finditer(tail):
            titles.add(tm.group(1))
    return titles


def extract_bash_blocks(markdown_text: str) -> list[tuple[int, str]]:
    """Return ``(line_number, block_body)`` tuples for ``bash`` fenced blocks.

    Only ``bash`` fences (case-insensitive) match; ``text``/``yaml``/``json``
    are ignored. ``bash title="..."`` is accepted.

    The line number is 1-based and points at the first content line inside
    the fence so reporting aligns with what the reader sees.
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
        if info_raw.lower() != "bash":
            i += 1
            while i < len(lines):
                if _FENCE_CLOSE_RE.match(lines[i]):
                    i += 1
                    break
                i += 1
            continue
        start_body_line = i + 2
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

    Returns ``(relative_line_offset, joined_line)`` where the offset is the
    0-based index of the first physical line of each logical line.
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
            buf.append(stripped_end[:-1])
            continue
        buf.append(line)
        logical.append((buf_start or 0, " ".join(s.strip() for s in buf).strip()))
        buf = []
        buf_start = None
    if buf:
        logical.append((buf_start or 0, " ".join(s.strip() for s in buf).strip()))
    return logical


def strip_prompt(line: str) -> str:
    """Strip a leading ``$ `` shell prompt, if present."""
    if line.startswith("$ "):
        return line[2:]
    return line


def strip_env_prefix(line: str) -> str:
    """Strip leading ``VAR=value ...`` env-var assignments from a command line."""
    while True:
        m = _ENV_ASSIGN_RE.match(line)
        if not m:
            return line
        line = line[m.end() :]


def _contains_cli_placeholder_token(tokens: Iterable[str]) -> bool:
    """Return True if any token is a CLI usage placeholder (``<FILE>``, ``[OPTIONS]``)."""
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
            out.append(ch)
            out.append(line[i + 1])
            i += 2
            continue
        if ch == "#" and (i == 0 or line[i - 1].isspace()):
            break
        if ch in (">", "<"):
            # `>` / `<` with whitespace on BOTH sides (or at EOL) is a shell
            # redirect; `<` immediately before a non-space char is a metavar
            # like `<FILE>` and stays in the token.
            prev_is_space = i == 0 or line[i - 1].isspace()
            next_is_space = i + 1 >= len(line) or line[i + 1].isspace()
            if prev_is_space and (next_is_space or ch == ">"):
                break
            out.append(ch)
            i += 1
            continue
        if ch == "&":
            break
        out.append(ch)
        i += 1
    return "".join(out).rstrip()


def extract_sonda_commands(
    md_file: Path, markdown_text: str
) -> list[ExtractedCommand]:
    """Extract every ``sonda <known-subcommand> ...`` invocation from a markdown file."""
    commands: list[ExtractedCommand] = []
    tutorial_titles = frozenset(extract_tutorial_file_titles(markdown_text))
    for block_line, block_body in extract_bash_blocks(markdown_text):
        block_is_tutorial = any(
            title in block_body for title in tutorial_titles
        )
        for rel_offset, raw_line in join_continuations(block_body):
            stripped = raw_line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            cleaned = strip_prompt(stripped)
            cleaned = strip_env_prefix(cleaned)
            if "sonda" not in cleaned:
                continue
            # Split pipelines / chains so `sonda ... | curl ...` still parses.
            segments = re.split(r"\s*(?:\|\||&&|;|\|)\s*", cleaned)
            for seg in segments:
                seg = seg.strip()
                if not seg.startswith("sonda "):
                    continue
                seg = _trim_shell_trailers(seg)
                if not seg:
                    continue
                try:
                    tokens = tuple(shlex.split(seg, comments=True))
                except ValueError:
                    continue
                if not tokens or tokens[0] != "sonda":
                    continue
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
                if cmd.subcommand is None:
                    continue
                commands.append(cmd)
    return commands


# --- Validation --------------------------------------------------------------


def is_metavar_placeholder(path: str) -> bool:
    """Return True for CLI-reference metavar placeholders like ``<FILE>``."""
    return "<" in path or ">" in path


def is_repo_relative_path(path: str) -> bool:
    """Return True when ``path`` is a repo-relative reference whose first
    segment is in :data:`REPO_RELATIVE_PATH_ROOTS`.

    Bare filenames, absolute paths, ``~/`` paths, and metavar placeholders
    all return False.
    """
    if is_metavar_placeholder(path):
        return False
    if path.startswith(("/", "~", "@")):
        return False
    first, sep, _rest = path.partition("/")
    if not sep:
        return False
    return first in REPO_RELATIVE_PATH_ROOTS


def extract_scenario_path(argv: Sequence[str]) -> str | None:
    """Return the ``--scenario`` path value (handles both ``--scenario foo`` and
    ``--scenario=foo`` forms), or ``None`` if absent."""
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

    Returns ``None`` for ``init --from @name`` (catalog reference, not a path).
    """
    if len(argv) < 2:
        return None
    sub = None
    for tok in argv[1:]:
        if tok.startswith("-"):
            continue
        sub = tok
        break
    if sub == "import":
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
                if tok in {"-o", "--output", "--columns", "--rate", "--duration"}:
                    skip_next = True
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

    ``sonda import --run`` qualifies; plain ``sonda import --analyze`` or
    ``sonda import -o foo.yaml`` does not.
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

    # Skip dry-run on cases that can't fail "for docs drift reasons":
    # tutorial paths the reader creates, `@name` and `catalog run <name>`
    # catalog lookups, and tutorial blocks generally.
    if scenario_path is not None and not scenario_path.startswith("@"):
        if not scenario_is_repo_path:
            return ValidationResult(command=cmd, ok=True)
    if scenario_path is not None and scenario_path.startswith("@"):
        return ValidationResult(command=cmd, ok=True)
    if cmd.action == "run" and cmd.subcommand in {"catalog", "scenarios", "packs"}:
        return ValidationResult(command=cmd, ok=True)
    if cmd.block_is_tutorial:
        return ValidationResult(command=cmd, ok=True)
    if referenced_file is not None and not is_repo_relative_path(referenced_file):
        return ValidationResult(command=cmd, ok=True)

    if sonda_bin is None:
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
    """Replace ``sonda`` with ``sonda_bin`` and inject ``--dry-run`` if absent."""
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
