#!/usr/bin/env python3
"""Turn the example-scenario tables on ``test/examples.md`` into a live gallery.

The page lists 66 files in ``examples/`` across fifteen markdown tables. Every
row names a real scenario the reader could run — and, until now, could only
read about. This hook reads each named file at build time and emits a card
carrying the file's own YAML, base64url-encoded, which ``livegen.js`` mounts
as a mini-chart and an "Open in playground →" link.

Three properties are load-bearing:

**It augments, never replaces.** The tables stay exactly as authored; the
gallery is appended after each one. With JavaScript off, or the wasm engine
blocked, the page is the page it was before (Law 5). Nothing in a card is
built by assigning markup, and every value that reaches the HTML goes through
:func:`escape` — the YAML is repo-authored, but the rule is absolute.

**It claims nothing the engine has not been asked.** A card does not say "this
runs"; it carries the file and lets the browser's own engine answer. The five
possible answers are decided by ``galleryCardState`` in ``sonda-pure.js``,
which is where the difference between "ok" and "there is a chart" lives —
every csv_replay example samples to ``ok: true`` with no entries at all.

**It is deterministic.** Same sources, byte-identical page: rows are processed
in document order, no timestamps, no set iteration, no filesystem ordering.
``scripts/validate_docs_scenarios.py`` builds twice and diffs to prove it, and
also compiles every file this hook cards so a card cannot outlive its example.

Which rows get a card is decided by the same ``runnableScenario`` detector
that decides which fences get a button: a row pointing at a vmalert rules
file, an Alertmanager config or a ``kind: composable`` pack is left as a plain
table row.

Runs standalone for its own tests::

    python3 docs/site/hooks/examples_gallery.py --self-test
"""

from __future__ import annotations

import base64
import re
import sys
from dataclasses import dataclass
from html import escape
from pathlib import Path

PAGE = "test/examples.md"

# A table row whose first cell is a single inline-code filename. The examples
# tables are hand-authored and uniform in this respect; a row that is not this
# shape (a header, the |---| rule, prose) is skipped rather than guessed at.
ROW_RE = re.compile(r"^\|\s*`([^`]+\.ya?ml)`\s*\|(.*)\|\s*$")


@dataclass(frozen=True)
class Card:
    """One gallery card: an example file and the row that introduced it."""

    path: str  # as written in the table, e.g. "alertmanager/alerting-scenario.yaml"
    description: str
    yaml_b64: str


def normalize_fence(text: str) -> str:
    """Mirror of ``normalizeFence`` in sonda-pure.js.

    Strips a UTF-8 BOM, folds CRLF, and removes the indentation common to
    every non-blank line. The dedent earns nothing on the files this hook
    reads — an example on disk is not indented — but it is here so the hook
    answers the *whole* shared case table rather than a convenient subset of
    it. A detector that agrees with its twins only on the cases it happens to
    reach is not held to them at all.
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
    """Mirror of ``runnableScenario`` (sonda-pure.js) — see that docstring.

    Duplicated here rather than imported from ``scripts/`` because a mkdocs
    hook must run from a plain ``mkdocs build`` with no path setup. The two
    copies are held together by the shared case table in
    ``docs/site/tools/tests/runnable-cases.json``, which
    ``validate_docs_scenarios.py`` runs against this function as well as its
    own — so a rule change that misses one of the three implementations fails
    CI rather than drifting.
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


def to_base64url(text: str) -> str:
    """URL-safe, unpadded base64 of ``text`` as UTF-8.

    Matches ``toBase64Url`` in sonda-pure.js, which is what reads it back —
    the padding has to go because the value lands in a URL fragment.
    """
    return base64.urlsafe_b64encode(text.encode("utf-8")).decode("ascii").rstrip("=")


def strip_markdown(cell: str) -> str:
    """Reduce a table cell to plain text for a card's description.

    Cells carry inline code, links and emphasis. A card shows one line of
    prose, so the markup is unwrapped rather than rendered: link text without
    the target, code without the backticks. Escaping happens later, at the
    point of emission, not here — this function's output is text, not HTML.
    """
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", cell)  # [text](target) -> text
    text = text.replace("`", "")
    text = re.sub(r"\*\*([^*]*)\*\*", r"\1", text)
    text = re.sub(r"(?<!\*)\*([^*]+)\*(?!\*)", r"\1", text)
    return " ".join(text.split())


def read_example(examples_dir: Path, name: str) -> str | None:
    """Return the text of ``examples/<name>``, or None if it is not there.

    A table row naming a file that does not exist is a docs bug worth seeing,
    but a build is the wrong place to fail on one — the row simply gets no
    card, and ``validate_docs_scenarios.py`` reports it as an error.
    """
    candidate = (examples_dir / name).resolve()
    try:
        # Table rows are repo-authored, but a `../` in one must not read
        # outside the examples tree just because nobody thought to check.
        candidate.relative_to(examples_dir.resolve())
    except ValueError:
        return None
    if not candidate.is_file():
        return None
    return candidate.read_text(encoding="utf-8")


