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
the NATIVE binary, built with the docs job's feature list in ``ci.yml``
(``http,kafka,remote-write,otlp,gnmi,snmp``) — but the button
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

``--self-test`` runs the inline unit tests plus the shared case table. The
tests that compile a wrapped fence need the same binary as a normal run
(``--sonda`` or the default path) and fail without it; ``--self-test
--no-binary`` skips exactly those, by name. ``--list`` prints what would be
checked and exits.
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
from collections import Counter
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
    # Set by `extract_builtin_pack_scenarios`. Carried rather than recomputed
    # so the floor in `run_validation` counts what was actually extracted.
    builtin_pack: bool = False
    # Set by `extract_fragments` for a fence `synthesize_block_fragment`
    # wrapped: a bare `encoder:` and/or `sink:` block.
    block: bool = False


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
_FRAGMENT_SCHEDULE = """defaults:
  rate: 1
  duration: 10s
"""

# The encoder and sink a fragment that names neither is compiled with. A
# bare block fragment (below) takes the half it leaves out from here too.
_DEFAULT_BLOCK = {
    "encoder": "{ type: json_lines }",
    "sink": "{ type: stdout }",
}

_FRAGMENT_DEFAULTS = (
    _FRAGMENT_SCHEDULE
    + f"  encoder: {_DEFAULT_BLOCK['encoder']}\n"
    + f"  sink: {_DEFAULT_BLOCK['sink']}\n"
)


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
    shaped like a scenario or a list of them. A bare ``encoder:`` or ``sink:``
    block is not either; :func:`synthesize_block_fragment` wraps those.

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


# --- Encoder and sink blocks -------------------------------------------------
#
# The encoder and sink pages show their options as bare blocks:
#
#     sink:
#       type: kafka
#       brokers: "127.0.0.1:9092"
#
# A fence whose top-level keys are a non-empty subset of {encoder, sink}, each
# a map with a `type:`, is a BLOCK FRAGMENT. It is wrapped under `defaults:`
# in the same minimal file the fragments above use, the half it leaves out
# defaulted, with constant metrics, and dry-run like any other fragment.
#
# Validation pairs some halves with each other in both directions, so the
# default for the missing half would be refused for them. `_PAIRED_SINK` and
# `_PAIRED_ENCODER` name the minimal partner instead.
#
# The wrapper has one metric, `doc_fragment` with no labels, except under a
# fence's own `snmp` or `gnmi` encoder. Validation requires each of their
# metrics to have a template and a label for each placeholder of it, and a
# gnmi encoder's every `values:` and `paths:` key to name a metric the file
# emits. There `_wrapper_metrics` gives the wrapper one metric per key of
# `oids:` (snmp) or of `values:` and `paths:` (gnmi), each with a label "1"
# for every required placeholder of its template and, for snmp, every
# `label_columns:` key. Reading those keys takes `_block_entries` and
# `_flow_entries`, a reader for the block and flow mappings these fences use,
# not a YAML parser. What it cannot read leaves a metric without a template
# or a label, or a gnmi key without a metric, which the engine refuses: a
# misread turns the run red rather than passing it.

# A paired encoder's minimal sink.
_PAIRED_SINK = {
    "snmp": '{ type: snmp_agent, listen: "127.0.0.1:0" }',
    "gnmi": '{ type: gnmi_target, listen: "127.0.0.1:0" }',
}

# A paired sink's minimal encoder. The OID is a scalar in net-snmp's
# experimental subtree, which the agent's system group does not cover.
_PAIRED_ENCODER = {
    "snmp_agent": '{ type: snmp, oids: { doc_fragment: "1.3.6.1.4.1.8072.9999.1.1.0" } }',
    "snmp_trap": '{ type: snmp, oids: { doc_fragment: "1.3.6.1.4.1.8072.9999.1.1.0" } }',
    "gnmi_target": '{ type: gnmi, path: "/sonda/doc-fragment" }',
}

_BLOCK_KEYS = ("encoder", "sink")

# `key:` or `key: value` at the start of a stripped line. A key is plain or
# quoted; the colon must be followed by a space or the end of the line.
_MAP_LINE_RE = re.compile(
    r"""^(?P<key>"[^"]*"|'[^']*'|[^\s"'#\-{\[][^:#]*?)[ \t]*:(?:[ \t]+(?P<value>.*?))?[ \t]*$"""
)

# A template placeholder: `{label}`, snmp's `{label:str}` or gnmi's optional
# `{label?}`, which needs no label.
_PLACEHOLDER_RE = re.compile(r"\{([A-Za-z_][A-Za-z0-9_]*)(:str|\?)?\}")


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _unquote(text: str) -> str:
    """A scalar's text: a quoted scalar without its quotes, else as written."""
    text = text.strip()
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "\"'":
        return text[1:-1]
    return text


