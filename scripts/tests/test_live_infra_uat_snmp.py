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


def refused_v1(at_s: float) -> str:
    """An error of the v1 input, as Telegraf 1.40.2 logs it."""
    return telegraf_line(
        at_s,
        "E! [inputs.snmp::v1] Error in plugin: agent udp://sonda:1161: gathering table "
        "interface_v1: performing bulk walk for field ifOperStatus: error reading from "
        "socket: read udp 172.18.0.3:35909->172.17.0.1:1161: recvfrom: connection refused",
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
    minutes before the run, in the second of the first sample, and in the
    second the run ended), and nothing else."""
    return (
        telegraf_line(STARTED_S - 600, "I! Loading config: /etc/telegraf/telegraf.conf")
        + telegraf_line(STARTED_S - 600, "I! Loaded inputs: snmp (2x)")
        + telegraf_line(STARTED_S - 600, "I! Loaded outputs: health influxdb")
        + refused(STARTED_S - 600)
        + refused_v1(STARTED_S - 600)
        + refused(FIRST_POLL_S)
        + refused_v1(FIRST_POLL_S)
        + refused(ENDED_S)
        + refused_v1(ENDED_S)
    )


def poll_times(
    first_s: float = FIRST_POLL_S, last_s: float = ENDED_S - 1
) -> list[float]:
    times, at = [], first_s
    while at <= last_s:
        times.append(at)
        at += POLL_S
    return times


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
    uptime=lambda at_s: round((at_s - STARTED_S) * 100),
    status=oper_status,
    v1_interfaces: tuple[str, ...] = uat.SNMP_INTERFACES,
    v1_fields: tuple[str, ...] = V1_FIELDS,
    v2c_from_s: float = FIRST_POLL_S,
    tags=if_name_tag,
    v1_tags=None,
) -> bytes:
    """What VictoriaMetrics exports for a run in which Telegraf read every
    poll: each field of each of ``interfaces``, each of ``v1_fields`` of each
    of ``v1_interfaces`` in the interface_v1 table, every row tagged with
    ``tags`` of its interface (``v1_tags`` in the interface_v1 table, when
    given), and snmp_uptime. The v2c input's series (all but interface_v1)
    start at ``v2c_from_s``."""
    times = poll_times()
    v2c_times = poll_times(first_s=v2c_from_s)
    lines = []
    for table, indices, fields, polls, tagged in (
        ("interface", interfaces, FIELDS, v2c_times, tags),
        ("interface_v1", v1_interfaces, v1_fields, times, v1_tags or tags),
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
            len(uat.SNMP_INTERFACES) * (len(FIELDS) + len(V1_FIELDS)) + 1,
        )
        self.assertIn((uat.SNMP_UPTIME_SERIES, ""), samples)
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
        self.assertEqual(len(found), 4, found)
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
        )
        self.assertEqual(
            len(found),
            len(uat.SNMP_INTERFACE_SERIES + uat.SNMP_V1_INTERFACE_SERIES) * len(uat.SNMP_INTERFACES),
        )
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
        snmp, _ = conf["inputs"]["snmp"]
        self.assertEqual(snmp["agents"], ["udp://sonda:1161"])
        self.assertEqual(snmp["version"], 2)
        self.assertEqual(snmp["community"], "public")
        self.assertEqual(snmp["agent_host_tag"], "source")
        self.assertIn('source="sonda"', uat.SNMP_EXPORT_MATCH)
        self.assertNotIn("translator", snmp)
        self.assertEqual(conf["agent"]["interval"], "2s")

    def test_every_oid_is_numeric(self) -> None:
        fields = [
            field
            for snmp in telegraf_conf()["inputs"]["snmp"]
            for field in snmp.get("field", []) + [f for table in snmp["table"] for f in table["field"]]
        ]
        self.assertEqual(len(fields), 16)
        for field in fields:
            self.assertRegex(field["oid"], r"^(\.[0-9]+)+$", field["name"])

    def test_each_block_has_the_alias_the_checker_attributes_errors_by(self) -> None:
        v2c, v1 = telegraf_conf()["inputs"]["snmp"]
        self.assertEqual((v2c["alias"], v1["alias"]), (uat.SNMP_V2C_INPUT, uat.SNMP_V1_INPUT))
        self.assertEqual(v2c["version"], 2)
        (table,) = v1["table"]
        self.assertEqual(f"{table['name']}_", uat.SNMP_V1_SERIES_PREFIX)
        for alias in (uat.SNMP_V2C_INPUT, uat.SNMP_V1_INPUT):
            match = uat.SNMP_ERROR_LINE.search(f"E! [inputs.snmp::{alias}] Error in plugin: x")
            self.assertIsNotNone(match, alias)
            self.assertEqual(match.group("alias"), alias)

    def test_the_v1_block_polls_the_same_table_as_v1(self) -> None:
        v2c, v1 = telegraf_conf()["inputs"]["snmp"]
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
        snmp, _ = telegraf_conf()["inputs"]["snmp"]
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
        self.assertIs(row.during, uat.walk_snmp_versions)
        self.assertIs(row.post_verify, uat.verify_snmp_row)
        self.assertEqual(row.failure_log_containers[0], "telegraf-snmp")
        self.assertEqual(row.extra_args, ("--duration", "90s"))
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
                    during=uat.SnmpWalks(v2c=walked(), v1=v1_walked()),
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
            self.assertEqual(
                uat.verify_snmp_row(REPO_ROOT, STARTED_S, ENDED_S),
                "the in-container walk printed nothing\n"
                "the in-container v1 walk printed nothing",
            )

    def test_a_failed_hook_stands_for_both_walks(self) -> None:
        with mock.patch.object(uat, "http_get", return_value=(200, good_export())), mock.patch.object(
            uat, "compose_logs_tail", return_value=good_log()
        ):
            failure = uat.verify_snmp_row(
                REPO_ROOT, STARTED_S, ENDED_S, during="<the during hook raised boom>"
            )
        found = [line for line in failure.splitlines() if line.startswith("the in-container")]
        self.assertEqual(
            [line.split(" from ")[0] for line in found],
            ["the in-container walk differs", "the in-container v1 walk differs"],
            failure,
        )
        self.assertIn("+<the during hook raised boom>", failure)

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


class WalkSnmpVersionsTests(unittest.TestCase):
    def runner(self, results: list[tuple[int, str, str]], argvs: list[list[str]], clock=None):
        def run(argv, **_):
            argvs.append(argv)
            if clock is not None:
                clock.now_s += 4.0
            code, out, err = results.pop(0) if len(results) > 1 else results[0]
            return subprocess.CompletedProcess(argv, code, out, err)

        return run

    def test_it_walks_over_v2c_then_v1_inside_the_telegraf_snmp_service(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        walks = uat.walk_snmp_versions(
            REPO_ROOT,
            run=self.runner([(0, walked(), ""), (0, v1_walked(), "")], argvs),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walks, uat.SnmpWalks(v2c=walked(), v1=v1_walked()))
        self.assertEqual([argv[4:9] for argv in argvs], [["exec", "-T", "-e", "MIBS=", "telegraf-snmp"]] * 2)
        self.assertEqual(
            [argv[9:] for argv in argvs],
            [
                ["snmpbulkwalk", "-v2c", "-c", "public", "-On", "sonda:1161", "1.3.6.1.2.1"],
                ["snmpwalk", "-v1", "-c", "public", "-On", "sonda:1161", "1.3.6.1.2.1"],
            ],
        )
        self.assertEqual(clock.slept, [])

    def test_the_v1_walk_retries_until_it_matches_its_own_golden(self) -> None:
        argvs: list[list[str]] = []
        clock = Clock()
        walks = uat.walk_snmp_versions(
            REPO_ROOT,
            run=self.runner([(0, walked(), ""), (0, walked(), ""), (0, v1_walked(), "")], argvs),
            now=clock.now,
            sleep=clock.sleep,
        )
        self.assertEqual(walks.v1, v1_walked())
        self.assertEqual(len(argvs), 3)
        self.assertEqual(clock.slept, [1.0])

    def test_both_walks_share_one_window(self) -> None:
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
        self.assertEqual(walks, uat.SnmpWalks(v2c=timeout, v1=timeout))
        # Each attempt takes 4 s and each retry waits 1 s: the v2c walk
        # attempts at 0, 5 and 10 s and gives up at 14 s, past the window,
        # so the v1 walk has its one-second floor and gives up after its
        # first attempt, at 18 s.
        self.assertEqual([argv[9] for argv in argvs], ["snmpbulkwalk"] * 3 + ["snmpwalk"])
        self.assertEqual(clock.now_s, 18.0)


if __name__ == "__main__":
    unittest.main()