def cards_for_row(examples_dir: Path, path: str, rest: str) -> Card | None:
    """Build the card for one table row, or None if the row does not get one."""
    text = read_example(examples_dir, path)
    if text is None or not is_runnable_scenario(text):
        return None
    cells = [c.strip() for c in rest.split("|")]
    description = strip_markdown(cells[-1]) if cells else ""
    return Card(path=path, description=description, yaml_b64=to_base64url(text))


def render_gallery(cards: list[Card]) -> str:
    """Render one gallery block. Pure string work — no I/O, no page state."""
    out = ['<div class="sonda-gallery">']
    for card in cards:
        out.append('  <div class="sonda-gallery__card">')
        out.append(f'    <p class="sonda-gallery__name"><code>{escape(card.path)}</code></p>')
        if card.description:
            out.append(f'    <p class="sonda-gallery__desc">{escape(card.description)}</p>')
        out.append(
            '    <div class="sonda-livegen sonda-gallery__live"'
            f' data-title="{escape(card.path, quote=True)}"'
            f' data-yaml-b64="{escape(card.yaml_b64, quote=True)}"></div>'
        )
        out.append("  </div>")
    out.append("</div>")
    return "\n".join(out)


def augment(markdown: str, examples_dir: Path) -> str:
    """Append a gallery after every table that names example files.

    Walks the document once. A run of consecutive lines starting with ``|`` is
    a table; when it ends, the cards collected from it are emitted before the
    next line. A table naming no runnable example produces nothing at all —
    the "Alerting pipeline" table, which lists an Alertmanager config and a
    vmalert rules file, is left as it was written.
    """
    lines = markdown.split("\n")
    out: list[str] = []
    pending: list[Card] = []
    in_table = False
    in_fence = False

    def flush() -> None:
        nonlocal pending
        if pending:
            out.append("")
            out.append(render_gallery(pending))
            pending = []

    for line in lines:
        # A pipe-prefixed line inside a fenced block is code, not a table.
        if line.lstrip().startswith("```"):
            in_fence = not in_fence

        if not in_fence and line.startswith("|"):
            in_table = True
            match = ROW_RE.match(line)
            if match:
                card = cards_for_row(examples_dir, match.group(1), match.group(2))
                if card is not None:
                    pending.append(card)
            out.append(line)
            continue

        if in_table:
            in_table = False
            flush()
        out.append(line)

    if in_table:
        flush()
    return "\n".join(out)


def find_examples_dir(docs_dir: Path) -> Path:
    """Locate ``examples/`` from mkdocs' ``docs_dir`` (docs/site/docs)."""
    for parent in [docs_dir, *docs_dir.parents]:
        candidate = parent / "examples"
        if candidate.is_dir() and (parent / "Cargo.toml").is_file():
            return candidate
    raise RuntimeError(f"could not locate the examples/ directory above {docs_dir}")


def on_page_markdown(markdown: str, page, config, files):  # noqa: ANN001, ARG001
    """mkdocs hook entry point."""
    if page.file.src_uri != PAGE:
        return None
    return augment(markdown, find_examples_dir(Path(config["docs_dir"])))


# --- Self-tests --------------------------------------------------------------


