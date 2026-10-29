"""Tests for the snmp-if-mib row of scripts/live_infra_uat.py.

Each test drives the harness's own functions: if_mib_findings over a
synthetic VictoriaMetrics export (through parse_vm_export) and a synthetic
Telegraf log, if_mib_during and verify_if_mib_row with their I/O replaced,
and the real golden walks, Telegraf conf and packs. Run with
``python3 -m pytest scripts/tests``.
"""

from __future__ import annotations

import datetime
import json
import re
import subprocess
import sys
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
ENDED_S = STARTED_S + uat.IF_MIB_RUN_S
# Telegraf polls every 2 s; the first poll that sonda answers lands 1 s in.
POLL_S = 2.0
FIRST_POLL_S = STARTED_S + 1.0
FLAP_7, FLAP_15 = uat.IF_MIB_FLAPPING_PORTS
END_OF_MIB = "No more variables left in this MIB View (It is past the end of the MIB tree)"


def telegraf_line(at_s: float, message: str) -> str:
    """A telegraf-if-mib log line stamped ``at_s``, as ``docker compose logs``
    prints it."""
    stamp = datetime.datetime.fromtimestamp(at_s, datetime.timezone.utc)
    return f"telegraf-if-mib-1  | {stamp:%Y-%m-%dT%H:%M:%S}Z {message}\n"


def refused(at_s: float) -> str:
    """An error of the if_mib input, as Telegraf 1.40.2 logs it."""
    return telegraf_line(
        at_s,
        "E! [inputs.snmp::if_mib] Error in plugin: agent udp://sonda:1163: performing get "
        "on field ifNumber: error reading from socket: read udp 172.18.0.4:40860->"
        "172.17.0.1:1163: recvfrom: connection refused",
    )


def good_log() -> str:
    """Telegraf's startup and errors from polls sonda was not listening for:
    ten minutes before the run, in the second of the first sample, and in
    the second the run ended."""
    return (
        telegraf_line(STARTED_S - 600, "I! Loading config: /etc/telegraf/telegraf.conf")
        + telegraf_line(STARTED_S - 600, "I! Loaded inputs: snmp")
        + telegraf_line(STARTED_S - 600, "I! Loaded outputs: health influxdb")
        + refused(STARTED_S - 600)
        + refused(FIRST_POLL_S)
        + refused(ENDED_S)
    )


def poll_times(first_s: float = FIRST_POLL_S, last_s: float = ENDED_S - 1) -> list[float]:
    """Telegraf's polls every POLL_S from ``first_s`` to ``last_s``."""
    times, at = [], first_s
    while at <= last_s:
        times.append(at)
        at += POLL_S
    return times


def oper_status(port: str, at_s: float) -> float:
    """The example's ifOperStatus: port 7 down from 60 s to 65 s, port 15
    from 30 s to 35 s and from 95 s to 100 s."""
    since = (at_s - STARTED_S) % 65
    if port == FLAP_7 and 60 <= since < 65:
        return 2.0
    if port == FLAP_15 and 30 <= since < 35:
        return 2.0
    return 1.0


def conf_fields() -> tuple[str, ...]:
    return uat.if_mib_conf_fields((REPO_ROOT / uat.IF_MIB_TELEGRAF_CONF).read_text())


def value_of(field: str, port: str, at_s: float) -> float:
    """What Telegraf reads for ``field`` of ``port`` at ``at_s``."""
    if field == uat.IF_MIB_INDEX_FIELD:
        return uat.IF_MIB_PORTS[port]
    if field in uat.IF_MIB_CONSTANT_FIELDS:
        return uat.IF_MIB_CONSTANT_FIELDS[field]
    if field == uat.IF_MIB_OPER_STATUS_FIELD:
        return oper_status(port, at_s)
    if field in uat.IF_MIB_COUNTER_FIELDS:
        return 125_000.0 * (at_s - STARTED_S + 1)
    raise AssertionError(f"no value for {field}")


def export_line(name: str, labels: dict[str, str], samples: list[tuple[float, float]]) -> str:
    return json.dumps(
        {
            "metric": {"__name__": name, "db": "telegraf", "source": "sonda", **labels},
            "values": [v for _, v in samples],
            "timestamps": [int(at * 1000) for at, _ in samples],
        }
    )


def good_export(
    ports=tuple(uat.IF_MIB_PORTS),
    fields: tuple[str, ...] | None = None,
    value=value_of,
    number: float = 24.0,
    polls: list[float] | None = None,
    extra: tuple[str, ...] = (),
) -> bytes:
    """What VictoriaMetrics exports for a run in which Telegraf read every
    poll: each of ``fields`` (the conf's) of each of ``ports``, tagged with
    the port's ifDescr, valued by ``value``, and snmp_ifNumber at
    ``number``; plus ``extra`` lines as they are."""
    fields = conf_fields() if fields is None else fields
    polls = poll_times() if polls is None else polls
    lines = []
    for port in ports:
        for field in fields:
            samples = [(at, value(field, port, at)) for at in polls]
            lines.append(
                export_line(f"interface_{field}", {uat.IF_MIB_DESCR_TAG: port}, samples)
            )
    lines.append(export_line(uat.IF_MIB_NUMBER_SERIES, {}, [(at, number) for at in polls]))
    lines.extend(extra)
    return "\n".join(lines).encode()


