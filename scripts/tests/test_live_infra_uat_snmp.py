"""Tests for the snmp row of scripts/live_infra_uat.py.

Each test drives the harness's own functions: snmp_findings over a synthetic
VictoriaMetrics export (through parse_vm_export) and a synthetic Telegraf log,
verify_snmp_row and walk_snmp_agent with their I/O replaced, and run_row with
the sonda run replaced. Run with ``python3 -m pytest scripts/tests``.
"""

from __future__ import annotations

import datetime
import json
import re
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
import urllib.parse
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import live_infra_uat as uat  # noqa: E402

REPO_ROOT = uat.find_repo_root(Path(__file__).parent)

# 2026-01-01T00:00:00Z.
STARTED_S = 1_767_225_600.0
ENDED_S = STARTED_S + uat.SNMP_RUN_S
# Telegraf polls every 2 s; the first poll that sonda answers lands 1 s in.
POLL_S = 2.0
FIRST_POLL_S = STARTED_S + 1.0
# When the during hook stopped sonda and started it again: at once, between
# two polls.
STOPPED_S = STARTED_S + uat.SNMP_RESTART_AT_S
RESTARTED_S = STOPPED_S + 0.2
RESTART = uat.SnmpRestart(stopped_s=STOPPED_S, restarted_s=RESTARTED_S)
# Every interface table field examples/telegraf/telegraf-snmp.conf names.
FIELDS = ("ifOperStatus", "ifInErrors", "ifOutErrors", "ifHCInOctets", "ifHCOutOctets")
# The fields Telegraf writes for the interface_v1 table: SNMPv1 carries no
# Counter64, so the two HC columns read as empty.
V1_FIELDS = ("ifOperStatus", "ifInErrors", "ifOutErrors")


def telegraf_line(at_s: float, message: str) -> str:
    """A telegraf-snmp log line stamped ``at_s``, as ``docker compose logs``
    prints it."""
    stamp = datetime.datetime.fromtimestamp(at_s, datetime.timezone.utc)
    return f"telegraf-snmp-1  | {stamp:%Y-%m-%dT%H:%M:%S}Z {message}\n"


def refused(at_s: float) -> str:
    """An error of the v2c input, as Telegraf 1.40.2 logs it."""
    return telegraf_line(
        at_s,
        "E! [inputs.snmp::v2c] Error in plugin: agent udp://sonda:1161: performing get "
        "on field uptime: error reading from socket: read udp 172.18.0.3:40860->"
        "172.17.0.1:1161: recvfrom: connection refused",
    )


def refused_v3(at_s: float) -> str:
    """An error of the v3 input, as Telegraf 1.40.2 logs it."""
    return telegraf_line(
        at_s,
        "E! [inputs.snmp::v3] Error in plugin: agent udp://sonda:1161: performing get "
        "on field uptime: error reading from socket: read udp 172.18.0.3:51234->"
        "172.17.0.1:1161: recvfrom: connection refused",
    )


def refused_v1(at_s: float) -> str:
    """An error of the v1 input, as Telegraf 1.40.2 logs it."""
    return telegraf_line(
        at_s,
        "E! [inputs.snmp::v1] Error in plugin: agent udp://sonda:1161: gathering table "
        "interface_v1: performing bulk walk for field ifOperStatus: error reading from "
        "socket: read udp 172.18.0.3:35909->172.17.0.1:1161: recvfrom: connection refused",
    )


def time_window(at_s: float) -> str:
    """gosnmp's trace line, through the v3 input, for a request answered
    usmStatsNotInTimeWindows, as Telegraf 1.40.2 logs it."""
    return telegraf_line(
        at_s, f"T! [inputs.snmp::v3] agent sonda: {uat.SNMP_TIME_WINDOW_LINE}"
    )


def timed_out(at_s: float) -> str:
    """The v2c input's error for a poll whose retries ran out, as CI logged it."""
    return telegraf_line(
        at_s,
        "E! [inputs.snmp::v2c] Error in plugin: agent udp://sonda:1161: gathering table "
        "interface: performing bulk walk for field ifOperStatus: request timeout "
        "(after 3 retries)",
    )


def good_log() -> str:
    """Telegraf's startup, errors from polls sonda was not listening for (ten
    minutes before the run, in the second of the first sample, in the second
    of the first poll after the restart, and in the second the run ended),
    gosnmp's time-window line after the restart, and nothing else. The v3
    input has no error at the restart: Telegraf would have discovered the
    engine again after one, and logged no time-window line."""
    return (
        telegraf_line(STARTED_S - 600, "I! Loading config: /etc/telegraf/telegraf.conf")
        + telegraf_line(STARTED_S - 600, "I! Loaded inputs: snmp (2x)")
        + telegraf_line(STARTED_S - 600, "I! Loaded outputs: health influxdb")
        + refused(STARTED_S - 600)
        + refused_v1(STARTED_S - 600)
        + refused(FIRST_POLL_S)
        + refused_v1(FIRST_POLL_S)
        + refused(STOPPED_S + 1)
        + refused_v1(STOPPED_S + 1)
        + time_window(RESTARTED_S)
        + refused(ENDED_S)
        + refused_v1(ENDED_S)
    )


def poll_times(
    first_s: float = FIRST_POLL_S, last_s: float = ENDED_S - 1
) -> list[float]:
    """Telegraf's polls every POLL_S from ``first_s`` to ``last_s`` that sonda
    answered: none while it was stopped for the restart."""
    times, at = [], first_s
    while at <= last_s:
        if not STOPPED_S <= at < RESTARTED_S:
            times.append(at)
        at += POLL_S
    return times


def sys_up_time(at_s: float) -> int:
    """sysUpTime at ``at_s``: hundredths of a second since sonda last
    started."""
    since = RESTARTED_S if at_s >= STOPPED_S else STARTED_S
    return round((at_s - since) * 100)


def export_line(name: str, labels: dict[str, str], samples: list[tuple[float, float]]) -> str:
    return json.dumps(
        {
            "metric": {"__name__": name, "db": "telegraf", "source": "sonda", **labels},
            "values": [v for _, v in samples],
            "timestamps": [int(at * 1000) for at, _ in samples],
        }
    )


def oper_status(index: str, at_s: float) -> float:
    """The example's ifOperStatus: interface 1 down from 60 s to 65 s."""
    if index == uat.SNMP_FLAPPING_INTERFACE and 60 <= at_s - STARTED_S < 65:
        return 2.0
    return 1.0


def if_name_tag(index: str) -> dict[str, str]:
    """The ifName tag Telegraf sets on interface ``index``'s rows."""
    return {uat.SNMP_IF_NAME_TAG: uat.SNMP_IF_NAMES[index]}


def good_export(
    interfaces: tuple[str, ...] = uat.SNMP_INTERFACES,
    uptime=sys_up_time,
    status=oper_status,
    v1_interfaces: tuple[str, ...] = uat.SNMP_INTERFACES,
    v1_fields: tuple[str, ...] = V1_FIELDS,
    v2c_from_s: float = FIRST_POLL_S,
    tags=if_name_tag,
    v1_tags=None,
    v3_interfaces: tuple[str, ...] = uat.SNMP_INTERFACES,
    v3_fields: tuple[str, ...] = FIELDS,
    v3_tags=if_name_tag,
    v3_uptime=lambda at_s: sys_up_time(at_s) + 1,
    v3_from_s: float = FIRST_POLL_S,
    v3_resumed_s: float = RESTARTED_S,
) -> bytes:
    """What VictoriaMetrics exports for a run in which Telegraf read every
    poll: each field of each of ``interfaces``, each of ``v1_fields`` of each
    of ``v1_interfaces`` in the interface_v1 table, each of ``v3_fields`` of
    each of ``v3_interfaces`` in the interface_v3 table, every row tagged
    with ``tags`` of its interface (``v1_tags`` in the interface_v1 table,
    when given; ``v3_tags``, which ``tags`` does not change, in the
    interface_v3 table), snmp_uptime and snmp_v3_uptime. The v2c input's
    series (interface and snmp_uptime) start at ``v2c_from_s``, the v3
    input's (interface_v3 and snmp_v3_uptime) at ``v3_from_s`` and, after
    the restart, at ``v3_resumed_s``."""
    times = poll_times()
    v2c_times = poll_times(first_s=v2c_from_s)
    v3_times = [
        at for at in poll_times(first_s=v3_from_s) if at < STOPPED_S or at >= v3_resumed_s
    ]
    lines = []
    for table, indices, fields, polls, tagged in (
        ("interface", interfaces, FIELDS, v2c_times, tags),
        ("interface_v1", v1_interfaces, v1_fields, times, v1_tags or tags),
        ("interface_v3", v3_interfaces, v3_fields, v3_times, v3_tags),
    ):
        for index in indices:
            for field in fields:
                if field == "ifOperStatus":
                    samples = [(at, status(index, at)) for at in polls]
                else:
                    samples = [(at, 125_000.0 * (at - STARTED_S)) for at in polls]
                labels = {"index": index, **tagged(index)}
                lines.append(export_line(f"{table}_{field}", labels, samples))
    lines.append(
        export_line(
            uat.SNMP_UPTIME_SERIES,
            {"hostname": "rtr-core-01"},
            [(at, float(uptime(at))) for at in v2c_times],
        )
    )
    lines.append(
        export_line(
            uat.SNMP_V3_UPTIME_SERIES, {}, [(at, float(v3_uptime(at))) for at in v3_times]
        )
    )
    return "\n".join(lines).encode()


def golden() -> str:
    return (REPO_ROOT / uat.SNMP_WALK_GOLDEN).read_text()


def walked(ticks: int = 2284, octets: int = 2_750_000) -> str:
    """The golden walk as net-snmp prints it, before masking."""
    text = golden().replace("Timeticks: (*)", f"Timeticks: ({ticks}) 0:00:22.84")
    return text.replace("Counter64: *", f"Counter64: {octets}")


def v1_golden() -> str:
    return (REPO_ROOT / uat.SNMP_V1_WALK_GOLDEN).read_text()