def _self_test() -> int:
    import json
    import tempfile
    import unittest

    repo_root = Path(__file__).resolve().parents[3]

    class Augment(unittest.TestCase):
        def setUp(self) -> None:
            self.dir = Path(tempfile.mkdtemp())
            (self.dir / "good.yaml").write_text(
                "version: 2\nkind: runnable\nname: cpu\n"
                "generator:\n  type: constant\n  value: 1\n",
                encoding="utf-8",
            )
            (self.dir / "rules.yml").write_text(
                "groups:\n  - name: x\n    rules: []\n", encoding="utf-8"
            )
            (self.dir / "pack.yaml").write_text(
                "version: 2\nkind: composable\nname: p\nmetrics: []\n", encoding="utf-8"
            )

        def test_runnable_row_gets_a_card(self) -> None:
            md = "| File | Description |\n|---|---|\n| `good.yaml` | A wave |\n"
            out = augment(md, self.dir)
            self.assertIn("sonda-gallery__card", out)
            self.assertIn("data-yaml-b64=", out)
            self.assertIn("A wave", out)

        def test_table_is_kept_verbatim(self) -> None:
            md = "| File | Description |\n|---|---|\n| `good.yaml` | A wave |\n"
            out = augment(md, self.dir)
            self.assertIn("| `good.yaml` | A wave |", out)

        def test_non_scenario_row_gets_no_card(self) -> None:
            md = "| File | Description |\n|---|---|\n| `rules.yml` | vmalert rules |\n"
            self.assertNotIn("sonda-gallery", augment(md, self.dir))

        def test_composable_pack_gets_no_card(self) -> None:
            md = "| File | Description |\n|---|---|\n| `pack.yaml` | A pack |\n"
            self.assertNotIn("sonda-gallery", augment(md, self.dir))

        def test_missing_file_gets_no_card(self) -> None:
            md = "| File | Description |\n|---|---|\n| `nope.yaml` | Gone |\n"
            self.assertNotIn("sonda-gallery", augment(md, self.dir))

        def test_traversal_out_of_examples_is_refused(self) -> None:
            outside = self.dir.parent / "outside.yaml"
            outside.write_text("version: 2\nkind: runnable\nname: x\n", encoding="utf-8")
            md = "| File | Description |\n|---|---|\n| `../outside.yaml` | Escape |\n"
            self.assertNotIn("sonda-gallery", augment(md, self.dir))

        def test_gallery_lands_after_the_table_not_inside_it(self) -> None:
            md = "| File | Description |\n|---|---|\n| `good.yaml` | A wave |\n\nProse after.\n"
            out = augment(md, self.dir)
            table_end = out.index("| `good.yaml` | A wave |")
            self.assertLess(table_end, out.index("sonda-gallery"))
            self.assertLess(out.index("sonda-gallery"), out.index("Prose after."))

        def test_pipe_lines_inside_a_fence_are_not_a_table(self) -> None:
            md = "```text\n| `good.yaml` | not a table |\n```\n"
            self.assertNotIn("sonda-gallery", augment(md, self.dir))

        def test_two_tables_get_two_galleries(self) -> None:
            md = (
                "| File | D |\n|---|---|\n| `good.yaml` | one |\n"
                "\n## Next\n\n"
                "| File | D |\n|---|---|\n| `good.yaml` | two |\n"
            )
            self.assertEqual(augment(md, self.dir).count('<div class="sonda-gallery"'), 2)

        def test_output_is_deterministic(self) -> None:
            md = "| File | D |\n|---|---|\n| `good.yaml` | one |\n"
            self.assertEqual(augment(md, self.dir), augment(md, self.dir))

        def test_a_page_with_no_tables_is_returned_unchanged(self) -> None:
            md = "# Title\n\nJust prose.\n"
            self.assertEqual(augment(md, self.dir), md)

    class Escaping(unittest.TestCase):
        def setUp(self) -> None:
            self.dir = Path(tempfile.mkdtemp())
            (self.dir / "x.yaml").write_text(
                "version: 2\nkind: runnable\nname: cpu\n", encoding="utf-8"
            )

        def test_description_markup_is_escaped(self) -> None:
            md = "| File | D |\n|---|---|\n| `x.yaml` | a <script>alert(1)</script> b |\n"
            out = augment(md, self.dir)
            self.assertNotIn("<script>", out.split('class="sonda-gallery"')[1])
            self.assertIn("&lt;script&gt;", out)

        def test_description_markdown_is_unwrapped(self) -> None:
            md = "| File | D |\n|---|---|\n| `x.yaml` | See [the guide](../a.md) for `rate` |\n"
            out = augment(md, self.dir)
            self.assertIn("See the guide for rate", out)

        def test_base64_is_urlsafe_and_unpadded(self) -> None:
            md = "| File | D |\n|---|---|\n| `x.yaml` | d |\n"
            out = augment(md, self.dir)
            value = re.search(r'data-yaml-b64="([^"]*)"', out).group(1)
            self.assertNotIn("=", value)
            self.assertNotIn("+", value)
            self.assertNotIn("/", value)
            padded = value + "=" * (-len(value) % 4)
            self.assertIn("kind: runnable", base64.urlsafe_b64decode(padded).decode("utf-8"))

        def test_unicode_round_trips(self) -> None:
            (self.dir / "u.yaml").write_text(
                'version: 2\nkind: runnable\nname: "東京 ☃"\n', encoding="utf-8"
            )
            md = "| File | D |\n|---|---|\n| `u.yaml` | unicode |\n"
            value = re.search(r'data-yaml-b64="([^"]*)"', augment(md, self.dir)).group(1)
            padded = value + "=" * (-len(value) % 4)
            self.assertIn("東京 ☃", base64.urlsafe_b64decode(padded).decode("utf-8"))

    class SharedDetectorTable(unittest.TestCase):
        """The hook's detector answers the same table as the other two."""

        def test_shared_cases(self) -> None:
            table = repo_root / "docs/site/tools/tests/runnable-cases.json"
            cases = json.loads(table.read_text(encoding="utf-8"))["cases"]
            self.assertGreater(len(cases), 20)
            for case in cases:
                with self.subTest(case=case["name"]):
                    self.assertEqual(is_runnable_scenario(case["text"]), case["expected"])

    loader = unittest.TestLoader()
    suite = unittest.TestSuite(
        loader.loadTestsFromTestCase(c) for c in (Augment, Escaping, SharedDetectorTable)
    )
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        raise SystemExit(_self_test())
    print(__doc__)
