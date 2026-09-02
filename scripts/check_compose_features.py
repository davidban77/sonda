#!/usr/bin/env python3
"""A compose service that builds the image must build the sinks it wires up.

Three rules, all derived from what a service declares rather than from a list
kept here:

    1. a service builds with every feature its scenarios need
    2. a service that names a feature beyond the image default has a scenario
       that needs it
    3. every environment key a service declares is interpolated by some
       scenario

The first breaks *by inheritance*, silently. A compose service names no feature
at all — it inherits the Dockerfile's ``ARG FEATURES`` default — so lowering that
default drops a sink out from under a stack whose own environment, profiles and
docs still promise it. Nothing in the compose file is wrong to look at; the value
it relied on changed somewhere else.

The second is what keeps the first answerable. A service and its scenarios are
linked by an environment key, and an edit that reads as a simplification —
replacing "${OTLP_GRPC_ENDPOINT:-http://localhost:4317}" with the literal the
compose already supplies — cuts that link. Cut, the first rule has nothing left
to compare and passes; the override it was protecting can then be deleted, still
green, and the stack ships without the sink. Demanding evidence for every named
feature fails on the first of those two edits instead of neither.

The third reads that same link from the other end, because the first two only
read it forwards. Cut the link and then delete the override, and both are
answered: one has nothing left to compare, the other has no override left to
justify. What remains is an environment key naming a collector, a profile that
still starts one, and an image built without the sink — and the key nothing
reads is the part still visible. A rule that fires on the *absence* of a reader
reports that end state, and reports the first of the two edits on its own.

    service build:   -> the Dockerfile it builds, hence its inherited FEATURES
    service entry:   -> the binary it runs, hence whose crate features apply
    service env keys -> the scenario YAMLs interpolating "${KEY...}"
    scenario types   -> the feature each type needs, read from the
                        "type 'x' requires the 'y' feature" arms in sonda-core

Adding a sink type to sonda-core extends that table on its own.

Not every declared key is a scenario's to read. RUST_LOG, a SONDA_* override,
anything the binary consults for itself is legitimately declared and never
interpolated. EXEMPT_ENV_KEYS carries those, one reason per key, so an exemption
arrives in a diff and has to be argued for — rather than being granted by a
prefix or a naming convention this script decided on for itself.

Limits, stated rather than papered over. The first three are silent:

- The interpolated env key is the only link recognised. A scenario tied to a
  stack by a hard-coded service name is invisible here.
- Only "environment:" is read, not "env_file:". A service that keeps its keys in
  a file wires no scenario as far as this check can tell.
- Only tracked YAML is read. A compose file or scenario not yet `git add`ed does
  not exist here — CI sees the committed tree, a local run does not.
- Rule 3 catches dead wiring: a key declared, nothing reading it. It does not
  catch *consistent* removal — drop the environment key, the profile that needs
  it and the FEATURES override in one edit and no rule here has anything left to
  find. Whether the stack still promises that sink in its docs is a
  docs-vs-compose question, and a different guard's.

A service that declares no environment at all wires no scenario, needs nothing,
and checks nothing under rule 3. That is the answer rather than an oversight: no
wiring means there is none of it to be dead.

The two structural assertions in `check` catch a *wholesale* break — no service
builds the image, no env key reaches any scenario — and nothing finer. A link
that breaks for one service, or for one feature, leaves both counters healthy;
that gap is what the second and third rules cover.

Run:  python3 scripts/check_compose_features.py [--self-test]
Needs PyYAML and Python 3.11+ for tomllib; locally, `uv run --with pyyaml python`.
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Mapping, NamedTuple

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent

FEATURE_ARM_RE = re.compile(
    r"(?:sink|encoder) type '([a-z0-9_]+)' requires the '([a-z0-9-]+)' feature"
)
ENV_REF_RE = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)")
ARG_FEATURES_RE = re.compile(r"^ARG FEATURES=(\S+)", re.MULTILINE)
ENTRYPOINT_RE = re.compile(r"^ENTRYPOINT\s+(.+)$", re.MULTILINE)
CARGO_PACKAGE_RE = re.compile(r"-p\s+([A-Za-z0-9_-]+)")

# Environment keys a service may declare that no scenario will ever interpolate,
# because something other than a scenario reads them. One entry per key, and the
# reason is the entry: an exemption granted here is an exemption someone has to
# write down and defend in review.
#
#   "RUST_LOG": "read by the binary's own tracing subscriber, not by a scenario",
EXEMPT_ENV_KEYS: dict[str, str] = {}


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


class ImageBuild(NamedTuple):
    inherited: set[str]
    inherited_text: str
    packages: set[str]
    with_defaults: bool
    entrypoint: str | None


def dockerfile_build(root: Path) -> ImageBuild:
    path = root / "Dockerfile"
    if not path.is_file():
        raise CheckError(f"missing {path}")
    text = path.read_text(encoding="utf-8")
    code = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
    defaults = ARG_FEATURES_RE.findall(code)
    if not defaults:
        raise CheckError(f"{path}: no `ARG FEATURES=` line — nothing to inherit")
    if len(defaults) > 1:
        raise CheckError(
            f"{path}: {len(defaults)} `ARG FEATURES=` lines ({', '.join(defaults)}) — "
            "which one a service inherits is ambiguous"
        )
    build_lines = [line for line in code.splitlines() if "cargo build" in line]
    packages = {name for line in build_lines for name in CARGO_PACKAGE_RE.findall(line)}
    if not packages:
        raise CheckError(f"{path}: no `cargo build … -p <crate>` line found")
    entrypoints = ENTRYPOINT_RE.findall(code)
    return ImageBuild(
        inherited=split_features(defaults[0]),
        inherited_text=defaults[0],
        packages=packages,
        with_defaults=not any("--no-default-features" in line for line in build_lines),
        entrypoint=argv_program(entrypoints[-1]) if entrypoints else None,
    )


def argv_program(value: str) -> str | None:
    """The program name from an exec-form or shell-form ENTRYPOINT."""
    text = value.strip()
    try:
        parts = json.loads(text) if text.startswith("[") else shlex.split(text)
    except ValueError:
        return None
    if not parts or not isinstance(parts[0], str):
        return None
    return PurePosixPath(parts[0]).name or None


def split_features(value: str) -> set[str]:
    return {part.strip() for part in re.split(r"[,\s]+", value.strip()) if part.strip()}


class PackageFeatures(NamedTuple):
    declared: set[str]
    on_by_default: set[str]


def package_features(root: Path, packages: Iterable[str]) -> dict[str, PackageFeatures]:
    crate_dirs = workspace_crate_dirs(root)
    resolved: dict[str, PackageFeatures] = {}
    for package in packages:
        if package not in crate_dirs:
            raise CheckError(f"the Dockerfile builds -p {package}, absent from the workspace")
        features = crate_features(crate_dirs[package])
        resolved[package] = PackageFeatures(set(features), default_feature_closure(features))
    return resolved


def runtime_features(
    packages: dict[str, PackageFeatures], binary: str | None
) -> tuple[str, PackageFeatures]:
    """What the binary a service runs actually carries — one crate's features,
    not the union of everything the image builds. Unresolvable binaries fall
    back to what every built crate agrees on."""
    if binary in packages:
        return binary, packages[binary]
    return "the image binaries", PackageFeatures(
        set.intersection(*(entry.declared for entry in packages.values())),
        set.intersection(*(entry.on_by_default for entry in packages.values())),
    )


def env_keys(service: dict) -> set[str]:
    environment = service.get("environment")
    if isinstance(environment, dict):
        return {str(key) for key in environment}
    if isinstance(environment, list):
        return {str(entry).split("=", 1)[0].strip() for entry in environment}
    return set()


def service_binary(service: dict, image_entrypoint: str | None) -> str | None:
    entrypoint = service.get("entrypoint")
    if entrypoint is None:
        return image_entrypoint
    if isinstance(entrypoint, list):
        return PurePosixPath(str(entrypoint[0])).name if entrypoint else None
    return argv_program(str(entrypoint))


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


def check(root: Path, exempt: Mapping[str, str] = EXEMPT_ENV_KEYS) -> list[str]:
    dockerfile = root / "Dockerfile"
    image = dockerfile_build(root)
    packages = package_features(root, image.packages)
    requirements = feature_requirements(root)
    gated = set(requirements.values())
    known = set().union(*(entry.declared for entry in packages.values()))

    unknown = gated - known
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

    inherited_typo = image.inherited - known
    if inherited_typo:
        problems.append(
            f"Dockerfile: `ARG FEATURES={image.inherited_text}` names {sorted(inherited_typo)}, "
            f"which is not a feature any built crate declares"
        )

    for compose_path, name, service in services:
        declared = build_args(service["build"]).get("FEATURES")
        selected = split_features(declared) if declared is not None else set(image.inherited)
        if declared is not None:
            typo = selected - known
            if typo:
                problems.append(
                    f"{compose_path.relative_to(root)}: service '{name}' builds with "
                    f"FEATURES={declared}, but {sorted(typo)} is not a feature any built "
                    f"crate declares"
                )
        binary, runtime = runtime_features(packages, service_binary(service, image.entrypoint))
        always_on = runtime.on_by_default if image.with_defaults else set()
        effective = (selected & runtime.declared) | always_on
        keys = env_keys(service)
        evidence: dict[str, list[str]] = {}
        demanded: set[str] = set()
        reached: set[str] = set()
        for scenario_path, refs, types in scenarios:
            shared = refs & keys
            if not shared:
                continue
            reached |= shared
            for type_name in sorted(types):
                feature = requirements.get(type_name)
                if feature is None:
                    continue
                wired += 1
                demanded.add(feature)
                if feature not in effective:
                    evidence.setdefault(feature, []).append(
                        f"{scenario_path.relative_to(root)} declares '{type_name}' "
                        f"under {sorted(shared)[0]}"
                    )
        source = "its build args" if declared is not None else "the Dockerfile default"
        listed = ",".join(sorted(selected)) or "none"
        for feature, found in sorted(evidence.items()):
            problems.append(
                f"{compose_path.relative_to(root)}: service '{name}' runs {binary}, built "
                f"without the '{feature}' feature ({source}: {listed}), "
                f"but {'; '.join(found)}. Add FEATURES to its build args."
            )
        if declared is not None:
            stale = ((selected - image.inherited - always_on) & gated) - demanded
            if stale:
                problems.append(
                    f"{compose_path.relative_to(root)}: service '{name}' builds with "
                    f"FEATURES={declared}, naming {sorted(stale)} beyond the image default "
                    f"({image.inherited_text}), but no scenario its environment reaches needs "
                    f"it. Either the override is stale, or the scenario that justified it "
                    f"stopped interpolating one of this service's environment keys."
                )
        dead = sorted(keys - reached - set(exempt))
        if dead:
            problems.append(
                f"{compose_path.relative_to(root)}: service '{name}' declares "
                f"{dead}, which no scenario interpolates. Either the key is stale, or "
                f"the scenario that read it stopped interpolating it — the same cut "
                f"link, seen from the end that survives deleting the build args too. "
                f"A key something other than a scenario reads belongs in "
                f"EXEMPT_ENV_KEYS, with its reason."
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
    print(
        "every compose service that builds the image carries the features its scenarios "
        "need, and declares no environment key none of them reads"
    )
    return 0


class FakeRepo:
    def __init__(self, root: Path) -> None:
        self.root = root
        subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
        self.write(
            "Cargo.toml",
            '[workspace]\nmembers = ["sonda", "sonda-server", "sonda-core"]\n',
        )
        self.crate("sonda")
        self.crate("sonda-server")
        self.write("sonda-core/Cargo.toml", '[package]\nname = "sonda-core"\n')
        self.dockerfile()
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

    def crate(self, name: str, default: str = '["config", "http"]', extra: str = "") -> None:
        self.write(
            f"{name}/Cargo.toml",
            f'[package]\nname = "{name}"\n\n[features]\ndefault = {default}\n'
            f"config = []\nhttp = []\nremote-write = []\nkafka = []\notlp = []\ntls = []\n{extra}",
        )

    BUILD = 'RUN cargo build --release --features "${FEATURES}" -p sonda -p sonda-server'

    def dockerfile(self, arg: str = "ARG FEATURES=remote-write,kafka", build: str = BUILD) -> None:
        self.write("Dockerfile", f'{arg}\n{build}\nENTRYPOINT ["/sonda-server"]\n')

    def compose(self, build: str, environment: str = "", rel: str = "compose.yml") -> None:
        self.write(rel, f"services:\n  sonda-server:\n{build}{environment}")

    def checked(self, exempt: dict[str, str] | None = None) -> list[str]:
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True)
        return check(self.root, exempt if exempt is not None else {})


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
        self.repo.dockerfile(
            build='RUN cargo build --no-default-features --features "${FEATURES}" '
            "-p sonda -p sonda-server"
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
        self.repo.dockerfile(arg="")
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        with self.assertRaises(CheckError):
            self.repo.checked()

    def test_a_commented_out_arg_does_not_stand_in_for_the_real_one(self) -> None:
        self.repo.dockerfile(arg="# ARG FEATURES=remote-write,kafka,otlp")
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        with self.assertRaises(CheckError):
            self.repo.checked()

    def test_a_second_arg_features_line_is_ambiguous_not_ignored(self) -> None:
        self.repo.dockerfile(arg="ARG FEATURES=remote-write,kafka\nARG FEATURES=otlp")
        self.repo.compose(self.PLAIN_BUILD, self.OTLP_ENV)
        with self.assertRaisesRegex(CheckError, "ambiguous"):
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

    OVERRIDE_BUILD = PLAIN_BUILD + "      args:\n        FEATURES: remote-write,kafka,otlp\n"
    BOTH_ENV = (
        "    environment:\n"
        '      OTLP_GRPC_ENDPOINT: "http://otel:4317"\n'
        '      LOKI_URL: "http://loki:3100"\n'
    )

    LITERAL_OTLP = "defaults:\n  sink:\n    type: otlp_grpc\n    endpoint: http://otel:4317\n"

    def test_an_override_the_env_link_no_longer_justifies_is_reported(self) -> None:
        self.repo.write("examples/otlp.yaml", self.LITERAL_OTLP)
        self.repo.compose(self.OVERRIDE_BUILD, self.BOTH_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 2, problems)
        self.assertIn("stale", problems[0])
        self.assertIn("OTLP_GRPC_ENDPOINT", problems[1])

    def test_the_key_left_behind_when_the_override_goes_too_is_reported(self) -> None:
        self.repo.write("examples/otlp.yaml", self.LITERAL_OTLP)
        self.repo.compose(self.PLAIN_BUILD, self.BOTH_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("OTLP_GRPC_ENDPOINT", problems[0])
        self.assertNotIn("LOKI_URL", problems[0])

    def test_an_exempt_key_is_declared_wiring_nothing_and_passes(self) -> None:
        self.repo.compose(self.PLAIN_BUILD, self.LOKI_ENV + '      RUST_LOG: "debug"\n')
        self.assertEqual(self.repo.checked({"RUST_LOG": "read by the binary"}), [])

    def test_the_same_key_without_its_exemption_is_reported(self) -> None:
        self.repo.compose(self.PLAIN_BUILD, self.LOKI_ENV + '      RUST_LOG: "debug"\n')
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("RUST_LOG", problems[0])

    def test_a_service_declaring_no_environment_has_no_wiring_to_be_dead(self) -> None:
        self.repo.compose(self.PLAIN_BUILD, '    ports:\n      - "8080:8080"\n')
        self.repo.compose(self.NESTED_BUILD, self.LOKI_ENV, rel="stack/compose.yml")
        self.assertEqual(self.repo.checked(), [])

    def test_an_override_wiring_no_scenario_at_all_is_reported(self) -> None:
        self.repo.compose(self.OVERRIDE_BUILD, '    ports:\n      - "8080:8080"\n')
        self.repo.compose(self.NESTED_BUILD, self.LOKI_ENV, rel="stack/compose.yml")
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("otlp", problems[0])

    def test_a_named_feature_no_sink_gates_needs_no_scenario(self) -> None:
        build = self.PLAIN_BUILD + "      args:\n        FEATURES: remote-write,kafka,tls\n"
        self.repo.compose(build, self.LOKI_ENV)
        self.assertEqual(self.repo.checked(), [])

    def test_a_newly_gated_sink_wired_and_built_passes(self) -> None:
        self.repo.crate("sonda", extra="syslog = []\n")
        self.repo.crate("sonda-server", extra="syslog = []\n")
        self.repo.write(
            "sonda-core/src/sink/mod.rs",
            "\"sink type 'otlp_grpc' requires the 'otlp' feature\"\n"
            "\"sink type 'syslog' requires the 'syslog' feature\"\n",
        )
        self.repo.write(
            "examples/syslog.yaml",
            'defaults:\n  sink:\n    type: syslog\n    endpoint: "${SYSLOG_ENDPOINT:-x}"\n',
        )
        build = self.PLAIN_BUILD + "      args:\n        FEATURES: remote-write,kafka,syslog\n"
        self.repo.compose(build, '    environment:\n      SYSLOG_ENDPOINT: "udp://syslog:514"\n')
        self.assertEqual(self.repo.checked(), [])

    def test_a_typo_in_the_inherited_default_names_the_dockerfile_once(self) -> None:
        self.repo.dockerfile(arg="ARG FEATURES=remote-write,kafkaa")
        self.repo.compose(self.PLAIN_BUILD, self.LOKI_ENV)
        self.repo.compose(self.NESTED_BUILD, self.LOKI_ENV, rel="stack/compose.yml")
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith("Dockerfile:"), problems[0])
        self.assertIn("kafkaa", problems[0])
        self.assertNotIn("None", problems[0])

    def test_the_entrypoint_binarys_own_default_decides_not_the_union(self) -> None:
        self.repo.crate("sonda-server", default='["config"]')
        self.repo.compose(self.PLAIN_BUILD, self.LOKI_ENV)
        problems = self.repo.checked()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("'http'", problems[0])
        self.assertIn("runs sonda-server", problems[0])

    def test_an_entrypoint_override_keys_on_the_binary_it_names(self) -> None:
        self.repo.crate("sonda-server", default='["config"]')
        self.repo.compose(self.PLAIN_BUILD + '    entrypoint: ["/sonda", "run"]\n', self.LOKI_ENV)
        self.assertEqual(self.repo.checked(), [])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