def _strip_comment(value: str) -> str:
    """``value`` without a trailing `` # comment`` outside quotes."""
    quote = ""
    for i, char in enumerate(value):
        if quote:
            if char == quote:
                quote = ""
        elif char in "\"'":
            quote = char
        elif char == "#" and (i == 0 or value[i - 1] in " \t"):
            return value[:i].rstrip()
    return value.rstrip()


def _block_entries(lines: Sequence[str]) -> list[tuple[str, str, list[str]]] | None:
    """The entries of the block mapping ``lines`` hold, or ``None``.

    Each entry is ``(key, value, children)``: the key unquoted, the text after
    its colon without a trailing comment, and the lines indented under it.
    The mapping's indent is that of its first line that is neither blank nor
    a comment. A line at that indent that is not ``key:`` or ``key: value``
    (a sequence item, a document marker, a scalar), or a line indented less,
    means the lines are not one block mapping.
    """
    entries: list[tuple[str, str, list[str]]] = []
    indent: int | None = None
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            if entries:
                entries[-1][2].append(line)
            continue
        level = _indent(line)
        if indent is None:
            indent = level
        if level > indent and entries:
            entries[-1][2].append(line)
            continue
        if level != indent:
            return None
        match = _MAP_LINE_RE.match(stripped)
        if match is None:
            return None
        value = _strip_comment(match.group("value") or "")
        entries.append((_unquote(match.group("key")), value, []))
    return entries if indent is not None else None


def _flow_entries(text: str) -> list[tuple[str, str]] | None:
    """The top-level ``(key, value)`` pairs of a flow mapping ``{ k: v, ... }``.

    Values are returned as written (a nested flow collection whole), keys
    unquoted. ``None`` when ``text`` is not one flow mapping.
    """
    text = text.strip()
    if not (text.startswith("{") and text.endswith("}")):
        return None
    parts: list[str] = []
    depth = 0
    quote = ""
    start = 1
    for i, char in enumerate(text):
        if quote:
            if char == quote:
                quote = ""
            continue
        if char in "\"'":
            quote = char
        elif char in "{[":
            depth += 1
        elif char in "}]":
            depth -= 1
            if depth == 0 and i != len(text) - 1:
                return None  # text after the mapping's closing brace
        elif char == "," and depth == 1:
            parts.append(text[start:i])
            start = i + 1
    if depth != 0 or quote:
        return None
    parts.append(text[start:-1])
    entries: list[tuple[str, str]] = []
    for part in parts:
        if not part.strip():
            continue
        match = _MAP_LINE_RE.match(part.strip())
        if match is None:
            return None
        entries.append((_unquote(match.group("key")), (match.group("value") or "").strip()))
    return entries


def _map_entries(value: str, children: list[str]) -> list[tuple[str, str, list[str]]] | None:
    """The entries of a mapping written as a block (``value`` empty) or a flow."""
    if value == "":
        return _block_entries(children)
    flow = _flow_entries(value)
    if flow is None:
        return None
    return [(key, inner, []) for key, inner in flow]


def block_fragment_types(text: str) -> dict[str, str] | None:
    """``{"encoder": type, "sink": type}`` for a block fragment, else ``None``.

    A block fragment is a fence whose top-level keys are a non-empty subset
    of ``{encoder, sink}``, each a map with a ``type:``. Only the halves the
    fence carries are in the result. A ``# sonda:static`` fence is never one.
    """
    body = normalize_fence(text)
    if re.search(r"^#[ \t]*sonda:static\b", body, re.MULTILINE):
        return None
    top = _block_entries(body.split("\n"))
    if not top:
        return None
    types: dict[str, str] = {}
    for key, value, children in top:
        if key not in _BLOCK_KEYS:
            return None
        entries = _map_entries(value, children)
        if entries is None:
            return None
        kind = next((_unquote(inner) for name, inner, _ in entries if name == "type"), "")
        if not kind:
            return None
        types[key] = kind
    return types