def golden() -> str:
    return (REPO_ROOT / uat.IF_MIB_WALK_GOLDEN).read_text()


def ifx_golden() -> str:
    return (REPO_ROOT / uat.IF_MIB_IFX_WALK_GOLDEN).read_text()


def walked(text: str | None = None) -> str:
    """A golden as net-snmp prints it, before masking."""
    text = golden() if text is None else text
    text = text.replace("Counter32: *", "Counter32: 3750000")
    text = text.replace("Counter64: *", "Counter64: 3750000")
    return text.replace("INTEGER: *", "INTEGER: 2")


def findings(
    log: str | None = None,
    export: bytes | None = None,
    walk: str | None = None,
    ifx_walk: str | None = None,
) -> list[str]:
    export = good_export() if export is None else export
    return uat.if_mib_findings(
        good_log() if log is None else log,
        uat.parse_vm_export(export, label=uat.IF_MIB_DESCR_TAG),
        walked() if walk is None else walk,
        golden(),
        STARTED_S,
        ENDED_S,
        fields=conf_fields(),
        ifx_walk=walked(ifx_golden()) if ifx_walk is None else ifx_walk,
        ifx_golden=ifx_golden(),
    )


def matching(found: list[str], fragment: str) -> list[str]:
    return [line for line in found if fragment in line]