def v1_walked(ticks: int = 2290) -> str:
    """The v1 golden walk as net-snmp prints it, before masking."""
    return v1_golden().replace("Timeticks: (*)", f"Timeticks: ({ticks}) 0:00:22.90")


def findings(
    log: str | None = None,
    export: bytes | None = None,
    walk: str | None = None,
    v1_walk: str | None = None,
    v3_get: str = uat.SNMP_V3_GET_EXPECTED,
    v3_priv_get: str = uat.SNMP_V3_GET_EXPECTED,
    restart: uat.SnmpRestart | str = RESTART,
) -> list[str]:
    export = good_export() if export is None else export
    return uat.snmp_findings(
        good_log() if log is None else log,
        uat.parse_vm_export(export, label="index"),
        uat.parse_vm_export_tags(export, uat.SNMP_IF_NAME_TAG),
        walked() if walk is None else walk,
        golden(),
        STARTED_S,
        ENDED_S,
        v1_walk=v1_walked() if v1_walk is None else v1_walk,
        v1_golden=v1_golden(),
        v3_get=v3_get,
        v3_priv_get=v3_priv_get,
        restart=restart,
    )


class SnmpFindingsTests(unittest.TestCase):
    def test_a_good_run_has_no_findings(self) -> None:
        self.assertEqual(findings(), [])

    def test_the_synthetic_export_has_every_series_it_claims(self) -> None:
        # Guards the green case: an export that parsed to nothing would pass
        # every per-interface check vacuously were the series check missing.
        samples = uat.parse_vm_export(good_export(), label="index")
        self.assertEqual(
            len(samples),
            len(uat.SNMP_INTERFACES) * (2 * len(FIELDS) + len(V1_FIELDS)) + 2,
        )
        self.assertIn((uat.SNMP_UPTIME_SERIES, ""), samples)
        self.assertIn((uat.SNMP_V3_UPTIME_SERIES, ""), samples)
        self.assertEqual(len(samples[("interface_v3_ifOperStatus", "4")]), len(poll_times()))
        self.assertEqual(len(samples[("interface_ifOperStatus", "4")]), len(poll_times()))
        self.assertEqual(len(samples[("interface_v1_ifOperStatus", "4")]), len(poll_times()))
        tags = uat.parse_vm_export_tags(good_export(), uat.SNMP_IF_NAME_TAG)
        self.assertEqual(
            tags[("interface_ifHCInOctets", "4")],
            {"GigabitEthernet0/0/3": [int(at * 1000) for at in poll_times()]},
        )
        self.assertEqual(
            tags[("interface_v1_ifInErrors", "4")],
            {"GigabitEthernet0/0/3": [int(at * 1000) for at in poll_times()]},
        )

    def test_a_missing_interface_is_named_in_each_series(self) -> None:
        found = findings(export=good_export(interfaces=("1", "2", "3"), v1_interfaces=("1", "2", "3")))
        self.assertEqual(
            found,
            [
                f'{series}{{index="4"}}: 0 sample(s) within 30s of the first '
                "sample, want >= 3"
                for series in uat.SNMP_INTERFACE_SERIES + uat.SNMP_V1_INTERFACE_SERIES
            ],
        )

    def test_an_interface_that_starts_late_is_named(self) -> None:
        lines = good_export().decode().splitlines()
        late = [
            line
            for line in lines
            if json.loads(line)["metric"].get("index") == "3"
        ]
        for line in late:
            lines.remove(line)
            entry = json.loads(line)
            kept = [
                (ts, v)
                for ts, v in zip(entry["timestamps"], entry["values"])
                if ts > (FIRST_POLL_S + 26) * 1000
            ]
            lines.append(
                export_line(
                    entry["metric"]["__name__"],
                    {"index": "3", **if_name_tag("3")},
                    [(ts / 1000, v) for ts, v in kept],
                )
            )
        found = findings(export="\n".join(lines).encode())
        self.assertEqual(len(found), 6, found)
        for line in found:
            self.assertIn('{index="3"}: 2 sample(s) within 30s', line)

    def test_a_flat_uptime_is_named(self) -> None:
        found = findings(export=good_export(uptime=lambda _: 97))
        self.assertEqual(len(found), 1, found)
        self.assertTrue(found[0].startswith("snmp_uptime did not increase: 97 then 97"), found)

    def test_too_few_uptime_samples_are_named(self) -> None:
        lines = [
            line
            for line in good_export().decode().splitlines()
            if uat.SNMP_UPTIME_SERIES not in line
        ]
        two = [(FIRST_POLL_S, 100.0), (FIRST_POLL_S + POLL_S, 300.0)]
        lines.append(export_line(uat.SNMP_UPTIME_SERIES, {}, two))
        self.assertEqual(
            findings(export="\n".join(lines).encode()),
            ["snmp_uptime: 2 sample(s), want >= 3"],
        )

    def test_no_loaded_inputs_line_is_named(self) -> None:
        log = good_log().replace("I! Loaded inputs: snmp (2x)", "I! Loaded inputs: cpu")
        self.assertEqual(
            findings(log=log),
            ["telegraf log has no 'Loaded inputs:' line naming snmp"],
        )

    def test_an_error_while_sonda_served_is_named(self) -> None:
        error = refused(STARTED_S + 30)
        found = findings(log=good_log() + error)
        self.assertEqual(
            found, [f"telegraf logged an error while sonda served: {error.strip()}"]
        )

    def test_an_unstamped_error_is_named(self) -> None:
        error = "E! [inputs.snmp] Error in plugin: no stamp\n"
        self.assertEqual(
            findings(log=good_log() + error),
            [f"telegraf logged an error while sonda served: {error.strip()}"],
        )

    def test_a_v2c_timeout_after_the_first_v1_sample_is_excused(self) -> None:
        # The two inputs poll independently. The v1 input answers first; the
        # v2c input's poll that began before sonda listened times out after
        # it, and the v2c input's own first sample comes after that.
        export = good_export(v2c_from_s=FIRST_POLL_S + 4)
        error = timed_out(FIRST_POLL_S + 3)
        self.assertEqual(findings(log=good_log() + error, export=export), [])
        # Guard: the same moment is not excused for the v1 input, whose first
        # sample came earlier.
        late_v1 = refused_v1(FIRST_POLL_S + 3)
        self.assertEqual(
            findings(log=good_log() + late_v1, export=export),
            [f"telegraf logged an error while sonda served: {late_v1.strip()}"],
        )

    def test_a_v2c_error_after_its_own_first_sample_is_named(self) -> None:
        export = good_export(v2c_from_s=FIRST_POLL_S + 4)
        error = timed_out(FIRST_POLL_S + 5)
        self.assertEqual(
            findings(log=good_log() + error, export=export),
            [f"telegraf logged an error while sonda served: {error.strip()}"],
        )

    def test_an_error_naming_no_known_alias_is_measured_from_the_first_sample(self) -> None:
        export = good_export(v2c_from_s=FIRST_POLL_S + 4)
        error = telegraf_line(
            FIRST_POLL_S + 3, "E! [inputs.snmp] Error in plugin: request timeout"
        )
        self.assertEqual(
            findings(log=good_log() + error, export=export),
            [f"telegraf logged an error while sonda served: {error.strip()}"],
        )

    def test_an_input_with_no_sample_has_its_errors_counted_from_the_start(self) -> None:
        export = good_export(v1_interfaces=())
        found = findings(log=good_log(), export=export)
        self.assertEqual(
            found[0], f"telegraf logged an error while sonda served: {refused_v1(FIRST_POLL_S).strip()}"
        )
        self.assertEqual(len(found), 1 + len(uat.SNMP_V1_INTERFACE_SERIES) * len(uat.SNMP_INTERFACES))

    def test_every_series_belongs_to_the_input_that_writes_it(self) -> None:
        for series in uat.SNMP_V1_INTERFACE_SERIES + uat.SNMP_V1_ABSENT_SERIES:
            self.assertEqual(uat.snmp_input_of(series), uat.SNMP_V1_INPUT, series)
        for series in uat.SNMP_INTERFACE_SERIES + (uat.SNMP_UPTIME_SERIES,):
            self.assertEqual(uat.snmp_input_of(series), uat.SNMP_V2C_INPUT, series)

    def test_an_error_a_second_after_the_first_sample_is_named(self) -> None:
        self.assertEqual(len(findings(log=good_log() + refused(FIRST_POLL_S + 1))), 1)

    def test_an_error_a_second_before_the_end_is_named(self) -> None:
        self.assertEqual(len(findings(log=good_log() + refused(ENDED_S - 1))), 1)

    def test_no_sample_stops_the_checks_after_it(self) -> None:
        # With no first sample, every error stamped from the run's start
        # counts: here the two a second in.
        found = findings(export=b"")
        self.assertEqual(
            found,
            [
                f"telegraf logged an error while sonda served: {refused(FIRST_POLL_S).strip()}",
                f"telegraf logged an error while sonda served: {refused_v1(FIRST_POLL_S).strip()}",
                f"no sample matching {uat.SNMP_EXPORT_MATCH} within the run's window",
            ],
        )

    def test_samples_outside_the_run_do_not_count(self) -> None:
        export = good_export().decode().replace(
            '"timestamps": [', '"timestamps": [1, '
        ).replace('"values": [', '"values": [9, ')
        self.assertEqual(findings(export=export.encode()), [])
        outside = uat.snmp_findings(
            good_log(),
            uat.parse_vm_export(good_export(), label="index"),
            uat.parse_vm_export_tags(good_export(), uat.SNMP_IF_NAME_TAG),
            walked(),
            golden(),
            ENDED_S + 10,
            ENDED_S + 20,
            v1_walk=v1_walked(),
            v1_golden=v1_golden(),
            v3_get=uat.SNMP_V3_GET_EXPECTED,
            v3_priv_get=uat.SNMP_V3_GET_EXPECTED,
            restart=RESTART,
        )
        self.assertEqual(
            outside[-1],
            f"no sample matching {uat.SNMP_EXPORT_MATCH} within the run's window",
        )

    def test_rows_without_the_if_name_tag_are_named(self) -> None:
        found = findings(export=good_export(tags=lambda index: {}))
        polls = len(poll_times())
        self.assertEqual(
            found,
            [
                line
                for series in uat.SNMP_INTERFACE_SERIES + uat.SNMP_V1_INTERFACE_SERIES
                for index in uat.SNMP_INTERFACES
                for line in (
                    f'{series}{{index="{index}"}}: no sample tagged '
                    f'ifName="GigabitEthernet0/0/{int(index) - 1}"',
                    f'{series}{{index="{index}"}}: {polls} sample(s) tagged no ifName, '
                    f'want ifName="GigabitEthernet0/0/{int(index) - 1}"',
                )
            ],
        )

    def test_one_interface_without_the_tag_is_named(self) -> None:
        tags = lambda index: {} if index == "2" else if_name_tag(index)  # noqa: E731
        found = findings(export=good_export(tags=tags))
        self.assertEqual(len(found), 8, found)
        for line in found:
            self.assertIn('{index="2"}', line)

    def test_v1_rows_without_the_if_name_tag_are_named(self) -> None:
        found = findings(export=good_export(v1_tags=lambda index: {}))
        polls = len(poll_times())
        self.assertEqual(
            found,
            [
                line
                for series in uat.SNMP_V1_INTERFACE_SERIES
                for index in uat.SNMP_INTERFACES
                for line in (
                    f'{series}{{index="{index}"}}: no sample tagged '
                    f'ifName="GigabitEthernet0/0/{int(index) - 1}"',
                    f'{series}{{index="{index}"}}: {polls} sample(s) tagged no ifName, '
                    f'want ifName="GigabitEthernet0/0/{int(index) - 1}"',
                )
            ],
        )

    def test_a_wrong_if_name_is_named(self) -> None:
        tags = lambda index: {"ifName": "Gi9"} if index == "4" else if_name_tag(index)  # noqa: E731
        found = findings(export=good_export(tags=tags))
        self.assertEqual(len(found), 8, found)
        for series in ("interface_ifHCInOctets", "interface_v1_ifInErrors"):
            self.assertIn(
                f'{series}{{index="4"}}: {len(poll_times())} sample(s) tagged '
                'ifName="Gi9", want ifName="GigabitEthernet0/0/3"',
                found,
            )

    def test_the_if_name_check_reads_the_export_it_is_given(self) -> None:
        # Guards the green case: tags read from the wrong label would give no
        # tagged sample at all, which is named rather than passed.
        found = uat.snmp_findings(
            good_log(),
            uat.parse_vm_export(good_export(), label="index"),
            uat.parse_vm_export_tags(good_export(), uat.SNMP_IF_NAME_TAG, label="name"),
            walked(),
            golden(),
            STARTED_S,
            ENDED_S,
            v1_walk=v1_walked(),
            v1_golden=v1_golden(),
            v3_get=uat.SNMP_V3_GET_EXPECTED,
            v3_priv_get=uat.SNMP_V3_GET_EXPECTED,
            restart=RESTART,
        )
        tagged_series = (
            uat.SNMP_INTERFACE_SERIES + uat.SNMP_V1_INTERFACE_SERIES + uat.SNMP_V3_INTERFACE_SERIES
        )
        self.assertEqual(len(found), len(tagged_series) * len(uat.SNMP_INTERFACES))
        for line in found:
            self.assertIn("no sample tagged ifName=", line)

    def test_a_flap_that_never_went_down_is_named(self) -> None:
        found = findings(export=good_export(status=lambda index, at: 1.0))
        self.assertEqual(
            found,
            ['interface_ifOperStatus{index="1"} never took down (2) within the run; saw [1.0]'],
        )

    def test_a_walk_that_differs_is_named_with_the_line(self) -> None:
        walk = walked().replace(".1.3.6.1.2.1.2.2.1.8.4 = INTEGER: 1", ".1.3.6.1.2.1.2.2.1.8.4 = INTEGER: 2")
        found = findings(walk=walk)
        self.assertEqual(len(found), 1, found)
        self.assertTrue(found[0].startswith(f"the in-container walk differs from {uat.SNMP_WALK_GOLDEN}"))
        self.assertIn("-.1.3.6.1.2.1.2.2.1.8.4 = INTEGER: 1", found[0])
        self.assertIn("+.1.3.6.1.2.1.2.2.1.8.4 = INTEGER: 2", found[0])

    def test_a_walk_missing_an_instance_is_named(self) -> None:
        walk = walked().replace(".1.3.6.1.2.1.2.2.1.14.3 = Counter32: 0\n", "")
        found = findings(walk=walk)
        self.assertEqual(len(found), 1, found)
        self.assertIn("-.1.3.6.1.2.1.2.2.1.14.3 = Counter32: 0", found[0])

    def test_an_empty_walk_is_named(self) -> None:
        self.assertEqual(findings(walk="  \n"), ["the in-container walk printed nothing"])

    def test_a_v1_row_lacking_if_oper_status_is_named(self) -> None:
        found = findings(export=good_export(v1_fields=("ifInErrors", "ifOutErrors")))
        self.assertEqual(
            found,
            [
                f'interface_v1_ifOperStatus{{index="{index}"}}: 0 sample(s) within 30s '
                "of the first sample, want >= 3"
                for index in uat.SNMP_INTERFACES
            ],
        )

    def test_a_v1_row_carrying_if_hc_in_octets_is_named(self) -> None:
        found = findings(export=good_export(v1_fields=V1_FIELDS + ("ifHCInOctets",)))
        self.assertEqual(
            found,
            [
                "interface_v1_ifHCInOctets: samples for index 1, 2, 3, 4; SNMPv1 "
                "carries no Counter64, so the agent must skip the column"
            ],
        )

    def test_one_v1_row_carrying_if_hc_out_octets_is_named_by_its_index(self) -> None:
        extra = export_line(
            "interface_v1_ifHCOutOctets", {"index": "2"}, [(FIRST_POLL_S, 5.0)]
        )
        found = findings(export=good_export() + b"\n" + extra.encode())
        self.assertEqual(len(found), 1, found)
        self.assertTrue(found[0].startswith("interface_v1_ifHCOutOctets: samples for index 2;"), found)

    def test_a_v1_walk_that_shows_a_counter64_is_named_with_the_line(self) -> None:
        hc = ".1.3.6.1.2.1.31.1.1.1.6.1 = Counter64: 2750000\n"
        walk = v1_walked().replace("End of MIB\n", hc + "End of MIB\n")
        self.assertIn(hc, walk)
        found = findings(v1_walk=walk)
        self.assertEqual(len(found), 1, found)
        self.assertTrue(
            found[0].startswith(f"the in-container v1 walk differs from {uat.SNMP_V1_WALK_GOLDEN}"),
            found,
        )
        self.assertIn("+.1.3.6.1.2.1.31.1.1.1.6.1 = Counter64: *", found[0])

    def test_an_empty_v1_walk_is_named(self) -> None:
        self.assertEqual(findings(v1_walk=""), ["the in-container v1 walk printed nothing"])

    def test_the_findings_come_in_check_order(self) -> None:
        log = good_log().replace("Loaded inputs: snmp (2x)", "Loaded inputs: cpu") + refused(STARTED_S + 30)
        export = good_export(
            interfaces=("1", "2", "3"),
            uptime=lambda _: 5,
            v1_interfaces=("1", "2", "3"),
            v1_fields=V1_FIELDS + ("ifHCInOctets",),
        )
        found = findings(log=log, export=export, walk="", v1_walk="")
        self.assertEqual(
            [line.split(":")[0].split("{")[0] for line in found],
            [
                "telegraf log has no 'Loaded inputs",
                "telegraf logged an error while sonda served",
                "interface_ifHCInOctets",
                "interface_ifOperStatus",
                "snmp_uptime did not increase",
                "interface_v1_ifOperStatus",
                "interface_v1_ifInErrors",
                "interface_v1_ifHCInOctets",
                "the in-container walk printed nothing",
                "the in-container v1 walk printed nothing",
            ],
        )