def _wrapper_metrics(body: str, encoder_type: str | None) -> list[tuple[str, dict[str, str]]]:
    """The wrapper's metrics, each ``(name, labels)``, in the order the fence names them.

    ``encoder_type`` is the type of the fence's own ``encoder:``, or ``None``
    when the fence has none. One ``doc_fragment`` with no labels, except for
    an ``snmp`` or ``gnmi`` encoder: see the section note above.
    """
    fallback = [("doc_fragment", {})]
    if encoder_type not in ("snmp", "gnmi"):
        return fallback
    top = _block_entries(body.split("\n")) or []
    encoder = next(((v, c) for k, v, c in top if k == "encoder"), None)
    if encoder is None:
        return fallback
    fields = {k: (v, c) for k, v, c in (_map_entries(*encoder) or [])}

    def field_map(name: str) -> list[tuple[str, str]]:
        if name not in fields:
            return []
        return [(k, _unquote(v)) for k, v, _ in (_map_entries(*fields[name]) or [])]

    templates: dict[str, str] = {}
    columns: list[str] = []
    if encoder_type == "snmp":
        templates = dict(field_map("oids"))
        columns = [column for column, _ in field_map("label_columns")]
    else:
        path = _unquote(fields["path"][0]) if "path" in fields else ""
        paths = dict(field_map("paths"))
        for name, _ in field_map("values") + list(paths.items()):
            templates.setdefault(name, paths.get(name, path))
        if not templates:
            templates["doc_fragment"] = path
    metrics: list[tuple[str, dict[str, str]]] = []
    for name, template in templates.items():
        labels = {column: "1" for column in columns}
        for match in _PLACEHOLDER_RE.finditer(template):
            label, form = match.group(1), match.group(2)
            if form == "?" or (encoder_type == "gnmi" and label == "name"):
                continue
            labels[label] = "1"
        metrics.append((name, dict(sorted(labels.items()))))
    return metrics or fallback