class IfMibFindingsTests(unittest.TestCase):
    def test_a_good_run_has_no_findings(self) -> None:
        self.assertEqual(findings(), [])

    def test_the_synthetic_export_has_every_series_it_claims(self) -> None:
        samples = uat.parse_vm_export(good_export(), label=uat.IF_MIB_DESCR_TAG)
        self.assertEqual(len(conf_fields()), 12)
        self.assertEqual(len(samples), 24 * 12 + 1)
        polls = len(poll_times())
        self.assertGreater(polls, 40)
        for port in uat.IF_MIB_PORTS:
            for field in conf_fields():
                self.assertEqual(len(samples[(f"interface_{field}", port)]), polls)
        statuses = {
            port: {v for _, v in samples[("interface_ifOperStatus", port)]}
            for port in uat.IF_MIB_PORTS
        }
        self.assertEqual(statuses[FLAP_7], {1.0, 2.0})
        self.assertEqual(statuses[FLAP_15], {1.0, 2.0})
        self.assertEqual(
            [port for port, seen in statuses.items() if seen != {1.0}],
            [FLAP_7, FLAP_15],
        )

    def test_23_rows_are_named(self) -> None:
        ports = tuple(uat.IF_MIB_PORTS)
        found = findings(export=good_export(ports=ports[:-1]))
        self.assertEqual(
            matching(found, "interface rows"),
            ["23 of 24 interface rows tagged ifDescr; none for GigabitEthernet1/0/24"],
        )

    def test_a_row_of_another_port_or_none_is_named(self) -> None:
        extra = (
            export_line("interface_ifOperStatus", {"ifDescr": "Gi0/0/0"}, [(FIRST_POLL_S, 1.0)]),
            export_line("interface_ifOperStatus", {"index": "1"}, [(FIRST_POLL_S, 1.0)]),
        )
        found = findings(export=good_export(extra=extra))
        self.assertEqual(
            matching(found, "not a port of the example"),
            [
                "an interface row tagged no ifDescr, which is not a port of the example",
                'an interface row tagged ifDescr="Gi0/0/0", which is not a port of the example',
            ],
        )

    def test_a_missing_column_is_named(self) -> None:
        fields = tuple(f for f in conf_fields() if f != "ifHighSpeed")
        found = findings(export=good_export(fields=fields))
        self.assertEqual(len(found), 1, found)
        self.assertTrue(
            found[0].startswith(
                "interface_ifHighSpeed: fewer than 3 samples for 24 port(s): "
                "GigabitEthernet1/0/1 (0), "
            ),
            found[0],
        )

    def test_a_port_short_of_samples_in_one_column_is_named(self) -> None:
        port = "GigabitEthernet1/0/5"
        others = tuple(p for p in uat.IF_MIB_PORTS if p != port)
        polls = poll_times()
        extra = tuple(
            export_line(
                f"interface_{field}",
                {uat.IF_MIB_DESCR_TAG: port},
                [(at, value_of(field, port, at)) for at in (polls[:2] if field == "ifSpeed" else polls)],
            )
            for field in conf_fields()
        )
        self.assertEqual(
            findings(export=good_export(ports=others, extra=extra)),
            ["interface_ifSpeed: fewer than 3 samples for 1 port(s): GigabitEthernet1/0/5 (2)"],
        )

    def test_a_run_short_of_polls_is_named_in_every_column(self) -> None:
        found = findings(export=good_export(polls=poll_times()[:2]))
        self.assertEqual(len(matching(found, "fewer than 3 samples for 24 port(s)")), 12)
        self.assertIn("snmp_ifNumber: 2 sample(s), want >= 3", found)

    def test_a_typed_column_that_is_not_its_value_is_named(self) -> None:
        def value(field: str, port: str, at_s: float) -> float:
            if field == "ifType" and port == "GigabitEthernet1/0/3":
                return 0.0
            if field == "ifIndex" and port == "GigabitEthernet1/0/4":
                return 3.0
            return value_of(field, port, at_s)

        found = findings(export=good_export(value=value))
        polls = len(poll_times())
        self.assertEqual(
            found,
            [
                f'interface_ifIndex{{ifDescr="GigabitEthernet1/0/4"}}: {polls} sample(s) not 4, '
                "the first 3",
                f'interface_ifType{{ifDescr="GigabitEthernet1/0/3"}}: {polls} sample(s) not 6, '
                "the first 0",
            ],
        )

    def test_if_number_other_than_24_is_named(self) -> None:
        found = findings(export=good_export(number=23.0))
        self.assertEqual(
            found, [f"snmp_ifNumber: {len(poll_times())} sample(s) not 24, the first 23"]
        )

    def test_no_if_number_is_named(self) -> None:
        export = b"\n".join(
            line for line in good_export().splitlines() if b"snmp_ifNumber" not in line
        )
        self.assertEqual(findings(export=export), ["snmp_ifNumber: 0 sample(s), want >= 3"])

    def test_a_flapping_port_that_never_left_up_is_named(self) -> None:
        def value(field: str, port: str, at_s: float) -> float:
            if field == "ifOperStatus" and port == FLAP_15:
                return 1.0
            return value_of(field, port, at_s)

        self.assertEqual(
            findings(export=good_export(value=value)),
            [
                'interface_ifOperStatus{ifDescr="GigabitEthernet1/0/15"} never took down (2) '
                "within the run; saw [1.0]"
            ],
        )

    def test_a_port_that_flaps_and_should_not_is_named(self) -> None:
        def value(field: str, port: str, at_s: float) -> float:
            if field == "ifOperStatus" and port == "GigabitEthernet1/0/8":
                return oper_status(FLAP_7, at_s)
            return value_of(field, port, at_s)

        self.assertEqual(
            findings(export=good_export(value=value)),
            [
                'interface_ifOperStatus{ifDescr="GigabitEthernet1/0/8"} took [1.0, 2.0], want '
                "only up (1): the port does not flap"
            ],
        )

    def test_a_counter_that_does_not_increase_is_named(self) -> None:
        def value(field: str, port: str, at_s: float) -> float:
            if field == "ifHCOutOctets" and port == "GigabitEthernet1/0/2":
                return 5.0
            return value_of(field, port, at_s)

        self.assertEqual(
            findings(export=good_export(value=value)),
            [
                'interface_ifHCOutOctets{ifDescr="GigabitEthernet1/0/2"} did not increase: 5 '
                "then 5, 3.0s after sonda started"
            ],
        )

    def test_no_loaded_inputs_line_is_named(self) -> None:
        log = good_log().replace("I! Loaded inputs: snmp", "I! Loaded inputs: cpu")
        self.assertEqual(findings(log=log), ["telegraf log has no 'Loaded inputs:' line naming snmp"])

    def test_an_error_after_the_first_sample_is_named(self) -> None:
        log = good_log() + refused(FIRST_POLL_S + 1)
        (found,) = findings(log=log)
        self.assertTrue(found.startswith("telegraf logged an error while sonda served: "), found)

    def test_an_error_a_second_before_the_end_is_named(self) -> None:
        self.assertEqual(len(findings(log=good_log() + refused(ENDED_S - 1))), 1)

    def test_an_unstamped_error_is_named(self) -> None:
        log = good_log() + "E! [inputs.snmp::if_mib] Error in plugin: no stamp\n"
        self.assertEqual(len(findings(log=log)), 1)

    def test_errors_before_the_first_sample_and_after_the_run_are_excused(self) -> None:
        # The premise: good_log() carries both, and the good run passes.
        self.assertEqual(good_log().count("E! [inputs.snmp::if_mib]"), 3)
        self.assertEqual(findings(), [])

    def test_no_sample_stops_the_checks_after_it(self) -> None:
        # With no sample, the errors are measured from the run's start, so
        # the one in the second of the first poll is named too.
        found = findings(export=b"", walk="")
        self.assertEqual(len(found), 2, found)
        self.assertTrue(found[0].startswith("telegraf logged an error while sonda served"))
        self.assertEqual(
            found[1], f"no sample matching {uat.IF_MIB_EXPORT_MATCH} within the run's window"
        )

    def test_samples_outside_the_run_do_not_count(self) -> None:
        late = good_export(polls=[at + uat.IF_MIB_RUN_S + 10 for at in poll_times()])
        found = findings(export=late)
        self.assertEqual(
            found[-1], f"no sample matching {uat.IF_MIB_EXPORT_MATCH} within the run's window"
        )
        self.assertEqual(len(found), 2, found)

    def test_a_walk_that_differs_is_named_with_the_line(self) -> None:
        walk = walked().replace(".1.3.6.1.2.1.2.2.1.3.5 = INTEGER: 6", ".1.3.6.1.2.1.2.2.1.3.5 = INTEGER: 1")
        (found,) = findings(walk=walk)
        self.assertTrue(found.startswith(f"the in-container walk differs from {uat.IF_MIB_WALK_GOLDEN}"))
        self.assertIn("+.1.3.6.1.2.1.2.2.1.3.5 = INTEGER: 1", found)

    def test_an_ifx_walk_missing_an_instance_is_named(self) -> None:
        walk = "".join(
            line for line in walked(ifx_golden()).splitlines(keepends=True)
            if not line.startswith(".1.3.6.1.2.1.31.1.1.1.15.24 ")
        )
        (found,) = findings(ifx_walk=walk)
        self.assertTrue(found.startswith(f"the in-container ifMIB walk differs from {uat.IF_MIB_IFX_WALK_GOLDEN}"))
        self.assertIn("-.1.3.6.1.2.1.31.1.1.1.15.24 = Gauge32: 1000", found)

    def test_an_empty_walk_is_named(self) -> None:
        self.assertEqual(findings(walk="", ifx_walk=" \n"), [
            "the in-container walk printed nothing",
            "the in-container ifMIB walk printed nothing",
        ])

    def test_the_goldens_of_the_other_example_are_red(self) -> None:
        other = (REPO_ROOT / uat.SNMP_WALK_GOLDEN).read_text()
        self.assertTrue(other.strip(), "the other example's golden exists")
        found = findings(walk=walked(other), ifx_walk=walked(other))
        self.assertEqual(len(found), 2, found)
        self.assertTrue(found[0].startswith("the in-container walk differs"), found[0])
        self.assertTrue(found[1].startswith("the in-container ifMIB walk differs"), found[1])

    def test_the_findings_come_in_check_order(self) -> None:
        def value(field: str, port: str, at_s: float) -> float:
            if field == "ifOperStatus" and port == FLAP_7:
                return 1.0
            return value_of(field, port, at_s)

        found = findings(
            log=good_log() + refused(FIRST_POLL_S + 4),
            export=good_export(ports=tuple(uat.IF_MIB_PORTS)[1:], value=value, number=0.0),
            walk="",
        )
        order = [
            "telegraf logged an error",
            "23 of 24 interface rows",
            "snmp_ifNumber",
            "never took down",
            "the in-container walk printed nothing",
        ]
        positions = [next(i for i, f in enumerate(found) if key in f) for key in order]
        self.assertEqual(positions, sorted(positions), found)


