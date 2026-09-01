#!/usr/bin/env python3
"""A compose service that builds the image must build the sinks it wires up.

The rule: anything that builds the image and then runs a scenario requiring a
cargo feature must build with that feature.

This file exists because that rule breaks *by inheritance*, silently. A compose
service names no feature at all — it inherits the Dockerfile's ``ARG FEATURES``
default — so lowering that default drops a sink out from under a stack whose own
environment, profiles and docs still promise it. Nothing in the compose file is
wrong to look at; the value it relied on changed somewhere else. A check that
compares feature strings between files cannot see it, because the file that broke
contains no feature string.

So the requirement is derived from what the service declares rather than from a
list kept here:

    service build:   -> the Dockerfile it builds, hence its inherited FEATURES
    service env keys -> the scenario YAMLs interpolating "${KEY...}"
    scenario types   -> the feature each type needs, read from the
                        "type 'x' requires the 'y' feature" arms in sonda-core

A service that declares no environment wires no scenario and needs nothing;
adding a sink type to sonda-core extends the table on its own.

Limitation, stated rather than papered over: the env-var interpolation is the
link. A scenario wired to a stack by a hard-coded service name is invisible here.

Run:  python3 scripts/check_compose_features.py [--self-test]
Needs PyYAML and Python 3.11+ for tomllib; locally, `uv run --with pyyaml python`.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from typing import Any, Iterable

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent

FEATURE_ARM_RE = re.compile(
    r"(?:sink|encoder) type '([a-z0-9_]+)' requires the '([a-z0-9-]+)' feature"
)
ENV_REF_RE = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)")
ARG_FEATURES_RE = re.compile(r"^ARG FEATURES=(\S+)", re.MULTILINE)
CARGO_PACKAGE_RE = re.compile(r"-p\s+([A-Za-z0-9_-]+)")


class CheckError(Exception):
    """The check cannot answer — a missing input, not a violation."""


def yaml_documents(root: Path) -> list[tuple[Path, str, Any]]:
    """Every tracked YAML file, parsed once, as (path, text, document).

    Tracked rather than walked, so build output and virtualenvs are excluded by
    what they are rather than by a list of directory names to keep current. A
    file no YAML parser will load is neither a compose file nor a scenario —
    compose and sonda read both with plain YAML — so it is skipped; the two
    structural assertions in `check` are what keep a wholesale skip loud.
    """
    listed = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", "--", "*.yml", "*.yaml"],
        capture_output=True,
        text=True,
    )
    if listed.returncode != 0:
        raise CheckError(f"{root}: not a git worktree, cannot enumerate tracked YAML")

    documents = []
    for name in sorted(entry for entry in listed.stdout.split("\0") if entry):
        path = root / name
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        try:
            documents.append((path, text, yaml.safe_load(text)))
        except yaml.YAMLError:
            continue
    if not documents:
        raise CheckError(f"{root}: no YAML file parsed — nothing to check")
    return documents


def feature_requirements(root: Path) -> dict[str, str]:
    """Sink/encoder type name -> the cargo feature it needs, read from core."""
    table: dict[str, str] = {}
    for rel in ("sonda-core/src/sink/mod.rs", "sonda-core/src/encoder/mod.rs"):
        path = root / rel
        if not path.is_file():
            raise CheckError(f"missing {rel}: the feature table has no source")
        for type_name, feature in FEATURE_ARM_RE.findall(path.read_text(encoding="utf-8")):
            table[type_name] = feature
    if not table:
        raise CheckError(
            "no \"type 'x' requires the 'y' feature\" arms found in sonda-core — "
            "the message shape changed and every check below would pass vacuously"
        )
    return table


def workspace_crate_dirs(root: Path) -> dict[str, Path]:
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    members = manifest.get("workspace", {}).get("members", [])
    dirs = {}
    for member in members:
        crate = root / member / "Cargo.toml"
        if not crate.is_file():
            continue
        name = tomllib.loads(crate.read_text(encoding="utf-8")).get("package", {}).get("name")
        if name:
            dirs[name] = root / member
    if not dirs:
        raise CheckError(f"{root / 'Cargo.toml'}: no workspace members resolved")
    return dirs


def crate_features(crate_dir: Path) -> dict[str, list[str]]:
    manifest = tomllib.loads((crate_dir / "Cargo.toml").read_text(encoding="utf-8"))
    return manifest.get("features", {})


def default_feature_closure(features: dict[str, list[str]]) -> set[str]:
    resolved: set[str] = set()
    pending = list(features.get("default", []))
    while pending:
        name = pending.pop()
        if name in resolved or name not in features:
            resolved.add(name)
            continue
        resolved.add(name)
        pending.extend(features[name])
    return resolved


def dockerfile_build(root: Path) -> tuple[set[str], set[str], bool]:
    """The Dockerfile's inherited FEATURES, the packages it builds, and whether
    the crates' default features come along."""
    path = root / "Dockerfile"
    if not path.is_file():
        raise CheckError(f"missing {path}")
    text = path.read_text(encoding="utf-8")
    code = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
    defaults = ARG_FEATURES_RE.findall(code)
    if not defaults:
        raise CheckError(f"{path}: no `ARG FEATURES=` line — nothing to inherit")
    build_lines = [line for line in code.splitlines() if "cargo build" in line]
    packages = {name for line in build_lines for name in CARGO_PACKAGE_RE.findall(line)}
    if not packages:
        raise CheckError(f"{path}: no `cargo build … -p <crate>` line found")
    with_defaults = not any("--no-default-features" in line for line in build_lines)
    return split_features(defaults[0]), packages, with_defaults


