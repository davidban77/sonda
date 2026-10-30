"""Tests for the snmp-traps rows of scripts/live_infra_uat.py.

Each test drives the harness's own functions: traps_findings over a
synthetic telegraf-snmp log (through parse_traps), the before and during
hooks with docker replaced, run_row's before hook, and the real example,
Telegraf conf and compose file. Run with ``python3 -m pytest scripts/tests``.
"""

from __future__ import annotations

import datetime
import json
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
ENDED_S = STARTED_S + uat.TRAPS_RUN_S
COLD_START, LINK_DOWN, LINK_UP = uat.TRAP_COLD_START, uat.TRAP_LINK_DOWN, uat.TRAP_LINK_UP


def telegraf_line(at_s: float, message: str) -> str:
    """A telegraf-snmp log line stamped ``at_s``, as ``docker compose logs``
    prints it."""
    stamp = datetime.datetime.fromtimestamp(at_s, datetime.timezone.utc)
    return f"telegraf-snmp-1  | {stamp:%Y-%m-%dT%H:%M:%S}Z {message}\n"


def trap_line(at_s: float, oid: str, *, version: str = "2c", community: str = "public") -> str:
    """One trap as Telegraf 1.40.2's outputs.file prints it (data_format
    json, json_timestamp_units 1ms, the gosmi translator without MIB files),
    received at ``at_s``, as ``docker compose logs`` prints it."""
    fields = {"iso.3.6.1.2.1.1.3.0": int((at_s - STARTED_S) * 100)}
    if oid != COLD_START:
        fields.update(
            {"iso.3.6.1.2.1.2.2.1.1.1": 1, "iso.3.6.1.2.1.2.2.1.7.1": 1, "iso.3.6.1.2.1.2.2.1.8.1": 2}
        )
    metric = {
        "fields": fields,
        "name": "snmp_trap",
        "tags": {
            "community": community,
            "mib": "",
            "name": "iso" + oid[2:],
            "oid": oid,
            "source": "172.18.0.1",
            "version": version,
        },
        "timestamp": int(at_s * 1000),
    }
    return f"telegraf-snmp-1  | {json.dumps(metric, separators=(',', ':'))}\n"


def startup() -> str:
    """Telegraf's startup lines, ten minutes before the run."""
    return (
        telegraf_line(STARTED_S - 600, "I! Loading config: /etc/telegraf/telegraf.conf")
        + telegraf_line(STARTED_S - 600, "I! Loaded inputs: snmp (3x) snmp_trap")
        + telegraf_line(STARTED_S - 600, "I! Loaded outputs: file health influxdb")
        + telegraf_line(STARTED_S - 600, "I! [inputs.snmp_trap::traps] Listening on udp://:1162")
    )


def received(cold_start_s: float = 0.1, link_traps=None) -> list[tuple[float, str]]:
    """The run's traps as (seconds into the run, oid): coldStart at
    ``cold_start_s``, then ``link_traps``, by default the ones the flap
    implies, each received 20 ms after its change."""
    if link_traps is None:
        link_traps = [(at + 0.02, oid) for at, oid in uat.implied_link_traps()]
    return [(cold_start_s, COLD_START), *link_traps]


def log(traps=None, extra: str = "") -> str:
    """Telegraf's log of a run that received ``traps`` (by default
    :func:`received`), with ``extra`` after its startup."""
    traps = received() if traps is None else traps
    return startup() + extra + "".join(trap_line(STARTED_S + at, oid) for at, oid in traps)


def findings(telegraf_log: str, **kwargs) -> list[str]:
    return uat.traps_findings(telegraf_log, STARTED_S, ENDED_S, **kwargs)