def synthesize_block_fragment(text: str) -> str | None:
    """Wrap a bare ``encoder:`` and/or ``sink:`` fence into a runnable file.

    ``None`` for anything :func:`block_fragment_types` refuses. The fence is
    indented under ``defaults:`` whole, so its own text is what is compiled;
    the missing half is ``_PAIRED_SINK``/``_PAIRED_ENCODER`` for a paired
    half, else ``_DEFAULT_BLOCK``.
    """
    types = block_fragment_types(text)
    if types is None:
        return None
    body = normalize_fence(text).rstrip("\n")
    added = []
    if "encoder" not in types:
        encoder = _PAIRED_ENCODER.get(types["sink"], _DEFAULT_BLOCK["encoder"])
        added.append(f"  encoder: {encoder}")
    if "sink" not in types:
        sink = _PAIRED_SINK.get(types["encoder"], _DEFAULT_BLOCK["sink"])
        added.append(f"  sink: {sink}")
    indented = ["  " + line if line.strip() else "" for line in body.split("\n")]
    entries: list[str] = []
    for name, labels in _wrapper_metrics(body, types.get("encoder")):
        entries += [
            "  - signal_type: metrics",
            f"    name: {json.dumps(name)}",
            "    generator: { type: constant, value: 1.0 }",
        ]
        if labels:
            pairs = ", ".join(f"{json.dumps(k)}: {json.dumps(v)}" for k, v in labels.items())
            entries.append(f"    labels: {{ {pairs} }}")
    return (
        _FRAGMENT_PREAMBLE
        + _FRAGMENT_SCHEDULE
        + "\n".join(indented + added)
        + "\nscenarios:\n"
        + "\n".join(entries)
        + "\n"
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


#: Floor on the builtin-pack fences in ``docs/``. This extractor's failure
#: mode is silence — a stale ``target/release/sonda`` knows fewer packs, so
#: every fence naming a newer one is declined and ``0 compiled`` reads as
#: ``all fine``. Raise it when you add fences.
MIN_BUILTIN_PACK_FENCES = 7


def builtin_pack_names(sonda_bin: Path | None) -> frozenset[str]:
    """Every pack the binary resolves with no ``--catalog``.

    Read from ``sonda list --json`` rather than restated here, so a pack added
    to or removed from the embedded set moves this set on its own.

    Raises rather than returning empty: an empty set declines every pack
    fence, which reads as a clean run.
    """
    if sonda_bin is None:
        return frozenset()
    proc = subprocess.run(
        [str(sonda_bin), "list", "--json", "--kind", "composable"],
        capture_output=True,
        text=True,
        timeout=DEFAULT_SUBPROCESS_TIMEOUT_S,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"`sonda list --json` failed: {proc.stderr.strip()[:300]}")
    names = frozenset(
        entry["name"]
        for entry in json.loads(proc.stdout)
        if entry.get("origin") == "builtin"
    )
    if not names:
        raise RuntimeError(
            f"`{sonda_bin} list --json --kind composable` returned no builtin "
            "packs. The binary embeds several, so this means the binary or its "
            "output shape is not what this gate expects — every pack fence "
            "would be declined and the run would report green."
        )
    return names


def packs_referenced(body: str) -> list[str]:
    """Every value a ``pack:`` key names in ``body``, in order."""
    return [
        m.group(1).strip().strip("'\"")
        for m in re.finditer(r"^[ \t]*(?:-[ \t]+)?pack:[ \t]*(\S+)", body, re.MULTILINE)
    ]


def extract_builtin_pack_scenarios(
    md_path: Path, markdown_text: str, builtins: frozenset[str]
) -> list[ExtractedScenario]:
    """Complete scenarios whose ``pack:`` references all resolve with no catalog.

    :func:`is_runnable_scenario` refuses anything carrying ``pack:``, and must
    keep refusing it: that function is the character-for-character mirror of
    ``runnableScenario`` in ``sonda-pure.js``, which decides whether a fence
    gets a "Run in playground" button — and the playground builds an empty
    resolver, so a pack reference cannot resolve in the browser at all.

    The CLI is a different matter. Since the builtin catalog shipped, ``pack:
    node_exporter_cpu`` compiles with no ``--catalog`` and no files on disk,
    so the compile gate CAN speak about these fences even though the button
    cannot. Without this, every example on the built-in packs page was
    declined and nothing checked it.

    Only fences whose every ``pack:`` value is a builtin qualify. One naming a
    user pack still needs a directory this gate cannot supply, and is left
    declined rather than compiled against a catalog that does not exist.
    """
    if not builtins:
        return []
    out: list[ExtractedScenario] = []
    for line, body, info in extract_yaml_fences(markdown_text):
        normalized = normalize_fence(body)
        referenced = packs_referenced(normalized)
        if not referenced or not all(name in builtins for name in referenced):
            continue
        if re.search(r"^#[ \t]*sonda:static\b", normalized, re.MULTILINE):
            continue
        if not re.search(r"^version:[ \t]*2[ \t]*$", normalized, re.MULTILINE):
            continue
        if not re.search(r"^kind:[ \t]*runnable[ \t]*$", normalized, re.MULTILINE):
            continue
        if not re.search(r"^scenarios:", normalized, re.MULTILINE):
            continue
        out.append(
            ExtractedScenario(
                file=md_path,
                line=line,
                body=normalized,
                info=info,
                builtin_pack=True,
            )
        )
    return out


def extract_fragments(md_path: Path, markdown_text: str) -> list[ExtractedScenario]:
    """Return every FRAGMENT fence, wrapped so the engine can parse it.

    The complement of :func:`extract_scenarios`: these fences carry no button
    and no gate could previously reach them, which is where three of this
    program's field-name bugs were found. See the module note above
    :func:`synthesize_fragment` for why the wrapping is kept minimal. A bare
    ``encoder:``/``sink:`` fence is wrapped by
    :func:`synthesize_block_fragment` and marked ``block``.
    """
    out: list[ExtractedScenario] = []
    for line, body, info in extract_yaml_fences(markdown_text):
        document = synthesize_fragment(body)
        block = False
        if document is None:
            document = synthesize_block_fragment(body)
            block = document is not None
        if document is None:
            continue
        out.append(
            ExtractedScenario(
                file=md_path,
                line=line,
                body=document,
                info=info,
                synthesized=True,
                block=block,
            )
        )
    return out


def decline_reason(text: str) -> str:
    """Why no tier compiles a fence: one line, the same for fences alike.

    Called only for fences none of :func:`extract_scenarios`,
    :func:`extract_builtin_pack_scenarios` and :func:`extract_fragments`
    took, checked in the order :func:`synthesize_fragment` refuses them: the
    opt-out marker, a ``pack:`` reference, a ``version:`` header, a
    ``scenarios:`` key that does not open a block list, and otherwise the
    fence's top-level keys, sorted, as written.
    """
    body = normalize_fence(text)
    if re.search(r"^#[ \t]*sonda:static\b", body, re.MULTILINE):
        return "marked # sonda:static"
    if re.search(r"^[ \t]*(?:-[ \t]+)?pack:", body, re.MULTILINE):
        return "pack: reference, not a complete scenario of builtin packs this run resolves"
    if re.search(r"^version:", body, re.MULTILINE):
        return "version: header, not a runnable scenario"
    if re.search(r"^scenarios:", body, re.MULTILINE):
        return "scenarios: is not a block list"
    keys = sorted(
        {
            m.group(1)
            for m in re.finditer(r"^([^\s#:\-][^:\n]*?):(?:[ \t]|$)", body, re.MULTILINE)
        }
    )
    if not keys:
        return "no top-level key"
    return "top-level " + ", ".join(f"{key}:" for key in keys)


@dataclasses.dataclass
class MarkdownScan:
    """What one markdown document holds for this gate."""

    scenarios: list[ExtractedScenario]
    # One entry per yaml fence no tier took: (first content line, reason).
    declined: list[tuple[int, str]]


def scan_markdown(
    md_path: Path, markdown_text: str, builtins: frozenset[str]
) -> MarkdownScan:
    """Every fence of one document, either extracted by a tier or declined."""
    scenarios = (
        extract_scenarios(md_path, markdown_text)
        + extract_builtin_pack_scenarios(md_path, markdown_text, builtins)
        + extract_fragments(md_path, markdown_text)
    )
    taken = {scenario.line for scenario in scenarios}
    declined = [
        (line, decline_reason(body))
        for line, body, _ in extract_yaml_fences(markdown_text)
        if line not in taken
    ]
    return MarkdownScan(scenarios=scenarios, declined=declined)


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
) -> tuple[list[ScenarioResult], list[ScenarioResult], Counter[str]]:
    """Validate every reachable fence.

    Returns ``(all_results, failures, declined)``, where ``declined`` counts the
    yaml fences no tier could speak about by their :func:`decline_reason`.
    Reported rather than inferred (review #545 M3): "52 fragments compiled"
    invites the question "out of how many?", and a reader of the run cannot
    tell 45 declines from 5 without a script of their own.
    """
    docs_root = repo_root / DOCS_GLOB_ROOT
    if not docs_root.is_dir():
        raise RuntimeError(f"docs root not found: {docs_root}")

    skip_set = {str(s) for s in skip_files}
    builtins = builtin_pack_names(sonda_bin)
    scenarios: list[ExtractedScenario] = []
    declined: Counter[str] = Counter()
    for md in iter_markdown_files(docs_root):
        rel = str(md.relative_to(repo_root)) if md.is_absolute() else str(md)
        if rel in skip_set:
            continue
        scan = scan_markdown(md, md.read_text(encoding="utf-8"), builtins)
        scenarios.extend(scan.scenarios)
        declined.update(reason for _, reason in scan.declined)

    results = [
        validate_scenario(
            scenario,
            repo_root=repo_root,
            sonda_bin=sonda_bin,
            subprocess_timeout=subprocess_timeout,
        )
        for scenario in scenarios
    ]
    if sonda_bin is not None:
        found = sum(1 for s in scenarios if s.builtin_pack)
        if found < MIN_BUILTIN_PACK_FENCES:
            raise RuntimeError(
                f"only {found} builtin-pack fences extracted, expected at least "
                f"{MIN_BUILTIN_PACK_FENCES}. Either fences were removed (lower the "
                "floor deliberately) or the builtin set this gate read is wrong — "
                f"it saw {sorted(builtins)}. A stale `target/release/sonda` does "
                "exactly this."
            )

    return results, [r for r in results if not r.ok], declined


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

    repo_root = find_repo_root(Path(__file__).parent)

    if args.self_test:
        if args.no_binary:
            return _run_self_tests(None)
        raw_bin = args.sonda or (repo_root / DEFAULT_SONDA_BINARY)
        return _run_self_tests(raw_bin if raw_bin.is_absolute() else repo_root / raw_bin)

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
    blocks = sum(1 for r in results if r.scenario.block)
    print(
        f"{len(results) - fragments} runnable scenario fences and {fragments} "
        f"fragments ({blocks} encoder/sink blocks) found, "
        f"{len(results) - skipped - len(failures)} compiled, "
        f"{skipped} skipped, {len(failures)} failed; "
        f"{sum(declined.values())} yaml fences declined:",
        file=sys.stderr,
    )
    for reason, count in sorted(declined.items(), key=lambda item: (-item[1], item[0])):
        print(f"  {count:3d}  {reason}", file=sys.stderr)

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

    def test_bare_encoder_block_is_not_an_entry(self) -> None:
        # Not an entry: it carries no key that only a scenario entry has.
        # `synthesize_block_fragment` is the tier that wraps it.
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