def pack(name: str) -> dict:
    import yaml

    return yaml.safe_load((REPO_ROOT / "packs" / name).read_text())


def served_columns() -> dict[str, str]:
    """Every column the if_mib pack serves, the base's included: each
    metric's hint and each label column, by name, as its object OID."""
    columns: dict[str, str] = {}
    for file in ("telegraf-snmp-interface.yaml", "if-mib.yaml"):
        doc = pack(file)
        for metric in doc["metrics"]:
            columns[metric["name"]] = metric["snmp"]["oid"].removesuffix(".{ifIndex}")
        for label, column in doc["snmp_label_columns"].items():
            oid = column["oid"] if isinstance(column, dict) else column
            columns[label] = oid.removesuffix(".{ifIndex}")
    return columns


def oid_order(oid: str) -> list[int]:
    return [int(arc) for arc in oid.strip(".").split(".")]


def golden_problems(walk: str) -> list[str]:
    """How ``walk`` is not the interfaces walk of the example: ifNumber, then
    every column the pack serves under the ifTable, 24 rows each, in walk
    order; ifIndex N, ifDescr the port's name, ifType 6, and ifOperStatus
    up but for the flapping ports."""
    ports = range(1, 25)
    table = sorted(
        (oid for oid in served_columns().values() if oid.startswith("1.3.6.1.2.1.2.2.1.")),
        key=oid_order,
    )
    names = [".1.3.6.1.2.1.2.1.0"] + [f".{column}.{n}" for column in table for n in ports]
    lines = walk.splitlines()
    problems = []
    if [line.split(" = ")[0] for line in lines] != names:
        problems.append("the instances are not ifNumber and the served ifTable columns")
    printed = dict(line.split(" = ", 1) for line in lines if " = " in line)
    want = {".1.3.6.1.2.1.2.1.0": "INTEGER: 24"}
    for n in ports:
        want[f".1.3.6.1.2.1.2.2.1.1.{n}"] = f"INTEGER: {n}"
        want[f".1.3.6.1.2.1.2.2.1.2.{n}"] = f'STRING: "GigabitEthernet1/0/{n}"'
        want[f".1.3.6.1.2.1.2.2.1.3.{n}"] = "INTEGER: 6"
        want[f".1.3.6.1.2.1.2.2.1.8.{n}"] = "INTEGER: *" if n in (7, 15) else "INTEGER: 1"
    for oid, value in want.items():
        if printed.get(oid) != value:
            problems.append(f"{oid} is {printed.get(oid)!r}, want {value!r}")
    return problems