class ImpliedLinkTrapsTests(unittest.TestCase):
    def test_the_run_holds_two_flap_cycles_away_from_its_ends(self) -> None:
        implied = uat.implied_link_traps()
        self.assertEqual(
            implied,
            [(60.0, LINK_DOWN), (65.0, LINK_UP), (125.0, LINK_DOWN), (130.0, LINK_UP)],
        )
        # No change within 5 s of the end: a tick early or late cannot
        # move one in or out of the run.
        self.assertLessEqual(implied[-1][0], uat.TRAPS_RUN_S - 5)

    def test_a_change_is_implied_only_before_the_end(self) -> None:
        self.assertEqual(uat.implied_link_traps(60.0), [])
        self.assertEqual(uat.implied_link_traps(65.0), [(60.0, LINK_DOWN)])
        self.assertEqual(uat.implied_link_traps(65.5), [(60.0, LINK_DOWN), (65.0, LINK_UP)])

    def test_the_constants_are_the_examples_flap_and_rate(self) -> None:
        import yaml

        example = yaml.safe_load((REPO_ROOT / uat.TRAPS_SCENARIO).read_text())
        (entry,) = example["scenarios"]
        self.assertEqual(entry["pack"], "telegraf_snmp_interface")
        flap = entry["overrides"]["ifOperStatus"]["generator"]
        self.assertEqual(flap["type"], "flap")
        self.assertEqual(flap["enum"], "oper_state")
        self.assertEqual(flap["up_duration"], f"{uat.TRAPS_FLAP_UP_S:g}s")
        self.assertEqual(flap["down_duration"], f"{uat.TRAPS_FLAP_DOWN_S:g}s")
        self.assertEqual(example["defaults"]["rate"], 1)

    def test_the_examples_informs_outlast_the_receivers_restart(self) -> None:
        import yaml

        sink = yaml.safe_load((REPO_ROOT / uat.TRAPS_SCENARIO).read_text())["defaults"]["sink"]
        self.assertEqual(sink["type"], "snmp_trap")
        self.assertIs(sink["inform"], True)
        timeout_s = float(sink["timeout"].removesuffix("s"))
        # The coldStart is sent at 0, timeout, 2 x timeout, ... retries x
        # timeout. At least three of those sends land after the receiver
        # is started again and has had five seconds to start listening.
        sends_s = [timeout_s * k for k in range(sink["retries"] + 1)]
        after = [at for at in sends_s if at >= uat.TRAPS_RECEIVER_DOWN_S + 5.0]
        self.assertGreaterEqual(len(after), 3, sends_s)


class ParseTrapsTests(unittest.TestCase):
    def test_it_reads_the_trap_lines_and_nothing_else(self) -> None:
        text = (
            startup()
            + trap_line(STARTED_S, COLD_START)
            + "telegraf-snmp-1  | {not json\n"
            + 'telegraf-snmp-1  | {"name":"interface","fields":{},"tags":{},"timestamp":1}\n'
            + trap_line(STARTED_S + 60, LINK_DOWN)
        )
        traps = uat.parse_traps(text)
        self.assertEqual([trap["tags"]["oid"] for trap in traps], [COLD_START, LINK_DOWN])

    def test_it_reads_telegrafs_own_line(self) -> None:
        # A line of the local run against Telegraf 1.40.2, as compose prints it.
        line = (
            'telegraf-snmp-1  | {"fields":{"iso.3.6.1.2.1.1.3.0":0},"name":"snmp_trap",'
            '"tags":{"community":"public","mib":"","name":"iso.3.6.1.6.3.1.1.5.1",'
            '"oid":".1.3.6.1.6.3.1.1.5.1","source":"127.0.0.1","version":"2c"},'
            '"timestamp":1793360265304}\n'
        )
        (trap,) = uat.parse_traps(line)
        self.assertEqual(trap["tags"]["oid"], COLD_START)
        self.assertEqual(trap["timestamp"], 1793360265304)


