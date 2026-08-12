#!/usr/bin/env python3
"""Compile gate for every runnable scenario fence in the user-facing docs.

Companion to :mod:`validate_docs_commands`, which checks the ``sonda`` command
lines in ``bash`` fences. This script checks the *scenarios* in ``yaml`` fences:
every fence the docs site offers a "Run in playground" button for must actually
compile, or the button is a promise the page cannot keep.

The rule that decides which fences are runnable lives in two places by
necessity — here, and in ``runnableScenario`` in
``docs/site/docs/javascripts/sonda-pure.js`` (the browser cannot import Python
and CI cannot run the DOM). They are kept honest by a shared case table,
``docs/site/tools/tests/runnable-cases.json``, which both sides answer:
``--self-test`` here, and ``pure.test.mjs`` there. Add hostile cases to the
table, never to one suite alone.

Three classes of fence are deliberately not compiled here. None of them is
silent — the first two are invisible to this gate because the shared rule
excludes them from being buttoned in the first place, and the third is printed
as a SKIP line:

* ``# sonda:static`` — an explicit opt-out comment in the fence, for examples
  whose failure IS the lesson, or shapes only ``sonda-server`` can run. The
  detector treats these as not-runnable, so they get no button either: the
  marker moves both gates at once.
* ``pack:`` references — they need a ``--catalog`` directory that neither a
  fence nor the browser can supply, so they are not runnable anywhere the
  button would take the reader.
* Missing input files — csv_replay tutorials that reference a CSV the reader
  exports themselves. The scenario is well-formed but uncheckable here, so it
  is reported as SKIP with the offending path.

One assumption is load-bearing enough to state. This gate compiles a fence with
the NATIVE binary, built with ``http,kafka,remote-write,otlp`` — but the button
it certifies opens that fence in the WASM build, which has none of those
features, and ``SinkConfig`` gates its variants with ``#[cfg(feature = ...)]``
at the enum level rather than only at construction. If a gated variant were
unparseable under wasm, this gate would happily pass fences the browser then
rejects, and 25 of the buttoned fences use gated sinks. Measured on the review
of PR #536 by building a ``--target nodejs`` bundle and parsing one scenario per
sink type: stdout, remote_write, http_push, loki, kafka, otlp_grpc, tcp and file
all parse in the wasm build, so the divergence does not exist and the CLI's
verdict transfers. Re-measure if sink parsing ever moves behind a feature gate.

Stdlib-only. Run from the repo root::

    python3 scripts/validate_docs_scenarios.py

``--self-test`` runs the inline unit tests plus the shared case table without
needing a ``sonda`` binary. ``--list`` prints what would be checked and exits.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Iterable, Sequence

# Reuse the docs-root walk from the sibling validator rather than restating it:
# the "which markdown files are user-facing" rule (and its docs/site/site
# exclusion) must not drift between the two gates.
from validate_docs_commands import (
    DEFAULT_SONDA_BINARY,
    DOCS_GLOB_ROOT,
    find_repo_root,
    iter_markdown_files,
)

# --- Configuration -----------------------------------------------------------

SHARED_CASE_TABLE = Path("docs/site/tools/tests/runnable-cases.json")

DEFAULT_SUBPROCESS_TIMEOUT_S = 30.0

# YAML keys whose value names an INPUT file the engine must read at compile
# time (csv_replay and its log sibling). A file sink's `path:` is an output
# and is deliberately not listed — it does not need to exist.
_INPUT_FILE_KEY_RE = re.compile(r"^\s*file:\s*(?P<path>\S+)\s*$", re.MULTILINE)


# --- Data model --------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class ExtractedScenario:
    """One ``yaml`` fence from a markdown file, dedented and ready to compile."""

    file: Path
    line: int  # 1-based, first content line inside the fence
    body: str
    info: str  # the fence's info string, e.g. 'yaml title="hello.yaml"'


@dataclasses.dataclass
class ScenarioResult:
    """Outcome of validating one :class:`ExtractedScenario`."""

    scenario: ExtractedScenario
    ok: bool
    message: str = ""
    skipped_reason: str = ""


# --- The shared rule ---------------------------------------------------------


def normalize_fence(text: str) -> str:
    """Mirror of ``normalizeFence`` in sonda-pure.js.

    Strips a UTF-8 BOM, folds CRLF, and removes the indentation common to
    every non-blank line — an admonition- or tab-nested fence carries four
    spaces on every line in markdown source that the browser never sees.
    """
    body = text.lstrip("﻿").replace("\r\n", "\n").replace("\r", "\n")
    lines = body.split("\n")
    common: int | None = None
    for line in lines:
        if not line.strip():
            continue  # blank lines carry no indentation signal
        indent = len(line) - len(line.lstrip(" \t"))
        if common is None or indent < common:
            common = indent
    if not common:
        return body
    return "\n".join(line[common:] if line.strip() else line for line in lines)


def is_runnable_scenario(text: str) -> bool:
    """Mirror of ``runnableScenario`` in sonda-pure.js — see that docstring.

    Complete iff it declares ``version: 2`` (the engine rejects a scenario
    file without one) AND carries a ``scenarios:`` list or the ``kind:
    runnable`` shorthand header, is not opted out with ``# sonda:static``, and
    makes no ``pack:`` reference (packs need a ``--catalog`` directory that
    neither a fence nor the browser can supply).

    ``kind:`` is pinned to ``runnable`` because the engine's other value is
    ``composable`` — a metric pack, which declares ``version: 2`` and
    ``kind:`` and passes ``sonda --dry-run run`` while emitting nothing. A
    compile gate cannot catch that, so the detector has to.

    Anchors are line-local and use ``[ \\t]`` rather than ``\\s``: in the JS
    twin ``\\s`` spans newlines, which would read ``version:`` on one line and
    ``2`` on the next as a version declaration. Python's ``\\s`` behaves the
    same way under ``re.MULTILINE``, so the two stay character-for-character
    comparable.
    """
    body = normalize_fence(text)
    if re.search(r"^#[ \t]*sonda:static\b", body, re.MULTILINE):
        return False
    if not re.search(r"^version:[ \t]*2[ \t]*$", body, re.MULTILINE):
        return False
    if re.search(r"^[ \t]*(?:-[ \t]+)?pack:", body, re.MULTILINE):
        return False
    return bool(
        re.search(r"^scenarios:", body, re.MULTILINE)
        or re.search(r"^kind:[ \t]*runnable[ \t]*$", body, re.MULTILINE)
    )


# --- Markdown extraction -----------------------------------------------------

# 3+ backticks, optionally indented (admonition/tabbed nesting), info string
# up to the first space so `yaml title="x.yaml"` still reads as `yaml`.
_FENCE_OPEN_RE = re.compile(r"^(?P<indent>[ \t]*)(?P<ticks>`{3,})(?P<info>[^\s`]*)")


def extract_yaml_fences(markdown_text: str) -> list[tuple[int, str, str]]:
    """Return ``(line_number, body, info)`` for every ``yaml`` fence.

    The closing fence must carry at least as many backticks as the opener, so
    a fence nested inside a longer-delimited block does not close it early.
    The line number is 1-based and points at the first content line, matching
    what the reader sees and what ``format_failure`` prints.
    """
    lines = markdown_text.splitlines()
    fences: list[tuple[int, str, str]] = []
    i = 0
    while i < len(lines):
        match = _FENCE_OPEN_RE.match(lines[i])
        if not match:
            i += 1
            continue
        ticks = match.group("ticks")
        info = match.group("info") or ""
        close_re = re.compile(r"^[ \t]*`{%d,}\s*$" % len(ticks))
        start_body_line = i + 2
        i += 1
        body: list[str] = []
        while i < len(lines):
            if close_re.match(lines[i]):
                i += 1
                break
            body.append(lines[i])
            i += 1
        if info.lower().startswith("yaml"):
            tail = lines[start_body_line - 2][match.end() :]
            fences.append((start_body_line, "\n".join(body), (info + tail).strip()))
    return fences


def extract_scenarios(md_path: Path, markdown_text: str) -> list[ExtractedScenario]:
    """Return every runnable scenario fence in one markdown document."""
    out: list[ExtractedScenario] = []
    for line, body, info in extract_yaml_fences(markdown_text):
        if not is_runnable_scenario(body):
            continue
        out.append(
            ExtractedScenario(
                file=md_path, line=line, body=normalize_fence(body), info=info
            )
        )
    return out


# --- Validation --------------------------------------------------------------


def missing_input_files(body: str, repo_root: Path) -> list[str]:
    """Return input paths the scenario reads that do not exist in the repo.

    A csv_replay tutorial pointing at a CSV the reader exports from Grafana is
    well-formed but uncheckable here — the scenario is skipped and the reason
    reported, rather than failing the build or being silently dropped.
    """
    missing: list[str] = []
    for match in _INPUT_FILE_KEY_RE.finditer(body):
        raw = match.group("path").strip().strip("\"'")
        if not raw or raw.startswith("<") or raw.startswith("$"):
            continue  # metavar placeholder, not a path
        if not (repo_root / raw).is_file():
            missing.append(raw)
    return missing


def validate_scenario(
    scenario: ExtractedScenario,
    repo_root: Path,
    sonda_bin: Path | None,
    subprocess_timeout: float = DEFAULT_SUBPROCESS_TIMEOUT_S,
) -> ScenarioResult:
    """Compile one scenario with ``sonda --dry-run run``."""
    missing = missing_input_files(scenario.body, repo_root)
    if missing:
        return ScenarioResult(
            scenario,
            ok=True,
            skipped_reason=f"reads input file(s) not in the repo: {', '.join(missing)}",
        )

    if sonda_bin is None:
        return ScenarioResult(scenario, ok=True, skipped_reason="no binary (--no-binary)")

    # NamedTemporaryFile with a .yaml suffix: the CLI takes the path
    # positionally and the scenario's own relative paths resolve against
    # repo_root, which is why cwd is pinned below.
    with tempfile.NamedTemporaryFile(
        "w", suffix=".yaml", encoding="utf-8", delete=False
    ) as handle:
        handle.write(scenario.body)
        temp_path = Path(handle.name)
    try:
        proc = subprocess.run(
            [str(sonda_bin), "run", "--dry-run", str(temp_path)],
            cwd=repo_root,
            capture_output=True,
            text=True,
            timeout=subprocess_timeout,
        )
    except subprocess.TimeoutExpired:
        return ScenarioResult(
            scenario, ok=False, message=f"timed out after {subprocess_timeout}s"
        )
    finally:
        temp_path.unlink(missing_ok=True)

    if proc.returncode == 0:
        return ScenarioResult(scenario, ok=True)
    detail = (proc.stderr or proc.stdout or "").strip()
    return ScenarioResult(
        scenario, ok=False, message=f"exit {proc.returncode}: {_first_lines(detail)}"
    )


def _first_lines(text: str, limit: int = 6) -> str:
    """Trim engine output to the first few lines for a readable CI log."""
    lines = [line for line in text.splitlines() if line.strip()]
    if len(lines) <= limit:
        return "\n    ".join(lines)
    return "\n    ".join(lines[:limit] + [f"... ({len(lines) - limit} more lines)"])


# --- The examples gallery ----------------------------------------------------
#
# test/examples.md is augmented at build time by docs/site/hooks/
# examples_gallery.py, which reads each file named in the page's tables and
# emits a card carrying that file's YAML for the browser to run. Two things
# can rot underneath that page and neither would fail a normal build:
#
#   * a table row naming an example that has been renamed or deleted — the row
#     silently loses its card and the reader is told to run a file that is not
#     there;
#   * an example whose YAML stops compiling — the card ships and the reader
#     finds out in the browser.
#
# So the gate below asks the compiler about every file the hook cards, the
# same way the fence gate asks about every fence it buttons. It is the same
# argument as WP1's: the page makes a promise, and CI is where the promise is
# checked.

GALLERY_PAGE = Path("docs/site/docs/test/examples.md")
GALLERY_HOOK = Path("docs/site/hooks/examples_gallery.py")


def load_gallery_hook(repo_root: Path):
    """Import the mkdocs hook as a module, by path.

    It is not on ``sys.path`` and must not be — mkdocs loads it by file, and
    adding a `docs/site/hooks` entry to the path for this one import would
    invite the two to disagree about which copy is running.
    """
    import importlib.util

    path = repo_root / GALLERY_HOOK
    spec = importlib.util.spec_from_file_location("sonda_examples_gallery", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load the gallery hook at {path}")
    module = importlib.util.module_from_spec(spec)
    # Registered before exec: the hook uses `from __future__ import
    # annotations` with a dataclass, and dataclasses resolve string
    # annotations through `sys.modules[cls.__module__]`. A module that is not
    # there yet fails with a bare AttributeError on NoneType.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def gallery_rows(repo_root: Path) -> list[tuple[str, bool]]:
    """Every example file named by a table row on the gallery page.

    Returns ``(path, exists)`` in document order, including rows the hook does
    not card — a missing file is worth reporting whether or not it would have
    become a widget.
    """
    hook = load_gallery_hook(repo_root)
    examples_dir = repo_root / "examples"
    rows: list[tuple[str, bool]] = []
    seen: set[str] = set()
    for line in (repo_root / GALLERY_PAGE).read_text(encoding="utf-8").split("\n"):
        match = hook.ROW_RE.match(line)
        if not match:
            continue
        name = match.group(1)
        if name in seen:
            continue  # a file listed in two tables is one file to check
        seen.add(name)
        rows.append((name, hook.read_example(examples_dir, name) is not None))
    return rows


def validate_gallery(
    repo_root: Path,
    sonda_bin: Path | None,
    subprocess_timeout: float = DEFAULT_SUBPROCESS_TIMEOUT_S,
) -> list[str]:
    """Check every example named on the gallery page. Returns failure lines."""
    hook = load_gallery_hook(repo_root)
    examples_dir = repo_root / "examples"
    failures: list[str] = []
    carded = 0

    for name, exists in gallery_rows(repo_root):
        if not exists:
            failures.append(
                f"FAIL {GALLERY_PAGE}: table row names `{name}`, which is not in examples/"
            )
            continue
        text = hook.read_example(examples_dir, name)
        if not hook.is_runnable_scenario(text):
            continue  # not carded: a rules file, a config, a pack
        carded += 1
        scenario = ExtractedScenario(
            file=examples_dir / name, line=1, body=text, info="yaml"
        )
        result = validate_scenario(
            scenario,
            repo_root=repo_root,
            sonda_bin=sonda_bin,
            subprocess_timeout=subprocess_timeout,
        )
        if not result.ok:
            failures.append(f"FAIL examples/{name} is carded in the gallery: {result.message}")

    print(f"gallery: {carded} carded examples checked, {len(failures)} failed")
    return failures


# --- Orchestration -----------------------------------------------------------


def run_validation(
    repo_root: Path,
    sonda_bin: Path | None,
    subprocess_timeout: float = DEFAULT_SUBPROCESS_TIMEOUT_S,
    skip_files: Iterable[str] = (),
) -> tuple[list[ScenarioResult], list[ScenarioResult]]:
    """Validate every runnable fence. Returns ``(all_results, failures)``."""
    docs_root = repo_root / DOCS_GLOB_ROOT
    if not docs_root.is_dir():
        raise RuntimeError(f"docs root not found: {docs_root}")

    skip_set = {str(s) for s in skip_files}
    scenarios: list[ExtractedScenario] = []
    for md in iter_markdown_files(docs_root):
        rel = str(md.relative_to(repo_root)) if md.is_absolute() else str(md)
        if rel in skip_set:
            continue
        scenarios.extend(extract_scenarios(md, md.read_text(encoding="utf-8")))

    results = [
        validate_scenario(
            scenario,
            repo_root=repo_root,
            sonda_bin=sonda_bin,
            subprocess_timeout=subprocess_timeout,
        )
        for scenario in scenarios
    ]
    return results, [r for r in results if not r.ok]


def format_failure(result: ScenarioResult, repo_root: Path) -> str:
    """Render one failure as a multi-line string suitable for CI logs."""
    try:
        rel = result.scenario.file.relative_to(repo_root)
    except ValueError:
        rel = result.scenario.file
    head = result.scenario.body.strip().splitlines()[:3]
    return (
        f"FAIL {rel}:{result.scenario.line}\n"
        + "".join(f"    | {line}\n" for line in head)
        + f"    {result.message}"
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Compile every runnable scenario fence in the docs.",
    )
    parser.add_argument(
        "--sonda",
        type=Path,
        default=None,
        help=(
            f"Path to the sonda binary. Defaults to {DEFAULT_SONDA_BINARY} "
            "relative to the repo root."
        ),
    )
    parser.add_argument(
        "--no-binary",
        action="store_true",
        help="Extract and report fences without compiling them.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_SUBPROCESS_TIMEOUT_S,
        help="Per-invocation timeout in seconds.",
    )
    parser.add_argument(
        "--skip-file",
        action="append",
        default=[],
        metavar="PATH",
        help=(
            "Repo-relative markdown path to skip entirely. Repeatable. "
            "A temporary escape hatch — prefer fixing the fence or marking it "
            "'# sonda:static'."
        ),
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="List the fences that would be checked, then exit.",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run inline unit tests plus the shared case table, and exit.",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return _run_self_tests()

    repo_root = find_repo_root(Path(__file__).parent)

    if args.list:
        docs_root = repo_root / DOCS_GLOB_ROOT
        total = 0
        for md in iter_markdown_files(docs_root):
            for scenario in extract_scenarios(md, md.read_text(encoding="utf-8")):
                total += 1
                print(f"{scenario.file.relative_to(repo_root)}:{scenario.line}")
        print(f"{total} runnable scenario fences", file=sys.stderr)
        return 0

    sonda_bin: Path | None
    if args.no_binary:
        sonda_bin = None
    else:
        raw_bin = args.sonda or (repo_root / DEFAULT_SONDA_BINARY)
        raw_bin = raw_bin if raw_bin.is_absolute() else (repo_root / raw_bin)
        if not raw_bin.is_file():
            print(
                f"sonda binary not found at {raw_bin}. Build it first "
                "(cargo build --release -p sonda) or pass --no-binary.",
                file=sys.stderr,
            )
            return 2
        sonda_bin = raw_bin

    results, failures = run_validation(
        repo_root=repo_root,
        sonda_bin=sonda_bin,
        subprocess_timeout=args.timeout,
        skip_files=args.skip_file,
    )

    # Skips are reported, never silent: a gate that quietly stops checking
    # things reads as "all green" when it is not.
    for result in results:
        if result.skipped_reason:
            rel = result.scenario.file.relative_to(repo_root)
            print(
                f"SKIP {rel}:{result.scenario.line} — {result.skipped_reason}",
                file=sys.stderr,
            )
    for failure in failures:
        print(format_failure(failure, repo_root), file=sys.stderr)

    skipped = sum(1 for r in results if r.skipped_reason)
    print(
        f"{len(results)} runnable scenario fences found, "
        f"{len(results) - skipped - len(failures)} compiled, "
        f"{skipped} skipped, {len(failures)} failed",
        file=sys.stderr,
    )

    gallery_failures = validate_gallery(
        repo_root=repo_root, sonda_bin=sonda_bin, subprocess_timeout=args.timeout
    )
    for line in gallery_failures:
        print(line, file=sys.stderr)

    return 0 if not failures and not gallery_failures else 1


# --- Self-tests --------------------------------------------------------------


class _SharedCaseTableTests(unittest.TestCase):
    """The other half of the contract with pure.test.mjs.

    Every case in runnable-cases.json is answered by BOTH implementations.
    A case added for the browser detector fails here until the Python twin
    agrees, which is the whole point of the file.
    """

    def test_shared_case_table(self) -> None:
        repo_root = find_repo_root(Path(__file__).parent)
        table = json.loads(
            (repo_root / SHARED_CASE_TABLE).read_text(encoding="utf-8")
        )
        cases = table["cases"]
        self.assertGreaterEqual(len(cases), 20, "table should be a real case table")
        self.assertTrue(any(c["expected"] for c in cases))
        self.assertTrue(any(not c["expected"] for c in cases))
        for case in cases:
            with self.subTest(case=case["name"]):
                self.assertEqual(
                    is_runnable_scenario(case["text"]),
                    case["expected"],
                    f"detector disagrees with the shared table on: {case['name']}",
                )


class _NormalizeFenceTests(unittest.TestCase):
    def test_flush_text_is_untouched(self) -> None:
        self.assertEqual(normalize_fence("version: 2\nkind: x\n"), "version: 2\nkind: x\n")

    def test_uniform_indent_is_removed(self) -> None:
        self.assertEqual(normalize_fence("    a\n    b\n"), "a\nb\n")

    def test_blank_line_does_not_pin_dedent_at_zero(self) -> None:
        self.assertEqual(normalize_fence("    a\n\n    b\n"), "a\n\nb\n")

    def test_ragged_indent_removes_common_prefix_only(self) -> None:
        self.assertEqual(normalize_fence("  a\n    b\n"), "a\n  b\n")

    def test_bom_and_crlf(self) -> None:
        self.assertEqual(normalize_fence("﻿version: 2"), "version: 2")
        self.assertEqual(normalize_fence("a\r\nb\rc"), "a\nb\nc")


class _ExtractYamlFencesTests(unittest.TestCase):
    def test_only_yaml_fences_match(self) -> None:
        md = (
            "intro\n"
            "```bash\n"
            "sonda run x.yaml\n"
            "```\n"
            "```yaml\n"
            "version: 2\n"
            "```\n"
        )
        fences = extract_yaml_fences(md)
        self.assertEqual(len(fences), 1)
        self.assertEqual(fences[0][1], "version: 2")

    def test_line_number_points_at_first_content_line(self) -> None:
        md = "intro\n\n```yaml\nversion: 2\n```\n"
        line, body, _ = extract_yaml_fences(md)[0]
        self.assertEqual(md.splitlines()[line - 1], "version: 2")

    def test_titled_fence_keeps_its_info_string(self) -> None:
        md = '```yaml title="hello.yaml"\nversion: 2\n```\n'
        _, _, info = extract_yaml_fences(md)[0]
        self.assertIn('title="hello.yaml"', info)

    def test_indented_fence_inside_a_tab_block(self) -> None:
        md = '=== "Tab"\n\n    ```yaml\n    version: 2\n    scenarios: []\n    ```\n'
        fences = extract_yaml_fences(md)
        self.assertEqual(len(fences), 1)
        self.assertEqual(normalize_fence(fences[0][1]), "version: 2\nscenarios: []")

    def test_longer_delimiter_is_not_closed_by_a_shorter_inner_fence(self) -> None:
        md = "````yaml\nversion: 2\n```\nstill inside\n````\n"
        fences = extract_yaml_fences(md)
        self.assertEqual(len(fences), 1)
        self.assertIn("still inside", fences[0][1])


class _ExtractScenariosTests(unittest.TestCase):
    def test_fragments_are_not_extracted(self) -> None:
        md = "```yaml\ngenerator:\n  type: sine\n```\n"
        self.assertEqual(extract_scenarios(Path("x.md"), md), [])

    def test_complete_scenario_is_extracted_dedented(self) -> None:
        md = '=== "Tab"\n\n    ```yaml\n    version: 2\n    kind: runnable\n    ```\n'
        found = extract_scenarios(Path("x.md"), md)
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0].body, "version: 2\nkind: runnable")

    def test_static_marked_fence_is_not_extracted(self) -> None:
        md = "```yaml\n# sonda:static\nversion: 2\nkind: runnable\n```\n"
        self.assertEqual(extract_scenarios(Path("x.md"), md), [])


class _MissingInputFilesTests(unittest.TestCase):
    def test_reports_a_path_that_does_not_exist(self) -> None:
        repo_root = find_repo_root(Path(__file__).parent)
        body = "version: 2\nkind: runnable\ngenerator:\n  file: nope/absent.csv\n"
        self.assertEqual(missing_input_files(body, repo_root), ["nope/absent.csv"])

    def test_existing_path_is_not_reported(self) -> None:
        repo_root = find_repo_root(Path(__file__).parent)
        body = "version: 2\nkind: runnable\ngenerator:\n  file: Cargo.toml\n"
        self.assertEqual(missing_input_files(body, repo_root), [])

    def test_metavar_placeholders_are_ignored(self) -> None:
        repo_root = find_repo_root(Path(__file__).parent)
        body = "generator:\n  file: <your-export>.csv\n"
        self.assertEqual(missing_input_files(body, repo_root), [])

    def test_output_sink_paths_are_not_treated_as_inputs(self) -> None:
        repo_root = find_repo_root(Path(__file__).parent)
        body = "sink:\n  type: file\n  path: /tmp/out.txt\n"
        self.assertEqual(missing_input_files(body, repo_root), [])


class _GalleryTests(unittest.TestCase):
    """The examples gallery's build-time claims, checked without a build.

    Compiling the carded examples needs the sonda binary and happens in the
    main run; what belongs here is everything that does not: that the page
    still has rows, that every row names a file that exists, and that the
    hook's detector — the THIRD implementation of the runnable rule, after
    sonda-pure.js and this file — answers the same shared case table as the
    other two. Three copies of a rule are two too many to hold in anyone's
    head; the table is what holds them.
    """

    def setUp(self) -> None:
        self.repo_root = find_repo_root(Path(__file__).parent)
        self.hook = load_gallery_hook(self.repo_root)

    def test_gallery_page_still_has_rows(self) -> None:
        rows = gallery_rows(self.repo_root)
        self.assertGreater(len(rows), 40, "the examples page lists dozens of files")

    def test_every_row_names_a_file_that_exists(self) -> None:
        missing = [name for name, exists in gallery_rows(self.repo_root) if not exists]
        self.assertEqual(missing, [], "table rows naming files not in examples/")

    def test_hook_detector_answers_the_shared_case_table(self) -> None:
        table = json.loads((self.repo_root / SHARED_CASE_TABLE).read_text(encoding="utf-8"))
        for case in table["cases"]:
            with self.subTest(case=case["name"]):
                self.assertEqual(
                    self.hook.is_runnable_scenario(case["text"]), case["expected"]
                )

    def test_hook_and_validator_agree_on_every_example_file(self) -> None:
        """Not just the table — the real corpus, which is where drift shows."""
        examples = sorted((self.repo_root / "examples").rglob("*.y*ml"))
        self.assertGreater(len(examples), 50)
        for path in examples:
            text = path.read_text(encoding="utf-8")
            with self.subTest(path=path.name):
                self.assertEqual(
                    self.hook.is_runnable_scenario(text), is_runnable_scenario(text)
                )

    def test_base64_encoders_agree(self) -> None:
        """The hook encodes what sonda-pure.js decodes; one shared shape."""
        for text in ("version: 2\n", "東京 ☃\n", "a" * 300, "?/+=\n"):
            with self.subTest(text=text[:12]):
                encoded = self.hook.to_base64url(text)
                self.assertNotIn("=", encoded)
                self.assertNotIn("+", encoded)
                self.assertNotIn("/", encoded)
                padded = encoded + "=" * (-len(encoded) % 4)
                import base64 as _b64

                self.assertEqual(_b64.urlsafe_b64decode(padded).decode("utf-8"), text)


def _run_self_tests() -> int:
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromModule(sys.modules[__name__])
    runner = unittest.TextTestRunner(verbosity=2)
    return 0 if runner.run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