def ifx_golden_problems(walk: str) -> list[str]:
    """How ``walk`` is not the ifMIB walk of the example: every column the
    pack serves under the ifXTable, 24 rows each, in walk order, then the end
    of the agent's MIB; ifName the port's name, ifAlias the uplinks' text and
    empty elsewhere."""
    ports = range(1, 25)
    table = sorted(
        (oid for oid in served_columns().values() if oid.startswith("1.3.6.1.2.1.31.1.1.1.")),
        key=oid_order,
    )
    names = [f".{column}.{n}" for column in table for n in ports]
    lines = walk.splitlines()
    problems = []
    if [line.split(" = ")[0] for line in lines[:-1]] != names:
        problems.append("the instances are not the served ifXTable columns")
    if not lines or lines[-1] != f"{names[-1]} = {END_OF_MIB}":
        problems.append("the walk does not end at the end of the agent's MIB")
    printed = dict(line.split(" = ", 1) for line in lines[:-1] if " = " in line)
    aliases = {23: "uplink to dist-01", 24: "uplink to dist-02"}
    for n in ports:
        for oid, value in (
            (f".1.3.6.1.2.1.31.1.1.1.1.{n}", f'STRING: "GigabitEthernet1/0/{n}"'),
            # net-snmp prints an empty OCTET STRING as "".
            (
                f".1.3.6.1.2.1.31.1.1.1.18.{n}",
                f'STRING: "{aliases[n]}"' if n in aliases else '""',
            ),
            (f".1.3.6.1.2.1.31.1.1.1.15.{n}", "Gauge32: 1000"),
        ):
            if printed.get(oid) != value:
                problems.append(f"{oid} is {printed.get(oid)!r}, want {value!r}")
    return problems


class GoldenWalkTests(unittest.TestCase):
    def test_the_pack_serves_ten_if_table_columns_and_five_if_x_table_columns(self) -> None:
        columns = served_columns()
        self.assertEqual(len(columns), 15)
        self.assertEqual(sum(o.startswith("1.3.6.1.2.1.2.2.1.") for o in columns.values()), 10)
        self.assertEqual(sum(o.startswith("1.3.6.1.2.1.31.1.1.1.") for o in columns.values()), 5)

    def test_the_interfaces_golden_is_if_number_and_every_served_if_table_column(self) -> None:
        self.assertEqual(golden_problems(golden()), [])
        self.assertEqual(len(golden().splitlines()), 1 + 10 * 24)

    def test_the_ifx_golden_is_every_served_if_x_table_column(self) -> None:
        self.assertEqual(ifx_golden_problems(ifx_golden()), [])
        self.assertEqual(len(ifx_golden().splitlines()), 5 * 24 + 1)

    def test_the_golden_checks_are_red_on_the_other_examples_golden(self) -> None:
        other = (REPO_ROOT / uat.SNMP_WALK_GOLDEN).read_text()
        self.assertTrue(other.strip(), "the other example's golden exists")
        self.assertNotEqual(golden_problems(other), [])
        self.assertNotEqual(ifx_golden_problems(other), [])

    def test_the_goldens_mask_only_the_moving_values(self) -> None:
        masked = [line for line in golden().splitlines() if line.endswith("*")]
        self.assertEqual(len(masked), 2 * 24 + 2, "both octet counters and two statuses")
        for line in masked:
            self.assertRegex(
                line,
                r"^\.1\.3\.6\.1\.2\.1\.2\.2\.1\.((10|16)\.\d+ = Counter32|8\.(7|15) = INTEGER): \*$",
            )
        masked = [line for line in ifx_golden().splitlines() if line.endswith("*")]
        self.assertEqual(len(masked), 2 * 24, "both 64-bit octet counters")
        for line in masked:
            self.assertRegex(line, r"^\.1\.3\.6\.1\.2\.1\.31\.1\.1\.1\.(6|10)\.\d+ = Counter64: \*$")

    def test_the_masks_keep_the_values_that_do_not_move(self) -> None:
        walk = (
            ".1.3.6.1.2.1.2.2.1.10.1 = Counter32: 125000\n"
            ".1.3.6.1.2.1.2.2.1.14.1 = Counter32: 3\n"
            ".1.3.6.1.2.1.2.2.1.8.7 = INTEGER: 2\n"
            ".1.3.6.1.2.1.2.2.1.8.8 = INTEGER: 2\n"
            ".1.3.6.1.2.1.31.1.1.1.6.1 = Counter64: 99\n"
        )
        self.assertEqual(
            uat.mask_if_mib_walk(walk),
            ".1.3.6.1.2.1.2.2.1.10.1 = Counter32: *\n"
            ".1.3.6.1.2.1.2.2.1.14.1 = Counter32: 3\n"
            ".1.3.6.1.2.1.2.2.1.8.7 = INTEGER: *\n"
            ".1.3.6.1.2.1.2.2.1.8.8 = INTEGER: 2\n"
            ".1.3.6.1.2.1.31.1.1.1.6.1 = Counter64: *\n",
        )

    def test_the_walk_commands_name_the_two_subtrees_on_the_examples_port(self) -> None:
        self.assertEqual(uat.IF_MIB_WALK_COMMAND[-2:], ("sonda:1163", "1.3.6.1.2.1.2"))
        self.assertEqual(uat.IF_MIB_IFX_WALK_COMMAND[-2:], ("sonda:1163", "1.3.6.1.2.1.31"))
        self.assertEqual(uat.IF_MIB_WALK_COMMAND[:-1], uat.IF_MIB_IFX_WALK_COMMAND[:-1])