class TrapsFindingsTests(unittest.TestCase):
    def test_the_good_run_has_no_findings(self) -> None:
        self.assertEqual(findings(log()), [])

    def test_a_missing_cold_start_is_named(self) -> None:
        traps = received()[1:]
        (found,) = findings(log(traps))
        self.assertIn("0 coldStart traps, want exactly one", found)

    def test_a_duplicated_cold_start_is_named(self) -> None:
        traps = [(0.1, COLD_START), *received(cold_start_s=5.1)]
        (found,) = findings(log(traps))
        self.assertIn("2 coldStart traps, want exactly one", found)
        self.assertIn("received: coldStart coldStart linkDown linkUp linkDown linkUp", found)

    def test_a_link_trap_before_cold_start_is_named(self) -> None:
        traps = sorted(received(cold_start_s=61.0))
        self.assertEqual([oid for _, oid in traps][:2], [LINK_DOWN, COLD_START], "the premise")
        (found,) = findings(log(traps))
        self.assertIn("a trap came before coldStart", found)

    def test_two_consecutive_link_downs_are_named(self) -> None:
        traps = received(link_traps=[(60.0, LINK_DOWN), (65.0, LINK_DOWN), (125.0, LINK_DOWN), (130.0, LINK_UP)])
        found = findings(log(traps))
        self.assertTrue(any("linkDown twice in a row, at 60.0s and 65.0s" in f for f in found), found)
        self.assertTrue(any("linkDown twice in a row, at 65.0s and 125.0s" in f for f in found), found)

    def test_a_count_off_by_one_is_named(self) -> None:
        implied = [(at + 0.02, oid) for at, oid in uat.implied_link_traps()]
        for traps in (implied[:-1], implied + [(135.0, LINK_DOWN)]):
            with self.subTest(len(traps)):
                found = findings(log(received(link_traps=traps)))
                self.assertTrue(
                    any(f"{len(traps)} link traps, the schedule implies 4 in 140s" in f for f in found),
                    found,
                )

    def test_a_first_link_trap_that_is_link_up_is_named(self) -> None:
        traps = received(link_traps=[(65.0, LINK_UP), (125.0, LINK_DOWN), (130.0, LINK_UP), (135.0, LINK_DOWN)])
        (found,) = findings(log(traps))
        self.assertIn("the first link trap is linkUp at 65.0s, want linkDown", found)

    def test_a_trap_the_sink_does_not_send_is_named(self) -> None:
        traps = received() + [(70.0, ".1.3.6.1.6.3.1.1.5.2")]
        found = findings(log(sorted(traps)))
        self.assertTrue(
            any("a trap the sink does not send, .1.3.6.1.6.3.1.1.5.2, at 70.0s" in f for f in found),
            found,
        )

    def test_another_version_or_community_is_named(self) -> None:
        text = log(extra="") + trap_line(STARTED_S + 70, LINK_DOWN, version="1", community="private")
        found = findings(text)
        self.assertTrue(
            any("is version '1' with community 'private'" in f for f in found), found
        )

    def test_no_trap_is_named_and_ends_the_checks(self) -> None:
        self.assertEqual(
            findings(startup()),
            ["no snmp_trap line from telegraf-snmp within the run's window"],
        )

    def test_traps_outside_the_window_do_not_count(self) -> None:
        earlier = "".join(
            trap_line(STARTED_S - 300 + at, oid) for at, oid in received()
        )
        later = trap_line(ENDED_S + 1, COLD_START)
        self.assertEqual(findings(startup() + earlier + log()[len(startup()):] + later), [])
        self.assertEqual(
            findings(startup() + earlier),
            ["no snmp_trap line from telegraf-snmp within the run's window"],
            "guard: the earlier run's traps are a whole good run",
        )

    def test_a_trap_input_error_during_the_run_is_named(self) -> None:
        error = (
            "E! [inputs.snmp_trap::traps] Error resolving OID oid=.1.3.6.1.2.1.1.3.0, "
            "source=172.18.0.1: not found"
        )
        before = telegraf_line(STARTED_S - 60, error)
        self.assertEqual(findings(log(extra=before)), [], "an error before the run is not")
        during = telegraf_line(STARTED_S + 60, error)
        (found,) = findings(log(extra=during))
        self.assertIn("telegraf logged a trap input error during the run", found)

    def test_a_log_without_the_trap_input_is_named(self) -> None:
        text = log().replace("Loaded inputs: snmp (3x) snmp_trap", "Loaded inputs: snmp (3x)")
        (found,) = findings(text)
        self.assertEqual(found, "telegraf log has no 'Loaded inputs:' line naming snmp_trap")

    def test_with_a_restart_the_cold_start_arrives_after_it(self) -> None:
        restart = uat.ReceiverRestart(started_s=STARTED_S + 30.0, healthy_s=STARTED_S + 33.0)
        self.assertEqual(findings(log(received(cold_start_s=35.0)), restart=restart), [])
        (found,) = findings(log(received(cold_start_s=0.1)), restart=restart)
        self.assertIn(
            "coldStart arrived 0.1s into the run, before the receiver was started again at 30.0s",
            found,
        )


class VerifyTests(unittest.TestCase):
    def test_the_restart_row_names_a_missing_restart_first(self) -> None:
        with mock.patch.object(uat, "compose_logs_tail", return_value=log()):
            text = uat.verify_traps_restart_row(
                REPO_ROOT, STARTED_S, ENDED_S, during="<docker compose start telegraf-snmp exited 1: no>"
            )
            self.assertEqual(
                text,
                "the receiver was not started again: <docker compose start telegraf-snmp exited 1: no>",
            )
            self.assertEqual(uat.verify_traps_row(REPO_ROOT, STARTED_S, ENDED_S), "")
            restart = uat.ReceiverRestart(started_s=STARTED_S + 30.0, healthy_s=STARTED_S + 33.0)
            text = uat.verify_traps_restart_row(REPO_ROOT, STARTED_S, ENDED_S, during=restart)
            self.assertIn("before the receiver was started again", text)

    def test_the_service_log_is_read_in_full(self) -> None:
        with mock.patch.object(uat, "compose_logs_tail", return_value=log()) as tail:
            uat.verify_traps_row(REPO_ROOT, STARTED_S, ENDED_S)
        tail.assert_called_once_with(REPO_ROOT, uat.TRAPS_SERVICE, tail_lines=None)