class SnmpV3FindingsTests(unittest.TestCase):
    """The v3 input's evidence: its rows, its uptime, its errors and the
    in-container SNMPv3 get. Each red case runs beside the good run, which
    has none (SnmpFindingsTests.test_a_good_run_has_no_findings)."""

    def test_the_good_run_has_v3_rows_and_a_v3_uptime(self) -> None:
        samples = uat.parse_vm_export(good_export(), label="index")
        for series in uat.SNMP_V3_INTERFACE_SERIES:
            for index in uat.SNMP_INTERFACES:
                self.assertTrue(samples.get((series, index)), (series, index))
        self.assertGreaterEqual(len(samples[(uat.SNMP_V3_UPTIME_SERIES, "")]), uat.SNMP_MIN_SAMPLES)
        self.assertEqual(findings(), [])

    def test_a_missing_interface_v3_row_is_named(self) -> None:
        found = findings(export=good_export(v3_interfaces=("1", "2", "3")))
        self.assertEqual(
            found,
            [
                f'{series}{{index="4"}}: 0 sample(s) within 30s of the first sample, want >= 3'
                for series in uat.SNMP_V3_INTERFACE_SERIES
            ],
        )

    def test_no_interface_v3_rows_at_all_are_named(self) -> None:
        found = findings(export=good_export(v3_interfaces=()))
        self.assertEqual(len(found), len(uat.SNMP_V3_INTERFACE_SERIES) * len(uat.SNMP_INTERFACES))
        self.assertTrue(all(line.startswith("interface_v3_") for line in found), found)

    def test_a_v3_row_lacking_if_oper_status_is_named(self) -> None:
        found = findings(export=good_export(v3_fields=("ifHCInOctets", "ifInErrors")))
        self.assertEqual(
            found,
            [
                f'interface_v3_ifOperStatus{{index="{index}"}}: 0 sample(s) within 30s '
                "of the first sample, want >= 3"
                for index in uat.SNMP_INTERFACES
            ],
        )

    def test_v3_rows_without_the_if_name_tag_are_named(self) -> None:
        found = findings(export=good_export(v3_tags=lambda index: {}))
        self.assertEqual(len(found), 2 * len(uat.SNMP_V3_INTERFACE_SERIES) * len(uat.SNMP_INTERFACES))
        self.assertIn(
            'interface_v3_ifOperStatus{index="2"}: no sample tagged ifName="GigabitEthernet0/0/1"',
            found,
        )

    def test_too_few_v3_uptime_samples_are_named(self) -> None:
        lines = [
            line
            for line in good_export().decode().splitlines()
            if uat.SNMP_V3_UPTIME_SERIES not in line
        ]
        self.assertEqual(len(lines), len(good_export().decode().splitlines()) - 1)
        lines.append(export_line(uat.SNMP_V3_UPTIME_SERIES, {}, [(FIRST_POLL_S, 100.0)]))
        self.assertEqual(
            findings(export="\n".join(lines).encode()),
            ["snmp_v3_uptime: 1 sample(s), want >= 3"],
        )

    def test_a_flat_v3_uptime_is_named(self) -> None:
        found = findings(export=good_export(v3_uptime=lambda _: 42))
        self.assertEqual(len(found), 1, found)
        self.assertTrue(found[0].startswith("snmp_v3_uptime did not increase: 42 then 42"), found)

    def test_a_v3_error_before_its_own_first_sample_is_excused(self) -> None:
        export = good_export(v3_from_s=FIRST_POLL_S + 4)
        error = refused_v3(FIRST_POLL_S + 3)
        self.assertEqual(findings(log=good_log() + error, export=export), [])
        # Guard: the same moment is not excused for the v2c input.
        late_v2c = refused(FIRST_POLL_S + 3)
        self.assertEqual(
            findings(log=good_log() + late_v2c, export=export),
            [f"telegraf logged an error while sonda served: {late_v2c.strip()}"],
        )

    def test_a_v3_error_after_its_own_first_sample_is_named(self) -> None:
        export = good_export(v3_from_s=FIRST_POLL_S + 4)
        error = refused_v3(FIRST_POLL_S + 5)
        self.assertEqual(
            findings(log=good_log() + error, export=export),
            [f"telegraf logged an error while sonda served: {error.strip()}"],
        )

    def test_every_v3_series_belongs_to_the_v3_input(self) -> None:
        for series in uat.SNMP_V3_INTERFACE_SERIES + (uat.SNMP_V3_UPTIME_SERIES,):
            self.assertEqual(uat.snmp_input_of(series), uat.SNMP_V3_INPUT, series)
        self.assertEqual(uat.snmp_input_of(uat.SNMP_UPTIME_SERIES), uat.SNMP_V2C_INPUT)

    def test_a_failed_v3_get_is_named(self) -> None:
        failure = "snmpget: Authentication failure (incorrect password, community or key)\n"
        self.assertEqual(
            findings(v3_get=failure),
            [
                "the in-container snmpget -v3 printed 'snmpget: Authentication failure "
                "(incorrect password, community or key)', want "
                "'.1.3.6.1.2.1.1.5.0 = STRING: \"rtr-core-01\"'"
            ],
        )

    def test_a_v3_get_of_another_name_is_named(self) -> None:
        found = findings(v3_get='.1.3.6.1.2.1.1.5.0 = STRING: "rtr-2"\n')
        self.assertEqual(len(found), 1, found)
        self.assertIn("printed '.1.3.6.1.2.1.1.5.0 = STRING: \"rtr-2\"'", found[0])

    def test_an_empty_v3_get_is_named(self) -> None:
        self.assertEqual(findings(v3_get=""), ["the in-container snmpget -v3 printed nothing"])

    def test_the_v3_findings_come_after_the_v1_ones_and_before_the_walks(self) -> None:
        found = findings(
            export=good_export(v1_interfaces=("1", "2", "3"), v3_interfaces=("1", "2", "3")),
            walk="",
            v3_get="",
        )
        self.assertEqual(
            [line.split(":")[0].split("{")[0] for line in found],
            [
                "interface_v1_ifOperStatus",
                "interface_v1_ifInErrors",
                "interface_v3_ifHCInOctets",
                "interface_v3_ifOperStatus",
                "the in-container walk printed nothing",
                "the in-container snmpget -v3 printed nothing",
            ],
        )