def telegraf_conf() -> dict:
    return tomllib.loads((REPO_ROOT / uat.IF_MIB_TELEGRAF_CONF).read_text())


class TelegrafConfTests(unittest.TestCase):
    def test_one_v2c_input_polls_the_examples_port(self) -> None:
        conf = telegraf_conf()
        (snmp,) = conf["inputs"]["snmp"]
        self.assertEqual(snmp["alias"], uat.IF_MIB_INPUT)
        self.assertEqual(snmp["agents"], ["udp://sonda:1163"])
        self.assertEqual((snmp["version"], snmp["community"]), (2, "public"))
        self.assertEqual(snmp["agent_host_tag"], "source")
        self.assertEqual(conf["agent"]["interval"], "2s")
        match = uat.SNMP_ERROR_LINE.search(f"E! [inputs.snmp::{uat.IF_MIB_INPUT}] Error in plugin: x")
        self.assertIsNotNone(match)
        self.assertEqual(match.group("alias"), uat.IF_MIB_INPUT)

    def test_the_example_serves_the_port_the_conf_polls(self) -> None:
        import yaml

        example = yaml.safe_load((REPO_ROOT / "examples/if-mib-switch.yaml").read_text())
        self.assertEqual(example["defaults"]["sink"]["listen"], "0.0.0.0:1163")
        self.assertEqual(example["defaults"]["sink"]["versions"], ["v2c"])
        self.assertEqual(example["defaults"]["sink"]["community"], "public")
        ports = [entry for entry in example["scenarios"] if entry.get("pack") == "if_mib"]
        self.assertEqual(
            [entry["labels"]["ifDescr"] for entry in ports], list(uat.IF_MIB_PORTS)
        )
        flapping = [entry["labels"]["ifDescr"] for entry in ports if "overrides" in entry]
        self.assertEqual(flapping, list(uat.IF_MIB_FLAPPING_PORTS))

    def test_the_documented_layout_tags_the_rows_with_if_descr(self) -> None:
        (snmp,) = telegraf_conf()["inputs"]["snmp"]
        (table,) = snmp["table"]
        self.assertEqual(f"{table['name']}_", uat.IF_MIB_TABLE_PREFIX)
        self.assertNotIn("index_as_tag", table)
        tags = [field for field in table["field"] if field.get("is_tag")]
        self.assertEqual(
            tags, [{"name": uat.IF_MIB_DESCR_TAG, "oid": ".1.3.6.1.2.1.2.2.1.2", "is_tag": True}]
        )
        self.assertEqual(
            snmp["field"], [{"name": "ifNumber", "oid": ".1.3.6.1.2.1.2.1.0"}]
        )
        self.assertEqual(f"snmp_{snmp['field'][0]['name']}", uat.IF_MIB_NUMBER_SERIES)

    def test_every_field_is_an_if_mib_column_the_pack_serves_by_its_name(self) -> None:
        (snmp,) = telegraf_conf()["inputs"]["snmp"]
        (table,) = snmp["table"]
        columns = served_columns()
        for field in table["field"]:
            self.assertRegex(field["oid"], r"^(\.[0-9]+)+$", field["name"])
            self.assertEqual(field["oid"].lstrip("."), columns.get(field["name"]), field["name"])
        # Every numeric column the pack serves is read; ifName and ifAlias,
        # the strings the base pack's conf tags its rows with, are not.
        read = {field["name"] for field in table["field"]}
        self.assertEqual(sorted(set(columns) - read), ["ifAlias", "ifName"])

    def test_the_checker_reads_the_confs_fields_in_order(self) -> None:
        (snmp,) = telegraf_conf()["inputs"]["snmp"]
        (table,) = snmp["table"]
        names = tuple(f["name"] for f in table["field"] if not f.get("is_tag"))
        self.assertEqual(conf_fields(), names)
        self.assertEqual(len(names), 12)
        for field in (
            uat.IF_MIB_INDEX_FIELD,
            uat.IF_MIB_OPER_STATUS_FIELD,
            *uat.IF_MIB_COUNTER_FIELDS,
            *uat.IF_MIB_CONSTANT_FIELDS,
        ):
            self.assertIn(field, names)

    def test_a_conf_of_another_shape_is_refused(self) -> None:
        with self.assertRaises(ValueError):
            uat.if_mib_conf_fields((REPO_ROOT / uat.SNMP_TELEGRAF_CONF).read_text())
        with self.assertRaises(ValueError):
            uat.if_mib_conf_fields("")

    def test_the_health_output_is_the_backend_the_harness_waits_for(self) -> None:
        (health,) = telegraf_conf()["outputs"]["health"]
        port = health["service_address"].rsplit(":", 1)[1]
        backends = {backend.name: backend for backend in uat.BACKENDS_BY_PROFILE["snmp"]}
        self.assertEqual(backends[uat.IF_MIB_SERVICE].health_url, f"http://localhost:{port}/")

    def test_the_compose_service_mounts_the_conf_under_the_snmp_profile(self) -> None:
        import yaml

        compose = yaml.safe_load((REPO_ROOT / uat.COMPOSE_FILE).read_text())
        service = compose["services"][uat.IF_MIB_SERVICE]
        self.assertEqual(service["profiles"], ["snmp"])
        self.assertEqual(
            service["volumes"],
            [f"./telegraf/{uat.IF_MIB_TELEGRAF_CONF.name}:/etc/telegraf/telegraf.conf:ro"],
        )
        self.assertEqual(service["ports"], ["8098:8098"])
        self.assertEqual(service["image"], compose["services"]["telegraf-snmp"]["image"])


