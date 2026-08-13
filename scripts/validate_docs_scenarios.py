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
    # True when `body` is not what the page shows: a fragment wrapped in a
    # synthesized preamble. Reporting has to say so, or a reader chasing a
    # failure looks for lines that are not in their file.
    synthesized: bool = False


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


# --- Fragment synthesis ------------------------------------------------------
#
# The gate above only ever sees COMPLETE scenarios, because that is what a
# "Run in playground" button needs. TWO of the bugs this program has found
# lived in fragments — `scenario_name` (PR #536) and `rate_multiplier` (#543)
# — and a fragment is invisible to a compile gate precisely because it is a
# fragment.
#
# The third bug of that family, the pack fence (#541), is NOT one of them, and
# this gate cannot see it either (review #545 M1). A pack carries `version: 2`
# and `kind: composable`, so it was never a fragment; it was invisible because
# the ORACLE accepts it — `sonda --dry-run run` reports `OK (0 scenarios)` for
# a pack and exits 0. Compiling one here would report green and prove nothing,
# which is why `synthesize_fragment` refuses anything carrying `pack:`. The
# scenario-count assertion in `validate_scenario` is the piece that would have
# to grow for this gate to ever speak about packs.
#
# The insight is the reviewer's, from #543: a fragment is only a fragment
# because it LACKS A PREAMBLE. The engine rejects unknown fields, so wrapping
# a fragment in the minimal `version: 2 / kind: runnable / defaults /
# scenarios:` shape and dry-running it catches exactly the class those three
# bugs belong to — a misspelled or renamed field under `gaps:`, `bursts:`,
# `generator:`, `while:`. It does not type-check values the way a schema would,
# which is what leaves WP11 worth doing.
#
# The design constraint that shapes everything below: THE SYNTHESIZED PARTS
# MUST NOT BE WHAT IS UNDER TEST. Every field this function invents is a field
# whose errors would belong to the gate rather than to the docs, so it invents
# as little as it can and only what the fragment has left out.
#
# Nothing is suppressed on the strength of that, though. Across today's corpus
# no fragment fails for a field the scaffolding should have supplied, so
# filtering out `missing field` errors would only hide real ones — the
# `period_secs` bug this gate found on its first run is exactly that shape. If
# a future fragment does fail because the scaffolding is too thin, the honest
# fixes are to widen the scaffolding or mark the fence `# sonda:static`; both
# are visible, where a suppressed error class is not.

_FRAGMENT_PREAMBLE = """version: 2
kind: runnable
"""

# Defaults, not content. Every key here is one the engine would otherwise
# demand and that a prose fragment has no reason to carry.
_FRAGMENT_DEFAULTS = """defaults:
  rate: 1
  duration: 10s
  encoder: { type: json_lines }
  sink: { type: stdout }
"""


def _scenarios_is_a_sequence(body: str) -> bool:
    """True when a top-level ``scenarios:`` key introduces a LIST of entries.

    The discriminator that keeps Helm values files out. ``deploy/kubernetes.md``
    documents a chart whose ``scenarios:`` key maps FILENAMES to file contents:

        scenarios:
          cpu-metrics.yaml: |
            name: cpu_usage

    That is a valid document about Sonda which is not a Sonda scenario, and
    wrapping it produces ``invalid type: map, expected a sequence`` — a failure
    that says nothing about the docs. Reading whether the first non-blank line
    under the key starts a sequence item separates the two exactly.
    """
    match = re.search(r"^scenarios:[ \t]*$", body, re.MULTILINE)
    if not match:
        return False
    for line in body[match.end() :].splitlines():
        if not line.strip():
            continue
        return line.lstrip().startswith("- ")
    return False


def _looks_like_a_scenario_entry(body: str) -> bool:
    """True when a fragment is the body of one scenario entry.

    Requires a key only a Sonda entry has. An earlier version accepted any
    fence opening with ``name:``, which swept in three GITHUB ACTIONS
    WORKFLOWS from ``test/end-to-end-pipelines.md`` — ``name: Alert Rule
    Validation`` followed by ``on:`` — and reported ``unknown field `on` `` as
    a docs bug. `name:` is the most common key in YAML; it discriminates
    nothing.
    """
    if not re.match(
        r"^(signal_type|name|id|generator|log_generator|distribution):",
        body.strip().splitlines()[0] if body.strip() else "",
    ):
        return False
    return bool(
        re.search(r"^(signal_type|generator|log_generator|distribution):", body, re.MULTILINE)
    )