#: Floor on the bare ``encoder:``/``sink:`` fences in ``docs/`` that the block
#: tier wraps. Asserted by the self-test before it asserts they compile, so a
#: tier that stops recognising them fails instead of compiling nothing.
MIN_BLOCK_FRAGMENTS = 2

# The binary the compiling self-tests use; `None` under --no-binary. Set by
# `_run_self_tests` from the same flags a normal run reads.
_SELF_TEST_BINARY: Path | None = None


def _wrap(text: str) -> str:
    """``synthesize_block_fragment(text)``, failing the calling test on ``None``."""
    document = synthesize_block_fragment(text)
    if document is None:
        raise AssertionError(f"not wrapped as a block fragment:\n{text}")
    return document


class _BlockFragmentShapeTests(unittest.TestCase):
    """Which fences the block tier wraps, and the file it wraps them into."""

    def test_bare_encoder_is_wrapped_with_the_default_sink(self) -> None:
        out = _wrap("encoder:\n  type: prometheus_text\n  precision: 2\n")
        self.assertTrue(out.startswith("version: 2\nkind: runnable\ndefaults:\n"))
        # The fence's own text, whole, one indent under defaults:.
        self.assertIn("\n  encoder:\n    type: prometheus_text\n    precision: 2\n", out)
        self.assertIn(f"\n  sink: {_DEFAULT_BLOCK['sink']}\n", out)
        self.assertEqual(out.count("encoder:"), 1)
        self.assertIn('    name: "doc_fragment"\n', out)
        self.assertIn("    generator: { type: constant, value: 1.0 }\n", out)

    def test_bare_sink_is_wrapped_with_the_default_encoder(self) -> None:
        out = _wrap('sink:\n  type: kafka\n  brokers: "127.0.0.1:9092"\n  topic: t\n')
        self.assertIn('\n  sink:\n    type: kafka\n    brokers: "127.0.0.1:9092"\n', out)
        self.assertIn(f"\n  encoder: {_DEFAULT_BLOCK['encoder']}\n", out)
        self.assertEqual(out.count("sink:"), 1)

    def test_a_fence_with_both_halves_has_nothing_defaulted(self) -> None:
        out = _wrap(
            "encoder:\n  type: remote_write\nsink:\n  type: remote_write\n"
            '  url: "http://localhost:8428/api/v1/write"\n'
        )
        self.assertEqual(out.count("encoder:"), 1)
        self.assertEqual(out.count("sink:"), 1)
        self.assertNotIn("json_lines", out)
        self.assertNotIn("stdout", out)

    def test_flow_halves_and_a_comment_are_read(self) -> None:
        self.assertEqual(
            block_fragment_types(
                "# shown on the sinks page\nencoder: { type: otlp }\n"
                "sink: { type: otlp_grpc, endpoint: 'http://x:4317' }  # local\n"
            ),
            {"encoder": "otlp", "sink": "otlp_grpc"},
        )

    def test_an_indented_fence_is_read_after_normalising(self) -> None:
        self.assertEqual(
            block_fragment_types("    sink:\n      type: stdout\n"), {"sink": "stdout"}
        )

    def test_an_snmp_encoder_gets_the_agent_partner_and_its_own_metrics(self) -> None:
        out = _wrap(
            "encoder:\n  type: snmp\n  oids:\n"
            '    ifHCInOctets: "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"\n'
            '    sysDescrLike: "1.3.6.1.4.1.8072.9999.1.2.{ifName:str}"\n'
            "  label_columns:\n"
            '    ifType: { oid: "1.3.6.1.2.1.2.2.1.3.{ifIndex}", type: integer }\n'
        )
        self.assertIn(f"\n  sink: {_PAIRED_SINK['snmp']}\n", out)
        self.assertIn("snmp_agent", _PAIRED_SINK["snmp"])
        self.assertIn('127.0.0.1:0"', _PAIRED_SINK["snmp"])
        # One metric per oids: key, a label for each placeholder and column.
        self.assertIn(
            '    name: "ifHCInOctets"\n    generator: { type: constant, value: 1.0 }\n'
            '    labels: { "ifIndex": "1", "ifType": "1" }\n',
            out,
        )
        self.assertIn('    labels: { "ifName": "1", "ifType": "1" }\n', out)
        self.assertNotIn("doc_fragment", out)

    def test_snmp_sinks_get_the_snmp_encoder_partner(self) -> None:
        for sink in ("snmp_agent", "snmp_trap"):
            with self.subTest(sink=sink):
                out = _wrap(f"sink:\n  type: {sink}\n")
                self.assertIn(f"\n  encoder: {_PAIRED_ENCODER[sink]}\n", out)
                self.assertIn("type: snmp,", _PAIRED_ENCODER[sink])
                self.assertIn('    name: "doc_fragment"\n', out)

    def test_a_gnmi_encoder_gets_the_target_partner_and_one_metric_per_key(self) -> None:
        out = _wrap(
            "encoder:\n  type: gnmi\n"
            '  path: "/interfaces/interface[name={ifName}]/state/counters/{name}"\n'
            "  values:\n    in_octets: uint\n    oper_status:\n      enum: { 1: UP }\n"
            "  paths:\n"
            '    oper_status: "/interfaces/interface[name={ifName}]/state[le={le?}]"\n'
            '    admin: "/admin[unit={unit}]"\n'
        )
        self.assertIn(f"\n  sink: {_PAIRED_SINK['gnmi']}\n", out)
        names = re.findall(r'^    name: "([^"]+)"$', out, re.MULTILINE)
        self.assertEqual(names, ["in_octets", "oper_status", "admin"])
        # {name} is the metric name and {le?} is optional: neither is a label.
        self.assertEqual(out.count('labels: { "ifName": "1" }'), 2)
        self.assertIn('labels: { "unit": "1" }', out)

    def test_a_gnmi_target_sink_gets_the_gnmi_encoder_partner(self) -> None:
        out = _wrap('sink:\n  type: gnmi_target\n  listen: "127.0.0.1:57400"\n')
        self.assertIn(f"\n  encoder: {_PAIRED_ENCODER['gnmi_target']}\n", out)

    def test_a_third_top_level_key_is_not_a_block_fragment(self) -> None:
        text = "encoder:\n  type: json_lines\nsink:\n  type: stdout\nrate: 1\n"
        self.assertIsNone(block_fragment_types(text))
        self.assertIsNone(synthesize_block_fragment(text))

    def test_a_half_without_a_type_is_not_a_block_fragment(self) -> None:
        self.assertIsNone(block_fragment_types("sink:\n  url: http://x\n"))
        self.assertIsNone(block_fragment_types("sink: stdout\n"))
        self.assertIsNone(block_fragment_types("encoder: { precision: 2 }\n"))
        # A type: nested deeper is not the block's own.
        self.assertIsNone(block_fragment_types("sink:\n  tls:\n    type: x\n"))

    def test_other_shapes_are_not_block_fragments(self) -> None:
        for text in (
            "",
            "# only a comment\n",
            "- sink:\n    type: stdout\n",
            "sink:\n  type: stdout\n---\nsink:\n  type: stdout\n",
            "generator:\n  type: constant\n",
        ):
            with self.subTest(text=text):
                self.assertIsNone(block_fragment_types(text))

    def test_a_static_block_stays_static(self) -> None:
        text = "# sonda:static - excerpt\nsink:\n  type: gnmi_target\n  listen: 0.0.0.0:57401\n"
        self.assertIsNone(synthesize_block_fragment(text))
        self.assertEqual(decline_reason(text), "marked # sonda:static")


