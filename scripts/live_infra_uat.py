#!/usr/bin/env python3
"""Live-infra UAT harness for the e2e coverage matrix in
``docs/site/docs/test/end-to-end-pipelines.md``.

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
import datetime
import functools
import inspect
import json
import math
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Callable, Iterable, Sequence
from unittest import mock


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
    # CLI verb to invoke. "run" pushes data and then polls verify_url;
    # "test" is self-verifying (its exit code IS the verdict), so rows
    # using it leave verify_url empty and set the expectations below.
    verb: str = "run"
    # Extra argv appended after the scenario path (e.g. --prometheus-url).
    extra_args: tuple[str, ...] = ()
    # Expected exit code. Non-zero rows are negative controls proving the
    # failure path fails; expect_stderr then pins WHICH failure it was.
    expect_exit: int = 0
    # Substring that must appear in stderr. Empty = no stderr assertion.
    expect_stderr: str = ""
    # Per-row wall-clock budget. Alert-lifecycle rows outlive the default.
    timeout_s: float = SCENARIO_TIMEOUT_S
    # Called after verify_url passes with (repo_root, started_s, ended_s),
    # the wall-clock bounds of the sonda run. Returns "" when every check
    # holds, otherwise the failures, one per line.
    post_verify: Callable[[Path, float, float], str] | None = None


@dataclasses.dataclass
class RowResult:
    """Outcome of running one matrix row."""

    row: MatrixRow
    ok: bool
    message: str = ""
    duration_s: float = 0.0


# --- gNMI row checks ---------------------------------------------------------

# How long the gnmi row's `sonda run` serves (its --duration).
GNMI_RUN_S = 90.0
# Telegraf's debug line once its Subscribe stream is open and the request sent.
GNMI_CONNECTED_LINE = "Connection to gNMI device sonda:57400 established"
# The same line from the telegraf-tls service, which dials the TLS target.
GNMI_TLS_CONNECTED_LINE = "Connection to gNMI device sonda:57401 established"
# The UTC timestamp, to the second, that begins each Telegraf log line.
TELEGRAF_LOG_TIME = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})Z ")
# The series Telegraf's influx output produces, as VictoriaMetrics names them:
# measurement (the subscription name) + "_" + field, one per interface. The
# counters are subscribed SAMPLE; the oper-status ON_CHANGE.
GNMI_COUNTER_SERIES = (
    "ifcounters_in_octets",
    "ifcounters_out_octets",
)
GNMI_STATE_SERIES = "ifstate_oper_status"
GNMI_SERIES = (*GNMI_COUNTER_SERIES, GNMI_STATE_SERIES)
GNMI_INTERFACES = tuple(f"GigabitEthernet0/0/{i}" for i in range(4))
GNMI_MIN_SAMPLES = 3
# The oper-status subscription's heartbeat_interval in
# examples/telegraf/telegraf.conf.
GNMI_HEARTBEAT_S = 30.0
# The first sample must arrive this soon after sonda starts.
GNMI_FIRST_SAMPLE_WITHIN_S = 60.0
# examples/telegraf/telegraf.conf writes oper-status as its enum code.
GNMI_OPER_STATUS_CODES = {"UP": 1.0, "DOWN": 2.0}
# The interface whose oper_status flaps (up 60s / down 5s) in the example.
GNMI_FLAPPING_INTERFACE = GNMI_INTERFACES[0]
# Both Telegraf configs tag `source="sonda"`; examples/telegraf/telegraf-tls.conf
# also tags every metric `gnmi_row="tls"` ([global_tags]). The gnmi row reads
# the samples without that tag, the gnmi-tls row the samples with it.
GNMI_EXPORT_MATCH = (
    '{__name__=~"ifcounters_.*|ifstate_.*",source="sonda",gnmi_row!="tls"}'
)
GNMI_TLS_EXPORT_MATCH = (
    '{__name__=~"ifcounters_.*|ifstate_.*",source="sonda",gnmi_row="tls"}'
)
# The Telegraf configs of the two rows.
GNMI_TELEGRAF_CONF = Path("examples/telegraf/telegraf.conf")
GNMI_TLS_TELEGRAF_CONF = Path("examples/telegraf/telegraf-tls.conf")


def parse_vm_export(body: bytes) -> dict[tuple[str, str], list[tuple[int, float]]]:
    """Group a VictoriaMetrics ``/api/v1/export`` body by (series name, interface).

    The body is one JSON object per line, each with ``metric``, ``values`` and
    ``timestamps`` (milliseconds). Samples are returned as (timestamp, value)
    pairs; a line that does not parse is skipped.
    """
    out: dict[tuple[str, str], list[tuple[int, float]]] = {}
    for line in body.splitlines():
        try:
            entry = json.loads(line)
            key = (entry["metric"]["__name__"], entry["metric"].get("name", ""))
            pairs = zip(entry["timestamps"], entry["values"])
        except (json.JSONDecodeError, KeyError, TypeError, ValueError):
            continue
        out.setdefault(key, []).extend((int(ts), float(v)) for ts, v in pairs)
    return out


def telegraf_connected_at(
    telegraf_log: str,
    started_s: float,
    ended_s: float,
    connected_line: str = GNMI_CONNECTED_LINE,
) -> float | None:
    """The time, in seconds since the epoch, on the first line of
    ``telegraf_log`` that carries ``connected_line`` and is stamped within
    [started_s - 1, ended_s]; the second's slack is the log's precision.
    None when no such line has a timestamp that parses.
    """
    for line in telegraf_log.splitlines():
        if connected_line not in line:
            continue
        match = TELEGRAF_LOG_TIME.search(line)
        if match is None:
            continue
        stamped = datetime.datetime.strptime(
            match.group(1), "%Y-%m-%dT%H:%M:%S"
        ).replace(tzinfo=datetime.timezone.utc)
        at_s = stamped.timestamp()
        if started_s - 1 <= at_s <= ended_s:
            return at_s
    return None


def gnmi_findings(
    telegraf_log: str,
    samples: dict[tuple[str, str], list[tuple[int, float]]],
    started_s: float,
    ended_s: float,
    connected_line: str = GNMI_CONNECTED_LINE,
    export_match: str = GNMI_EXPORT_MATCH,
) -> list[str]:
    """Every way a gnmi row's evidence falls short, in check order; empty
    when all hold.

    Only samples timestamped within [started_s, ended_s] count. In order:
    Telegraf logged ``connected_line``, and the line carries a timestamp
    within the window (:func:`telegraf_connected_at`); some sample exists, the
    earliest no later than GNMI_FIRST_SAMPLE_WITHIN_S after started_s (with
    none, named with ``export_match``, the selector the samples were exported
    by, the checks after this one do not run); each series in
    GNMI_COUNTER_SERIES has at least GNMI_MIN_SAMPLES samples for each
    interface; GNMI_STATE_SERIES has at least one for the flapping interface
    and, for each steady interface, at least one and at least one fewer than
    its expected count: the initial value plus one heartbeat per whole
    GNMI_HEARTBEAT_S from the connection to ended_s (one when the connection
    time is unknown); each shortfall named; the flapping interface's
    oper_status takes both UP and DOWN within GNMI_RUN_S of started_s; and,
    when it has any sample, it has at most one per value change between
    consecutive samples, plus one per started GNMI_HEARTBEAT_S of the window,
    plus two.
    """
    findings: list[str] = []
    connected_s = telegraf_connected_at(
        telegraf_log, started_s, ended_s, connected_line
    )
    if connected_line not in telegraf_log:
        findings.append(f"telegraf log has no {connected_line!r}")
    elif connected_s is None:
        findings.append(
            f"telegraf log has no {connected_line!r} line stamped within "
            "the run's window, so its heartbeats cannot be counted"
        )

    start_ms, end_ms = int(started_s * 1000), int(ended_s * 1000)
    in_window = {
        key: [(ts, v) for ts, v in pairs if start_ms <= ts <= end_ms]
        for key, pairs in samples.items()
    }
    timestamps = [ts for pairs in in_window.values() for ts, _ in pairs]
    if not timestamps:
        findings.append(
            f"no sample matching {export_match} within the run's window"
        )
        return findings
    first_after_s = (min(timestamps) - start_ms) / 1000
    if first_after_s > GNMI_FIRST_SAMPLE_WITHIN_S:
        findings.append(
            f"first sample arrived {first_after_s:.1f}s after sonda started "
            f"(bound {GNMI_FIRST_SAMPLE_WITHIN_S:.0f}s)"
        )

    for series in GNMI_SERIES:
        for interface in GNMI_INTERFACES:
            count = len(in_window.get((series, interface), []))
            reason = ""
            if series != GNMI_STATE_SERIES:
                least = GNMI_MIN_SAMPLES
            elif interface == GNMI_FLAPPING_INTERFACE or connected_s is None:
                least = 1
            else:
                heartbeats = math.floor((ended_s - connected_s) / GNMI_HEARTBEAT_S)
                least = max(heartbeats, 1)
                reason = (
                    f" (1 + {heartbeats} heartbeat(s) after telegraf connected "
                    f"{connected_s - started_s:.0f}s after sonda started, less 1)"
                )
            if count < least:
                findings.append(
                    f'{series}{{name="{interface}"}}: {count} sample(s), '
                    f"want >= {least}{reason}"
                )

    flap_end_ms = start_ms + int(GNMI_RUN_S * 1000)
    seen = {
        v
        for ts, v in in_window.get(("ifstate_oper_status", GNMI_FLAPPING_INTERFACE), [])
        if ts <= flap_end_ms
    }
    for state, code in GNMI_OPER_STATUS_CODES.items():
        if code not in seen:
            findings.append(
                f'ifstate_oper_status{{name="{GNMI_FLAPPING_INTERFACE}"}} never '
                f"took {state} ({code:g}) within {GNMI_RUN_S:.0f}s; saw "
                f"{sorted(seen)}"
            )

    flapping = sorted(in_window.get((GNMI_STATE_SERIES, GNMI_FLAPPING_INTERFACE), []))
    # An empty series meets any upper bound; the count check above names it.
    if flapping:
        changes = sum(1 for (_, a), (_, b) in zip(flapping, flapping[1:]) if a != b)
        heartbeats = math.ceil((end_ms - start_ms) / 1000 / GNMI_HEARTBEAT_S)
        bound = changes + heartbeats + 2
        if len(flapping) > bound:
            findings.append(
                f'ifstate_oper_status{{name="{GNMI_FLAPPING_INTERFACE}"}}: '
                f"{len(flapping)} samples, want at most {bound} ({changes} "
                f"change(s) + {heartbeats} heartbeat(s) + 2)"
            )
    return findings


def verify_gnmi_row(
    repo_root: Path,
    started_s: float,
    ended_s: float,
    service: str = "telegraf",
    connected_line: str = GNMI_CONNECTED_LINE,
    export_match: str = GNMI_EXPORT_MATCH,
) -> str:
    """Collect the log of the Telegraf compose ``service`` and the run's
    samples matching the series selector ``export_match``, then apply
    :func:`gnmi_findings` with ``connected_line`` and ``export_match``."""
    telegraf_log = compose_logs_tail(repo_root, service, tail_lines=None)
    query = urllib.parse.urlencode(
        {
            "match[]": export_match,
            "start": f"{started_s:.3f}",
            "end": f"{ended_s:.3f}",
        }
    )
    status, body = http_get(f"http://localhost:8428/api/v1/export?{query}")
    if status != 200:
        return f"VictoriaMetrics export answered {status}: {body[:200]!r}"
    return "\n".join(
        gnmi_findings(
            telegraf_log,
            parse_vm_export(body),
            started_s,
            ended_s,
            connected_line,
            export_match,
        )
    )


# --- gNMI TLS certificates -----------------------------------------------------

# Where the gnmi-tls row's certificates are made: gitignored, and mounted into
# the telegraf-tls service at /etc/telegraf/certs.
GNMI_TLS_CERTS_DIR = Path("examples/telegraf/certs")
# The names the server certificate covers: the one Telegraf dials, and the
# host's own for gnmic.
GNMI_TLS_SERVER_NAMES = "DNS:sonda,DNS:localhost,IP:127.0.0.1"


def gnmi_tls_cert_commands(certs: Path) -> list[list[str]]:
    """The openssl invocations that make, in ``certs``, a CA (``ca.pem``,
    ``ca-key.pem``) and a server certificate it signs (``server.pem``,
    ``server-key.pem``) for GNMI_TLS_SERVER_NAMES, each valid for one day.
    The last reads its extensions from ``server.ext``."""
    return [
        [
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-days", "1", "-subj", "/CN=sonda-uat-ca",
            "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign",
            "-keyout", str(certs / "ca-key.pem"), "-out", str(certs / "ca.pem"),
        ],
        [
            "openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=sonda",
            "-keyout", str(certs / "server-key.pem"),
            "-out", str(certs / "server.csr"),
        ],
        [
            "openssl", "x509", "-req", "-in", str(certs / "server.csr"),
            "-CA", str(certs / "ca.pem"), "-CAkey", str(certs / "ca-key.pem"),
            "-CAcreateserial", "-days", "1",
            "-extfile", str(certs / "server.ext"), "-out", str(certs / "server.pem"),
        ],
    ]


def make_gnmi_tls_certs(
    repo_root: Path,
    which: Callable[[str], str | None] = shutil.which,
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> str:
    """Make the gnmi-tls row's certificates in GNMI_TLS_CERTS_DIR under
    ``repo_root``, replacing any there. Returns "" when they were made,
    otherwise what failed: no ``openssl`` on PATH, or the invocation that
    exited non-zero with its stderr."""
    if which("openssl") is None:
        return (
            "the gnmi-tls row makes its certificates with the openssl CLI, "
            "and there is no openssl on PATH"
        )
    certs = repo_root / GNMI_TLS_CERTS_DIR
    certs.mkdir(parents=True, exist_ok=True)
    (certs / "server.ext").write_text(
        f"subjectAltName={GNMI_TLS_SERVER_NAMES}\nextendedKeyUsage=serverAuth\n"
    )
    for argv in gnmi_tls_cert_commands(certs):
        done = run(argv, capture_output=True, text=True)
        if done.returncode != 0:
            return (
                f"{' '.join(argv[:3])} exited {done.returncode} making the "
                f"gnmi-tls certificates: {done.stderr.strip()}"
            )
    # Telegraf reads the CA as its own user inside the container.
    (certs / "ca.pem").chmod(0o644)
    return ""


# Files a profile's services read that are made before the stack starts.
PROFILE_SETUP: dict[str, Callable[[Path], str]] = {
    "gnmi-tls": make_gnmi_tls_certs,
}


def prepare_profiles(repo_root: Path, profiles: Iterable[str]) -> str:
    """Run the PROFILE_SETUP step of each of ``profiles``. Returns "" when
    every step succeeded, otherwise the first failure."""
    for prof in profiles:
        setup = PROFILE_SETUP.get(prof)
        if setup is not None:
            failure = setup(repo_root)
            if failure:
                return failure
    return ""


# --- Matrix ------------------------------------------------------------------

# Hardcoded against the current matrix in
# docs/site/docs/test/end-to-end-pipelines.md (rows added in PR #248). Update both
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
    # -- `sonda test` alert rows (self-verifying: exit code is the verdict) --
    # Both fixtures scope their expectations to their own `host` label, so
    # neither row's verdict can come from the other's ALERTS residue —
    # ordering between them is a readability choice, not a correctness one.
    MatrixRow(
        name="alert-test-fail-control",
        profiles=("alerting",),
        subcommand="metrics",
        scenario=Path("tests/e2e/alert-negative-control.yaml"),
        verify_url="",
        failure_log_containers=("vmalert", "victoriametrics"),
        verb="test",
        extra_args=(
            "--prometheus-url",
            "http://localhost:8428",
            "--interval",
            "5s",
        ),
        expect_exit=1,
        expect_stderr="did not fire within",
        timeout_s=120.0,
    ),
    MatrixRow(
        name="alert-test-pass",
        profiles=("alerting",),
        subcommand="metrics",
        scenario=Path("examples/alert-lifecycle-test.yaml"),
        verify_url="",
        failure_log_containers=("vmalert", "victoriametrics"),
        verb="test",
        extra_args=(
            "--prometheus-url",
            "http://localhost:8428",
            "--interval",
            "5s",
        ),
        # Exit 0 already implies both checks held; pinning the resolution
        # line proves the half this scenario exists to exercise actually
        # ran, rather than inferring it.
        expect_stderr="resolved after",
        # 90s scenario + firing/resolution polling; observed ~105s live.
        timeout_s=300.0,
    ),
    # -- The same two rows one hop further down: Alertmanager acquisition.
    # These prove the *notification* path, not just rule evaluation. A
    # vmalert whose --notifier.url is wrong (or an Alertmanager route that
    # drops the alert) is green on the rows above and red on these.
    MatrixRow(
        name="alert-test-fail-control-am",
        profiles=("alerting",),
        subcommand="metrics",
        scenario=Path("tests/e2e/alert-negative-control.yaml"),
        verify_url="",
        failure_log_containers=("alertmanager", "vmalert", "victoriametrics"),
        verb="test",
        extra_args=(
            "--alertmanager-url",
            "http://localhost:9093",
            "--interval",
            "5s",
        ),
        expect_exit=1,
        expect_stderr="did not fire within",
        timeout_s=120.0,
    ),
    MatrixRow(
        name="alert-test-pass-am",
        profiles=("alerting",),
        subcommand="metrics",
        scenario=Path("examples/alert-lifecycle-test.yaml"),
        verify_url="",
        failure_log_containers=("alertmanager", "vmalert", "victoriametrics"),
        verb="test",
        extra_args=(
            "--alertmanager-url",
            "http://localhost:9093",
            "--interval",
            "5s",
        ),
        # "resolved after" is the half this scenario exists to exercise, and
        # on this path it is also the proof that Alertmanager's own expiry
        # semantics were read correctly rather than the alert merely
        # vanishing from the API.
        expect_stderr="resolved after",
        timeout_s=300.0,
    ),
    # -- gNMI: sonda is the target and Telegraf (profile gnmi) the client.
    # `sonda run` serves for GNMI_RUN_S while Telegraf subscribes and writes
    # to VictoriaMetrics. The verify URL proves some series arrived at all;
    # verify_gnmi_row then checks every expected series and the flap by name.
    MatrixRow(
        name="gnmi",
        profiles=("gnmi",),
        subcommand="metrics",
        scenario=Path("examples/gnmi-telegraf.yaml"),
        verify_url=(
            "http://localhost:8428/api/v1/query?query={source=~\"sonda.*\"}"
        ),
        failure_log_containers=("telegraf", "victoriametrics"),
        extra_args=("--duration", f"{GNMI_RUN_S:.0f}s"),
        timeout_s=GNMI_RUN_S + 60.0,
        post_verify=verify_gnmi_row,
    ),
    # -- The gnmi row over TLS with credentials: Telegraf (profile gnmi-tls)
    # dials sonda:57401 with tls_enable and a username and password, and asks
    # for JSON_IETF. Its samples can only arrive through the TLS handshake, so
    # the same checks as the gnmi row hold only when TLS, the credentials and
    # JSON_IETF work. The row reads only the samples its Telegraf tags.
    MatrixRow(
        name="gnmi-tls",
        profiles=("gnmi-tls",),
        subcommand="metrics",
        scenario=Path("examples/gnmi-telegraf-tls.yaml"),
        verify_url=(
            "http://localhost:8428/api/v1/query?query={source=~\"sonda.*\"}"
        ),
        failure_log_containers=("telegraf-tls", "victoriametrics"),
        extra_args=("--duration", f"{GNMI_RUN_S:.0f}s"),
        timeout_s=GNMI_RUN_S + 60.0,
        post_verify=functools.partial(
            verify_gnmi_row,
            service="telegraf-tls",
            connected_line=GNMI_TLS_CONNECTED_LINE,
            export_match=GNMI_TLS_EXPORT_MATCH,
        ),
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
        Backend(name="otel-collector", health_url="http://localhost:13133/"),
    ),
    "alerting": (
        Backend(name="vmalert", health_url="http://localhost:8880/health"),
        Backend(name="alertmanager", health_url="http://localhost:9093/-/healthy"),
    ),
    "gnmi": (
        Backend(name="telegraf", health_url="http://localhost:8095/"),
    ),
    "gnmi-tls": (
        Backend(name="telegraf-tls", health_url="http://localhost:8096/"),
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
    tail_lines: int | None = DOCKER_LOG_TAIL_LINES,
    timeout_s: float = 30.0,
) -> str:
    """Return the last ``tail_lines`` lines of the named service's logs, or
    all of them when ``tail_lines`` is None.

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
                "all" if tail_lines is None else str(tail_lines),
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
    verb: str = "run",
    extra_args: Sequence[str] = (),
    timeout_s: float = SCENARIO_TIMEOUT_S,
) -> subprocess.CompletedProcess[str]:
    """Invoke ``sonda <verb> <scenario> [extra_args...]`` and return the
    completed process. Run from ``repo_root`` so relative paths resolve.
    The ``subcommand`` parameter is kept on ``MatrixRow`` as a hint about
    the signal type documented in the matrix; the 1.9 CLI dispatches purely
    by YAML."""
    _ = subcommand
    return subprocess.run(
        [str(sonda_bin), verb, str(scenario), *extra_args],
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

    started_wall_s = time.time()
    try:
        proc = run_scenario(
            sonda_bin,
            row.subcommand,
            row.scenario,
            repo_root,
            verb=row.verb,
            extra_args=row.extra_args,
            timeout_s=row.timeout_s,
        )
    except subprocess.TimeoutExpired:
        return RowResult(
            row=row,
            ok=False,
            message=f"scenario timed out after {row.timeout_s:.0f}s",
            duration_s=_now_s() - start,
        )
    except FileNotFoundError:
        return RowResult(
            row=row,
            ok=False,
            message=f"sonda binary not found: {sonda_bin}",
            duration_s=_now_s() - start,
        )

    ended_wall_s = time.time()

    if proc.returncode != row.expect_exit:
        stderr_tail = (proc.stderr or "").strip().splitlines()[-20:]
        return RowResult(
            row=row,
            ok=False,
            message=(
                f"sonda exited {proc.returncode} (expected {row.expect_exit})\n"
                + "\n".join(f"    {line}" for line in stderr_tail)
            ),
            duration_s=_now_s() - start,
        )

    if row.expect_stderr and row.expect_stderr not in (proc.stderr or ""):
        stderr_tail = (proc.stderr or "").strip().splitlines()[-20:]
        return RowResult(
            row=row,
            ok=False,
            message=(
                f"stderr missing expected substring {row.expect_stderr!r}\n"
                + "\n".join(f"    {line}" for line in stderr_tail)
            ),
            duration_s=_now_s() - start,
        )

    if row.verify_url:
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

    if row.post_verify is not None:
        problems = row.post_verify(repo_root, started_wall_s, ended_wall_s)
        if problems:
            return RowResult(
                row=row,
                ok=False,
                message="post-verify failed:\n"
                + "\n".join(f"    {line}" for line in problems.splitlines()),
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


def boot_only(repo_root: Path, profiles: Sequence[str]) -> int:
    """Bring the stack up for ``profiles``, wait for readiness, leave it running.

    The ``sonda-action`` self-test needs the same live stack this matrix
    uses, but runs its assertions through the action rather than through
    :func:`run_all`. Exposing the bootstrap here keeps one definition of
    "which backends exist, where their health endpoints are, and how long
    they get to start" — duplicating that in workflow YAML is how the two
    drift apart.

    Deliberately does **not** tear down: the caller is mid-job and still
    needs the stack. The runner reclaims it.
    """
    ordered = tuple(sorted(set(profiles)))
    failure = prepare_profiles(repo_root, ordered)
    if failure:
        print(failure, file=sys.stderr)
        return 1
    print(
        f"==> compose up (profiles: {','.join(ordered) or '<default>'})",
        file=sys.stderr,
    )
    up = compose_up(repo_root, ordered)
    if up.returncode != 0:
        print(up.stdout, file=sys.stderr)
        print(up.stderr, file=sys.stderr)
        print(f"docker compose up failed (exit {up.returncode})", file=sys.stderr)
        return 1

    backends: list[Backend] = []
    seen: set[str] = set()
    for prof in ("",) + ordered:
        for backend in BACKENDS_BY_PROFILE.get(prof, ()):
            if backend.name not in seen:
                seen.add(backend.name)
                backends.append(backend)

    for backend in backends:
        print(f"==> waiting for {backend.name} ({backend.health_url})", file=sys.stderr)
        if not wait_for_backend_ready(backend):
            print(
                f"{backend.name} never became ready at {backend.health_url} "
                f"within {READINESS_TIMEOUT_S:.0f}s",
                file=sys.stderr,
            )
            return 1

    print(f"==> stack ready ({len(backends)} backend(s))", file=sys.stderr)
    return 0


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

    failure = prepare_profiles(repo_root, profiles)
    if failure:
        raise RuntimeError(failure)
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
            "docs/site/docs/test/end-to-end-pipelines.md against live containers."
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
        "--boot-only",
        action="append",
        default=None,
        metavar="PROFILE",
        help=(
            "Bring the compose stack up for this profile, wait until every "
            "backend is ready, then exit 0 leaving it running. Repeatable. "
            "No matrix rows run and nothing is torn down. Used by the "
            "sonda-action self-test so it boots the same stack this matrix "
            "does instead of duplicating the bootstrap in workflow YAML."
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

    # Bootstrap-only mode needs no sonda binary — it starts containers.
    if args.boot_only is not None:
        return boot_only(repo_root, args.boot_only)

    raw_bin = args.sonda or (repo_root / DEFAULT_SONDA_BINARY)
    raw_bin = raw_bin if raw_bin.is_absolute() else (repo_root / raw_bin)
    if not raw_bin.is_file():
        print(
            f"sonda binary not found at {raw_bin}. Build it first "
            "(cargo build --release -p sonda --features "
            "http,kafka,remote-write,otlp,gnmi).",
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

    def test_scenario_paths_are_under_known_roots(self) -> None:
        # examples/ for user-facing scenarios; tests/ for harness-only
        # fixtures (e.g. the deliberately-failing negative control).
        for row in MATRIX:
            self.assertIn(row.scenario.parts[0], {"examples", "tests"}, row.name)

    def test_verbs_are_known(self) -> None:
        for row in MATRIX:
            self.assertIn(row.verb, {"run", "test"}, row.name)

    def test_self_verifying_rows_have_no_verify_url(self) -> None:
        # `sonda test` rows carry their verdict in the exit code — a verify
        # URL on one would mean the row's success criteria are ambiguous.
        for row in MATRIX:
            if row.verb == "test":
                self.assertEqual(row.verify_url, "", row.name)
            else:
                self.assertTrue(row.verify_url, row.name)

    def test_negative_controls_pin_their_failure(self) -> None:
        # A row expected to fail must also pin WHICH failure via stderr,
        # otherwise any crash would satisfy it.
        for row in MATRIX:
            if row.expect_exit != 0:
                self.assertTrue(row.expect_stderr, row.name)

    def test_profiles_are_recognised(self) -> None:
        # Every profile a row asks for must appear in BACKENDS_BY_PROFILE,
        # even with an empty backend tuple, so a typo surfaces here.
        for row in MATRIX:
            for prof in row.profiles:
                self.assertIn(prof, BACKENDS_BY_PROFILE, f"{row.name}:{prof}")

    def test_failure_containers_non_empty(self) -> None:
        for row in MATRIX:
            self.assertTrue(row.failure_log_containers, row.name)

    def test_profile_setup_profiles_are_recognised(self) -> None:
        for prof in PROFILE_SETUP:
            self.assertIn(prof, BACKENDS_BY_PROFILE, prof)

    def test_the_gnmi_tls_row_reads_its_own_telegraf(self) -> None:
        rows = {row.name: row for row in MATRIX}
        self.assertIn("gnmi-tls", rows)
        verify = rows["gnmi-tls"].post_verify
        self.assertIsInstance(verify, functools.partial)
        self.assertIs(verify.func, verify_gnmi_row)
        self.assertEqual(
            verify.keywords,
            {
                "service": "telegraf-tls",
                "connected_line": GNMI_TLS_CONNECTED_LINE,
                "export_match": GNMI_TLS_EXPORT_MATCH,
            },
        )

    def test_the_gnmi_row_reads_the_untagged_samples(self) -> None:
        rows = {row.name: row for row in MATRIX}
        self.assertIn("gnmi", rows)
        self.assertIs(rows["gnmi"].post_verify, verify_gnmi_row)
        defaults = inspect.signature(verify_gnmi_row).parameters
        self.assertEqual(defaults["export_match"].default, GNMI_EXPORT_MATCH)


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


class _ParseVmExportTests(unittest.TestCase):
    def test_groups_samples_by_series_and_interface(self) -> None:
        body = (
            b'{"metric":{"__name__":"ifstate_oper_status","name":"Gi0"},'
            b'"values":[1,2],"timestamps":[1000,2000]}\n'
            b'{"metric":{"__name__":"ifstate_oper_status","name":"Gi1"},'
            b'"values":[1],"timestamps":[1500]}\n'
        )
        self.assertEqual(
            parse_vm_export(body),
            {
                ("ifstate_oper_status", "Gi0"): [(1000, 1.0), (2000, 2.0)],
                ("ifstate_oper_status", "Gi1"): [(1500, 1.0)],
            },
        )

    def test_skips_lines_that_do_not_parse(self) -> None:
        self.assertEqual(parse_vm_export(b"not json\n{}\n"), {})


def _telegraf_line(at_s: float, message: str) -> str:
    """A Telegraf log line stamped ``at_s`` seconds after the epoch, as
    ``docker compose logs`` prints it."""
    stamp = datetime.datetime.fromtimestamp(at_s, datetime.timezone.utc)
    return f"telegraf-1  | {stamp:%Y-%m-%dT%H:%M:%S}Z D! [inputs.gnmi] {message}\n"


class _GnmiFindingsTests(unittest.TestCase):
    STARTED_S = 1_000.0
    ENDED_S = STARTED_S + GNMI_RUN_S
    # Connected 5 s in: each steady interface expects 1 + floor(85 / 30) = 3.
    CONNECTED_S = STARTED_S + 5
    LOG = _telegraf_line(CONNECTED_S, GNMI_CONNECTED_LINE)

    def _passing(self) -> dict[tuple[str, str], list[tuple[int, float]]]:
        start_ms = int(self.STARTED_S * 1000)
        samples = {
            (series, interface): [
                (start_ms + 5_000 + i * 10_000, float(i)) for i in range(5)
            ]
            for series in GNMI_SERIES
            for interface in GNMI_INTERFACES
        }
        samples[("ifstate_oper_status", GNMI_FLAPPING_INTERFACE)] = [
            (start_ms + 5_000, 1.0),
            (start_ms + 61_000, 2.0),
            (start_ms + 66_000, 1.0),
        ]
        return samples

    def _findings(self, samples, log: str | None = None) -> list[str]:
        return gnmi_findings(
            self.LOG if log is None else log, samples, self.STARTED_S, self.ENDED_S
        )

    def test_passing_evidence_has_no_findings(self) -> None:
        self.assertEqual(self._findings(self._passing()), [])

    def test_the_tls_row_needs_its_own_connection_line(self) -> None:
        tls = gnmi_findings(
            self.LOG, self._passing(), self.STARTED_S, self.ENDED_S,
            connected_line=GNMI_TLS_CONNECTED_LINE,
        )
        self.assertEqual(tls, [f"telegraf log has no {GNMI_TLS_CONNECTED_LINE!r}"])
        log = _telegraf_line(self.CONNECTED_S, GNMI_TLS_CONNECTED_LINE)
        self.assertEqual(
            gnmi_findings(
                log, self._passing(), self.STARTED_S, self.ENDED_S,
                connected_line=GNMI_TLS_CONNECTED_LINE,
            ),
            [],
        )

    def test_verify_exports_by_the_selector_it_is_given(self) -> None:
        calls: dict[str, str] = {}

        def logs(repo_root: Path, service: str, **_) -> str:
            calls["service"] = service
            return _telegraf_line(self.CONNECTED_S, GNMI_TLS_CONNECTED_LINE)

        def get(url: str, *_) -> tuple[int, bytes]:
            calls["url"] = url
            return 200, b""

        with mock.patch.dict(globals(), {"compose_logs_tail": logs, "http_get": get}):
            message = verify_gnmi_row(
                Path("."), self.STARTED_S, self.ENDED_S,
                service="telegraf-tls",
                connected_line=GNMI_TLS_CONNECTED_LINE,
                export_match=GNMI_TLS_EXPORT_MATCH,
            )
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(calls["url"]).query)
        self.assertEqual(query["match[]"], [GNMI_TLS_EXPORT_MATCH])
        self.assertEqual(calls["service"], "telegraf-tls")
        self.assertEqual(
            message,
            f"no sample matching {GNMI_TLS_EXPORT_MATCH} within the run's window",
        )

    def test_missing_connection_line_is_named(self) -> None:
        findings = self._findings(self._passing(), log="nothing here\n")
        self.assertEqual(len(findings), 1)
        self.assertIn(GNMI_CONNECTED_LINE, findings[0])

    def test_no_samples_fails_on_that_alone(self) -> None:
        findings = self._findings({})
        self.assertEqual(len(findings), 1)
        self.assertIn("no sample", findings[0])

    def test_no_samples_names_the_selector_of_the_row(self) -> None:
        self.assertIn(GNMI_EXPORT_MATCH, self._findings({})[0])
        tls = gnmi_findings(
            self.LOG, {}, self.STARTED_S, self.ENDED_S,
            export_match=GNMI_TLS_EXPORT_MATCH,
        )
        self.assertEqual(
            tls, [f"no sample matching {GNMI_TLS_EXPORT_MATCH} within the run's window"]
        )

    def test_samples_outside_the_window_do_not_count(self) -> None:
        late = {
            key: [(ts + int(GNMI_RUN_S * 1000) * 2, v) for ts, v in pairs]
            for key, pairs in self._passing().items()
        }
        self.assertIn("no sample", self._findings(late)[0])

    def test_a_series_short_of_samples_is_named(self) -> None:
        samples = self._passing()
        samples[("ifcounters_out_octets", "GigabitEthernet0/0/2")] = samples[
            ("ifcounters_out_octets", "GigabitEthernet0/0/2")
        ][:2]
        findings = self._findings(samples)
        self.assertEqual(
            findings,
            [
                'ifcounters_out_octets{name="GigabitEthernet0/0/2"}: 2 sample(s), '
                f"want >= {GNMI_MIN_SAMPLES}"
            ],
        )

    def test_a_missing_series_is_named(self) -> None:
        samples = {
            key: pairs
            for key, pairs in self._passing().items()
            if key[0] != "ifcounters_in_octets"
        }
        findings = self._findings(samples)
        self.assertEqual(len(findings), len(GNMI_INTERFACES))
        for finding, interface in zip(findings, GNMI_INTERFACES):
            self.assertIn(f'ifcounters_in_octets{{name="{interface}"}}: 0', finding)

    def test_a_flap_that_never_goes_down_is_named(self) -> None:
        samples = self._passing()
        samples[("ifstate_oper_status", GNMI_FLAPPING_INTERFACE)] = [
            (ts, 1.0) for ts, _ in samples[("ifstate_oper_status", GNMI_FLAPPING_INTERFACE)]
        ]
        findings = self._findings(samples)
        self.assertEqual(len(findings), 1)
        self.assertIn("never took DOWN (2)", findings[0])

    def test_a_state_series_with_no_sample_is_named(self) -> None:
        samples = self._passing()
        del samples[(GNMI_STATE_SERIES, "GigabitEthernet0/0/2")]
        del samples[(GNMI_STATE_SERIES, GNMI_FLAPPING_INTERFACE)]
        self.assertEqual(
            self._findings(samples),
            [
                f'ifstate_oper_status{{name="{GNMI_FLAPPING_INTERFACE}"}}: 0 sample(s), '
                "want >= 1",
                'ifstate_oper_status{name="GigabitEthernet0/0/2"}: 0 sample(s), '
                "want >= 2 (1 + 2 heartbeat(s) after telegraf connected 5s after "
                "sonda started, less 1)",
                f'ifstate_oper_status{{name="{GNMI_FLAPPING_INTERFACE}"}} never took '
                "UP (1) within 90s; saw []",
                f'ifstate_oper_status{{name="{GNMI_FLAPPING_INTERFACE}"}} never took '
                "DOWN (2) within 90s; saw []",
            ],
        )

    def _steady_state_samples(self, count: int):
        samples = self._passing()
        for interface in GNMI_INTERFACES[1:]:
            samples[(GNMI_STATE_SERIES, interface)] = samples[
                (GNMI_STATE_SERIES, interface)
            ][:count]
        return samples

    def test_a_steady_interface_one_short_of_its_expected_count_passes(self) -> None:
        self.assertEqual(self._findings(self._steady_state_samples(2)), [])

    def test_a_steady_interface_two_short_of_its_expected_count_is_named(self) -> None:
        self.assertEqual(
            self._findings(self._steady_state_samples(1)),
            [
                f'ifstate_oper_status{{name="{interface}"}}: 1 sample(s), want >= 2 '
                "(1 + 2 heartbeat(s) after telegraf connected 5s after sonda "
                "started, less 1)"
                for interface in GNMI_INTERFACES[1:]
            ],
        )

    def test_the_expected_count_runs_from_the_connection(self) -> None:
        # Connected at the start: 1 + floor(90 / 30) = 4, so 3 are needed.
        early = _telegraf_line(self.STARTED_S, GNMI_CONNECTED_LINE)
        self.assertEqual(len(self._findings(self._steady_state_samples(2), log=early)), 3)
        self.assertEqual(self._findings(self._steady_state_samples(3), log=early), [])
        # Connected 61 s in: 1 + floor(29 / 30) = 1, and never fewer than 1.
        late = _telegraf_line(self.STARTED_S + 61, GNMI_CONNECTED_LINE)
        self.assertEqual(self._findings(self._steady_state_samples(1), log=late), [])
        self.assertEqual(len(self._findings(self._steady_state_samples(0), log=late)), 3)

    def test_the_first_connection_in_the_window_counts(self) -> None:
        log = (
            _telegraf_line(self.STARTED_S - 30, GNMI_CONNECTED_LINE)
            + _telegraf_line(self.STARTED_S + 61, GNMI_CONNECTED_LINE)
            + _telegraf_line(self.STARTED_S + 80, GNMI_CONNECTED_LINE)
        )
        self.assertEqual(
            telegraf_connected_at(log, self.STARTED_S, self.ENDED_S), self.STARTED_S + 61
        )

    def test_a_connection_line_without_a_timestamp_is_named(self) -> None:
        log = f"... D! [inputs.gnmi] {GNMI_CONNECTED_LINE}\n"
        self.assertEqual(
            self._findings(self._steady_state_samples(1), log=log),
            [
                f"telegraf log has no {GNMI_CONNECTED_LINE!r} line stamped within the "
                "run's window, so its heartbeats cannot be counted"
            ],
        )

    def test_a_connection_line_stamped_outside_the_window_is_named(self) -> None:
        log = _telegraf_line(self.STARTED_S - 30, GNMI_CONNECTED_LINE)
        self.assertEqual(len(self._findings(self._passing(), log=log)), 1)
        self.assertIn("stamped within the run's window", self._findings(self._passing(), log=log)[0])

    def test_a_flap_sent_on_every_publish_is_named(self) -> None:
        samples = self._passing()
        start_ms = int(self.STARTED_S * 1000)
        # One sample a second through the run: UP, DOWN from 60 s to 65 s, UP.
        samples[(GNMI_STATE_SERIES, GNMI_FLAPPING_INTERFACE)] = [
            (start_ms + ms, 2.0 if 60_000 <= ms < 65_000 else 1.0)
            for ms in range(0, int(GNMI_RUN_S * 1000), 1000)
        ]
        bound = 2 + math.ceil(GNMI_RUN_S / GNMI_HEARTBEAT_S) + 2
        self.assertEqual(
            self._findings(samples),
            [
                f'ifstate_oper_status{{name="{GNMI_FLAPPING_INTERFACE}"}}: '
                f"{int(GNMI_RUN_S)} samples, want at most {bound} (2 change(s) + "
                f"{bound - 4} heartbeat(s) + 2)"
            ],
        )

    def test_the_flap_bound_is_inclusive(self) -> None:
        samples = self._passing()
        start_ms = int(self.STARTED_S * 1000)
        changes = [(start_ms + 5_000, 1.0), (start_ms + 61_000, 2.0), (start_ms + 66_000, 1.0)]
        heartbeats = math.ceil(GNMI_RUN_S / GNMI_HEARTBEAT_S)
        held = [(start_ms + 70_000 + i, 1.0) for i in range(heartbeats + 1)]
        samples[(GNMI_STATE_SERIES, GNMI_FLAPPING_INTERFACE)] = changes + held
        self.assertEqual(self._findings(samples), [], "at the bound")
        samples[(GNMI_STATE_SERIES, GNMI_FLAPPING_INTERFACE)].append(
            (start_ms + 80_000, 1.0)
        )
        self.assertEqual(len(self._findings(samples)), 1, "one past the bound")

    def test_a_late_first_sample_is_named(self) -> None:
        start_ms = int(self.STARTED_S * 1000)
        samples = {
            key: [(ts, v) for ts, v in pairs if ts >= start_ms + 61_000]
            for key, pairs in self._passing().items()
        }
        self.assertIn("first sample arrived", self._findings(samples)[0])


def _telegraf_conf(path: Path) -> dict:
    """The Telegraf config at ``path``, relative to the repository root."""
    root = find_repo_root(Path(__file__).parent)
    return tomllib.loads((root / path).read_text())


class _GnmiTelegrafConfTests(unittest.TestCase):
    TAG = ("gnmi_row", "tls")

    def test_only_the_tls_conf_carries_the_row_tag(self) -> None:
        key, value = self.TAG
        tls = _telegraf_conf(GNMI_TLS_TELEGRAF_CONF)
        plain = _telegraf_conf(GNMI_TELEGRAF_CONF)
        self.assertEqual(tls.get("global_tags"), {key: value})
        self.assertNotIn(key, plain.get("global_tags", {}))

    def test_the_export_matches_differ_only_in_the_row_tag(self) -> None:
        key, value = self.TAG
        tagged, untagged = f'{key}="{value}"', f'{key}!="{value}"'
        self.assertEqual(GNMI_TLS_EXPORT_MATCH.count(tagged), 1)
        self.assertEqual(GNMI_EXPORT_MATCH.count(untagged), 1)
        self.assertEqual(
            GNMI_TLS_EXPORT_MATCH.replace(tagged, untagged), GNMI_EXPORT_MATCH
        )

    def test_the_tls_conf_asks_for_json_ietf_and_the_plain_for_proto(self) -> None:
        tls = _telegraf_conf(GNMI_TLS_TELEGRAF_CONF)["inputs"]["gnmi"]
        plain = _telegraf_conf(GNMI_TELEGRAF_CONF)["inputs"]["gnmi"]
        self.assertEqual([i["encoding"] for i in tls], ["json_ietf"])
        self.assertEqual([i["encoding"] for i in plain], ["proto"])

    def test_the_tls_conf_converts_every_counter_to_a_number(self) -> None:
        measurement = {series.split("_", 1)[0] for series in GNMI_COUNTER_SERIES}
        fields = sorted(series.split("_", 1)[1] for series in GNMI_COUNTER_SERIES)
        self.assertEqual(measurement, {"ifcounters"})
        converters = _telegraf_conf(GNMI_TLS_TELEGRAF_CONF)["processors"]["converter"]
        self.assertEqual(len(converters), 1)
        self.assertEqual(converters[0]["namepass"], ["ifcounters"])
        self.assertEqual(converters[0]["fields"], {"unsigned": fields})


class _GnmiTlsCertTests(unittest.TestCase):
    def test_missing_openssl_is_named(self) -> None:
        with tempfile.TemporaryDirectory() as repo:
            failure = make_gnmi_tls_certs(Path(repo), which=lambda _: None)
            self.assertIn("openssl", failure)
            self.assertFalse((Path(repo) / GNMI_TLS_CERTS_DIR).exists())

    def test_a_failing_invocation_is_reported_with_its_stderr(self) -> None:
        def run(argv, **_):
            return subprocess.CompletedProcess(argv, 1, "", "bad option\n")

        with tempfile.TemporaryDirectory() as repo:
            failure = make_gnmi_tls_certs(
                Path(repo), which=lambda _: "/usr/bin/openssl", run=run
            )
        self.assertIn("openssl req -x509 exited 1", failure)
        self.assertIn("bad option", failure)

    @unittest.skipUnless(shutil.which("openssl"), "needs the openssl CLI")
    def test_the_server_certificate_covers_the_dialled_names_for_one_day(self) -> None:
        with tempfile.TemporaryDirectory() as repo:
            self.assertEqual(make_gnmi_tls_certs(Path(repo)), "")
            certs = Path(repo) / GNMI_TLS_CERTS_DIR

            def openssl(*args: str) -> subprocess.CompletedProcess:
                return subprocess.run(
                    ["openssl", *args], capture_output=True, text=True
                )

            names = openssl(
                "x509", "-in", str(certs / "server.pem"), "-noout",
                "-ext", "subjectAltName",
            ).stdout
            for name in ("DNS:sonda", "DNS:localhost", "IP Address:127.0.0.1"):
                self.assertIn(name, names)
            verified = openssl(
                "verify", "-CAfile", str(certs / "ca.pem"), str(certs / "server.pem")
            )
            self.assertEqual(verified.returncode, 0, verified.stdout + verified.stderr)
            for pem in ("ca.pem", "server.pem"):
                path = str(certs / pem)
                hour = openssl("x509", "-in", path, "-noout", "-checkend", "3600")
                self.assertEqual(hour.returncode, 0, f"{pem} lasts an hour")
                # One day and an hour: a certificate made for longer outlives it.
                day = openssl("x509", "-in", path, "-noout", "-checkend", "90000")
                self.assertEqual(day.returncode, 1, f"{pem} expires within a day")
            self.assertTrue((certs / "server-key.pem").is_file())

    def test_prepare_profiles_runs_the_setup_of_the_listed_profiles(self) -> None:
        ran: list[str] = []
        original = dict(PROFILE_SETUP)
        try:
            PROFILE_SETUP.clear()
            PROFILE_SETUP["a"] = lambda _: ran.append("a") or ""
            PROFILE_SETUP["b"] = lambda _: ran.append("b") or "b failed"
            self.assertEqual(prepare_profiles(Path("/repo"), ("loki",)), "")
            self.assertEqual(ran, [])
            self.assertEqual(prepare_profiles(Path("/repo"), ("a", "b", "c")), "b failed")
            self.assertEqual(ran, ["a", "b"])
        finally:
            PROFILE_SETUP.clear()
            PROFILE_SETUP.update(original)


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
        _ParseVmExportTests,
        _GnmiFindingsTests,
        _GnmiTelegrafConfTests,
        _GnmiTlsCertTests,
    ):
        suite.addTests(loader.loadTestsFromTestCase(cls))
    runner = unittest.TextTestRunner(verbosity=2)
    result = runner.run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