class SnmpRestartFindingsTests(unittest.TestCase):
    """The restart's evidence: the series resuming, gosnmp's time-window
    line, no re-discovery, the gap's errors excused and both uptimes
    starting again. Each red case runs beside the good run, which has none."""

    def test_the_good_run_restarts_and_resumes(self) -> None:
        samples = uat.parse_vm_export(good_export(), label="index")
        stopped_ms, restarted_ms = int(STOPPED_S * 1000), int(RESTARTED_S * 1000)
        for (series, index), pairs in samples.items():
            self.assertFalse(
                [ts for ts, _ in pairs if stopped_ms <= ts < restarted_ms], (series, index)
            )
            self.assertTrue([ts for ts, _ in pairs if ts >= restarted_ms], (series, index))
        uptime = dict(samples[(uat.SNMP_V3_UPTIME_SERIES, "")])
        # The polls a second either side of the restart.
        self.assertLess(uptime[stopped_ms + 1000], uptime[stopped_ms - 1000], "it starts again")
        self.assertIn(time_window(RESTARTED_S), good_log())
        self.assertIn(refused(STOPPED_S + 1), good_log())
        self.assertEqual(findings(), [])

    def test_a_v3_series_that_does_not_resume_is_named(self) -> None:
        found = findings(export=good_export(v3_resumed_s=ENDED_S + 1))
        self.assertEqual(
            found,
            [
                f'{series}{{index="{index}"}}: no sample within 30s of sonda starting again'
                for series in uat.SNMP_V3_INTERFACE_SERIES
                for index in uat.SNMP_INTERFACES
            ],
        )

    def test_a_v3_series_resuming_late_is_named(self) -> None:
        # The first poll from here is 30.8 s after the restart.
        found = findings(export=good_export(v3_resumed_s=RESTARTED_S + 30))
        self.assertEqual(len(found), len(uat.SNMP_V3_INTERFACE_SERIES) * len(uat.SNMP_INTERFACES))
        self.assertTrue(all("no sample within 30s of sonda starting again" in line for line in found), found)
        # Guard: resuming at the last poll inside the window, 28.8 s after the
        # restart, is in time.
        self.assertEqual(findings(export=good_export(v3_resumed_s=RESTARTED_S + 28)), [])

    def test_no_time_window_line_is_named(self) -> None:
        log = good_log().replace(time_window(RESTARTED_S), "")
        self.assertEqual(
            findings(log=log),
            [
                f"telegraf's v3 input logged no {uat.SNMP_TIME_WINDOW_LINE!r} after sonda "
                "restarted: gosnmp was not answered usmStatsNotInTimeWindows"
            ],
        )

    def test_a_v3_error_after_the_stop_is_named_with_the_missing_time_window(self) -> None:
        error = refused_v3(STOPPED_S + 1)
        log = good_log().replace(time_window(RESTARTED_S), "") + error
        self.assertEqual(
            findings(log=log),
            [
                f"telegraf's v3 input logged no {uat.SNMP_TIME_WINDOW_LINE!r} after sonda "
                "restarted: gosnmp was not answered usmStatsNotInTimeWindows; the input "
                "errored first, and Telegraf discovers the engine again after an error: "
                f"{error.strip()}"
            ],
        )
        # Guard: a v3 error before the run, excused, is not named with it.
        self.assertEqual(
            findings(log=good_log().replace(time_window(RESTARTED_S), "") + refused_v3(STARTED_S - 600)),
            [
                f"telegraf's v3 input logged no {uat.SNMP_TIME_WINDOW_LINE!r} after sonda "
                "restarted: gosnmp was not answered usmStatsNotInTimeWindows"
            ],
        )

    def test_a_time_window_line_before_the_stop_does_not_count(self) -> None:
        log = good_log().replace(time_window(RESTARTED_S), time_window(STOPPED_S - 10))
        self.assertIn(time_window(STOPPED_S - 10), log)
        self.assertEqual(len(findings(log=log)), 1)

    def test_a_time_window_line_of_another_input_does_not_count(self) -> None:
        other = time_window(RESTARTED_S).replace("inputs.snmp::v3", "inputs.snmp::v2c")
        log = good_log().replace(time_window(RESTARTED_S), other)
        self.assertIn("inputs.snmp::v2c] agent sonda: WARNING", log)
        self.assertEqual(len(findings(log=log)), 1)

    def test_a_rediscovery_after_the_restart_is_named(self) -> None:
        line = telegraf_line(
            RESTARTED_S + 2, f"T! [inputs.snmp::v3] agent sonda: {uat.SNMP_REDISCOVERY_LINE}"
        )
        self.assertEqual(
            findings(log=good_log() + line),
            [f"telegraf's v3 input discovered the engine again after sonda restarted: {line.strip()}"],
        )

    def test_a_failed_restart_is_named_and_excuses_nothing(self) -> None:
        reason = "<the restart failed: sonda exited (0) before the restart>"
        found = findings(restart=reason)
        self.assertIn(f"sonda was not restarted: {reason}", found)
        for error in (refused(STOPPED_S + 1), refused_v1(STOPPED_S + 1)):
            self.assertIn(f"telegraf logged an error while sonda served: {error.strip()}", found)

    def test_an_error_after_its_input_resumed_is_named(self) -> None:
        # The v2c input's first sample after the restart is at STOPPED_S + 1.
        self.assertEqual(findings(log=good_log() + refused(STOPPED_S + 1)), [])
        error = refused(RESTARTED_S + 4)
        self.assertEqual(
            findings(log=good_log() + error),
            [f"telegraf logged an error while sonda served: {error.strip()}"],
        )

    def test_an_error_before_sonda_stopped_is_named(self) -> None:
        self.assertEqual(findings(log=good_log() + refused(STOPPED_S - 1)), [])
        error = refused(STOPPED_S - 2)
        self.assertEqual(
            findings(log=good_log() + error),
            [f"telegraf logged an error while sonda served: {error.strip()}"],
        )

    def test_an_uptime_that_falls_within_one_run_is_named(self) -> None:
        # A poll 13 s after the stop; the one before it read 10.8 s of uptime.
        fallen = STOPPED_S + 13
        found = findings(
            export=good_export(uptime=lambda at: 0 if at == fallen else sys_up_time(at))
        )
        self.assertEqual(len(found), 1, found)
        self.assertTrue(found[0].startswith("snmp_uptime did not increase: 1080 then 0"), found)

    def test_a_failed_priv_get_is_named(self) -> None:
        self.assertEqual(
            findings(v3_priv_get="snmpget: Decryption error\n"),
            [
                "the in-container snmpget -v3 -l authPriv printed 'snmpget: Decryption error', "
                "want '.1.3.6.1.2.1.1.5.0 = STRING: \"rtr-core-01\"'"
            ],
        )