def split_features(value: str) -> set[str]:
    return {part.strip() for part in re.split(r"[,\s]+", value.strip()) if part.strip()}


def declared_features(root: Path, packages: Iterable[str]) -> tuple[set[str], set[str]]:
    """(features the built crates declare, features on by default)."""
    crate_dirs = workspace_crate_dirs(root)
    known: set[str] = set()
    on_by_default: set[str] = set()
    for package in packages:
        if package not in crate_dirs:
            raise CheckError(f"the Dockerfile builds -p {package}, absent from the workspace")
        features = crate_features(crate_dirs[package])
        known |= set(features)
        on_by_default |= default_feature_closure(features)
    return known, on_by_default


def env_keys(service: dict) -> set[str]:
    environment = service.get("environment")
    if isinstance(environment, dict):
        return {str(key) for key in environment}
    if isinstance(environment, list):
        return {str(entry).split("=", 1)[0].strip() for entry in environment}
    return set()


def build_args(build: dict) -> dict[str, str]:
    args = build.get("args")
    if isinstance(args, dict):
        return {str(k): str(v) for k, v in args.items()}
    if isinstance(args, list):
        pairs = (str(entry).split("=", 1) for entry in args)
        return {pair[0].strip(): (pair[1] if len(pair) > 1 else "") for pair in pairs}
    return {}


def image_building_services(
    compose_path: Path, document: Any, dockerfile: Path
) -> list[tuple[str, dict]]:
    if not isinstance(document, dict) or not isinstance(document.get("services"), dict):
        return []
    matched = []
    for name, service in document["services"].items():
        if not isinstance(service, dict):
            continue
        build = service.get("build")
        if isinstance(build, str):
            build = {"context": build}
        if not isinstance(build, dict):
            continue
        context = compose_path.parent / str(build.get("context", "."))
        candidate = (context / str(build.get("dockerfile", "Dockerfile"))).resolve()
        if candidate == dockerfile.resolve():
            matched.append((str(name), {**service, "build": build}))
    return matched


def collect_types(node: Any) -> set[str]:
    types: set[str] = set()
    if isinstance(node, dict):
        value = node.get("type")
        if isinstance(value, str):
            types.add(value)
        for child in node.values():
            types |= collect_types(child)
    elif isinstance(node, list):
        for child in node:
            types |= collect_types(child)
    return types


def scenario_index(
    documents: list[tuple[Path, str, Any]], compose_paths: set[Path]
) -> list[tuple[Path, set[str], set[str]]]:
    """(path, env keys it interpolates, `type:` values it declares) per YAML."""
    index = []
    for path, text, document in documents:
        if path in compose_paths:
            continue
        refs = set(ENV_REF_RE.findall(text))
        if refs:
            index.append((path, refs, collect_types(document)))
    return index