class Clock:
    """A monotonic clock that sleeping advances."""

    def __init__(self) -> None:
        self.t = 0.0

    def now(self) -> float:
        return self.t

    def sleep(self, s: float) -> None:
        self.t += s


class HookTests(unittest.TestCase):
    def test_the_before_hook_stops_the_service(self) -> None:
        calls = []

        def run(argv, **_kwargs):
            calls.append(argv)
            return subprocess.CompletedProcess(argv, 0, "", "")

        self.assertEqual(uat.stop_trap_receiver(REPO_ROOT, run=run), "")
        (argv,) = calls
        self.assertEqual(argv[:2], ["docker", "compose"])
        self.assertEqual(argv[-2:], ["stop", uat.TRAPS_SERVICE])
        self.assertIn("--profile", argv)

        def failing(argv, **_kwargs):
            return subprocess.CompletedProcess(argv, 1, "", "no such service\n")

        self.assertEqual(
            uat.stop_trap_receiver(REPO_ROOT, run=failing),
            "docker compose stop telegraf-snmp exited 1: no such service",
        )

    def test_the_during_hook_starts_the_service_after_the_outage(self) -> None:
        clock = Clock()
        calls = []

        def run(argv, **_kwargs):
            calls.append((clock.t, argv))
            return subprocess.CompletedProcess(argv, 0, "", "")

        def ready() -> bool:
            clock.t += 2.0
            return True

        restart = uat.traps_restart_during(
            REPO_ROOT, run=run, ready=ready, now=clock.now, wall=lambda: 1000 + clock.t, sleep=clock.sleep
        )
        ((at, argv),) = calls
        self.assertEqual(at, uat.TRAPS_RECEIVER_DOWN_S)
        self.assertEqual(argv[-2:], ["start", uat.TRAPS_SERVICE])
        self.assertEqual(
            restart,
            uat.ReceiverRestart(started_s=1000 + uat.TRAPS_RECEIVER_DOWN_S, healthy_s=1032.0),
        )

    def test_the_during_hook_reports_a_failed_start_or_an_unhealthy_service(self) -> None:
        clock = Clock()

        def failing(argv, **_kwargs):
            return subprocess.CompletedProcess(argv, 1, "", "boom\n")

        def ok(argv, **_kwargs):
            return subprocess.CompletedProcess(argv, 0, "", "")

        self.assertEqual(
            uat.traps_restart_during(REPO_ROOT, run=failing, now=clock.now, sleep=clock.sleep),
            "<docker compose start telegraf-snmp exited 1: boom>",
        )
        self.assertEqual(
            uat.traps_restart_during(
                REPO_ROOT, run=ok, ready=lambda: False, now=clock.now, sleep=clock.sleep
            ),
            "<telegraf-snmp was not healthy within 30s of its start>",
        )


class RunRowBeforeTests(unittest.TestCase):
    def row(self, before) -> uat.MatrixRow:
        return uat.MatrixRow(
            name="t",
            profiles=(),
            subcommand="metrics",
            scenario=Path("scenario.yaml"),
            verify_url="",
            failure_log_containers=("x",),
            before=before,
            post_verify=lambda *_: "",
        )

    def run_row(self, row: uat.MatrixRow, ran: list) -> uat.RowResult:
        def run_scenario(*_, **__):
            ran.append(time.monotonic())
            return subprocess.CompletedProcess([], 0, "", "")

        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "scenario.yaml").write_text("")
            with mock.patch.object(uat, "run_scenario", run_scenario):
                return uat.run_row(row, Path("sonda"), Path(root))

    def test_a_failed_before_hook_fails_the_row_and_runs_no_sonda(self) -> None:
        ran: list = []
        result = self.run_row(self.row(lambda _root: "the receiver is still up"), ran)
        self.assertFalse(result.ok)
        self.assertEqual(result.message, "the before hook failed: the receiver is still up")
        self.assertEqual(ran, [])

    def test_the_before_hook_runs_before_sonda(self) -> None:
        ran: list = []
        hooked: list = []
        result = self.run_row(self.row(lambda _root: hooked.append(time.monotonic()) or ""), ran)
        self.assertTrue(result.ok, result.message)
        self.assertEqual((len(hooked), len(ran)), (1, 1))
        self.assertLess(hooked[0], ran[0])