class MatrixRowTests(unittest.TestCase):
    def test_the_row_runs_the_example_walks_it_and_verifies_its_evidence(self) -> None:
        rows = {row.name: row for row in uat.MATRIX}
        row = rows["snmp-if-mib"]
        self.assertEqual(row.profiles, ("snmp",))
        self.assertEqual(row.scenario, Path("examples/if-mib-switch.yaml"))
        self.assertTrue((REPO_ROOT / row.scenario).is_file())
        self.assertIs(row.during, uat.if_mib_during)
        self.assertFalse(row.during_gets_sonda)
        self.assertIs(row.post_verify, uat.verify_if_mib_row)
        self.assertEqual(row.failure_log_containers[0], uat.IF_MIB_SERVICE)
        self.assertEqual(row.extra_args, ("--duration", "100s"))
        # Both flapping ports go down within the run, with polls after.
        self.assertGreater(uat.IF_MIB_RUN_S, 65 + 2 * POLL_S)
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(row.verify_url).query)
        self.assertEqual(query["query"], [uat.IF_MIB_VERIFY_QUERY])

    def test_the_row_comes_after_the_snmp_row_and_the_matrix_table_lists_it(self) -> None:
        names = [row.name for row in uat.MATRIX]
        self.assertEqual(names.index("snmp-if-mib"), names.index("snmp") + 1)
        lines = (REPO_ROOT / "docs/site/docs/test/end-to-end-pipelines.md").read_text().splitlines()
        (at,) = [i for i, line in enumerate(lines) if "| Signal | Encoder |" in line]
        indent = lines[at][: len(lines[at]) - len(lines[at].lstrip())]
        # The table is the header and every line after it, without a break,
        # that starts with the header's indentation and a pipe: Markdown
        # ends it at the first line that does not.
        end = at
        while end < len(lines) and lines[end].startswith(f"{indent}|"):
            end += 1
        table = lines[at:end]
        self.assertTrue(
            any("`examples/snmp-telegraf.yaml`" in line for line in table),
            "the block found is the matrix: it holds the snmp row",
        )
        rows = [line for line in table if "`examples/if-mib-switch.yaml`" in line]
        self.assertEqual(len(rows), 1, "one row of the matrix table names the example")
        stray = [
            line
            for line in lines[:at] + lines[end:]
            if line.lstrip().startswith("|") and "`examples/if-mib-switch.yaml`" in line
        ]
        self.assertEqual(stray, [], "no row naming the example sits outside the table")