class _ScanMarkdownTests(unittest.TestCase):
    """Every fence of a document is extracted by one tier or declined, counted."""

    def test_block_fragments_are_extracted_and_marked(self) -> None:
        md = "```yaml\nsink:\n  type: stdout\n```\n\n```yaml\ngenerator:\n  type: sine\n```\n"
        scan = scan_markdown(Path("x.md"), md, frozenset())
        self.assertEqual([(s.line, s.block) for s in scan.scenarios], [(2, True), (7, False)])
        self.assertTrue(all(s.synthesized for s in scan.scenarios))
        self.assertEqual(scan.declined, [])

    def test_a_third_top_level_key_is_declined_and_counted(self) -> None:
        md = "```yaml\nencoder:\n  type: json_lines\nsink:\n  type: stdout\nrate: 1\n```\n"
        scan = scan_markdown(Path("x.md"), md, frozenset())
        self.assertEqual(scan.scenarios, [])
        self.assertEqual(scan.declined, [(2, "top-level encoder:, rate:, sink:")])

    def test_decline_reasons_follow_the_tiers_order(self) -> None:
        cases = {
            "# sonda:static\nversion: 2\nkind: runnable\n": "marked # sonda:static",
            "scenarios:\n  - pack: my_own_pack\n": (
                "pack: reference, not a complete scenario of builtin packs this run resolves"
            ),
            "version: 2\nkind: composable\n": "version: header, not a runnable scenario",
            "scenarios:\n  cpu.yaml: |\n    x\n": "scenarios: is not a block list",
            "groups:\n  - name: x\n": "top-level groups:",
            "- just: a list\n": "no top-level key",
        }
        for text, reason in cases.items():
            with self.subTest(reason=reason):
                self.assertEqual(decline_reason(text), reason)