class MaskTests(unittest.TestCase):
    def test_moving_values_are_masked_and_the_rest_kept(self) -> None:
        walk = (
            ".1.3.6.1.2.1.1.3.0 = Timeticks: (8640000) 1 day, 0:00:00.00\n"
            ".1.3.6.1.2.1.31.1.1.1.6.1 = Counter64: 2750000\n"
            ".1.3.6.1.2.1.2.2.1.14.1 = Counter32: 7\n"
            ".1.3.6.1.2.1.2.2.1.8.1 = INTEGER: 2\n"
        )
        self.assertEqual(
            uat.mask_snmp_walk(walk),
            ".1.3.6.1.2.1.1.3.0 = Timeticks: (*)\n"
            ".1.3.6.1.2.1.31.1.1.1.6.1 = Counter64: *\n"
            ".1.3.6.1.2.1.2.2.1.14.1 = Counter32: 7\n"
            ".1.3.6.1.2.1.2.2.1.8.1 = INTEGER: 2\n",
        )


def pack_hint_columns() -> list[str]:
    """The IF-MIB column of each snmp: hint in the telegraf_snmp_interface
    pack, in the pack's order."""
    return [column for column, _ in pack_hints()]


def pack_hints() -> list[tuple[str, str]]:
    """The IF-MIB column and SNMP type of each snmp: hint in the
    telegraf_snmp_interface pack, in the pack's order."""
    pack = (REPO_ROOT / "packs/telegraf-snmp-interface.yaml").read_text()
    return re.findall(r'oid: "([0-9.]+)\.\{ifIndex\}"\s+type: (\w+)', pack)


def oid_order(oid: str) -> list[int]:
    return [int(arc) for arc in oid.strip(".").split(".")]


def pack_label_columns() -> dict[str, str]:
    """The IF-MIB column of each label in the telegraf_snmp_interface pack's
    snmp_label_columns."""
    pack = (REPO_ROOT / "packs/telegraf-snmp-interface.yaml").read_text()
    block = pack.split("\nsnmp_label_columns:\n", 1)[1].split("\n\n", 1)[0]
    return dict(re.findall(r'^  (\w+): "([0-9.]+)\.\{ifIndex\}"$', block, re.MULTILINE))


class GoldenWalkTests(unittest.TestCase):
    def test_the_pack_serves_if_name_and_if_alias(self) -> None:
        self.assertEqual(
            pack_label_columns(),
            {"ifName": "1.3.6.1.2.1.31.1.1.1.1", "ifAlias": "1.3.6.1.2.1.31.1.1.1.18"},
        )

    def test_the_golden_walk_is_the_system_group_and_every_served_column(self) -> None:
        columns = pack_hint_columns() + list(pack_label_columns().values())
        self.assertEqual(len(columns), 7)
        lines = golden().splitlines()
        self.assertEqual(
            lines[-1],
            ".1.3.6.1.2.1.31.1.1.1.18.4 = No more variables left in this MIB "
            "View (It is past the end of the MIB tree)",
        )
        names = [line.split(" = ")[0] for line in lines[:-1]]
        system = [f".1.3.6.1.2.1.1.{n}.0" for n in range(1, 8)]
        instances = sorted(
            (f".{column}.{index}" for column in columns for index in uat.SNMP_INTERFACES),
            key=lambda oid: [int(arc) for arc in oid.strip(".").split(".")],
        )
        self.assertEqual(names, system + instances)

    def test_the_golden_walk_is_masked_and_names_the_example_router(self) -> None:
        text = golden()
        self.assertEqual(uat.mask_snmp_walk(text), text)
        self.assertIn('.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"', text)
        self.assertEqual(text.count("Counter64: *"), 8)
        for index, name in uat.SNMP_IF_NAMES.items():
            self.assertIn(f'.1.3.6.1.2.1.31.1.1.1.1.{index} = STRING: "{name}"', text)

    def test_the_v1_golden_walk_is_the_system_group_and_every_column_but_counter64(self) -> None:
        hints = pack_hints()
        self.assertEqual(len(hints), 5)
        kept = [column for column, kind in hints if kind != "counter64"]
        self.assertEqual(len(kept), 3, hints)
        # Label columns are OCTET STRINGs, which SNMPv1 carries.
        kept += list(pack_label_columns().values())
        lines = v1_golden().splitlines()
        self.assertEqual(lines[-1], "End of MIB")
        names = [line.split(" = ")[0] for line in lines[:-1]]
        system = [f".1.3.6.1.2.1.1.{n}.0" for n in range(1, 8)]
        instances = sorted(
            (f".{column}.{index}" for column in kept for index in uat.SNMP_INTERFACES),
            key=oid_order,
        )
        self.assertEqual(names, system + instances)

    def test_the_v1_golden_walk_is_masked_and_holds_no_counter64(self) -> None:
        text = v1_golden()
        self.assertEqual(uat.mask_snmp_walk(text), text)
        self.assertIn('.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"', text)
        self.assertIn("Timeticks: (*)", text)
        self.assertNotIn("Counter64", text)
        self.assertEqual(
            v1_golden().splitlines()[:7], golden().splitlines()[:7], "the same system group"
        )
        for index, name in uat.SNMP_IF_NAMES.items():
            self.assertIn(f'.1.3.6.1.2.1.31.1.1.1.1.{index} = STRING: "{name}"', text)


def telegraf_conf() -> dict:
    return tomllib.loads((REPO_ROOT / uat.SNMP_TELEGRAF_CONF).read_text())