class Clock:
    """A monotonic clock that sleeping advances."""

    def __init__(self) -> None:
        self.t = 0.0

    def now(self) -> float:
        return self.t

    def sleep(self, s: float) -> None:
        self.t += s


class IfMibDuringTests(unittest.TestCase):
    def test_it_walks_both_subtrees_inside_the_if_mib_service_until_each_matches(self) -> None:
        clock = Clock()
        asked: list[list[str]] = []
        replies = {
            "1.3.6.1.2.1.2": [walked().splitlines(keepends=True)[0], walked()],
            "1.3.6.1.2.1.31": [walked(ifx_golden())],
        }

        def run(argv, **_kwargs):
            asked.append(argv)
            clock.t += 0.5
            stdout = replies[argv[-1]].pop(0)
            return subprocess.CompletedProcess(argv, 0, stdout, "")

        walks = uat.if_mib_during(REPO_ROOT, run=run, now=clock.now, sleep=clock.sleep)
        self.assertEqual(walks, uat.IfMibWalks(interfaces=walked(), ifx=walked(ifx_golden())))
        self.assertEqual(len(asked), 3, "the first walk retried once")
        prefix = uat.compose_command(REPO_ROOT, "exec", "-T", "-e", "MIBS=", uat.IF_MIB_SERVICE)
        self.assertEqual(asked[0], prefix + list(uat.IF_MIB_WALK_COMMAND))
        self.assertEqual(asked[2], prefix + list(uat.IF_MIB_IFX_WALK_COMMAND))

    def test_both_walks_share_one_window(self) -> None:
        clock = Clock()

        def run(argv, **_kwargs):
            clock.t += 1.0
            return subprocess.CompletedProcess(argv, 0, "never the golden\n", "")

        walks = uat.if_mib_during(REPO_ROOT, run=run, now=clock.now, sleep=clock.sleep)
        self.assertEqual(walks.interfaces, "never the golden\n")
        self.assertLessEqual(clock.t, uat.SNMP_WALK_WITHIN_S + 3.0)


class VerifyIfMibRowTests(unittest.TestCase):
    def verify(self, export: bytes = b"", status: int = 200, during=None) -> tuple[str, list[str]]:
        asked: list[str] = []
        if during is None:
            during = uat.IfMibWalks(interfaces=walked(), ifx=walked(ifx_golden()))

        def http_get(url, **_kwargs):
            asked.append(url)
            return status, export

        with mock.patch.object(uat, "compose_logs_tail", return_value=good_log()) as logs, \
                mock.patch.object(uat, "http_get", side_effect=http_get):
            found = uat.verify_if_mib_row(REPO_ROOT, STARTED_S, ENDED_S, during=during)
        logs.assert_called_once_with(REPO_ROOT, uat.IF_MIB_SERVICE, tail_lines=None)
        return found, asked

    def test_it_reads_the_log_the_export_the_conf_and_the_goldens(self) -> None:
        found, asked = self.verify(export=good_export())
        self.assertEqual(found, "")
        (url,) = asked
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
        self.assertEqual(query["match[]"], [uat.IF_MIB_EXPORT_MATCH])
        self.assertEqual(float(query["start"][0]), STARTED_S)
        self.assertEqual(float(query["end"][0]), ENDED_S)

    def test_without_samples_it_is_red(self) -> None:
        found, _ = self.verify(export=b"")
        self.assertIn("no sample matching", found)

    def test_a_failed_hook_stands_for_both_walks(self) -> None:
        found, _ = self.verify(export=good_export(), during="<the during hook raised X>")
        lines = found.splitlines()
        self.assertEqual(lines[0], "the walks were not taken: <the during hook raised X>")
        self.assertIn("the in-container walk differs", found)
        self.assertIn("the in-container ifMIB walk differs", found)

    def test_a_row_without_the_hook_is_red(self) -> None:
        found, _ = self.verify(export=good_export(), during="")
        self.assertEqual(
            found.splitlines()[0], "the walks were not taken: the row has no during hook"
        )

    def test_an_export_error_is_reported(self) -> None:
        found, _ = self.verify(export=b"boom", status=500)
        self.assertEqual(found, "VictoriaMetrics export answered 500: b'boom'")

    def test_a_missing_golden_is_reported(self) -> None:
        walks = uat.IfMibWalks(interfaces=walked(), ifx=walked(ifx_golden()))
        with mock.patch.object(uat, "IF_MIB_IFX_WALK_GOLDEN", Path("tests/e2e/absent.walk")):
            found, _ = self.verify(export=good_export(), during=walks)
        self.assertTrue(found.startswith("cannot read the snmp-if-mib row's inputs: "), found)


if __name__ == "__main__":
    unittest.main()