def synthesize_fragment(text: str) -> str | None:
    """Wrap a docs fragment in the smallest preamble that makes it compilable.

    Returns ``None`` for anything this gate cannot speak about honestly:
    complete scenarios (the gate above already compiles those), ``sonda:static``
    opt-outs, ``pack:`` references, Helm values maps, and any fence that is not
    shaped like a scenario or a list of them.

    Two shapes are handled, and the difference is how much has to be invented:

    ``scenarios:`` already present
        Only the version header is prepended — and ``defaults:`` too, unless
        the fragment brought its own. Nothing structural is guessed, so a
        failure here is the fragment's own.

    a single entry's body
        Wrapped under ``scenarios:`` at one indent, and ``signal_type:``/
        ``name:`` are injected IF ABSENT, because a prose fragment showing a
        `generator:` block has no reason to repeat them. `signal_type` is read
        off the fragment: a `log_generator:` means logs.

    Those two injections are why suppressing `missing field` errors from this
    tier is TEMPTING — and they are not a reason to do it. The filter was
    considered and refused; see the module note above. An earlier draft of
    this docstring said a caller "must not report" that class, which is the
    opposite of what the code does and would have talked a future maintainer
    into deleting the check that found the `period_secs` bug (review #545 W1,
    which settled it by removing `period_secs` from a tier-2 fragment and
    confirming the gate still reports it).
    """
    body = normalize_fence(text)
    if is_runnable_scenario(body):
        return None
    if re.search(r"^#[ \t]*sonda:static\b", body, re.MULTILINE):
        return None
    if re.search(r"^[ \t]*(?:-[ \t]+)?pack:", body, re.MULTILINE):
        return None
    # A fence that already declares a version is either complete (handled
    # above) or deliberately not v2; either way this gate has nothing to add.
    if re.search(r"^version:", body, re.MULTILINE):
        return None

    if _scenarios_is_a_sequence(body):
        head = _FRAGMENT_PREAMBLE
        if not re.search(r"^defaults:", body, re.MULTILINE):
            head += _FRAGMENT_DEFAULTS
        return head + body if body.endswith("\n") else head + body + "\n"

    if _looks_like_a_scenario_entry(body):
        injected = []
        if not re.search(r"^signal_type:", body, re.MULTILINE):
            # Read off the fragment rather than defaulted, because the entry's
            # required fields depend on it: a `distribution:` block belongs to
            # a histogram (a metrics entry would then be asked for a
            # `generator:` it never had), and a `log_generator:` to logs.
            if re.search(r"^log_generator:", body, re.MULTILINE):
                kind = "logs"
            elif re.search(r"^distribution:", body, re.MULTILINE) and not re.search(
                r"^generator:", body, re.MULTILINE
            ):
                kind = "histogram"
            else:
                kind = "metrics"
            injected.append(f"signal_type: {kind}")
        if not re.search(r"^name:", body, re.MULTILINE):
            injected.append("name: doc_fragment")
        entry = "\n".join(injected + [body.rstrip("\n")])
        indented = "\n".join("    " + line if line.strip() else "" for line in entry.splitlines())
        return (
            _FRAGMENT_PREAMBLE
            + _FRAGMENT_DEFAULTS
            + "scenarios:\n"
            + indented.replace("    ", "  - ", 1)
            + "\n"
        )

    return None


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


