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
import concurrent.futures
import dataclasses
import datetime
import difflib
import functools
import inspect
import json
import math
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
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
    # the wall-clock bounds of the sonda run, and with during= when the row
    # has a during hook. Returns "" when every check holds, otherwise the
    # failures, one per line.
    post_verify: Callable[..., str] | None = None
    # Called with repo_root on a thread while the sonda run is in flight;
    # what it returns reaches post_verify as its `during` keyword. For
    # evidence only a running sonda can give.
    during: Callable[..., object] | None = None
    # When set, sonda runs as a SondaRun and the during hook is also given
    # it, as `sonda=`, to stop and start again; the run ends when the last
    # sonda it started exits.
    during_gets_sonda: bool = False


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


def parse_vm_export(
    body: bytes, label: str = "name"
) -> dict[tuple[str, str], list[tuple[int, float]]]:
    """Group a VictoriaMetrics ``/api/v1/export`` body by (series name, the
    value of ``label``), the interface; "" for a series without the label.

    The body is one JSON object per line, each with ``metric``, ``values`` and
    ``timestamps`` (milliseconds). Samples are returned as (timestamp, value)
    pairs; a line that does not parse is skipped.
    """
    out: dict[tuple[str, str], list[tuple[int, float]]] = {}
    for line in body.splitlines():
        try:
            entry = json.loads(line)
            key = (entry["metric"]["__name__"], entry["metric"].get(label, ""))
            pairs = zip(entry["timestamps"], entry["values"])
        except (json.JSONDecodeError, KeyError, TypeError, ValueError):
            continue
        out.setdefault(key, []).extend((int(ts), float(v)) for ts, v in pairs)
    return out


def parse_vm_export_tags(
    body: bytes, tag: str, label: str = "index"
) -> dict[tuple[str, str], dict[str, list[int]]]:
    """Group a VictoriaMetrics ``/api/v1/export`` body by (series name, the
    value of ``label``), as :func:`parse_vm_export` does, then by the value of
    ``tag`` ("" for a series without it), keeping each sample's timestamp
    (milliseconds). A line that does not parse is skipped.
    """
    out: dict[tuple[str, str], dict[str, list[int]]] = {}
    for line in body.splitlines():
        try:
            entry = json.loads(line)
            metric = entry["metric"]
            key = (metric["__name__"], metric.get(label, ""))
            value = metric.get(tag, "")
            stamps = [int(ts) for ts in entry["timestamps"]]
        except (json.JSONDecodeError, KeyError, TypeError, ValueError):
            continue
        out.setdefault(key, {}).setdefault(value, []).extend(stamps)
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


# --- SNMP row checks ---------------------------------------------------------