class TelegrafConfTests(unittest.TestCase):
    def test_telegraf_polls_the_agent_and_tags_its_host_as_source(self) -> None:
        conf = telegraf_conf()
        snmp, _, _ = conf["inputs"]["snmp"]
        self.assertEqual(snmp["agents"], ["udp://sonda:1161"])
        self.assertEqual(snmp["version"], 2)
        self.assertEqual(snmp["community"], "public")
        self.assertEqual(snmp["agent_host_tag"], "source")
        self.assertIn('source="sonda"', uat.SNMP_EXPORT_MATCH)
        self.assertNotIn("translator", snmp)
        self.assertEqual(conf["agent"]["interval"], "2s")

    def test_the_inputs_poll_on_the_clocks_multiples_of_the_restarts_interval(self) -> None:
        conf = telegraf_conf()
        agent = conf["agent"]
        self.assertEqual(agent["interval"], f"{uat.SNMP_POLL_INTERVAL_S:g}s")
        # round_interval is Telegraf's default, and no jitter or offset moves
        # a poll off the multiple.
        self.assertTrue(agent.get("round_interval", True))
        for key in ("collection_jitter", "collection_offset"):
            self.assertEqual(agent.get(key, "0s"), "0s", key)
        self.assertEqual(len(conf["inputs"]["snmp"]), 3)
        for snmp in conf["inputs"]["snmp"]:
            self.assertNotIn("interval", snmp, snmp["alias"])

    def test_every_oid_is_numeric(self) -> None:
        fields = [
            field
            for snmp in telegraf_conf()["inputs"]["snmp"]
            for field in snmp.get("field", []) + [f for table in snmp["table"] for f in table["field"]]
        ]
        self.assertEqual(len(fields), 24)
        for field in fields:
            self.assertRegex(field["oid"], r"^(\.[0-9]+)+$", field["name"])

    def test_each_block_has_the_alias_the_checker_attributes_errors_by(self) -> None:
        v2c, v1, v3 = telegraf_conf()["inputs"]["snmp"]
        self.assertEqual(
            (v2c["alias"], v1["alias"], v3["alias"]),
            (uat.SNMP_V2C_INPUT, uat.SNMP_V1_INPUT, uat.SNMP_V3_INPUT),
        )
        self.assertEqual(v2c["version"], 2)
        (table,) = v1["table"]
        self.assertEqual(f"{table['name']}_", uat.SNMP_V1_SERIES_PREFIX)
        (table,) = v3["table"]
        self.assertEqual(f"{table['name']}_", uat.SNMP_V3_SERIES_PREFIX)
        for alias in (uat.SNMP_V2C_INPUT, uat.SNMP_V1_INPUT, uat.SNMP_V3_INPUT):
            match = uat.SNMP_ERROR_LINE.search(f"E! [inputs.snmp::{alias}] Error in plugin: x")
            self.assertIsNotNone(match, alias)
            self.assertEqual(match.group("alias"), alias)

    def test_the_v1_block_polls_the_same_table_as_v1(self) -> None:
        v2c, v1, _ = telegraf_conf()["inputs"]["snmp"]
        self.assertEqual(v1["version"], 1)
        for key in ("agents", "community", "agent_host_tag"):
            self.assertEqual(v1[key], v2c[key], key)
        self.assertNotIn("translator", v1)
        self.assertNotIn("field", v1, "the v2c block alone writes snmp_uptime")
        (table,) = v1["table"]
        (v2c_table,) = v2c["table"]
        self.assertEqual(table["name"], "interface_v1")
        self.assertIs(table["index_as_tag"], True)
        self.assertEqual(table["field"], v2c_table["field"])
        polled = {f["name"] for f in table["field"]}
        for series in uat.SNMP_V1_INTERFACE_SERIES + uat.SNMP_V1_ABSENT_SERIES:
            self.assertIn(series.removeprefix("interface_v1_"), polled)

    def test_the_interface_table_polls_the_columns_the_pack_hints(self) -> None:
        snmp, _, _ = telegraf_conf()["inputs"]["snmp"]
        (table,) = snmp["table"]
        self.assertEqual(table["name"], "interface")
        self.assertIs(table["index_as_tag"], True)
        polled = {
            f["name"]: f["oid"].lstrip(".") for f in table["field"] if not f.get("is_tag")
        }
        self.assertEqual(sorted(polled.values()), sorted(pack_hint_columns()))
        for series in uat.SNMP_INTERFACE_SERIES:
            self.assertIn(series.removeprefix("interface_"), polled)
        tags = {f["name"]: f["oid"].lstrip(".") for f in table["field"] if f.get("is_tag")}
        self.assertEqual(tags, pack_label_columns())
        self.assertIn(uat.SNMP_IF_NAME_TAG, tags)
        uptime = {f["name"]: f["oid"] for f in snmp["field"]}
        self.assertEqual(uptime["uptime"], ".1.3.6.1.2.1.1.3.0")

    def test_the_v3_block_polls_the_same_table_over_v3_as_the_examples_user(self) -> None:
        v2c, _, v3 = telegraf_conf()["inputs"]["snmp"]
        self.assertEqual(v3["version"], 3)
        self.assertEqual(
            (v3["sec_level"], v3["sec_name"], v3["auth_protocol"], v3["auth_password"]),
            ("authPriv", uat.SNMP_V3_USER, "SHA256", uat.SNMP_V3_AUTH_PASSWORD),
        )
        self.assertEqual(
            (v3["priv_protocol"], v3["priv_password"]), ("AES", uat.SNMP_V3_PRIV_PASSWORD)
        )
        # gosnmp's trace is where the restart's evidence is read from.
        self.assertEqual(v3["log_level"], "trace")
        for key in ("agents", "agent_host_tag", "max_repetitions"):
            self.assertEqual(v3[key], v2c[key], key)
        self.assertNotIn("community", v3)
        self.assertEqual(f"{v3['name']}_uptime", uat.SNMP_V3_UPTIME_SERIES)
        self.assertEqual({f["name"]: f["oid"] for f in v3["field"]}, {"uptime": ".1.3.6.1.2.1.1.3.0"})
        (table,) = v3["table"]
        (v2c_table,) = v2c["table"]
        self.assertEqual(table["name"], "interface_v3")
        self.assertIs(table["index_as_tag"], True)
        self.assertEqual(table["field"], v2c_table["field"])
        polled = {f["name"] for f in table["field"]}
        for series in uat.SNMP_V3_INTERFACE_SERIES:
            self.assertIn(series.removeprefix(uat.SNMP_V3_SERIES_PREFIX), polled)

    def test_the_example_serves_the_v3_users_the_conf_and_the_gets_use(self) -> None:
        import yaml

        example = yaml.safe_load((REPO_ROOT / "examples/snmp-telegraf.yaml").read_text())
        sink = example["defaults"]["sink"]
        self.assertEqual(sink["versions"], ["v1", "v2c", "v3"])
        private, auth_only = sink["users"]
        auth = {"protocol": "sha256", "password": uat.SNMP_V3_AUTH_PASSWORD}
        self.assertEqual(
            private,
            {
                "name": uat.SNMP_V3_USER,
                "auth": auth,
                "priv": {"protocol": "aes128", "password": uat.SNMP_V3_PRIV_PASSWORD},
            },
        )
        self.assertEqual(auth_only, {"name": uat.SNMP_V3_AUTH_ONLY_USER, "auth": auth})
        self.assertEqual(sink["system"]["name"], "rtr-core-01")
        self.assertIn('STRING: "rtr-core-01"', uat.SNMP_V3_GET_EXPECTED)
        command = uat.SNMP_V3_GET_COMMAND
        self.assertEqual(
            (
                command[command.index("-l") + 1],
                command[command.index("-u") + 1],
                command[command.index("-a") + 1],
                command[command.index("-A") + 1],
            ),
            ("authNoPriv", uat.SNMP_V3_AUTH_ONLY_USER, "SHA-256", uat.SNMP_V3_AUTH_PASSWORD),
        )
        self.assertNotIn("-x", command)
        command = uat.SNMP_V3_PRIV_GET_COMMAND
        self.assertEqual(
            (
                command[command.index("-l") + 1],
                command[command.index("-u") + 1],
                command[command.index("-a") + 1],
                command[command.index("-A") + 1],
                command[command.index("-x") + 1],
                command[command.index("-X") + 1],
            ),
            (
                "authPriv",
                uat.SNMP_V3_USER,
                "SHA-256",
                uat.SNMP_V3_AUTH_PASSWORD,
                "AES",
                uat.SNMP_V3_PRIV_PASSWORD,
            ),
        )

    def test_the_health_output_is_the_backend_the_harness_waits_for(self) -> None:
        (health,) = telegraf_conf()["outputs"]["health"]
        port = health["service_address"].rsplit(":", 1)[1]
        (backend,) = uat.BACKENDS_BY_PROFILE["snmp"]
        self.assertEqual(backend.name, "telegraf-snmp")
        self.assertEqual(backend.health_url, f"http://localhost:{port}/")


class MatrixRowTests(unittest.TestCase):
    def test_the_snmp_row_walks_the_agent_and_verifies_its_evidence(self) -> None:
        rows = {row.name: row for row in uat.MATRIX}
        row = rows["snmp"]
        self.assertEqual(row.profiles, ("snmp",))
        self.assertTrue((REPO_ROOT / row.scenario).is_file())
        self.assertEqual(row.scenario, Path("examples/snmp-telegraf.yaml"))
        self.assertIs(row.during, uat.snmp_during)
        self.assertTrue(row.during_gets_sonda)
        self.assertIs(row.post_verify, uat.verify_snmp_row)
        self.assertEqual(row.failure_log_containers[0], "telegraf-snmp")
        self.assertEqual(row.extra_args, ("--duration", "210s"))
        # The restart is past the 150 s time window by more than two polls
        # (the engine time gosnmp keeps is up to a poll old at the stop, and
        # the restarted agent's clock up to a poll on at its first request);
        # it lands between two polls; it leaves the series time to resume
        # before the run ends.
        self.assertGreater(uat.SNMP_RESTART_AT_S - 2 * uat.SNMP_POLL_INTERVAL_S, 150.0)
        self.assertLess(0.0, uat.SNMP_RESTART_PHASE_S)
        self.assertLess(uat.SNMP_RESTART_PHASE_S, uat.SNMP_POLL_INTERVAL_S)
        self.assertGreaterEqual(
            uat.SNMP_RUN_S,
            uat.SNMP_RESTART_AT_S + uat.SNMP_POLL_INTERVAL_S + uat.SNMP_RESUME_WITHIN_S,
        )
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(row.verify_url).query)
        self.assertEqual(query["query"], [uat.SNMP_EXPORT_MATCH])

    def test_only_the_snmp_row_has_a_during_hook(self) -> None:
        self.assertEqual([row.name for row in uat.MATRIX if row.during], ["snmp"])


class VerifySnmpRowTests(unittest.TestCase):
    def test_it_reads_the_log_the_export_and_the_golden_file(self) -> None:
        asked: list[str] = []

        def http_get(url: str, timeout_s: float = 0) -> tuple[int, bytes]:
            asked.append(url)
            return 200, good_export()

        with mock.patch.object(uat, "http_get", http_get), mock.patch.object(
            uat, "compose_logs_tail", return_value=good_log()
        ) as logs:
            self.assertEqual(
                uat.verify_snmp_row(
                    REPO_ROOT,
                    STARTED_S,
                    ENDED_S,
                    during=uat.SnmpEvidence(
                        walks=uat.SnmpWalks(
                            v2c=walked(),
                            v1=v1_walked(),
                            v3_get=uat.SNMP_V3_GET_EXPECTED,
                            v3_priv_get=uat.SNMP_V3_GET_EXPECTED,
                        ),
                        restart=RESTART,
                    ),
                ),
                "",
            )
        self.assertEqual(logs.call_args.args[1], "telegraf-snmp")
        self.assertIsNone(logs.call_args.kwargs["tail_lines"])
        (url,) = asked
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
        self.assertEqual(query["match[]"], [uat.SNMP_EXPORT_MATCH])
        self.assertEqual(query["start"], [f"{STARTED_S:.3f}"])
        self.assertEqual(query["end"], [f"{ENDED_S:.3f}"])

    def test_without_the_walk_it_is_red(self) -> None:
        with mock.patch.object(uat, "http_get", return_value=(200, good_export())), mock.patch.object(
            uat, "compose_logs_tail", return_value=good_log()
        ):
            failure = uat.verify_snmp_row(REPO_ROOT, STARTED_S, ENDED_S)
        # Without the hook there is no restart either, so the errors while
        # sonda was stopped and the uptimes starting again are named too.
        self.assertEqual(
            failure.splitlines()[-5:],
            [
                "sonda was not restarted: the during hook reported no restart",
                "the in-container walk printed nothing",
                "the in-container v1 walk printed nothing",
                "the in-container snmpget -v3 printed nothing",
                "the in-container snmpget -v3 -l authPriv printed nothing",
            ],
        )

    def test_a_failed_hook_stands_for_the_walks_the_gets_and_the_restart(self) -> None:
        with mock.patch.object(uat, "http_get", return_value=(200, good_export())), mock.patch.object(
            uat, "compose_logs_tail", return_value=good_log()
        ):
            failure = uat.verify_snmp_row(
                REPO_ROOT, STARTED_S, ENDED_S, during="<the during hook raised boom>"
            )
        found = [line for line in failure.splitlines() if line.startswith("the in-container")]
        self.assertEqual(
            [line.split(" from ")[0].split(" printed ")[0] for line in found],
            [
                "the in-container walk differs",
                "the in-container v1 walk differs",
                "the in-container snmpget -v3",
                "the in-container snmpget -v3 -l authPriv",
            ],
            failure,
        )
        self.assertIn("+<the during hook raised boom>", failure)
        self.assertIn("sonda was not restarted: <the during hook raised boom>", failure.splitlines())
        self.assertEqual(
            failure.splitlines()[-1],
            "the in-container snmpget -v3 -l authPriv printed '<the during hook raised boom>', "
            "want '.1.3.6.1.2.1.1.5.0 = STRING: \"rtr-core-01\"'",
        )

    def test_an_export_error_is_reported(self) -> None:
        with mock.patch.object(uat, "http_get", return_value=(503, b"busy")), mock.patch.object(
            uat, "compose_logs_tail", return_value=good_log()
        ):
            self.assertEqual(
                uat.verify_snmp_row(REPO_ROOT, STARTED_S, ENDED_S, during=walked()),
                "VictoriaMetrics export answered 503: b'busy'",
            )

    def test_a_missing_golden_file_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as root, mock.patch.object(
            uat, "http_get", return_value=(200, good_export())
        ), mock.patch.object(uat, "compose_logs_tail", return_value=good_log()):
            failure = uat.verify_snmp_row(Path(root), STARTED_S, ENDED_S, during=walked())
        self.assertTrue(failure.startswith(f"cannot read {uat.SNMP_WALK_GOLDEN}"), failure)

    def test_a_missing_v1_golden_file_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as root, mock.patch.object(
            uat, "http_get", return_value=(200, good_export())
        ), mock.patch.object(uat, "compose_logs_tail", return_value=good_log()):
            golden_path = Path(root) / uat.SNMP_WALK_GOLDEN
            golden_path.parent.mkdir(parents=True)
            golden_path.write_text(golden())
            failure = uat.verify_snmp_row(Path(root), STARTED_S, ENDED_S, during=walked())
        self.assertTrue(failure.startswith(f"cannot read {uat.SNMP_V1_WALK_GOLDEN}"), failure)


