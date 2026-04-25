#!/usr/bin/env python3
"""Live-infra UAT harness for the e2e coverage matrix in
``docs/site/docs/guides/e2e-testing.md``.

For each matrix row the harness:

1. Starts the compose stack ONCE with the union of required profiles.
2. Polls each backend's healthcheck endpoint until ready (no bare ``sleep``).
3. Runs the row's sonda subcommand against ``examples/*.yaml`` to push data.
4. Polls the row's verify URL with backoff and asserts the expected shape.
5. Tears down the stack ONCE, even on failure.

Stdlib-only. Run from the repo root via
``python3 scripts/live_infra_uat.py`` once Docker is up + ``target/release/sonda``
is built. ``--self-test`` runs the inline unit tests with no Docker dependency.

Exit code 0 on green, 1 on red. On failure, the failing container's
``docker logs`` tail is dumped to stderr so the GHA log is self-diagnosing.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import subprocess
import sys
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from typing import Callable, Iterable, Sequence


# --- Configuration -----------------------------------------------------------

COMPOSE_FILE = Path("examples/docker-compose-victoriametrics.yml")
DEFAULT_SONDA_BINARY = Path("target/release/sonda")

# Per-step time budgets. Tuned for healthcheck `start_period` + flush latency.
READINESS_TIMEOUT_S = 30.0
READINESS_POLL_INTERVAL_S = 1.0
VERIFY_TIMEOUT_S = 30.0
VERIFY_POLL_INTERVAL_S = 1.0
SCENARIO_TIMEOUT_S = 120.0
COMPOSE_UP_TIMEOUT_S = 300.0
COMPOSE_DOWN_TIMEOUT_S = 120.0
HTTP_REQUEST_TIMEOUT_S = 5.0
DOCKER_LOG_TAIL_LINES = 100


# --- Data model --------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class Backend:
    """A backend service the harness must wait for before running scenarios."""

    name: str
    health_url: str


@dataclasses.dataclass(frozen=True)
class MatrixRow:
    """One row of the e2e coverage matrix."""

    name: str
    profiles: tuple[str, ...]
    subcommand: str
    scenario: Path
    verify_url: str
    # Container name(s) whose logs we attach on failure. First entry is the
    # primary suspect (the backend the row queries); additional entries cover
    # forwarders in the path (e.g., otel-collector).
    failure_log_containers: tuple[str, ...]


@dataclasses.dataclass
class RowResult:
    """Outcome of running one matrix row."""

    row: MatrixRow
    ok: bool
    message: str = ""
    duration_s: float = 0.0


# --- Matrix ------------------------------------------------------------------

# Hardcoded against the current matrix in
# docs/site/docs/guides/e2e-testing.md (rows added in PR #248). Update both
# the table and this list together when adding rows.
#
# Container names (suspect_logs) are the compose project's default service
# names. The compose project name defaults to the parent directory of the
# compose file ("examples"); container names are then "<project>-<svc>-<n>".
# We resolve them dynamically via `docker compose ps -q` so renaming the
# project doesn't require code changes.
MATRIX: tuple[MatrixRow, ...] = (
    MatrixRow(
        name="vmagent",
        profiles=(),
        subcommand="metrics",
        scenario=Path("examples/remote-write-vmagent.yaml"),
        verify_url=(
            "http://localhost:8428/api/v1/query?query=cpu_usage_vmagent"
        ),
        failure_log_containers=("vmagent", "victoriametrics"),
    ),
    MatrixRow(
        name="prometheus",
        profiles=("prometheus",),
        subcommand="metrics",
        scenario=Path("examples/remote-write-prometheus.yaml"),
        verify_url=(
            "http://localhost:9090/api/v1/query?query=cpu_usage_prom"
        ),
        failure_log_containers=("prometheus",),
    ),
    MatrixRow(
        name="otel-metrics",
        profiles=("otel-collector",),
        subcommand="metrics",
        scenario=Path("examples/otlp-metrics.yaml"),
        verify_url="http://localhost:8428/api/v1/query?query=cpu_usage",
        failure_log_containers=("otel-collector", "victoriametrics"),
    ),
    MatrixRow(
        name="otel-logs",
        profiles=("otel-collector", "loki"),
        subcommand="logs",
        scenario=Path("examples/otlp-logs.yaml"),
        # Window is built dynamically (last 5 min relative to now) by
        # `materialize_verify_url` because Loki rejects open-ended queries.
        verify_url=(
            "http://localhost:3100/loki/api/v1/query_range"
            "?query={service_name=\"sonda\"}"
            "&start={start_ns}&end={end_ns}"
        ),
        failure_log_containers=("otel-collector", "loki"),
    ),
)

# Backends keyed by compose profile. The empty-tuple key is for the default
# (always-on) profile-less services.
BACKENDS_BY_PROFILE: dict[str, tuple[Backend, ...]] = {
    "": (
        Backend(name="victoriametrics", health_url="http://localhost:8428/health"),
        Backend(name="vmagent", health_url="http://localhost:8429/health"),
    ),
    "prometheus": (
        Backend(name="prometheus", health_url="http://localhost:9090/-/ready"),
    ),
    "loki": (
        Backend(name="loki", health_url="http://localhost:3100/ready"),
    ),
    "otel-collector": (
        # Collector exposes no health endpoint by default; we rely on the
        # downstream backends being ready and a short post-up grace via the
        # verify-with-backoff loop.
    ),
}


# --- Time + HTTP helpers -----------------------------------------------------


def _now_s() -> float:
    """Wall-clock seconds. Wrapped for self-test injection."""
    return time.monotonic()


def materialize_verify_url(template: str, now_ns: int | None = None) -> str:
    """Expand ``{start_ns}`` / ``{end_ns}`` placeholders in a verify URL.

    The Loki query window is "now - 5min" .. "now". Other URLs without
    placeholders pass through unchanged. Uses literal substitution (not
    ``str.format``) because Loki query selectors like ``{job="sonda"}``
    contain braces that would otherwise be interpreted as format fields.
    """
    if "{start_ns}" not in template and "{end_ns}" not in template:
        return template
    if now_ns is None:
        now_ns = time.time_ns()
    start_ns = now_ns - 5 * 60 * 1_000_000_000
    return template.replace("{start_ns}", str(start_ns)).replace(
        "{end_ns}", str(now_ns)
    )


def http_get(
    url: str, timeout_s: float = HTTP_REQUEST_TIMEOUT_S
) -> tuple[int, bytes]:
    """GET ``url`` and return ``(status, body)``.

    Raises ``urllib.error.URLError`` on connection failure; HTTP error
    statuses are returned as ``(status, body)`` rather than raised so the
    caller can decide whether to retry.
    """
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            return resp.getcode(), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read() if e.fp else b""


def has_non_empty_data_result(body: bytes) -> bool:
    """Return True when the JSON body has a non-empty ``data.result`` array.

    Matches the shape returned by Prometheus / VictoriaMetrics
    ``/api/v1/query`` AND Loki ``/api/v1/query_range``. Returns False on
    parse failure or any missing layer of the path.
    """
    try:
        parsed = json.loads(body)
    except (json.JSONDecodeError, ValueError):
        return False
    if not isinstance(parsed, dict):
        return False
    data = parsed.get("data")
    if not isinstance(data, dict):
        return False
    result = data.get("result")
    if not isinstance(result, list):
        return False
    return len(result) > 0


# --- Polling -----------------------------------------------------------------


def poll_until(
    predicate: Callable[[], bool],
    timeout_s: float,
    interval_s: float,
    *,
    now: Callable[[], float] = _now_s,
    sleep: Callable[[float], None] = time.sleep,
) -> bool:
    """Call ``predicate`` until it returns True or ``timeout_s`` elapses.

    Returns True if the predicate succeeded, False on timeout. ``predicate``
    exceptions are swallowed (treated as transient) so connection-refused
    during startup doesn't crash the harness.
    """
    deadline = now() + timeout_s
    while True:
        try:
            if predicate():
                return True
        except Exception:  # noqa: BLE001 — transient backend failures
            pass
        if now() >= deadline:
            return False
        sleep(interval_s)


def wait_for_backend_ready(
    backend: Backend,
    *,
    timeout_s: float = READINESS_TIMEOUT_S,
    interval_s: float = READINESS_POLL_INTERVAL_S,
) -> bool:
    """Poll ``backend.health_url`` until it returns a 2xx, or timeout."""

    def _check() -> bool:
        status, _ = http_get(backend.health_url, timeout_s=HTTP_REQUEST_TIMEOUT_S)
        return 200 <= status < 300

    return poll_until(_check, timeout_s=timeout_s, interval_s=interval_s)


def wait_for_verify(
    url: str,
    *,
    timeout_s: float = VERIFY_TIMEOUT_S,
    interval_s: float = VERIFY_POLL_INTERVAL_S,
) -> bool:
    """Poll the verify URL until it returns 200 with non-empty ``data.result``."""

    def _check() -> bool:
        status, body = http_get(url, timeout_s=HTTP_REQUEST_TIMEOUT_S)
        if status != 200:
            return False
        return has_non_empty_data_result(body)

    return poll_until(_check, timeout_s=timeout_s, interval_s=interval_s)


# --- Compose lifecycle -------------------------------------------------------


def required_profiles(rows: Iterable[MatrixRow]) -> tuple[str, ...]:
    """Sorted, deduped union of profiles needed across the given rows."""
    seen: set[str] = set()
    for row in rows:
        for prof in row.profiles:
            seen.add(prof)
    return tuple(sorted(seen))


def required_backends(rows: Iterable[MatrixRow]) -> tuple[Backend, ...]:
    """Backends to wait for, given the union of profiles across ``rows``.

    Always includes the default (no-profile) backends.
    """
    profiles = ("",) + required_profiles(rows)
    out: list[Backend] = []
    seen_names: set[str] = set()
    for prof in profiles:
        for backend in BACKENDS_BY_PROFILE.get(prof, ()):
            if backend.name in seen_names:
                continue
            seen_names.add(backend.name)
            out.append(backend)
    return tuple(out)


def compose_command(
    repo_root: Path,
    *args: str,
    profiles: Sequence[str] = (),
) -> list[str]:
    """Build a ``docker compose`` argv with the given profiles + extra args."""
    cmd = ["docker", "compose", "-f", str(repo_root / COMPOSE_FILE)]
    for prof in profiles:
        cmd.extend(["--profile", prof])
    cmd.extend(args)
    return cmd


def compose_up(
    repo_root: Path,
    profiles: Sequence[str],
    *,
    timeout_s: float = COMPOSE_UP_TIMEOUT_S,
) -> subprocess.CompletedProcess[str]:
    """Bring the stack up with the given profiles. Detached."""
    return subprocess.run(
        compose_command(repo_root, "up", "-d", profiles=profiles),
        capture_output=True,
        text=True,
        timeout=timeout_s,
        check=False,
    )


def compose_down(
    repo_root: Path,
    profiles: Sequence[str],
    *,
    timeout_s: float = COMPOSE_DOWN_TIMEOUT_S,
) -> subprocess.CompletedProcess[str]:
    """Tear the stack down with volumes. Profiles must match the up command
    so all started services are addressed."""
    return subprocess.run(
        compose_command(repo_root, "down", "-v", profiles=profiles),
        capture_output=True,
        text=True,
        timeout=timeout_s,
        check=False,
    )


def compose_logs_tail(
    repo_root: Path,
    service: str,
    *,
    tail_lines: int = DOCKER_LOG_TAIL_LINES,
    timeout_s: float = 30.0,
) -> str:
    """Return the last ``tail_lines`` lines of the named service's logs.

    Returns a placeholder string on failure rather than raising — log
    collection is best-effort diagnostic output.
    """
    try:
        proc = subprocess.run(
            compose_command(
                repo_root,
                "logs",
                "--no-color",
                "--tail",
                str(tail_lines),
                service,
            ),
            capture_output=True,
            text=True,
            timeout=timeout_s,
            check=False,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError) as e:
        return f"<logs unavailable for {service}: {e}>"
    return (proc.stdout or "") + (proc.stderr or "")


# --- Scenario execution ------------------------------------------------------


def run_scenario(
    sonda_bin: Path,
    subcommand: str,
    scenario: Path,
    repo_root: Path,
    *,
    timeout_s: float = SCENARIO_TIMEOUT_S,
) -> subprocess.CompletedProcess[str]:
    """Invoke ``sonda <subcommand> --scenario <scenario>`` and return the
    completed process. Run from ``repo_root`` so relative paths resolve."""
    return subprocess.run(
        [str(sonda_bin), subcommand, "--scenario", str(scenario)],
        capture_output=True,
        text=True,
        timeout=timeout_s,
        check=False,
        cwd=str(repo_root),
    )


def run_row(
    row: MatrixRow,
    sonda_bin: Path,
    repo_root: Path,
) -> RowResult:
    """Execute one matrix row: scenario push, then verify with backoff."""
    start = _now_s()

    scenario_path = repo_root / row.scenario
    if not scenario_path.is_file():
        return RowResult(
            row=row,
            ok=False,
            message=f"scenario file missing: {row.scenario}",
            duration_s=_now_s() - start,
        )

    try:
        proc = run_scenario(
            sonda_bin, row.subcommand, row.scenario, repo_root,
        )
    except subprocess.TimeoutExpired:
        return RowResult(
            row=row,
            ok=False,
            message=f"scenario timed out after {SCENARIO_TIMEOUT_S:.0f}s",
            duration_s=_now_s() - start,
        )
    except FileNotFoundError:
        return RowResult(
            row=row,
            ok=False,
            message=f"sonda binary not found: {sonda_bin}",
            duration_s=_now_s() - start,
        )

    if proc.returncode != 0:
        stderr_tail = (proc.stderr or "").strip().splitlines()[-20:]
        return RowResult(
            row=row,
            ok=False,
            message=(
                f"sonda exited {proc.returncode}\n"
                + "\n".join(f"    {line}" for line in stderr_tail)
            ),
            duration_s=_now_s() - start,
        )

    verify_url = materialize_verify_url(row.verify_url)
    if not wait_for_verify(verify_url):
        return RowResult(
            row=row,
            ok=False,
            message=(
                f"verify URL did not return non-empty data.result within "
                f"{VERIFY_TIMEOUT_S:.0f}s: {verify_url}"
            ),
            duration_s=_now_s() - start,
        )

    return RowResult(row=row, ok=True, duration_s=_now_s() - start)


# --- Failure attribution -----------------------------------------------------


def attribute_failure(
    result: RowResult, repo_root: Path
) -> str:
    """Render a failure as a multi-line stderr block including container logs."""
    lines = [
        f"FAIL [{result.row.name}] ({result.duration_s:.1f}s)",
        f"    scenario: {result.row.scenario}",
        f"    verify  : {result.row.verify_url}",
        f"    {result.message}",
    ]
    for container in result.row.failure_log_containers:
        logs = compose_logs_tail(repo_root, container)
        lines.append(f"--- docker logs (tail) for {container} ---")
        lines.append(logs.rstrip() or "(no output)")
        lines.append(f"--- end {container} ---")
    return "\n".join(lines)


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


def filter_skipped(
    rows: Sequence[MatrixRow], skip: Iterable[str]
) -> tuple[list[MatrixRow], list[str]]:
    """Split rows into kept + skipped (by name). Unknown skip names are
    returned in the second list verbatim so the caller can warn."""
    skip_set = set(skip)
    kept: list[MatrixRow] = []
    for row in rows:
        if row.name in skip_set:
            continue
        kept.append(row)
    known_names = {r.name for r in rows}
    unknown = [s for s in skip_set if s not in known_names]
    return kept, unknown


def run_all(
    rows: Sequence[MatrixRow],
    sonda_bin: Path,
    repo_root: Path,
) -> list[RowResult]:
    """Spin the stack once, run every row in order, tear down once.

    Tear-down runs in a ``finally`` block so a panicking row or KeyboardInterrupt
    still cleans up volumes.
    """
    profiles = required_profiles(rows)
    backends = required_backends(rows)
    results: list[RowResult] = []

    print(
        f"==> compose up (profiles: {','.join(profiles) or '<default>'})",
        file=sys.stderr,
    )
    up = compose_up(repo_root, profiles)
    if up.returncode != 0:
        print(up.stdout, file=sys.stderr)
        print(up.stderr, file=sys.stderr)
        raise RuntimeError(
            f"docker compose up failed (exit {up.returncode}); see stderr above"
        )

    try:
        for backend in backends:
            print(
                f"==> waiting for {backend.name} ({backend.health_url})",
                file=sys.stderr,
            )
            if not wait_for_backend_ready(backend):
                results.append(
                    RowResult(
                        row=MatrixRow(
                            name=f"readiness:{backend.name}",
                            profiles=(),
                            subcommand="-",
                            scenario=Path("-"),
                            verify_url=backend.health_url,
                            failure_log_containers=(backend.name,),
                        ),
                        ok=False,
                        message=(
                            f"{backend.name} did not become ready within "
                            f"{READINESS_TIMEOUT_S:.0f}s"
                        ),
                    )
                )
                # Bail out — running rows against an unready stack just
                # produces noisy verify failures.
                return results

        for row in rows:
            print(f"==> running [{row.name}]", file=sys.stderr)
            result = run_row(row, sonda_bin, repo_root)
            status = "PASS" if result.ok else "FAIL"
            print(
                f"    [{row.name}] {status} ({result.duration_s:.1f}s)",
                file=sys.stderr,
            )
            results.append(result)
    finally:
        print("==> compose down -v", file=sys.stderr)
        down = compose_down(repo_root, profiles)
        if down.returncode != 0:
            print(
                f"WARN: compose down exited {down.returncode}\n"
                f"    stdout: {down.stdout.strip()}\n"
                f"    stderr: {down.stderr.strip()}",
                file=sys.stderr,
            )

    return results


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Run the e2e coverage matrix from "
            "docs/site/docs/guides/e2e-testing.md against live containers."
        ),
    )
    parser.add_argument(
        "--sonda",
        type=Path,
        default=None,
        help=(
            "Path to the sonda binary. Defaults to "
            f"{DEFAULT_SONDA_BINARY} relative to the repo root."
        ),
    )
    parser.add_argument(
        "--skip-row",
        action="append",
        default=[],
        metavar="NAME",
        help=(
            "Matrix row name to skip. Repeatable. Intended as a temporary "
            "escape hatch when an upstream image goes flaky; the workflow "
            "comment must name the reason."
        ),
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run inline unit tests and exit. No Docker required.",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return _run_self_tests()

    repo_root = find_repo_root(Path(__file__).parent)

    raw_bin = args.sonda or (repo_root / DEFAULT_SONDA_BINARY)
    raw_bin = raw_bin if raw_bin.is_absolute() else (repo_root / raw_bin)
    if not raw_bin.is_file():
        print(
            f"sonda binary not found at {raw_bin}. Build it first "
            "(cargo build --release -p sonda --features "
            "http,kafka,remote-write,otlp).",
            file=sys.stderr,
        )
        return 2

    rows, unknown_skips = filter_skipped(MATRIX, args.skip_row)
    for name in unknown_skips:
        print(
            f"WARN: --skip-row {name!r} does not match any matrix row",
            file=sys.stderr,
        )

    if not rows:
        print(
            "no matrix rows to run after applying --skip-row filters",
            file=sys.stderr,
        )
        return 0

    try:
        results = run_all(rows, raw_bin, repo_root)
    except KeyboardInterrupt:
        print("\nInterrupted; tear-down already attempted.", file=sys.stderr)
        return 130

    failures = [r for r in results if not r.ok]
    for failure in failures:
        print(attribute_failure(failure, repo_root), file=sys.stderr)
    print(
        f"{len(results)} rows checked, {len(failures)} failed",
        file=sys.stderr,
    )
    return 0 if not failures else 1


# --- Self-tests --------------------------------------------------------------


class _MaterializeVerifyUrlTests(unittest.TestCase):
    def test_passthrough_when_no_placeholders(self) -> None:
        self.assertEqual(
            materialize_verify_url(
                "http://localhost:8428/api/v1/query?query=cpu_usage"
            ),
            "http://localhost:8428/api/v1/query?query=cpu_usage",
        )

    def test_substitutes_loki_window(self) -> None:
        # 2026-01-01T00:00:00Z in nanoseconds.
        now_ns = 1_767_225_600_000_000_000
        out = materialize_verify_url(
            "http://localhost:3100/loki/api/v1/query_range"
            "?query={app=\"x\"}&start={start_ns}&end={end_ns}",
            now_ns=now_ns,
        )
        self.assertIn(f"end={now_ns}", out)
        self.assertIn(f"start={now_ns - 5 * 60 * 1_000_000_000}", out)

    def test_window_is_five_minutes(self) -> None:
        now_ns = 2_000_000_000_000_000_000
        out = materialize_verify_url(
            "?start={start_ns}&end={end_ns}", now_ns=now_ns
        )
        # Pull values back out and check the delta.
        parts = dict(p.split("=") for p in out.lstrip("?").split("&"))
        self.assertEqual(int(parts["end"]) - int(parts["start"]), 5 * 60 * 1_000_000_000)


class _HasNonEmptyDataResultTests(unittest.TestCase):
    def test_prom_shape_non_empty(self) -> None:
        body = json.dumps(
            {
                "status": "success",
                "data": {"resultType": "vector", "result": [{"metric": {}, "value": [0, "1"]}]},
            }
        ).encode()
        self.assertTrue(has_non_empty_data_result(body))

    def test_prom_shape_empty(self) -> None:
        body = json.dumps({"status": "success", "data": {"result": []}}).encode()
        self.assertFalse(has_non_empty_data_result(body))

    def test_loki_shape_non_empty(self) -> None:
        body = json.dumps(
            {
                "status": "success",
                "data": {
                    "resultType": "streams",
                    "result": [{"stream": {"x": "1"}, "values": [["1", "log"]]}],
                },
            }
        ).encode()
        self.assertTrue(has_non_empty_data_result(body))

    def test_missing_data_key(self) -> None:
        self.assertFalse(has_non_empty_data_result(b'{"status": "success"}'))

    def test_data_not_dict(self) -> None:
        self.assertFalse(has_non_empty_data_result(b'{"data": []}'))

    def test_result_not_list(self) -> None:
        self.assertFalse(
            has_non_empty_data_result(b'{"data": {"result": "wrong"}}')
        )

    def test_invalid_json(self) -> None:
        self.assertFalse(has_non_empty_data_result(b"not json"))

    def test_empty_body(self) -> None:
        self.assertFalse(has_non_empty_data_result(b""))

    def test_top_level_array(self) -> None:
        # Some endpoints return arrays; we only treat the documented
        # Prom/Loki envelope as success.
        self.assertFalse(has_non_empty_data_result(b"[]"))


class _PollUntilTests(unittest.TestCase):
    def test_returns_true_on_first_success(self) -> None:
        calls = {"n": 0}

        def pred() -> bool:
            calls["n"] += 1
            return True

        self.assertTrue(
            poll_until(pred, timeout_s=1.0, interval_s=0.01, sleep=lambda _s: None)
        )
        self.assertEqual(calls["n"], 1)

    def test_eventually_succeeds(self) -> None:
        attempts = iter([False, False, True])

        def pred() -> bool:
            return next(attempts)

        self.assertTrue(
            poll_until(pred, timeout_s=1.0, interval_s=0.01, sleep=lambda _s: None)
        )

    def test_times_out_when_predicate_never_true(self) -> None:
        clock = [0.0]

        def now() -> float:
            return clock[0]

        def sleep(s: float) -> None:
            clock[0] += s

        self.assertFalse(
            poll_until(
                lambda: False,
                timeout_s=0.5,
                interval_s=0.1,
                now=now,
                sleep=sleep,
            )
        )
        # We advanced past the deadline.
        self.assertGreaterEqual(clock[0], 0.5)

    def test_swallows_predicate_exceptions(self) -> None:
        attempts = iter([False, False, True])

        def pred() -> bool:
            v = next(attempts)
            if not v:
                raise ConnectionRefusedError("not yet")
            return True

        self.assertTrue(
            poll_until(pred, timeout_s=1.0, interval_s=0.01, sleep=lambda _s: None)
        )


class _RequiredProfilesAndBackendsTests(unittest.TestCase):
    def test_required_profiles_dedup_and_sort(self) -> None:
        rows = (
            MatrixRow("a", ("loki", "otel-collector"), "logs", Path("a"), "u", ()),
            MatrixRow("b", ("otel-collector",), "metrics", Path("b"), "u", ()),
            MatrixRow("c", (), "metrics", Path("c"), "u", ()),
        )
        self.assertEqual(required_profiles(rows), ("loki", "otel-collector"))

    def test_required_profiles_empty(self) -> None:
        self.assertEqual(required_profiles(()), ())

    def test_required_backends_includes_default(self) -> None:
        rows = (
            MatrixRow("a", ("prometheus",), "metrics", Path("a"), "u", ()),
        )
        names = [b.name for b in required_backends(rows)]
        # vmagent + victoriametrics (default) + prometheus
        self.assertIn("victoriametrics", names)
        self.assertIn("vmagent", names)
        self.assertIn("prometheus", names)

    def test_required_backends_dedup(self) -> None:
        rows = (
            MatrixRow("a", ("loki",), "logs", Path("a"), "u", ()),
            MatrixRow("b", ("loki",), "logs", Path("b"), "u", ()),
        )
        names = [b.name for b in required_backends(rows)]
        self.assertEqual(len(names), len(set(names)))


class _ComposeCommandTests(unittest.TestCase):
    def test_no_profiles(self) -> None:
        cmd = compose_command(Path("/repo"), "up", "-d")
        self.assertEqual(cmd[:2], ["docker", "compose"])
        self.assertEqual(cmd[-2:], ["up", "-d"])
        self.assertNotIn("--profile", cmd)

    def test_with_profiles(self) -> None:
        cmd = compose_command(
            Path("/repo"), "up", "-d", profiles=("loki", "otel-collector")
        )
        # Each profile appears as `--profile X`.
        self.assertEqual(cmd.count("--profile"), 2)
        self.assertIn("loki", cmd)
        self.assertIn("otel-collector", cmd)

    def test_compose_file_path_in_cmd(self) -> None:
        cmd = compose_command(Path("/repo"), "down")
        # `-f <repo>/<COMPOSE_FILE>` is present.
        self.assertIn("-f", cmd)
        f_idx = cmd.index("-f")
        self.assertEqual(
            cmd[f_idx + 1], str(Path("/repo") / COMPOSE_FILE)
        )


class _FilterSkippedTests(unittest.TestCase):
    def _row(self, name: str) -> MatrixRow:
        return MatrixRow(name, (), "metrics", Path("x"), "u", ())

    def test_no_skips(self) -> None:
        rows = (self._row("a"), self._row("b"))
        kept, unknown = filter_skipped(rows, [])
        self.assertEqual([r.name for r in kept], ["a", "b"])
        self.assertEqual(unknown, [])

    def test_known_skip_removed(self) -> None:
        rows = (self._row("a"), self._row("b"), self._row("c"))
        kept, unknown = filter_skipped(rows, ["b"])
        self.assertEqual([r.name for r in kept], ["a", "c"])
        self.assertEqual(unknown, [])

    def test_unknown_skip_surfaced(self) -> None:
        rows = (self._row("a"),)
        kept, unknown = filter_skipped(rows, ["nope"])
        self.assertEqual([r.name for r in kept], ["a"])
        self.assertEqual(unknown, ["nope"])

    def test_multiple_skips(self) -> None:
        rows = (self._row("a"), self._row("b"), self._row("c"))
        kept, unknown = filter_skipped(rows, ["a", "c"])
        self.assertEqual([r.name for r in kept], ["b"])
        self.assertEqual(unknown, [])


class _MatrixIntegrityTests(unittest.TestCase):
    """Static checks on the hardcoded MATRIX so doc/code drift is caught."""

    def test_row_names_unique(self) -> None:
        names = [r.name for r in MATRIX]
        self.assertEqual(len(names), len(set(names)))

    def test_subcommands_are_known(self) -> None:
        for row in MATRIX:
            self.assertIn(row.subcommand, {"metrics", "logs"}, row.name)

    def test_scenario_paths_are_under_examples(self) -> None:
        for row in MATRIX:
            self.assertEqual(row.scenario.parts[0], "examples", row.name)

    def test_profiles_are_recognised(self) -> None:
        # Every profile a row asks for must appear in BACKENDS_BY_PROFILE,
        # even with an empty backend tuple, so a typo surfaces here.
        for row in MATRIX:
            for prof in row.profiles:
                self.assertIn(prof, BACKENDS_BY_PROFILE, f"{row.name}:{prof}")

    def test_failure_containers_non_empty(self) -> None:
        for row in MATRIX:
            self.assertTrue(row.failure_log_containers, row.name)


class _AttributeFailureTests(unittest.TestCase):
    def test_includes_row_name_and_message(self) -> None:
        row = MatrixRow(
            name="vmagent",
            profiles=(),
            subcommand="metrics",
            scenario=Path("examples/x.yaml"),
            verify_url="http://x",
            failure_log_containers=(),
        )
        result = RowResult(row=row, ok=False, message="boom", duration_s=1.5)
        out = attribute_failure(result, repo_root=Path("/tmp"))
        self.assertIn("FAIL [vmagent]", out)
        self.assertIn("boom", out)
        self.assertIn("examples/x.yaml", out)
        self.assertIn("1.5s", out)


def _run_self_tests() -> int:
    loader = unittest.TestLoader()
    suite = unittest.TestSuite()
    for cls in (
        _MaterializeVerifyUrlTests,
        _HasNonEmptyDataResultTests,
        _PollUntilTests,
        _RequiredProfilesAndBackendsTests,
        _ComposeCommandTests,
        _FilterSkippedTests,
        _MatrixIntegrityTests,
        _AttributeFailureTests,
    ):
        suite.addTests(loader.loadTestsFromTestCase(cls))
    runner = unittest.TextTestRunner(verbosity=2)
    result = runner.run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