class _RealDocsBlockFragmentTests(unittest.TestCase):
    """The tier against the real docs: found, before anything is compiled."""

    def test_the_real_docs_hold_block_fragments(self) -> None:
        self.assertGreaterEqual(len(_real_docs_block_fragments()), MIN_BLOCK_FRAGMENTS)


def _real_docs_block_fragments() -> list[ExtractedScenario]:
    repo_root = find_repo_root(Path(__file__).parent)
    found: list[ExtractedScenario] = []
    for md in iter_markdown_files(repo_root / DOCS_GLOB_ROOT):
        scan = scan_markdown(md, md.read_text(encoding="utf-8"), frozenset())
        found.extend(s for s in scan.scenarios if s.block)
    return found


class _CompilingTestCase(unittest.TestCase):
    """A test that compiles with the sonda binary a normal run uses."""

    repo_root: Path

    def setUp(self) -> None:
        self.repo_root = find_repo_root(Path(__file__).parent)

    def binary(self) -> Path:
        if _SELF_TEST_BINARY is None:
            self.skipTest("--no-binary: this test compiles with the sonda binary")
        if not _SELF_TEST_BINARY.is_file():
            self.fail(
                f"sonda binary not found at {_SELF_TEST_BINARY}. Build it with the "
                "docs job's features (task site:scenario-check does) or pass "
                "--no-binary to skip the tests that compile."
            )
        return _SELF_TEST_BINARY

    def compile_markdown(self, md: str) -> list[ScenarioResult]:
        """Scan ``md`` as a file named ``page.md``; validate what it holds."""
        scan = scan_markdown(Path("page.md"), md, frozenset())
        self.assertTrue(scan.scenarios, "the document must hold something to compile")
        sonda_bin = self.binary()
        return [validate_scenario(s, self.repo_root, sonda_bin) for s in scan.scenarios]

    def assert_compiles(self, fence: str) -> None:
        results = self.compile_markdown(f"```yaml\n{fence}```\n")
        self.assertEqual(len(results), 1)
        self.assertTrue(results[0].scenario.block, "must be the block tier's")
        self.assertTrue(results[0].ok, results[0].message)
        self.assertEqual(results[0].skipped_reason, "")