class MatrixRowTests(unittest.TestCase):
    def rows(self) -> dict[str, uat.MatrixRow]:
        return {row.name: row for row in uat.MATRIX}

    def test_both_rows_run_the_example_after_the_if_mib_row(self) -> None:
        names = [row.name for row in uat.MATRIX]
        at = names.index("snmp-if-mib")
        self.assertEqual(names[at + 1 : at + 3], ["snmp-traps", "snmp-traps-restart"])
        for name in ("snmp-traps", "snmp-traps-restart"):
            row = self.rows()[name]
            self.assertEqual(row.profiles, ("snmp",))
            self.assertEqual(row.scenario, Path("examples/snmp-traps.yaml"))
            self.assertTrue((REPO_ROOT / row.scenario).is_file())
            self.assertEqual(row.extra_args, ("--duration", "140s"))
            self.assertEqual(row.failure_log_containers[0], uat.TRAPS_SERVICE)
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(row.verify_url).query)
            self.assertEqual(query["query"], [uat.TRAPS_VERIFY_QUERY])
            self.assertFalse(row.during_gets_sonda)

    def test_the_plain_row_has_no_hooks_and_the_restart_row_all_three(self) -> None:
        plain = self.rows()["snmp-traps"]
        self.assertIsNone(plain.before)
        self.assertIsNone(plain.during)
        self.assertIs(plain.post_verify, uat.verify_traps_row)
        restart = self.rows()["snmp-traps-restart"]
        self.assertIs(restart.before, uat.stop_trap_receiver)
        self.assertIs(restart.during, uat.traps_restart_during)
        self.assertIs(restart.post_verify, uat.verify_traps_restart_row)
        # The run outlasts its outage, the coldStart's wait and the flap.
        self.assertGreater(restart.timeout_s, uat.TRAPS_RUN_S + 55.0)

    def test_the_matrix_table_lists_the_example_once(self) -> None:
        lines = (REPO_ROOT / "docs/site/docs/test/end-to-end-pipelines.md").read_text().splitlines()
        (at,) = [i for i, line in enumerate(lines) if "| Signal | Encoder |" in line]
        indent = lines[at][: len(lines[at]) - len(lines[at].lstrip())]
        end = at
        while end < len(lines) and lines[end].startswith(f"{indent}|"):
            end += 1
        table = lines[at:end]
        self.assertTrue(
            any("`examples/snmp-telegraf.yaml`" in line for line in table),
            "the block found is the matrix: it holds the snmp row",
        )
        rows = [line for line in table if "`examples/snmp-traps.yaml`" in line]
        self.assertEqual(len(rows), 1, "one row of the matrix table names the example")
        stray = [
            line
            for line in lines[:at] + lines[end:]
            if line.lstrip().startswith("|") and "`examples/snmp-traps.yaml`" in line
        ]
        self.assertEqual(stray, [], "no row naming the example sits outside the table")


class TelegrafConfTests(unittest.TestCase):
    def conf(self) -> dict:
        return tomllib.loads((REPO_ROOT / uat.SNMP_TELEGRAF_CONF).read_text())

    def test_the_trap_input_listens_where_the_example_sends(self) -> None:
        import yaml

        (traps,) = self.conf()["inputs"]["snmp_trap"]
        self.assertEqual(traps["alias"], "traps")
        self.assertEqual(traps["service_address"], "udp://:1162")
        sink = yaml.safe_load((REPO_ROOT / uat.TRAPS_SCENARIO).read_text())["defaults"]["sink"]
        self.assertEqual(sink["destination"], "127.0.0.1:1162")
        compose = yaml.safe_load((REPO_ROOT / uat.COMPOSE_FILE).read_text())
        self.assertIn("1162:1162/udp", compose["services"][uat.TRAPS_SERVICE]["ports"])

    def test_the_traps_are_translated_by_gosmi(self) -> None:
        # netsnmp, Telegraf's default, drops every trap in an image without
        # MIB files.
        self.assertEqual(self.conf()["agent"]["snmp_translator"], "gosmi")

    def test_each_trap_is_printed_as_one_json_line(self) -> None:
        (file_output,) = self.conf()["outputs"]["file"]
        self.assertEqual(file_output["files"], ["stdout"])
        self.assertEqual(file_output["data_format"], "json")
        self.assertEqual(file_output["json_timestamp_units"], "1ms")
        self.assertEqual(file_output["namepass"], ["snmp_trap"])


if __name__ == "__main__":
    unittest.main()
