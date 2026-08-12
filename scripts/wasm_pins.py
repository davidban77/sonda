#!/usr/bin/env python3
"""Read and maintain the committed wasm bundle's build pins.

``docs/site/docs/javascripts/sonda_wasm.build.json`` records the inputs the
committed playground bundle is a product of — crate version, wasm-bindgen,
binaryen, host platform. Three places need them and used to carry their own
copies: ``task site:wasm`` (refusing an unsuitable host), the wasm-drift
workflow (reproducing the build), and whoever is trying to work out why a
rebuild does not match. This is the one parser.

The interesting one is ``crate_version``. rustc folds the crate version into
the symbol hash, so a release that only bumps versions changes the bundle's
bytes without changing its behaviour — which made the drift gate unsatisfiable
for machine-generated release commits. The gate now rebuilds at the recorded
version instead of the current one, so the bundle is rebuilt when the ENGINE
changes rather than when a release happens.

That only works while the recorded version is a version that actually
existed: :func:`check` refuses a record from the future, which is what a
hand-edit to silence the gate would look like.

Usage::

    python3 scripts/wasm_pins.py get binaryen
    python3 scripts/wasm_pins.py workspace-version
    python3 scripts/wasm_pins.py check
    python3 scripts/wasm_pins.py set-crate-version 1.20.0
    python3 scripts/wasm_pins.py rewrite-workspace-version 1.19.0
    python3 scripts/wasm_pins.py --self-test
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import unittest
from pathlib import Path

PINS = Path("docs/site/docs/javascripts/sonda_wasm.build.json")

# Every manifest that declares a version. sonda-wasm inherits the workspace's,
# so it is not listed; sonda-core is listed twice over because the workspace
# dependency entry carries a matching requirement that cargo checks.
VERSION_SITES: tuple[tuple[str, str], ...] = (
    ("Cargo.toml", r'(?m)^(?P<pre>version = ")(?P<v>[^"]+)(?P<post>")'),
    (
        "Cargo.toml",
        r'(?P<pre>sonda-core = \{ path = "sonda-core", version = ")(?P<v>[^"]+)(?P<post>" \})',
    ),
    ("sonda-core/Cargo.toml", r'(?m)^(?P<pre>version = ")(?P<v>[^"]+)(?P<post>")'),
    ("sonda/Cargo.toml", r'(?m)^(?P<pre>version = ")(?P<v>[^"]+)(?P<post>")'),
    ("sonda-server/Cargo.toml", r'(?m)^(?P<pre>version = ")(?P<v>[^"]+)(?P<post>")'),
)

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+$")


def find_repo_root(start: Path) -> Path:
    """Walk up from ``start`` until a directory with a ``Cargo.toml`` is found."""
    current = start.resolve()
    while True:
        if (current / "Cargo.toml").is_file():
            return current
        if current.parent == current:
            raise RuntimeError(f"could not locate repo root above {start}")
        current = current.parent


def load(repo_root: Path) -> dict:
    """Return the pins, minus the ``_``-prefixed prose."""
    data = json.loads((repo_root / PINS).read_text(encoding="utf-8"))
    return {k: v for k, v in data.items() if not k.startswith("_")}


def workspace_version(repo_root: Path) -> str:
    """The version in ``[workspace.package]`` — what a build would use today."""
    text = (repo_root / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'\[workspace\.package\]\s*\nversion = "([^"]+)"', text)
    if not match:
        raise RuntimeError("no [workspace.package] version in Cargo.toml")
    return match.group(1)


def version_key(version: str) -> tuple[int, ...]:
    """Sortable form of a plain ``x.y.z``."""
    if not SEMVER_RE.match(version):
        raise ValueError(f"not a plain x.y.z version: {version!r}")
    return tuple(int(part) for part in version.split("."))


def check(repo_root: Path) -> list[str]:
    """Return problems with the recorded pins. Empty means good."""
    problems: list[str] = []
    pins = load(repo_root)
    for key in ("crate_version", "wasm_bindgen", "binaryen", "platform"):
        if not pins.get(key):
            problems.append(f"{PINS}: missing '{key}'")
    if problems:
        return problems

    recorded, current = pins["crate_version"], workspace_version(repo_root)
    try:
        newer = version_key(recorded) > version_key(current)
    except ValueError as err:
        return [f"{PINS}: {err}"]
    if newer:
        problems.append(
            f"{PINS}: crate_version {recorded} is NEWER than the workspace's "
            f"{current}. The bundle cannot have been built at a version that "
            "does not exist yet — this is what editing the record to silence "
            "the drift gate looks like. Rebuild the bundle instead."
        )
    return problems


def rewrite_workspace_version(repo_root: Path, version: str) -> list[str]:
    """Set every manifest version to ``version``. Returns the files changed.

    Used by the drift gate inside a throwaway CI checkout so it can rebuild
    the bundle as it was built, and by nothing else. It is deliberately not
    wired into any task a contributor runs: rewriting versions in a working
    tree is release-please's job, not a build script's.
    """
    version_key(version)  # reject anything that is not a plain x.y.z
    changed: list[str] = []
    for relative, pattern in VERSION_SITES:
        path = repo_root / relative
        text = path.read_text(encoding="utf-8")
        new_text, count = re.subn(
            pattern, lambda m: f"{m.group('pre')}{version}{m.group('post')}", text, count=1
        )
        if count != 1:
            raise RuntimeError(f"{relative}: expected exactly one match for {pattern!r}")
        if new_text != text:
            path.write_text(new_text, encoding="utf-8")
            changed.append(relative)
    return changed


def set_crate_version(repo_root: Path, version: str) -> None:
    """Record ``version`` as the version the committed bundle was built at.

    Rewrites only that one value, leaving the file's prose and key order
    alone — the ``_``-prefixed explanations are the point of the file and a
    json.dump round-trip would reflow all of it.
    """
    version_key(version)
    path = repo_root / PINS
    text = path.read_text(encoding="utf-8")
    new_text, count = re.subn(
        r'("crate_version":\s*")[^"]+(")', rf"\g<1>{version}\g<2>", text, count=1
    )
    if count != 1:
        raise RuntimeError(f"{PINS}: expected exactly one crate_version entry")
    path.write_text(new_text, encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--self-test", action="store_true", help="Run inline tests and exit.")
    sub = parser.add_subparsers(dest="command")
    get = sub.add_parser("get", help="Print one pin.")
    get.add_argument("key")
    sub.add_parser("workspace-version", help="Print the workspace's current version.")
    sub.add_parser("check", help="Validate the recorded pins.")
    setter = sub.add_parser("set-crate-version", help="Record the bundle's build version.")
    setter.add_argument("version")
    rewriter = sub.add_parser(
        "rewrite-workspace-version", help="Set every manifest version (CI checkouts only)."
    )
    rewriter.add_argument("version")
    args = parser.parse_args(argv)

    if args.self_test:
        return _run_self_tests()
    if not args.command:
        parser.print_help()
        return 2

    repo_root = find_repo_root(Path(__file__).parent)

    if args.command == "get":
        pins = load(repo_root)
        if args.key not in pins:
            print(f"no such pin: {args.key}", file=sys.stderr)
            return 2
        print(pins[args.key])
        return 0
    if args.command == "workspace-version":
        print(workspace_version(repo_root))
        return 0
    if args.command == "check":
        problems = check(repo_root)
        for problem in problems:
            print(f"::error::{problem}", file=sys.stderr)
        if problems:
            return 1
        pins = load(repo_root)
        print(
            f"OK: bundle built at crate {pins['crate_version']} "
            f"(workspace is {workspace_version(repo_root)}), "
            f"wasm-bindgen {pins['wasm_bindgen']}, binaryen {pins['binaryen']}, "
            f"{pins['platform']}"
        )
        return 0
    if args.command == "set-crate-version":
        set_crate_version(repo_root, args.version)
        print(f"recorded crate_version {args.version}")
        return 0
    if args.command == "rewrite-workspace-version":
        changed = rewrite_workspace_version(repo_root, args.version)
        print(f"set workspace version to {args.version} in: {', '.join(changed) or 'nothing'}")
        return 0
    return 2


# --- Self-tests --------------------------------------------------------------


class _VersionKeyTests(unittest.TestCase):
    def test_orders_numerically_not_lexically(self) -> None:
        self.assertLess(version_key("1.9.0"), version_key("1.10.0"))
        self.assertLess(version_key("1.19.0"), version_key("1.20.0"))
        self.assertEqual(version_key("1.19.0"), version_key("1.19.0"))

    def test_rejects_anything_that_is_not_x_y_z(self) -> None:
        for bad in ("1.19", "1.19.0-rc1", "v1.19.0", "", "1.19.0 ", "latest"):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    version_key(bad)


class _RepoStateTests(unittest.TestCase):
    """The committed pins, as they stand right now."""

    def setUp(self) -> None:
        self.repo_root = find_repo_root(Path(__file__).parent)

    def test_pins_are_valid(self) -> None:
        self.assertEqual(check(self.repo_root), [])

    def test_every_version_site_matches_exactly_once(self) -> None:
        for relative, pattern in VERSION_SITES:
            with self.subTest(file=relative):
                text = (self.repo_root / relative).read_text(encoding="utf-8")
                self.assertEqual(
                    len(re.findall(pattern, text)),
                    1,
                    f"{relative}: {pattern!r} must match exactly once, or the "
                    "gate would rebuild at a half-rewritten version",
                )

    def test_wasm_bindgen_pin_matches_cargo_lock(self) -> None:
        """A pin that drifts from the lockfile ships broken glue."""
        lock = (self.repo_root / "Cargo.lock").read_text(encoding="utf-8")
        match = re.search(r'\[\[package\]\]\nname = "wasm-bindgen"\nversion = "([^"]+)"', lock)
        self.assertIsNotNone(match, "wasm-bindgen not found in Cargo.lock")
        self.assertEqual(load(self.repo_root)["wasm_bindgen"], match.group(1))


class _DriftTriggerTests(unittest.TestCase):
    """The workflow's path filter is part of the gate, so it gets a test.

    A gate that fires on a curated list of inputs is only as good as the list:
    an input that moves the bundle's bytes and is missing from the filter does
    not cause a failure, it delays one onto the next person to touch
    sonda-core, reported as their drift. That is not hypothetical — the filter
    shipped in #540 without ``rust-toolchain.toml``, and the compiler moves the
    bytes further than anything the sidecar records (review #542 W1: +21,687
    bytes across one minor version on linux x86_64, +44,785 on macOS arm64).

    So the list is asserted rather than trusted, from both trigger events. A
    new input to the build belongs in three places — the filter, this list,
    and the sidecar's prose — and forgetting the filter is the one that fails
    silently.
    """

    WORKFLOW = Path(".github/workflows/wasm-drift.yml")

    #: Every path whose change can invalidate the committed bundle.
    REQUIRED_TRIGGERS = (
        "sonda-wasm/**",  # the facade itself
        "sonda-core/**",  # the engine it wraps
        "Cargo.lock",  # dependency versions
        "Cargo.toml",  # the wasm-release profile, and the crate version
        "rust-toolchain.toml",  # the compiler — the largest effect of all
        "docs/site/docs/javascripts/sonda_wasm*",  # the bundle and this record
        ".github/workflows/wasm-drift.yml",  # the gate's own recipe
    )

    def setUp(self) -> None:
        self.repo_root = find_repo_root(Path(__file__).parent)
        self.text = (self.repo_root / self.WORKFLOW).read_text(encoding="utf-8")

    def _paths_for(self, event: str) -> list[str]:
        """The `paths:` list under one trigger, read without a YAML parser.

        Deliberately no yaml import: this script is stdlib-only so it can run
        in the docs job with no pip install, and PyYAML would also read the
        bare `on:` key as the boolean True, which is its own small trap.
        """
        start = self.text.index(f"\n  {event}:")
        after = self.text[start + 1 :]
        end = len(after)
        for candidate in ("\n  push:", "\n  pull_request:", "\n  workflow_dispatch:", "\nconcurrency:"):
            found = after.find(candidate, 1)
            if found != -1:
                end = min(end, found)
        return re.findall(r'^\s*- "([^"]+)"', after[:end], re.MULTILINE)

    def test_every_required_trigger_is_present_in_both_events(self) -> None:
        for event in ("push", "pull_request"):
            paths = self._paths_for(event)
            self.assertTrue(paths, f"no paths parsed for {event}")
            for required in self.REQUIRED_TRIGGERS:
                with self.subTest(event=event, path=required):
                    self.assertIn(
                        required,
                        paths,
                        f"{required} can change the bundle but does not trigger "
                        f"the drift gate on {event}",
                    )

    def test_both_events_filter_identically(self) -> None:
        """A path in one list and not the other passes on push and fails on the PR."""
        self.assertEqual(self._paths_for("push"), self._paths_for("pull_request"))

    def test_the_compiler_pin_exists_where_the_record_says_it_does(self) -> None:
        """The sidecar defers to rust-toolchain.toml; that file must own a channel."""
        toolchain = (self.repo_root / "rust-toolchain.toml").read_text(encoding="utf-8")
        self.assertRegex(toolchain, r'channel\s*=\s*"[^"]+"')


class _RewriteTests(unittest.TestCase):
    """Rewriting runs against copies; the real manifests are never touched."""

    def setUp(self) -> None:
        import shutil
        import tempfile

        self.repo_root = find_repo_root(Path(__file__).parent)
        self.tmp = Path(tempfile.mkdtemp())
        for relative, _ in VERSION_SITES:
            target = self.tmp / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            if not target.exists():
                shutil.copy(self.repo_root / relative, target)
        (self.tmp / PINS).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(self.repo_root / PINS, self.tmp / PINS)

    def test_rewrite_sets_every_site(self) -> None:
        rewrite_workspace_version(self.tmp, "9.9.9")
        self.assertEqual(workspace_version(self.tmp), "9.9.9")
        for relative, pattern in VERSION_SITES:
            text = (self.tmp / relative).read_text(encoding="utf-8")
            with self.subTest(file=relative):
                self.assertEqual(re.search(pattern, text).group("v"), "9.9.9")

    def test_rewrite_refuses_a_non_version(self) -> None:
        with self.assertRaises(ValueError):
            rewrite_workspace_version(self.tmp, "not-a-version")

    def test_set_crate_version_keeps_the_prose(self) -> None:
        before = (self.tmp / PINS).read_text(encoding="utf-8")
        set_crate_version(self.tmp, "2.0.0")
        after = (self.tmp / PINS).read_text(encoding="utf-8")
        self.assertEqual(load(self.tmp)["crate_version"], "2.0.0")
        self.assertIn("_crate_version", after, "the explanation must survive")
        self.assertEqual(before.count("\n"), after.count("\n"), "no reflow")

    def test_a_record_from_the_future_is_refused(self) -> None:
        set_crate_version(self.tmp, "99.0.0")
        problems = check(self.tmp)
        self.assertTrue(problems)
        self.assertIn("NEWER than the workspace", problems[0])

    def test_a_record_from_the_past_is_fine(self) -> None:
        """The whole point: the bundle lags the version until the engine changes."""
        rewrite_workspace_version(self.tmp, "1.20.0")
        set_crate_version(self.tmp, "1.19.0")
        self.assertEqual(check(self.tmp), [])


def _run_self_tests() -> int:
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromModule(sys.modules[__name__])
    return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