# How long the snmp row's `sonda run` serves in all (its --duration): the
# restart below, then long enough after it for the series to resume.
SNMP_RUN_S = 210.0
# When the snmp row's during hook stops sonda, from the run's start, at the
# earliest. Past the 150 s time window: Telegraf's v3 input keeps the engine
# time of the last response it read, about this far ahead of the restarted
# agent's clock, so its first request after the restart is answered
# usmStatsNotInTimeWindows.
SNMP_RESTART_AT_S = 160.0
# Telegraf's poll interval, telegraf-snmp.conf's [agent] interval: with
# round_interval, its default, every input polls on the wall clock's
# multiples of it.
SNMP_POLL_INTERVAL_S = 2.0
# Where in that cycle the hook stops sonda and starts it again at once:
# between two polls, so no request reaches the closed port. Telegraf 1.40
# closes an input's session after any error and discovers the engine again,
# which would send no request with the stale time.
SNMP_RESTART_PHASE_S = 1.0
# Each interface_v3 series has a sample this soon after the restart.
SNMP_RESUME_WITHIN_S = 30.0
# What gosnmp logs, at trace level through Telegraf's v3 input, when a
# request is answered usmStatsNotInTimeWindows (it re-synchronizes and asks
# again), and when one is answered usmStatsUnknownEngineIDs after the first
# discovery (the engine ID changed).
SNMP_TIME_WINDOW_LINE = "WARNING detected out-of-time-window ERROR"
SNMP_REDISCOVERY_LINE = "WARNING detected unknown engine id ERROR"
# Telegraf's trace lines of the v3 input.
SNMP_V3_TRACE_LINE = re.compile(r"T! \[inputs\.snmp::v3\]")
# Telegraf's startup line naming the inputs it loaded.
SNMP_LOADED_INPUTS = "Loaded inputs:"
# Every error Telegraf's inputs.snmp logs begins "E! [inputs.snmp]", or
# "E! [inputs.snmp::<alias>]" for an input with an alias.
SNMP_ERROR_LINE = re.compile(r"E! \[inputs\.snmp(?:::(?P<alias>[^\]]*))?\]")
# The aliases of examples/telegraf/telegraf-snmp.conf's three inputs.snmp
# blocks, which poll and fail independently: the SNMPv2c one, the SNMPv1 one
# (whose series are the interface_v1_ ones) and the SNMPv3 one (whose series
# are the interface_v3_ ones and SNMP_V3_UPTIME_SERIES).
SNMP_V2C_INPUT = "v2c"
SNMP_V1_INPUT = "v1"
SNMP_V3_INPUT = "v3"
SNMP_V1_SERIES_PREFIX = "interface_v1_"
SNMP_V3_SERIES_PREFIX = "interface_v3_"
# The series examples/telegraf/telegraf-snmp.conf has Telegraf write, as
# VictoriaMetrics names them: the table name + "_" + field, tagged
# index="<ifIndex>"; and the uptime field of the top level, untagged.
SNMP_INTERFACE_SERIES = ("interface_ifHCInOctets", "interface_ifOperStatus")
# The same table polled over SNMPv1 (the conf's second inputs.snmp block):
# the series every interface has, and the Counter64 ones SNMPv1 cannot carry.
SNMP_V1_INTERFACE_SERIES = ("interface_v1_ifOperStatus", "interface_v1_ifInErrors")
SNMP_V1_ABSENT_SERIES = ("interface_v1_ifHCInOctets", "interface_v1_ifHCOutOctets")
# The same table polled over SNMPv3 (the conf's third inputs.snmp block),
# and that block's sysUpTime, written under its own measurement.
SNMP_V3_INTERFACE_SERIES = ("interface_v3_ifHCInOctets", "interface_v3_ifOperStatus")
SNMP_V3_UPTIME_SERIES = "snmp_v3_uptime"
SNMP_OPER_STATUS_SERIES = "interface_ifOperStatus"
SNMP_UPTIME_SERIES = "snmp_uptime"
SNMP_INTERFACES = ("1", "2", "3", "4")
# The tag Telegraf sets on each interface row from the ifName column the
# agent serves, and the name the example gives each interface.
SNMP_IF_NAME_TAG = "ifName"
SNMP_IF_NAMES = {index: f"GigabitEthernet0/0/{int(index) - 1}" for index in SNMP_INTERFACES}
SNMP_MIN_SAMPLES = 3
# Each interface series has SNMP_MIN_SAMPLES this soon after the first sample.
SNMP_SAMPLES_WITHIN_S = 30.0
# The interface whose ifOperStatus flaps (up 60s / down 5s) in the example,
# and the codes it takes.
SNMP_FLAPPING_INTERFACE = "1"
SNMP_OPER_STATUS_CODES = {"up": 1.0, "down": 2.0}
# The conf tags every metric with the agent's host as `source` (agent_host_tag).
SNMP_EXPORT_MATCH = '{__name__=~"interface_.*|snmp_.*",source="sonda"}'
SNMP_TELEGRAF_CONF = Path("examples/telegraf/telegraf-snmp.conf")
# What net-snmp prints walking the example, with SNMP_WALK_MASKS applied.
SNMP_WALK_GOLDEN = Path("tests/e2e/snmp-telegraf.walk")
# Run inside the telegraf-snmp service, with MIBS set empty.
SNMP_WALK_COMMAND = (
    "snmpbulkwalk", "-v2c", "-c", "public", "-On", "sonda:1161", "1.3.6.1.2.1",
)
# The SNMPv1 walk and what it prints, masked: every instance but the
# Counter64 ones.
SNMP_V1_WALK_GOLDEN = Path("tests/e2e/snmp-telegraf-v1.walk")
SNMP_V1_WALK_COMMAND = (
    "snmpwalk", "-v1", "-c", "public", "-On", "sonda:1161", "1.3.6.1.2.1",
)
# The example's SNMPv3 users: the conf's v3 block reads as telegraf at
# authPriv; monitor reads at authNoPriv. In-container snmpgets of sysName.0
# (net-snmp, where Telegraf is gosnmp) as each, and what both print.
SNMP_V3_USER = "telegraf"
SNMP_V3_AUTH_PASSWORD = "lab-only-v3-password"
SNMP_V3_PRIV_PASSWORD = "lab-only-v3-privacy"
SNMP_V3_AUTH_ONLY_USER = "monitor"
SNMP_V3_GET_COMMAND = (
    "snmpget", "-v3", "-l", "authNoPriv", "-u", SNMP_V3_AUTH_ONLY_USER, "-a", "SHA-256",
    "-A", SNMP_V3_AUTH_PASSWORD, "-On", "sonda:1161", "1.3.6.1.2.1.1.5.0",
)
SNMP_V3_PRIV_GET_COMMAND = (
    "snmpget", "-v3", "-l", "authPriv", "-u", SNMP_V3_USER, "-a", "SHA-256",
    "-A", SNMP_V3_AUTH_PASSWORD, "-x", "AES", "-X", SNMP_V3_PRIV_PASSWORD,
    "-On", "sonda:1161", "1.3.6.1.2.1.1.5.0",
)
SNMP_V3_GET_EXPECTED = '.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"\n'
# How long walk_snmp_versions retries both walks and the SNMPv3 get before
# giving up; all are taken well before the flap's first down at 60s.
SNMP_WALK_WITHIN_S = 30.0
# The values that move between walks: sysUpTime and the octet counters.
SNMP_WALK_MASKS = (
    (re.compile(r"Timeticks: \(\d+\) .*"), "Timeticks: (*)"),
    (re.compile(r"Counter64: \d+"), "Counter64: *"),
)