def extract_fragments(md_path: Path, markdown_text: str) -> list[ExtractedScenario]:
    """Return every FRAGMENT fence, wrapped so the engine can parse it.

    The complement of :func:`extract_scenarios`: these fences carry no button
    and no gate could previously reach them, which is where three of this
    program's field-name bugs were found. See the module note above
    :func:`synthesize_fragment` for why the wrapping is kept minimal.
    """
    out: list[ExtractedScenario] = []
    for line, body, info in extract_yaml_fences(markdown_text):
        document = synthesize_fragment(body)
        if document is None:
            continue
        out.append(
            ExtractedScenario(
                file=md_path, line=line, body=document, info=info, synthesized=True
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

    detail = (proc.stderr or proc.stdout or "").strip()
    if proc.returncode != 0:
        return ScenarioResult(
            scenario, ok=False, message=f"exit {proc.returncode}: {_first_lines(detail)}"
        )
    return _check_scenario_count(scenario, proc.stdout, proc.stderr)


# `Validation: OK (2 scenarios)` — the CLI's own count, and the only part of a
# successful run that says the file did anything.
_VALIDATION_COUNT_RE = re.compile(r"Validation:\s*OK\s*\((?P<count>\d+)\s+scenarios?\)")


def _check_scenario_count(
    scenario: ExtractedScenario, stdout: str, stderr: str
) -> ScenarioResult:
    """Exit 0 is necessary but not sufficient: require at least one scenario.

    Review #545 M2. Until this existed the oracle was the exit code alone, and
    ``Validation: OK (0 scenarios)`` — a file the engine parses and then emits
    nothing from — was indistinguishable from success.

    That is not a hypothetical shape, it is the #541 bug exactly: a metric pack
    compiles clean and produces no scenarios, which is how a "Run in
    playground" button came to point at an empty chart while every gate stayed
    green. Leaving the hole unguarded inside the gate built to answer that
    family would have been the joke writing itself.

    A missing count is not treated as a failure. The assertion is about what
    the CLI reported, not about parsing its output successfully, and a future
    change to that banner should not turn every fence red at once — the
    self-tests pin the phrasing so such a change is visible instead.
    """
    match = _VALIDATION_COUNT_RE.search(stdout or "") or _VALIDATION_COUNT_RE.search(
        stderr or ""
    )
    if match is None:
        return ScenarioResult(scenario, ok=True)
    if int(match.group("count")) == 0:
        return ScenarioResult(
            scenario,
            ok=False,
            message=(
                "compiled clean but produced 0 scenarios — the engine parsed this "
                "file and would emit nothing from it"
            ),
        )
    return ScenarioResult(scenario, ok=True)


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
) -> tuple[list[ScenarioResult], list[ScenarioResult], int]:
    """Validate every reachable fence.

    Returns ``(all_results, failures, declined)``, where ``declined`` counts the
    yaml fences neither tier could speak about — Helm values, workflow files,
    Alertmanager and vmalert configs, bare ``encoder:`` blocks. Reported rather
    than inferred (review #545 M3): "52 fragments compiled" invites the
    question "out of how many?", and a reader of the run cannot tell 45
    declines from 5 without a script of their own.
    """
    docs_root = repo_root / DOCS_GLOB_ROOT
    if not docs_root.is_dir():
        raise RuntimeError(f"docs root not found: {docs_root}")

    skip_set = {str(s) for s in skip_files}
    scenarios: list[ExtractedScenario] = []
    fences = 0  # every yaml fence seen, so declines can be counted rather than inferred
    for md in iter_markdown_files(docs_root):
        rel = str(md.relative_to(repo_root)) if md.is_absolute() else str(md)
        if rel in skip_set:
            continue
        text = md.read_text(encoding="utf-8")
        fences += len(extract_yaml_fences(text))
        scenarios.extend(extract_scenarios(md, text))
        scenarios.extend(extract_fragments(md, text))

    results = [
        validate_scenario(
            scenario,
            repo_root=repo_root,
            sonda_bin=sonda_bin,
            subprocess_timeout=subprocess_timeout,
        )
        for scenario in scenarios
    ]
    return results, [r for r in results if not r.ok], fences - len(scenarios)


def format_failure(result: ScenarioResult, repo_root: Path) -> str:
    """Render one failure as a multi-line string suitable for CI logs."""
    try:
        rel = result.scenario.file.relative_to(repo_root)
    except ValueError:
        rel = result.scenario.file
    head = result.scenario.body.strip().splitlines()[:3]
    # A synthesized body starts with lines that are not in the reader's file.
    # Saying so is the difference between a diagnosable failure and a hunt for
    # a `version: 2` that the page does not contain.
    note = (
        "    (fragment, compiled under a synthesized preamble — the lines below "
        "are the wrapper, not the page)\n"
        if result.scenario.synthesized
        else ""
    )
    return (
        f"FAIL {rel}:{result.scenario.line}\n"
        + note
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

    results, failures, declined = run_validation(
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
    fragments = sum(1 for r in results if r.scenario.synthesized)
    print(
        f"{len(results) - fragments} runnable scenario fences and {fragments} "
        f"fragments found, "
        f"{len(results) - skipped - len(failures)} compiled, "
        f"{skipped} skipped, {len(failures)} failed; "
        f"{declined} yaml fences declined (not Sonda scenarios)",
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


class _SynthesizeFragmentTests(unittest.TestCase):
    """The fragment wrapper, and every shape that must NOT be wrapped.

    Each refusal below is a false positive this gate actually produced on the
    first run against the real corpus. They are named rather than counted,
    because "the gate reports 3 failures" is only useful if none of them is
    the gate misreading a file about something else entirely.
    """

    def test_scenarios_sequence_only_gets_a_preamble(self) -> None:
        out = synthesize_fragment(
            "scenarios:\n  - signal_type: metrics\n    name: x\n"
            "    generator: { type: constant, value: 1.0 }\n"
        )
        self.assertIsNotNone(out)
        assert out is not None
        self.assertTrue(out.startswith("version: 2\nkind: runnable\n"))
        # Nothing structural invented: the fragment's own text survives whole.
        self.assertIn("  - signal_type: metrics\n    name: x\n", out)

    def test_a_fragments_own_defaults_are_not_duplicated(self) -> None:
        out = synthesize_fragment(
            "defaults:\n  rate: 9\nscenarios:\n  - signal_type: metrics\n"
            "    name: x\n    generator: { type: constant, value: 1.0 }\n"
        )
        assert out is not None
        self.assertEqual(out.count("defaults:"), 1, "a second defaults: is a YAML duplicate key")
        self.assertIn("rate: 9", out)

    def test_helm_values_map_is_refused(self) -> None:
        # deploy/kubernetes.md documents a chart whose `scenarios:` maps
        # FILENAMES to file bodies. Wrapping it yields "invalid type: map,
        # expected a sequence" — a failure that says nothing about the docs.
        self.assertIsNone(
            synthesize_fragment("scenarios:\n  cpu-metrics.yaml: |\n    name: cpu_usage\n")
        )

    def test_github_actions_workflow_is_refused(self) -> None:
        # Three of these live in test/end-to-end-pipelines.md. An earlier rule
        # accepted any fence opening with `name:` and reported `unknown field
        # 'on'` as a docs bug. `name:` is the most common key in YAML.
        self.assertIsNone(
            synthesize_fragment("name: Alert Rule Validation\non:\n  pull_request:\n")
        )

    def test_bare_encoder_block_is_refused(self) -> None:
        # Not an entry: it carries no key that only a scenario entry has.
        self.assertIsNone(synthesize_fragment("encoder:\n  type: prometheus_text\n"))

    def test_static_and_pack_and_complete_are_refused(self) -> None:
        self.assertIsNone(
            synthesize_fragment("# sonda:static\nscenarios:\n  - signal_type: metrics\n")
        )
        self.assertIsNone(synthesize_fragment("scenarios:\n  - pack: node-exporter\n"))
        self.assertIsNone(
            synthesize_fragment("version: 2\nkind: runnable\nscenarios:\n  - id: a\n")
        )

    def test_signal_type_is_read_off_the_fragment(self) -> None:
        # The entry's REQUIRED fields depend on this, so guessing `metrics`
        # for everything makes the gate report its own wrapper's errors: a
        # bare `distribution:` block would be asked for a `generator:`.
        logs = synthesize_fragment("log_generator:\n  type: template\n")
        assert logs is not None
        self.assertIn("signal_type: logs", logs)
        hist = synthesize_fragment("distribution:\n  type: exponential\n  rate: 10.0\n")
        assert hist is not None
        self.assertIn("signal_type: histogram", hist)
        metric = synthesize_fragment("generator:\n  type: constant\n  value: 1.0\n")
        assert metric is not None
        self.assertIn("signal_type: metrics", metric)

    def test_present_scaffolding_is_not_overwritten(self) -> None:
        out = synthesize_fragment("signal_type: logs\nname: mine\nlog_generator:\n  type: template\n")
        assert out is not None
        self.assertEqual(out.count("signal_type:"), 1)
        self.assertIn("name: mine", out)
        self.assertNotIn("doc_fragment", out)

    def test_the_entry_is_indented_under_a_sequence_item(self) -> None:
        out = synthesize_fragment("generator:\n  type: constant\n  value: 1.0\n")
        assert out is not None
        self.assertIn("scenarios:\n  - signal_type: metrics", out)
        # Continuation lines sit at the item's indent, not the item's dash.
        self.assertIn("\n    generator:\n", out)
        self.assertIn("\n      type: constant\n", out)


class _ScenarioCountTests(unittest.TestCase):
    """The success oracle. Review #545 M2: exit 0 alone was the whole test.

    These pin the CLI's banner phrasing on purpose. If it ever changes, a
    parse miss degrades to "assume fine" rather than turning every fence red —
    so the failing test here is the only thing that would say so.
    """

    def _result(self, stdout: str = "", stderr: str = "") -> ScenarioResult:
        scenario = ExtractedScenario(file=Path("x.md"), line=1, body="", info="yaml")
        return _check_scenario_count(scenario, stdout, stderr)

    def test_zero_scenarios_is_a_failure(self) -> None:
        # The #541 shape: a pack parses clean and emits nothing.
        result = self._result("Validation: OK (0 scenarios)\n")
        self.assertFalse(result.ok)
        self.assertIn("0 scenarios", result.message)

    def test_one_or_more_passes(self) -> None:
        self.assertTrue(self._result("Validation: OK (1 scenario)\n").ok)
        self.assertTrue(self._result("Validation: OK (2 scenarios)\n").ok)
        self.assertTrue(self._result("Validation: OK (12 scenarios)\n").ok)

    def test_the_count_is_read_from_either_stream(self) -> None:
        self.assertFalse(self._result(stderr="Validation: OK (0 scenarios)\n").ok)

    def test_an_unrecognised_banner_does_not_fail_every_fence(self) -> None:
        # Deliberately permissive: this assertion is about what the CLI
        # reported, not about parsing its output successfully.
        self.assertTrue(self._result("something else entirely\n").ok)
        self.assertTrue(self._result("").ok)


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