def check(root: Path) -> list[str]:
    dockerfile = root / "Dockerfile"
    inherited, packages, with_defaults = dockerfile_build(root)
    known, on_by_default = declared_features(root, packages)
    requirements = feature_requirements(root)

    unknown = set(requirements.values()) - known
    if unknown:
        raise CheckError(
            f"sonda-core names feature(s) {sorted(unknown)} that no built crate declares"
        )

    documents = yaml_documents(root)
    compose_paths = set()
    services: list[tuple[Path, str, dict]] = []
    for path, _text, document in documents:
        for name, service in image_building_services(path, document, dockerfile):
            compose_paths.add(path)
            services.append((path, name, service))
    if not services:
        raise CheckError(
            f"no compose service builds {dockerfile} — the check has nothing to answer about"
        )

    scenarios = scenario_index(documents, compose_paths)
    problems: list[str] = []
    wired = 0

    for compose_path, name, service in services:
        declared = build_args(service["build"]).get("FEATURES")
        selected = split_features(declared) if declared is not None else set(inherited)
        typo = selected - known
        if typo:
            problems.append(
                f"{compose_path.relative_to(root)}: service '{name}' builds with "
                f"FEATURES={declared}, but {sorted(typo)} is not a feature any built crate declares"
            )
        effective = selected | (on_by_default if with_defaults else set())
        keys = env_keys(service)
        evidence: dict[str, list[str]] = {}
        for scenario_path, refs, types in scenarios:
            shared = refs & keys
            if not shared:
                continue
            for type_name in sorted(types):
                feature = requirements.get(type_name)
                if feature is None:
                    continue
                wired += 1
                if feature not in effective:
                    evidence.setdefault(feature, []).append(
                        f"{scenario_path.relative_to(root)} declares '{type_name}' "
                        f"under {sorted(shared)[0]}"
                    )
        source = "its build args" if declared is not None else "the Dockerfile default"
        for feature, found in sorted(evidence.items()):
            problems.append(
                f"{compose_path.relative_to(root)}: service '{name}' builds without the "
                f"'{feature}' feature ({source}: {','.join(sorted(selected)) or 'none'}), "
                f"but {'; '.join(found)}. Add FEATURES to its build args."
            )

    if wired == 0:
        raise CheckError(
            "no service's environment reaches a scenario needing a feature — "
            "the env-var link is broken and every service would pass vacuously"
        )
    return problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromName(__name__)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    try:
        problems = check(REPO_ROOT)
    except CheckError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        return 1
    print("every compose service that builds the image carries the features its scenarios need")
    return 0


class FakeRepo:
    def __init__(self, root: Path) -> None:
        self.root = root
        subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
        self.write(
            "Cargo.toml",
            '[workspace]\nmembers = ["sonda", "sonda-server", "sonda-core"]\n',
        )
        for crate in ("sonda", "sonda-server"):
            self.write(
                f"{crate}/Cargo.toml",
                f'[package]\nname = "{crate}"\n\n[features]\n'
                'default = ["config", "http"]\nconfig = []\nhttp = []\n'
                "remote-write = []\nkafka = []\notlp = []\n",
            )
        self.write("sonda-core/Cargo.toml", '[package]\nname = "sonda-core"\n')
        self.write(
            "Dockerfile",
            "ARG FEATURES=remote-write,kafka\n"
            'RUN cargo build --release --features "${FEATURES}" -p sonda -p sonda-server\n',
        )
        self.write(
            "sonda-core/src/sink/mod.rs",
            '"sink type \'otlp_grpc\' requires the \'otlp\' feature"\n'
            '"sink type \'kafka\' requires the \'kafka\' feature"\n'
            '"sink type \'loki\' requires the \'http\' feature"\n',
        )
        self.write(
            "sonda-core/src/encoder/mod.rs",
            '"encoder type \'otlp\' requires the \'otlp\' feature"\n',
        )
        self.write(
            "examples/otlp.yaml",
            'defaults:\n  sink:\n    type: otlp_grpc\n    endpoint: "${OTLP_GRPC_ENDPOINT:-x}"\n',
        )
        self.write(
            "examples/loki.yaml",
            'defaults:\n  sink:\n    type: loki\n    url: "${LOKI_URL:-x}"\n',
        )

    def write(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def compose(self, build: str, environment: str = "", rel: str = "compose.yml") -> None:
        self.write(rel, f"services:\n  sonda-server:\n{build}{environment}")

    def checked(self) -> list[str]:
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True)
        return check(self.root)


class ComposeFeatureCheckTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = FakeRepo(Path(self._tmp.name))
        self.addCleanup(self._tmp.cleanup)

    OTLP_ENV = '    environment:\n      OTLP_GRPC_ENDPOINT: "http://otel:4317"\n'
    LOKI_ENV = '    environment:\n      LOKI_URL: "http://loki:3100"\n'
    PLAIN_BUILD = "    build:\n      context: .\n      dockerfile: Dockerfile\n"

    def test_inherited_default_missing_the_wired_feature_is_reported(self) -> None:
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("sonda-server", problems[0])
        self.assertIn("'otlp'", problems[0])
        self.assertIn("the Dockerfile default", problems[0])

    def test_build_arg_supplying_the_feature_passes(self) -> None:
        build = self.PLAIN_BUILD + "      args:\n        FEATURES: remote-write,kafka,otlp\n"
        self.repo.compose(build, self.OTLP_ENV)
        self.assertEqual(self.repo.checked(), [])

    def test_shorthand_build_string_resolves_to_the_dockerfile(self) -> None:
        self.repo.compose("    build: .\n", self.OTLP_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("'otlp'", problems[0])

    def test_build_arg_list_form_is_read(self) -> None:
        build = self.PLAIN_BUILD + "      args:\n        - FEATURES=otlp\n"
        self.repo.compose(build, self.OTLP_ENV)
        self.assertEqual(self.repo.checked(), [])

    def test_environment_list_form_is_read(self) -> None:
        self.repo.compose(self.PLAIN_BUILD, "    environment:\n      - OTLP_GRPC_ENDPOINT=x\n")
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)

    NESTED_BUILD = "    build:\n      context: ..\n      dockerfile: Dockerfile\n"

    def test_service_wiring_no_scenario_needs_nothing(self) -> None:
        self.repo.compose(self.PLAIN_BUILD, '    ports:\n      - "8080:8080"\n')
        self.repo.compose(self.NESTED_BUILD, self.OTLP_ENV, rel="stack/compose.yml")
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("stack/compose.yml", problems[0])

    def test_a_feature_on_by_default_needs_no_build_arg(self) -> None:
        self.repo.compose(self.PLAIN_BUILD, self.LOKI_ENV)
        self.assertEqual(self.repo.checked(), [])

    def test_a_default_feature_dropped_by_no_default_features_is_reported(self) -> None:
        self.repo.write(
            "Dockerfile",
            "ARG FEATURES=remote-write,kafka\n"
            "RUN cargo build --no-default-features "
            '--features "${FEATURES}" -p sonda -p sonda-server\n',
        )
        self.repo.compose(self.PLAIN_BUILD, self.LOKI_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("'http'", problems[0])

    def test_a_misspelled_feature_is_reported(self) -> None:
        build = self.PLAIN_BUILD + "      args:\n        FEATURES: otlp,kafkaa\n"
        self.repo.compose(build, self.OTLP_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("kafkaa", problems[0])

    def test_a_service_building_another_dockerfile_is_ignored(self) -> None:
        self.repo.write("other/Dockerfile", "FROM scratch\n")
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV, rel="other/compose.yml")
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith("compose.yml: service"), problems[0])

    def test_no_service_building_the_image_is_an_error(self) -> None:
        with self.assertRaisesRegex(CheckError, "no compose service builds"):
            self.repo.checked()

    def test_a_broken_env_link_is_an_error_not_a_pass(self) -> None:
        self.repo.write(
            "examples/otlp.yaml",
            "defaults:\n  sink:\n    type: otlp_grpc\n    endpoint: http://otel:4317\n",
        )
        self.repo.write(
            "examples/loki.yaml",
            "defaults:\n  sink:\n    type: loki\n    url: http://loki:3100\n",
        )
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        with self.assertRaisesRegex(CheckError, "env-var link is broken"):
            self.repo.checked()

    def test_a_feature_table_that_stopped_parsing_is_an_error(self) -> None:
        self.repo.write("sonda-core/src/sink/mod.rs", "// nothing to see\n")
        self.repo.write("sonda-core/src/encoder/mod.rs", "// nothing to see\n")
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        with self.assertRaisesRegex(CheckError, "arms found in sonda-core"):
            self.repo.checked()

    def test_a_dockerfile_without_the_arg_is_an_error(self) -> None:
        self.repo.write("Dockerfile", "RUN cargo build -p sonda -p sonda-server\n")
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        with self.assertRaises(CheckError):
            self.repo.checked()

    def test_a_commented_out_arg_does_not_stand_in_for_the_real_one(self) -> None:
        self.repo.write(
            "Dockerfile",
            "# ARG FEATURES=remote-write,kafka,otlp\n"
            'RUN cargo build --features "${FEATURES}" -p sonda -p sonda-server\n',
        )
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        with self.assertRaises(CheckError):
            self.repo.checked()

    def test_a_directory_that_is_not_a_worktree_is_an_error(self) -> None:
        with tempfile.TemporaryDirectory() as loose:
            (Path(loose) / "compose.yml").write_text("services: {}\n", encoding="utf-8")
            with self.assertRaisesRegex(CheckError, "not a git worktree"):
                yaml_documents(Path(loose))

    def test_the_encoder_table_is_read_too(self) -> None:
        self.repo.write(
            "examples/otlp.yaml",
            'defaults:\n  encoder:\n    type: otlp\n  sink:\n    type: stdout\n'
            '    endpoint: "${OTLP_GRPC_ENDPOINT:-x}"\n',
        )
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("'otlp'", problems[0])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