class Clock:
    """A fake monotonic clock that sleep() advances."""

    def __init__(self) -> None:
        self.now_s = 0.0
        self.slept: list[float] = []

    def now(self) -> float:
        return self.now_s

    def sleep(self, seconds: float) -> None:
        self.slept.append(seconds)
        self.now_s += seconds


class WalkSnmpAgentTests(unittest.TestCase):
    def runner(self, results: list[tuple[int, str, str]], argvs: list[list[str]]):
        def run(argv, **_):
            argvs.append(argv)
            code, out, err = results.pop(0) if len(results) > 1 else results[0]
            return subprocess.CompletedProcess(argv, code, out, err)

        return run

    def test_it_walks_inside_the_telegraf_snmp_service_without_mibs(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        walk = uat.walk_snmp_agent(
            REPO_ROOT,
            run=self.runner([(0, walked(), "")], argvs),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walk, walked())
        (argv,) = argvs
        self.assertEqual(argv[:3], ["docker", "compose", "-f"])
        self.assertEqual(
            argv[4:],
            [
                "exec", "-T", "-e", "MIBS=", "telegraf-snmp",
                "snmpbulkwalk", "-v2c", "-c", "public", "-On", "sonda:1161", "1.3.6.1.2.1",
            ],
        )

    def test_it_retries_until_a_walk_matches_the_golden(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        system_only = "".join(golden().splitlines(keepends=True)[:7])
        walk = uat.walk_snmp_agent(
            REPO_ROOT,
            run=self.runner(
                [(1, "", "Timeout: No Response from sonda:1161\n"), (0, system_only, ""), (0, walked(), "")],
                argvs,
            ),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walk, walked())
        self.assertEqual(len(argvs), 3)
        self.assertEqual(clock.slept, [1.0, 1.0])

    def test_it_returns_the_last_attempt_at_the_deadline(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        walk = uat.walk_snmp_agent(
            REPO_ROOT,
            within_s=5.0,
            run=self.runner([(1, "", "Timeout: No Response from sonda:1161\n")], argvs),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walk, "Timeout: No Response from sonda:1161\n")
        self.assertEqual(len(argvs), 6)

    def test_a_walk_that_never_matches_is_returned_for_the_diff(self) -> None:
        clock = Clock()
        partial = "".join(golden().splitlines(keepends=True)[:7])
        walk = uat.walk_snmp_agent(
            REPO_ROOT,
            within_s=3.0,
            run=self.runner([(0, partial, "")], []),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walk, partial)

    def test_without_a_golden_file_the_first_walk_is_taken(self) -> None:
        clock = Clock()
        with tempfile.TemporaryDirectory() as root:
            walk = uat.walk_snmp_agent(
                Path(root),
                run=self.runner([(0, "partial\n", "")], []),
                now=clock.now,
                sleep=clock.sleep,
            )
        self.assertEqual(walk, "partial\n")
        self.assertEqual(clock.slept, [])


class RunRowDuringTests(unittest.TestCase):
    def row(self, during, post_verify) -> uat.MatrixRow:
        return uat.MatrixRow(
            name="t",
            profiles=(),
            subcommand="metrics",
            scenario=Path("scenario.yaml"),
            verify_url="",
            failure_log_containers=("x",),
            during=during,
            post_verify=post_verify,
        )

    def run_row(self, row: uat.MatrixRow, run_s: float = 0.05) -> uat.RowResult:
        def run_scenario(*_, **__):
            time.sleep(run_s)
            return subprocess.CompletedProcess([], 0, "", "")

        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "scenario.yaml").write_text("")
            with mock.patch.object(uat, "run_scenario", run_scenario):
                return uat.run_row(row, Path("sonda"), Path(root))

    def test_the_hook_runs_beside_sonda_and_its_output_reaches_post_verify(self) -> None:
        seen: dict = {}

        def during(_root: Path) -> str:
            time.sleep(0.5)
            return "walked"

        def post_verify(_root, started_s, ended_s, during):
            seen.update(started_s=started_s, ended_s=ended_s, during=during)
            return ""

        result = self.run_row(self.row(during, post_verify))
        self.assertTrue(result.ok, result.message)
        self.assertEqual(seen["during"], "walked")
        # The window ends when sonda does, not when the hook does.
        self.assertLess(seen["ended_s"] - seen["started_s"], 0.4)

    def test_a_hook_that_raises_is_reported_through_post_verify(self) -> None:
        def during(_root: Path) -> str:
            raise RuntimeError("boom")

        result = self.run_row(self.row(during, lambda *_, during: during))
        self.assertFalse(result.ok)
        self.assertIn("<the during hook raised RuntimeError('boom')>", result.message)

    def test_a_row_without_a_hook_calls_post_verify_with_the_window_only(self) -> None:
        calls: list[tuple] = []
        result = self.run_row(self.row(None, lambda *args: calls.append(args) or ""))
        self.assertTrue(result.ok, result.message)
        self.assertEqual(len(calls), 1)
        self.assertEqual(len(calls[0]), 3)


# A stand-in for `sonda run`: prints its argv, then sleeps for its
# --duration, or exits FAKE_SIGINT_EXIT (0) on SIGINT.
FAKE_SONDA = f"""#!{sys.executable}
import os, signal, sys, time
print("argv", " ".join(sys.argv[1:]), flush=True)
duration = float(sys.argv[sys.argv.index("--duration") + 1].rstrip("s"))
def stop(*_):
    print("stopped", flush=True)
    sys.exit(int(os.environ.get("FAKE_SIGINT_EXIT", "0")))
signal.signal(signal.SIGINT, stop)
time.sleep(duration)
print("done", flush=True)
"""


class FakeSondaCase(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        self.sonda = self.root / "sonda"
        self.sonda.write_text(FAKE_SONDA)
        self.sonda.chmod(0o755)

    def tearDown(self) -> None:
        import shutil

        shutil.rmtree(self.root)

    def run_of(self, duration_s: str) -> uat.SondaRun:
        return uat.SondaRun(
            [str(self.sonda), "run", "scenario.yaml", "--duration", duration_s], self.root
        )


class SondaRunTests(FakeSondaCase):
    def test_a_run_without_a_restart_is_waited_for(self) -> None:
        run = self.run_of("1s")
        run.start()
        done = run.wait(10.0)
        self.assertEqual(done.returncode, 0)
        self.assertEqual(done.stdout, "argv run scenario.yaml --duration 1s\ndone\n")

    def test_a_restart_stops_the_run_and_runs_what_is_left(self) -> None:
        run = self.run_of("4s")
        run.start()
        time.sleep(0.5)
        wall = time.time()
        restart = run.restart(0.2, 4.0)
        self.assertLessEqual(wall, restart.stopped_s)
        self.assertGreaterEqual(restart.restarted_s - restart.stopped_s, 0.2)
        done = run.wait(10.0)
        self.assertEqual(done.returncode, 0)
        # 4 s less the 0.7 s before the second run: 3.3 s, printed whole.
        self.assertEqual(
            done.stdout,
            "argv run scenario.yaml --duration 4s\nstopped\n"
            "argv run scenario.yaml --duration 3s\ndone\n",
        )

    def test_a_restart_after_the_run_exited_raises_and_starts_nothing(self) -> None:
        run = self.run_of("0.1s")
        run.start()
        run.wait(10.0)
        with self.assertRaisesRegex(RuntimeError, r"sonda exited \(0\) before the restart"):
            run.restart(0.0, 10.0)
        self.assertEqual(run.wait(1.0).stdout, "argv run scenario.yaml --duration 0.1s\ndone\n")

    def test_a_run_that_does_not_exit_0_on_sigint_fails_the_restart(self) -> None:
        run = self.run_of("10s")
        with mock.patch.dict("os.environ", {"FAKE_SIGINT_EXIT": "3"}):
            run.start()
        time.sleep(0.3)
        with self.assertRaisesRegex(RuntimeError, "sonda exited 3 on SIGINT"):
            run.restart(0.0, 10.0)

    def test_a_run_past_its_timeout_is_killed(self) -> None:
        run = self.run_of("30s")
        run.start()
        with self.assertRaises(subprocess.TimeoutExpired):
            run.wait(0.5)
        self.assertIsNotNone(run._proc.poll(), "killed")


class FakeRun:
    """A SondaRun stand-in that records its restart on a fake clock."""

    def __init__(self, clock: "Clock", fails: str = "") -> None:
        self.started = 0.0
        self.clock = clock
        self.fails = fails
        self.restarts: list[tuple[float, float, float]] = []

    def restart(self, down_s: float, run_s: float) -> uat.SnmpRestart:
        if self.fails:
            raise RuntimeError(self.fails)
        self.restarts.append((self.clock.now_s, down_s, run_s))
        return RESTART


class SnmpDuringTests(unittest.TestCase):
    def runner(self):
        results = [
            (0, walked(), ""),
            (0, v1_walked(), ""),
            (0, uat.SNMP_V3_GET_EXPECTED, ""),
            (0, uat.SNMP_V3_GET_EXPECTED, ""),
        ]

        def run(argv, **_):
            code, out, err = results.pop(0)
            return subprocess.CompletedProcess(argv, code, out, err)

        return run

    def test_it_walks_then_restarts_sonda_between_two_polls_at_its_time(self) -> None:
        # Where the wall clock is in the poll cycle when restart_at_s have
        # passed, and how long the hook then waits to restart sonda at once.
        for phase_s, wait_s in ((0.25, 0.75), (1.0, 0.0), (1.5, 1.5)):
            with self.subTest(phase_s=phase_s):
                clock = Clock()
                sonda = FakeRun(clock)
                evidence = uat.snmp_during(
                    REPO_ROOT,
                    sonda=sonda,
                    run=self.runner(),
                    now=clock.now,
                    wall=lambda: STARTED_S - uat.SNMP_RESTART_AT_S + phase_s + clock.now_s,
                    sleep=clock.sleep,
                )
                self.assertEqual(evidence.walks.v3_priv_get, uat.SNMP_V3_GET_EXPECTED)
                self.assertEqual(evidence.restart, RESTART)
                at_s = uat.SNMP_RESTART_AT_S + wait_s
                self.assertEqual(sonda.restarts, [(at_s, 0.0, uat.SNMP_RUN_S)])
                wall_s = STARTED_S - uat.SNMP_RESTART_AT_S + phase_s + at_s
                self.assertEqual(wall_s % uat.SNMP_POLL_INTERVAL_S, uat.SNMP_RESTART_PHASE_S)

    def test_a_failed_restart_is_returned_as_its_reason(self) -> None:
        clock = Clock()
        evidence = uat.snmp_during(
            REPO_ROOT,
            sonda=FakeRun(clock, fails="sonda exited 3 on SIGINT"),
            run=self.runner(),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(evidence.restart, "<the restart failed: sonda exited 3 on SIGINT>")
        self.assertEqual(evidence.walks.v2c, walked())


class RunRowRestartTests(FakeSondaCase):
    def test_the_hook_gets_the_running_sonda_and_the_window_spans_both_runs(self) -> None:
        seen: dict = {}

        def during(_root: Path, *, sonda: uat.SondaRun) -> uat.SnmpRestart:
            time.sleep(0.3)
            return sonda.restart(0.2, 1.5)

        def post_verify(_root, started_s, ended_s, during):
            seen.update(started_s=started_s, ended_s=ended_s, during=during)
            return ""

        (self.root / "scenario.yaml").write_text("")
        row = uat.MatrixRow(
            name="t",
            profiles=(),
            subcommand="metrics",
            scenario=Path("scenario.yaml"),
            verify_url="",
            failure_log_containers=("x",),
            extra_args=("--duration", "1.5s"),
            during=during,
            during_gets_sonda=True,
            post_verify=post_verify,
        )
        result = uat.run_row(row, self.sonda, self.root)
        self.assertTrue(result.ok, result.message)
        restart = seen["during"]
        self.assertIsInstance(restart, uat.SnmpRestart)
        self.assertLess(seen["started_s"], restart.stopped_s)
        # The window ends when the second run does: 1.5 s after the first
        # began, give or take the integer --duration of the second.
        self.assertGreater(seen["ended_s"], restart.restarted_s)

    def test_a_row_whose_sonda_cannot_start_is_reported(self) -> None:
        (self.root / "scenario.yaml").write_text("")
        row = uat.MatrixRow(
            name="t",
            profiles=(),
            subcommand="metrics",
            scenario=Path("scenario.yaml"),
            verify_url="",
            failure_log_containers=("x",),
            extra_args=("--duration", "1s"),
            during=lambda _root, *, sonda: None,
            during_gets_sonda=True,
        )
        result = uat.run_row(row, self.root / "missing", self.root)
        self.assertFalse(result.ok)
        self.assertIn("sonda binary not found", result.message)


class WalkSnmpVersionsTests(unittest.TestCase):
    def runner(self, results: list[tuple[int, str, str]], argvs: list[list[str]], clock=None):
        def run(argv, **_):
            argvs.append(argv)
            if clock is not None:
                clock.now_s += 4.0
            code, out, err = results.pop(0) if len(results) > 1 else results[0]
            return subprocess.CompletedProcess(argv, code, out, err)

        return run

    def walks(self, **kwargs) -> uat.SnmpWalks:
        return uat.SnmpWalks(
            **{
                "v2c": walked(),
                "v1": v1_walked(),
                "v3_get": uat.SNMP_V3_GET_EXPECTED,
                "v3_priv_get": uat.SNMP_V3_GET_EXPECTED,
                **kwargs,
            }
        )

    def test_it_walks_over_v2c_then_v1_then_gets_over_v3_inside_the_telegraf_snmp_service(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        walks = uat.walk_snmp_versions(
            REPO_ROOT,
            run=self.runner(
                [
                    (0, walked(), ""),
                    (0, v1_walked(), ""),
                    (0, uat.SNMP_V3_GET_EXPECTED, ""),
                    (0, uat.SNMP_V3_GET_EXPECTED, ""),
                ],
                argvs,
            ),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walks, self.walks())
        self.assertEqual([argv[4:9] for argv in argvs], [["exec", "-T", "-e", "MIBS=", "telegraf-snmp"]] * 4)
        self.assertEqual(
            [argv[9:] for argv in argvs],
            [
                ["snmpbulkwalk", "-v2c", "-c", "public", "-On", "sonda:1161", "1.3.6.1.2.1"],
                ["snmpwalk", "-v1", "-c", "public", "-On", "sonda:1161", "1.3.6.1.2.1"],
                [
                    "snmpget", "-v3", "-l", "authNoPriv", "-u", "monitor", "-a", "SHA-256",
                    "-A", "lab-only-v3-password", "-On", "sonda:1161", "1.3.6.1.2.1.1.5.0",
                ],
                [
                    "snmpget", "-v3", "-l", "authPriv", "-u", "telegraf", "-a", "SHA-256",
                    "-A", "lab-only-v3-password", "-x", "AES", "-X", "lab-only-v3-privacy",
                    "-On", "sonda:1161", "1.3.6.1.2.1.1.5.0",
                ],
            ],
        )
        self.assertEqual(clock.slept, [])

    def test_the_v1_walk_retries_until_it_matches_its_own_golden(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        walks = uat.walk_snmp_versions(
            REPO_ROOT,
            run=self.runner(
                [
                    (0, walked(), ""),
                    (0, walked(), ""),
                    (0, v1_walked(), ""),
                    (0, uat.SNMP_V3_GET_EXPECTED, ""),
                ],
                argvs,
            ),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walks.v1, v1_walked())
        self.assertEqual(len(argvs), 5)
        self.assertEqual(clock.slept, [1.0])

    def test_each_v3_get_retries_until_it_prints_the_system_name(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        failure = "snmpget: Authentication failure (incorrect password, community or key)\n"
        walks = uat.walk_snmp_versions(
            REPO_ROOT,
            run=self.runner(
                [
                    (0, walked(), ""),
                    (0, v1_walked(), ""),
                    (1, "", failure),
                    (0, '.1.3.6.1.2.1.1.5.0 = STRING: ""\n', ""),
                    (0, uat.SNMP_V3_GET_EXPECTED, ""),
                    (1, "", "snmpget: Decryption error\n"),
                    (0, uat.SNMP_V3_GET_EXPECTED, ""),
                ],
                argvs,
            ),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walks, self.walks())
        self.assertEqual(
            [(argv[9], argv[12]) for argv in argvs],
            [("snmpbulkwalk", "public"), ("snmpwalk", "public")]
            + [("snmpget", "authNoPriv")] * 3
            + [("snmpget", "authPriv")] * 2,
        )
        self.assertEqual(clock.slept, [1.0, 1.0, 1.0])

    def test_all_four_share_one_window(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        timeout = "Timeout: No Response from sonda:1161\n"
        walks = uat.walk_snmp_versions(
            REPO_ROOT,
            within_s=10.0,
            run=self.runner([(1, "", timeout)], argvs, clock),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(
            walks, uat.SnmpWalks(v2c=timeout, v1=timeout, v3_get=timeout, v3_priv_get=timeout)
        )
        # Each attempt takes 4 s and each retry waits 1 s: the v2c walk
        # attempts at 0, 5 and 10 s and gives up at 14 s, past the window,
        # so the v1 walk and the two v3 gets each have the one-second floor
        # and give up after their first attempt, at 18, 22 and 26 s.
        self.assertEqual(
            [argv[9] for argv in argvs], ["snmpbulkwalk"] * 3 + ["snmpwalk", "snmpget", "snmpget"]
        )
        self.assertEqual(clock.now_s, 26.0)


if __name__ == "__main__":
    unittest.main()