class _BlockFragmentCompileTests(_CompilingTestCase):
    """Wrapped block fragments compile, and a broken one fails the run."""

    def test_a_bare_encoder_compiles(self) -> None:
        self.assert_compiles("encoder:\n  type: influx_lp\n  field_key: cpu_percent\n")

    def test_a_bare_sink_compiles(self) -> None:
        self.assert_compiles('sink:\n  type: tcp\n  address: "127.0.0.1:9999"\n')

    def test_a_fence_with_both_halves_compiles(self) -> None:
        self.assert_compiles(
            "encoder:\n  type: remote_write\nsink:\n  type: remote_write\n"
            '  url: "http://localhost:8428/api/v1/write"\n'
        )

    def test_an_snmp_encoder_compiles_with_its_agent_partner(self) -> None:
        self.assert_compiles(
            "encoder:\n  type: snmp\n  oids:\n"
            '    ifHCInOctets: "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}"\n'
            "  types:\n    ifHCInOctets: counter64\n"
        )

    def test_paired_sinks_compile_with_their_encoder_partner(self) -> None:
        self.binary()  # one skip under --no-binary, not one per fence
        for fence in (
            "sink:\n  type: snmp_agent\n  listen: 0.0.0.0:1161\n",
            "sink:\n  type: snmp_trap\n  destination: 127.0.0.1:1162\n",
            "sink:\n  type: gnmi_target\n  listen: 0.0.0.0:57400\n",
        ):
            with self.subTest(fence=fence):
                self.assert_compiles(fence)

    def test_a_broken_fence_fails_the_run_naming_file_and_line(self) -> None:
        md = "# Agent\n\nProse.\n\n```yaml\nsink: { type: snmp_agent, listen: 999 }\n```\n"
        self.assertEqual(md.splitlines()[5], "sink: { type: snmp_agent, listen: 999 }")
        results = self.compile_markdown(md)
        self.assertEqual(len(results), 1)
        self.assertTrue(results[0].scenario.block)
        self.assertFalse(results[0].ok, "the broken listen must fail the run")
        self.assertIn('listen "999"', results[0].message)
        report = format_failure(results[0], self.repo_root)
        self.assertTrue(report.startswith("FAIL page.md:6\n"), report)

    def test_every_block_fragment_in_the_real_docs_compiles(self) -> None:
        found = _real_docs_block_fragments()
        self.assertGreaterEqual(len(found), MIN_BLOCK_FRAGMENTS)
        sonda_bin = self.binary()
        failed = [
            f"{s.file.relative_to(self.repo_root)}:{s.line}: {r.message}"
            for s in found
            for r in [validate_scenario(s, self.repo_root, sonda_bin)]
            if not r.ok
        ]
        self.assertEqual(failed, [])


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


def _run_self_tests(sonda_bin: Path | None) -> int:
    """Run every test in this module; ``sonda_bin`` ``None`` means --no-binary."""
    global _SELF_TEST_BINARY
    _SELF_TEST_BINARY = sonda_bin
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromModule(sys.modules[__name__])
    runner = unittest.TextTestRunner(verbosity=2)
    return 0 if runner.run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