def mask_snmp_walk(walk: str) -> str:
    """``walk`` with each SNMP_WALK_MASKS pattern replaced by its mask."""
    for pattern, mask in SNMP_WALK_MASKS:
        walk = pattern.sub(mask, walk)
    return walk


def walk_snmp_agent(
    repo_root: Path,
    *,
    command: tuple[str, ...] = SNMP_WALK_COMMAND,
    golden_path: Path = SNMP_WALK_GOLDEN,
    within_s: float = SNMP_WALK_WITHIN_S,
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
    now: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> str:
    """Walk sonda's agent with net-snmp from inside the telegraf-snmp service
    (:func:`net_snmp_in_container` of ``command``) until a walk prints
    ``golden_path`` under ``repo_root`` (both masked; any output when the
    golden cannot be read) or ``within_s`` has passed. The scenarios publish
    their first values after the agent binds, so an early walk can see fewer
    instances."""
    try:
        golden: str | None = mask_snmp_walk((repo_root / golden_path).read_text())
    except OSError:
        golden = None
    return net_snmp_in_container(
        repo_root,
        command,
        lambda stdout: golden is None or mask_snmp_walk(stdout) == golden,
        what="the walk",
        within_s=within_s,
        run=run,
        now=now,
        sleep=sleep,
    )


def net_snmp_in_container(
    repo_root: Path,
    command: tuple[str, ...],
    matches: Callable[[str], bool],
    *,
    what: str,
    within_s: float,
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
    now: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> str:
    """Run the net-snmp ``command`` inside the telegraf-snmp service with
    MIBS set empty, once a second until it exits 0 printing an stdout that
    ``matches`` accepts or ``within_s`` has passed. Returns the matching
    stdout, otherwise the last attempt's: its stdout, and its stderr when it
    did not exit 0. ``what`` names the command when an attempt times out."""
    argv = compose_command(
        repo_root, "exec", "-T", "-e", "MIBS=", "telegraf-snmp", *command
    )
    deadline = now() + within_s
    while True:
        try:
            done = run(
                argv, capture_output=True, text=True, timeout=within_s, check=False
            )
            if done.returncode == 0 and done.stdout.strip():
                last = done.stdout
                if matches(done.stdout):
                    return done.stdout
            else:
                last = (done.stdout or "") + (done.stderr or "")
        except subprocess.TimeoutExpired:
            last = f"<{what} did not finish within {within_s:.0f}s>"
        if now() >= deadline:
            return last
        sleep(1.0)


@dataclasses.dataclass(frozen=True)
class SnmpWalks:
    """The snmp row's two in-container walks and its two SNMPv3 gets."""

    # SNMP_WALK_COMMAND's output.
    v2c: str
    # SNMP_V1_WALK_COMMAND's output.
    v1: str
    # SNMP_V3_GET_COMMAND's output (authNoPriv).
    v3_get: str
    # SNMP_V3_PRIV_GET_COMMAND's output (authPriv).
    v3_priv_get: str


def walk_snmp_versions(
    repo_root: Path,
    *,
    within_s: float = SNMP_WALK_WITHIN_S,
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
    now: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> SnmpWalks:
    """:func:`walk_snmp_agent` over SNMPv2c against SNMP_WALK_GOLDEN, then
    over SNMPv1 against SNMP_V1_WALK_GOLDEN, then SNMP_V3_GET_COMMAND and
    SNMP_V3_PRIV_GET_COMMAND each until it prints SNMP_V3_GET_EXPECTED, each
    after the first with what is left of ``within_s`` (at least a second),
    so all four finish within one window."""
    deadline = now() + within_s
    v2c = walk_snmp_agent(repo_root, within_s=within_s, run=run, now=now, sleep=sleep)
    v1 = walk_snmp_agent(
        repo_root,
        command=SNMP_V1_WALK_COMMAND,
        golden_path=SNMP_V1_WALK_GOLDEN,
        within_s=max(deadline - now(), 1.0),
        run=run,
        now=now,
        sleep=sleep,
    )
    v3_get, v3_priv_get = (
        net_snmp_in_container(
            repo_root,
            command,
            lambda stdout: stdout == SNMP_V3_GET_EXPECTED,
            what="snmpget -v3",
            within_s=max(deadline - now(), 1.0),
            run=run,
            now=now,
            sleep=sleep,
        )
        for command in (SNMP_V3_GET_COMMAND, SNMP_V3_PRIV_GET_COMMAND)
    )
    return SnmpWalks(v2c=v2c, v1=v1, v3_get=v3_get, v3_priv_get=v3_priv_get)


@dataclasses.dataclass(frozen=True)
class SnmpRestart:
    """When the snmp row's during hook stopped sonda and started it again,
    in seconds since the epoch."""

    stopped_s: float
    restarted_s: float


@dataclasses.dataclass(frozen=True)
class SnmpEvidence:
    """What the snmp row's during hook returns: the walks and gets, and the
    restart, or why there was none."""

    walks: SnmpWalks
    restart: SnmpRestart | str


class SondaRun:
    """A ``sonda`` run as a child of the harness, which a during hook can
    stop and start again: :meth:`start` runs ``argv``, :meth:`restart` stops
    it with SIGINT (sonda's graceful stop) and runs it again for what is
    left of ``run_s`` (its ``--duration`` rewritten), and :meth:`wait`
    returns once the run that is current when one exits is the last one.
    Each run's output goes to temporary files, read back when it ends."""

    def __init__(
        self,
        argv: Sequence[str],
        cwd: Path,
        *,
        now: Callable[[], float] = time.monotonic,
        wall: Callable[[], float] = time.time,
        sleep: Callable[[float], None] = time.sleep,
        popen: Callable[..., subprocess.Popen] = subprocess.Popen,
    ) -> None:
        self.argv = list(argv)
        self.cwd = cwd
        self._now, self._wall, self._sleep, self._popen = now, wall, sleep, popen
        self._lock = threading.Lock()
        self._proc: subprocess.Popen | None = None
        self._files: tuple = ()
        self._stdout: list[str] = []
        self._stderr: list[str] = []
        self.started = 0.0

    def _spawn(self, argv: Sequence[str]) -> None:
        stdout = tempfile.TemporaryFile(mode="w+")
        stderr = tempfile.TemporaryFile(mode="w+")
        self._files = (stdout, stderr)
        self._proc = self._popen(
            list(argv), stdout=stdout, stderr=stderr, text=True, cwd=str(self.cwd)
        )

    def _collect(self) -> None:
        for file, sink in zip(self._files, (self._stdout, self._stderr)):
            file.seek(0)
            sink.append(file.read())
            file.close()
        self._files = ()

    def start(self) -> None:
        """Run ``argv``."""
        self.started = self._now()
        self._spawn(self.argv)

    def restart(self, down_s: float, run_s: float) -> SnmpRestart:
        """Stop the current run with SIGINT, wait ``down_s``, and run
        ``argv`` again with ``--duration`` what is left of ``run_s`` since
        :meth:`start`. Raises RuntimeError, starting nothing, when the run
        had already exited or does not exit 0 within 30 s of the signal."""
        with self._lock:
            proc = self._proc
            if proc is None or proc.poll() is not None:
                code = None if proc is None else proc.returncode
                raise RuntimeError(f"sonda exited ({code}) before the restart")
            proc.send_signal(signal.SIGINT)
            try:
                code = proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
                raise RuntimeError("sonda did not stop within 30s of SIGINT") from None
            stopped_s = self._wall()
            self._collect()
            if code != 0:
                raise RuntimeError(f"sonda exited {code} on SIGINT")
            self._sleep(down_s)
            left_s = max(run_s - (self._now() - self.started), 1.0)
            argv = list(self.argv)
            argv[argv.index("--duration") + 1] = f"{left_s:.0f}s"
            self._spawn(argv)
            return SnmpRestart(stopped_s=stopped_s, restarted_s=self._wall())

    def wait(self, timeout_s: float) -> subprocess.CompletedProcess[str]:
        """Wait for the last run to exit, at most ``timeout_s`` from now;
        its exit code and every run's output. On a timeout the current run
        is killed and subprocess.TimeoutExpired raised."""
        deadline = self._now() + timeout_s
        while True:
            with self._lock:
                proc = self._proc
            assert proc is not None, "start() first"
            try:
                proc.wait(timeout=max(deadline - self._now(), 0.0))
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
                raise
            with self._lock:
                if self._proc is proc:
                    if self._files:
                        self._collect()
                    return subprocess.CompletedProcess(
                        self.argv,
                        proc.returncode,
                        "".join(self._stdout),
                        "".join(self._stderr),
                    )


def snmp_during(
    repo_root: Path,
    *,
    sonda: SondaRun,
    restart_at_s: float = SNMP_RESTART_AT_S,
    run_s: float = SNMP_RUN_S,
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
    now: Callable[[], float] = time.monotonic,
    wall: Callable[[], float] = time.time,
    sleep: Callable[[float], None] = time.sleep,
) -> SnmpEvidence:
    """The snmp row's during hook: :func:`walk_snmp_versions`, then, once
    ``restart_at_s`` have passed since sonda started, at the next moment
    the wall clock is SNMP_RESTART_PHASE_S into a SNMP_POLL_INTERVAL_S
    cycle, ``sonda`` stopped and started again at once
    (:meth:`SondaRun.restart`). A restart that fails is returned as its
    reason."""
    walks = walk_snmp_versions(repo_root, run=run, now=now, sleep=sleep)
    sleep(max(sonda.started + restart_at_s - now(), 0.0))
    sleep((SNMP_RESTART_PHASE_S - wall()) % SNMP_POLL_INTERVAL_S)
    try:
        restart: SnmpRestart | str = sonda.restart(0.0, run_s)
    except RuntimeError as e:
        restart = f"<the restart failed: {e}>"
    return SnmpEvidence(walks=walks, restart=restart)


def walk_finding(walk: str, golden: str, name: str, golden_path: Path) -> str | None:
    """Why ``walk``, masked, is not ``golden``, masked, with the first lines
    of the diff; None when it is. ``name`` names the walk."""
    if not walk.strip():
        return f"the in-container {name} printed nothing"
    if mask_snmp_walk(walk) == mask_snmp_walk(golden):
        return None
    diff = difflib.unified_diff(
        mask_snmp_walk(golden).splitlines(),
        mask_snmp_walk(walk).splitlines(),
        str(golden_path),
        f"the {name}",
        lineterm="",
        n=1,
    )
    return f"the in-container {name} differs from {golden_path}:\n" + "\n".join(
        list(diff)[:40]
    )


def v3_get_finding(v3_get: str, name: str = "snmpget -v3") -> str | None:
    """Why the in-container SNMPv3 get ``name`` did not print
    SNMP_V3_GET_EXPECTED; None when it did."""
    if not v3_get.strip():
        return f"the in-container {name} printed nothing"
    if v3_get == SNMP_V3_GET_EXPECTED:
        return None
    return (
        f"the in-container {name} printed {v3_get.strip()!r}, want "
        f"{SNMP_V3_GET_EXPECTED.strip()!r}"
    )


def snmp_input_of(series: str) -> str:
    """The alias of the conf's inputs.snmp block that writes ``series``."""
    if series.startswith(SNMP_V1_SERIES_PREFIX):
        return SNMP_V1_INPUT
    if series.startswith(SNMP_V3_SERIES_PREFIX) or series == SNMP_V3_UPTIME_SERIES:
        return SNMP_V3_INPUT
    return SNMP_V2C_INPUT


def telegraf_log_stamp(line: str) -> float | None:
    """The UTC time, in seconds since the epoch, that begins a Telegraf log
    line; None when the line carries none."""
    match = TELEGRAF_LOG_TIME.search(line)
    if match is None:
        return None
    stamped = datetime.datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S")
    return stamped.replace(tzinfo=datetime.timezone.utc).timestamp()


def snmp_findings(
    telegraf_log: str,
    samples: dict[tuple[str, str], list[tuple[int, float]]],
    if_names: dict[tuple[str, str], dict[str, list[int]]],
    walk: str,
    golden: str,
    started_s: float,
    ended_s: float,
    *,
    v1_walk: str,
    v1_golden: str,
    v3_get: str,
    v3_priv_get: str,
    restart: SnmpRestart | str,
) -> list[str]:
    """Every way the snmp row's evidence falls short, in check order; empty
    when all hold.

    ``if_names`` is the export grouped by series, interface and
    SNMP_IF_NAME_TAG value (:func:`parse_vm_export_tags`). Only samples
    timestamped within [started_s, ended_s] count. In order: Telegraf logged
    a SNMP_LOADED_INPUTS line naming snmp; no SNMP_ERROR_LINE is stamped
    after the second of its input's first sample and at least a second
    before ended_s, or carries no stamp (a poll that began before sonda
    listened logs its error before the first poll of the same input that
    succeeds begins, and a sample is stamped when its poll begins; the two
    inputs poll independently, so one input's first sample says nothing
    about the other's polls; an error in the second the run ended may be the
    first poll after sonda stopped), nor from a second before ``restart``
    stopped sonda to the second of its input's first sample after it started
    again (to ended_s when there is none). An error's input is the alias in its
    prefix (SNMP_V2C_INPUT, SNMP_V1_INPUT, SNMP_V3_INPUT); a series' input is
    :func:`snmp_input_of` it. An error naming no known alias is measured
    from the first sample of any input; an input with no sample, from the
    run's start; some sample exists (with none, named with
    SNMP_EXPORT_MATCH, the checks after this one do not run); each
    SNMP_INTERFACE_SERIES has at least SNMP_MIN_SAMPLES samples for each
    interface within SNMP_SAMPLES_WITHIN_S of the first sample; every sample
    of each SNMP_INTERFACE_SERIES and SNMP_V1_INTERFACE_SERIES for each
    interface is tagged SNMP_IF_NAME_TAG with that interface's SNMP_IF_NAMES
    name (samples without the tag, or with another value, are named, and so
    is a series with samples and none so tagged); SNMP_UPTIME_SERIES has at
    least SNMP_MIN_SAMPLES samples, each greater than the one before; the
    flapping interface's ifOperStatus takes every SNMP_OPER_STATUS_CODES
    value; each SNMP_V1_INTERFACE_SERIES has at least SNMP_MIN_SAMPLES
    samples for each interface within SNMP_SAMPLES_WITHIN_S of the first
    sample, and no SNMP_V1_ABSENT_SERIES has any sample; each
    SNMP_V3_INTERFACE_SERIES has at least SNMP_MIN_SAMPLES samples for each
    interface within SNMP_SAMPLES_WITHIN_S of the first sample, every one of
    them tagged with the interface's SNMP_IF_NAMES name as above, and
    SNMP_V3_UPTIME_SERIES has at least SNMP_MIN_SAMPLES samples, each greater
    than the one before; the restart: ``restart`` is an SnmpRestart (a text
    is the hook's failure, named), each SNMP_V3_INTERFACE_SERIES has a
    sample for each interface within SNMP_RESUME_WITHIN_S of sonda starting
    again (each that had one before sonda stopped; one that had none is
    named above), a SNMP_V3_TRACE_LINE carrying SNMP_TIME_WINDOW_LINE is stamped
    from a second before sonda stopped on (gosnmp answered
    usmStatsNotInTimeWindows and re-synchronized; when none is, the v3
    input's first SNMP_ERROR_LINE from then on is named with it), and none carrying
    SNMP_REDISCOVERY_LINE is (the engine ID did not change); both uptimes
    are checked for increase before sonda stopped and after it, apart;
    ``walk``, masked, is ``golden``, masked, and ``v1_walk`` is
    ``v1_golden`` (an empty walk is named as such); ``v3_get`` and
    ``v3_priv_get`` are SNMP_V3_GET_EXPECTED (:func:`v3_get_finding`).
    """
    findings: list[str] = []
    loaded = any(
        SNMP_LOADED_INPUTS in line
        and "snmp" in line.split(SNMP_LOADED_INPUTS, 1)[1].split()
        for line in telegraf_log.splitlines()
    )
    if not loaded:
        findings.append(f"telegraf log has no {SNMP_LOADED_INPUTS!r} line naming snmp")

    start_ms, end_ms = int(started_s * 1000), int(ended_s * 1000)
    in_window = {
        key: sorted((ts, v) for ts, v in pairs if start_ms <= ts <= end_ms)
        for key, pairs in samples.items()
    }
    timestamps = [ts for pairs in in_window.values() for ts, _ in pairs]
    first_ms = min(timestamps) if timestamps else None
    input_first_ms: dict[str, int] = {}
    for (series, _), pairs in in_window.items():
        if pairs:
            alias = snmp_input_of(series)
            input_first_ms[alias] = min(input_first_ms.get(alias, pairs[0][0]), pairs[0][0])

    def excused_until_s(first: int | None) -> float:
        return first // 1000 if first is not None else math.floor(started_s) - 1

    # The restart's gap: from a second before sonda stopped, per input to
    # the second of its first sample after sonda started again.
    stopped_ms = restarted_ms = None
    input_resumed_ms: dict[str, int] = {}
    if isinstance(restart, SnmpRestart):
        stopped_ms = int(restart.stopped_s * 1000)
        restarted_ms = int(restart.restarted_s * 1000)
        for (series, _), pairs in in_window.items():
            after = [ts for ts, _ in pairs if ts >= restarted_ms]
            if after:
                alias = snmp_input_of(series)
                input_resumed_ms[alias] = min(input_resumed_ms.get(alias, after[0]), after[0])

    def in_gap(stamp: float, resumed: int | None) -> bool:
        if stopped_ms is None:
            return False
        until_s = resumed // 1000 if resumed is not None else ended_s
        return stopped_ms // 1000 - 1 <= stamp <= until_s

    for line in telegraf_log.splitlines():
        error = SNMP_ERROR_LINE.search(line)
        if error is None:
            continue
        alias = error.group("alias")
        if alias in (SNMP_V2C_INPUT, SNMP_V1_INPUT, SNMP_V3_INPUT):
            after_s = excused_until_s(input_first_ms.get(alias))
            resumed = input_resumed_ms.get(alias)
        else:
            after_s = excused_until_s(first_ms)
            resumed = min(input_resumed_ms.values()) if input_resumed_ms else None
        stamp = telegraf_log_stamp(line)
        if stamp is None or (after_s < stamp <= ended_s - 1 and not in_gap(stamp, resumed)):
            findings.append(f"telegraf logged an error while sonda served: {line.strip()}")

    if first_ms is None:
        findings.append(
            f"no sample matching {SNMP_EXPORT_MATCH} within the run's window"
        )
        return findings

    within_ms = first_ms + int(SNMP_SAMPLES_WITHIN_S * 1000)

    def every_interface(series_names: tuple[str, ...]) -> None:
        for series in series_names:
            for index in SNMP_INTERFACES:
                pairs = in_window.get((series, index), [])
                count = sum(1 for ts, _ in pairs if ts <= within_ms)
                if count < SNMP_MIN_SAMPLES:
                    findings.append(
                        f'{series}{{index="{index}"}}: {count} sample(s) within '
                        f"{SNMP_SAMPLES_WITHIN_S:.0f}s of the first sample, want >= "
                        f"{SNMP_MIN_SAMPLES}"
                    )

    every_interface(SNMP_INTERFACE_SERIES)

    def every_name_tagged(series_names: tuple[str, ...]) -> None:
        for series in series_names:
            for index in SNMP_INTERFACES:
                want = SNMP_IF_NAMES[index]
                tagged = {
                    value: sum(1 for ts in stamps if start_ms <= ts <= end_ms)
                    for value, stamps in if_names.get((series, index), {}).items()
                }
                if in_window.get((series, index)) and not tagged.get(want):
                    findings.append(
                        f'{series}{{index="{index}"}}: no sample tagged '
                        f'{SNMP_IF_NAME_TAG}="{want}"'
                    )
                for value, count in sorted(tagged.items()):
                    if value != want and count:
                        shown = (
                            f'{SNMP_IF_NAME_TAG}="{value}"' if value else f"no {SNMP_IF_NAME_TAG}"
                        )
                        findings.append(
                            f'{series}{{index="{index}"}}: {count} sample(s) tagged {shown}, '
                            f'want {SNMP_IF_NAME_TAG}="{want}"'
                        )

    every_name_tagged(SNMP_INTERFACE_SERIES + SNMP_V1_INTERFACE_SERIES)

    def increasing(series: str) -> None:
        uptime = in_window.get((series, ""), [])
        if len(uptime) < SNMP_MIN_SAMPLES:
            findings.append(f"{series}: {len(uptime)} sample(s), want >= {SNMP_MIN_SAMPLES}")
        # sysUpTime starts again from 0 when sonda does.
        runs = [uptime]
        if stopped_ms is not None:
            runs = [
                [pair for pair in uptime if pair[0] < stopped_ms],
                [pair for pair in uptime if pair[0] >= stopped_ms],
            ]
        falls = (
            (before, after, ts)
            for samples in runs
            for (_, before), (ts, after) in zip(samples, samples[1:])
            if after <= before
        )
        for before, after, ts in falls:
            findings.append(
                f"{series} did not increase: {before:g} then {after:g}, "
                f"{(ts - start_ms) / 1000:.1f}s after sonda started"
            )
            break

    increasing(SNMP_UPTIME_SERIES)

    seen = {v for _, v in in_window.get((SNMP_OPER_STATUS_SERIES, SNMP_FLAPPING_INTERFACE), [])}
    for state, code in SNMP_OPER_STATUS_CODES.items():
        if code not in seen:
            findings.append(
                f'{SNMP_OPER_STATUS_SERIES}{{index="{SNMP_FLAPPING_INTERFACE}"}} '
                f"never took {state} ({code:g}) within the run; saw {sorted(seen)}"
            )

    every_interface(SNMP_V1_INTERFACE_SERIES)
    for series in SNMP_V1_ABSENT_SERIES:
        carried = sorted(index for name, index in in_window if name == series)
        if carried:
            findings.append(
                f"{series}: samples for index {', '.join(carried)}; SNMPv1 carries "
                "no Counter64, so the agent must skip the column"
            )

    every_interface(SNMP_V3_INTERFACE_SERIES)
    every_name_tagged(SNMP_V3_INTERFACE_SERIES)
    increasing(SNMP_V3_UPTIME_SERIES)

    if isinstance(restart, SnmpRestart):
        # A series with no sample before sonda stopped is named above.
        resume_by_ms = restarted_ms + int(SNMP_RESUME_WITHIN_S * 1000)
        for series in SNMP_V3_INTERFACE_SERIES:
            for index in SNMP_INTERFACES:
                pairs = in_window.get((series, index), [])
                if not any(ts < stopped_ms for ts, _ in pairs):
                    continue
                if not any(restarted_ms <= ts <= resume_by_ms for ts, _ in pairs):
                    findings.append(
                        f'{series}{{index="{index}"}}: no sample within '
                        f"{SNMP_RESUME_WITHIN_S:.0f}s of sonda starting again"
                    )
        since_stop = [
            line
            for line in telegraf_log.splitlines()
            if (telegraf_log_stamp(line) or 0) >= stopped_ms // 1000 - 1
        ]
        traced = [line for line in since_stop if SNMP_V3_TRACE_LINE.search(line)]
        if not any(SNMP_TIME_WINDOW_LINE in line for line in traced):
            errored = ""
            for line in since_stop:
                error = SNMP_ERROR_LINE.search(line)
                if error is not None and error.group("alias") == SNMP_V3_INPUT:
                    errored = (
                        "; the input errored first, and Telegraf discovers the "
                        f"engine again after an error: {line.strip()}"
                    )
                    break
            findings.append(
                f"telegraf's v3 input logged no {SNMP_TIME_WINDOW_LINE!r} after sonda "
                f"restarted: gosnmp was not answered usmStatsNotInTimeWindows{errored}"
            )
        for line in traced:
            if SNMP_REDISCOVERY_LINE in line:
                findings.append(
                    f"telegraf's v3 input discovered the engine again after sonda "
                    f"restarted: {line.strip()}"
                )
                break
    else:
        findings.append(
            f"sonda was not restarted: {restart or 'the during hook reported no restart'}"
        )

    for found in (
        walk_finding(walk, golden, "walk", SNMP_WALK_GOLDEN),
        walk_finding(v1_walk, v1_golden, "v1 walk", SNMP_V1_WALK_GOLDEN),
        v3_get_finding(v3_get),
        v3_get_finding(v3_priv_get, "snmpget -v3 -l authPriv"),
    ):
        if found is not None:
            findings.append(found)
    return findings


def verify_snmp_row(
    repo_root: Path, started_s: float, ended_s: float, during: SnmpEvidence | str = ""
) -> str:
    """Collect the telegraf-snmp service's log, the run's samples matching
    SNMP_EXPORT_MATCH, SNMP_WALK_GOLDEN and SNMP_V1_WALK_GOLDEN, then apply
    :func:`snmp_findings` with ``during``, the walks, the SNMPv3 gets and the
    restart :func:`snmp_during` returned; a text ``during`` (no hook, or the
    hook's failure) stands for all of them."""
    telegraf_log = compose_logs_tail(repo_root, "telegraf-snmp", tail_lines=None)
    query = urllib.parse.urlencode(
        {
            "match[]": SNMP_EXPORT_MATCH,
            "start": f"{started_s:.3f}",
            "end": f"{ended_s:.3f}",
        }
    )
    status, body = http_get(f"http://localhost:8428/api/v1/export?{query}")
    if status != 200:
        return f"VictoriaMetrics export answered {status}: {body[:200]!r}"
    goldens = []
    for path in (SNMP_WALK_GOLDEN, SNMP_V1_WALK_GOLDEN):
        try:
            goldens.append((repo_root / path).read_text())
        except OSError as e:
            return f"cannot read {path}: {e}"
    evidence = (
        during
        if isinstance(during, SnmpEvidence)
        else SnmpEvidence(
            walks=SnmpWalks(v2c=during, v1=during, v3_get=during, v3_priv_get=during),
            restart=during,
        )
    )
    walks = evidence.walks
    return "\n".join(
        snmp_findings(
            telegraf_log,
            parse_vm_export(body, label="index"),
            parse_vm_export_tags(body, SNMP_IF_NAME_TAG),
            walks.v2c,
            goldens[0],
            started_s,
            ended_s,
            v1_walk=walks.v1,
            v1_golden=goldens[1],
            v3_get=walks.v3_get,
            v3_priv_get=walks.v3_priv_get,
            restart=evidence.restart,
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
    # -- SNMP: sonda is the agent and Telegraf (profile snmp) the poller.
    # `sonda run` serves for SNMP_RUN_S while Telegraf polls it every 2s and
    # writes to VictoriaMetrics over SNMPv2c, SNMPv1 and SNMPv3 (authPriv),
    # and snmp_during walks it in the first two versions and reads sysName
    # over SNMPv3 at authNoPriv and authPriv from inside the telegraf-snmp
    # container, then stops sonda past SNMP_RESTART_AT_S, between two polls,
    # and starts it again at once.
    # The verify URL proves some series arrived at all; verify_snmp_row then
    # checks each interface in the three tables, both uptimes, the flap, the
    # v3 series resuming after the restart through gosnmp's time-window
    # report, each walk against its golden file and both SNMPv3 reads.
    MatrixRow(
        name="snmp",
        profiles=("snmp",),
        subcommand="metrics",
        scenario=Path("examples/snmp-telegraf.yaml"),
        verify_url=(
            "http://localhost:8428/api/v1/query?"
            + urllib.parse.urlencode({"query": SNMP_EXPORT_MATCH})
        ),
        failure_log_containers=("telegraf-snmp", "victoriametrics"),
        extra_args=("--duration", f"{SNMP_RUN_S:.0f}s"),
        timeout_s=SNMP_RUN_S + 60.0,
        during=snmp_during,
        during_gets_sonda=True,
        post_verify=verify_snmp_row,
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
    "snmp": (
        Backend(name="telegraf-snmp", health_url="http://localhost:8097/"),
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
    all of them when ``tail_lines`` is None, bytes that are not UTF-8 (such
    as the raw packet fields in gosnmp's trace) replaced.

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
            errors="replace",
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
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)
    during = None
    try:
        if row.during_gets_sonda:
            sonda = SondaRun(
                [str(sonda_bin), row.verb, str(row.scenario), *row.extra_args], repo_root
            )
            sonda.start()
            if row.during is not None:
                during = pool.submit(row.during, repo_root, sonda=sonda)
            proc = sonda.wait(row.timeout_s)
        else:
            if row.during is not None:
                during = pool.submit(row.during, repo_root)
            proc = run_scenario(
                sonda_bin,
                row.subcommand,
                row.scenario,
                repo_root,
                verb=row.verb,
                extra_args=row.extra_args,
                timeout_s=row.timeout_s,
            )
        # Taken before waiting for the during hook: the run's window ends
        # when sonda does.
        ended_wall_s = time.time()
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
    finally:
        pool.shutdown(wait=True)

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
        kwargs = {}
        if during is not None:
            try:
                kwargs["during"] = during.result()
            except Exception as e:  # noqa: BLE001 — reported as the row's failure
                kwargs["during"] = f"<the during hook raised {e!r}>"
        problems = row.post_verify(repo_root, started_wall_s, ended_wall_s, **kwargs)
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
            "http,kafka,remote-write,otlp,gnmi,snmp).",
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
