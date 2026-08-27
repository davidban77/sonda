---
title: From a live Prometheus
description: Capture a PromQL range query with sonda new --from-prometheus and replay it verbatim — recorded values, recorded gaps, no pattern fitting.
---

# Capture from a live Prometheus

`sonda new --from-prometheus` runs one PromQL range query against a live
Prometheus-compatible TSDB and writes the two files that replay it: a CSV of the
values it recorded, and a scenario that plays them back at the cadence they were
captured at.

The case this closes is **alert regression**. An alert fired during an incident;
you want it to fire again, in CI, on the same numbers — not on a synthetic curve
that resembles them. So the values are replayed verbatim, and the gaps in the
data are replayed as gaps.

---

## The recipe

```bash
sonda new --from-prometheus http://localhost:9090 \
  --query 'up{job="api"}' \
  --range 1h --step 15s \
  --out incident.csv -o incident.yaml
```

That writes two files and prints a line to stderr saying what it captured:

```text title="stderr"
captured 1 series over 241 points to incident.csv (1 missing sample)
wrote incident.yaml
```

Then replay it like any other scenario:

```bash
sonda run incident.yaml
```

`incident.yaml` references `incident.csv` by the path you gave `--out`, so keep
the pair together — or pass an absolute path if the scenario will run from
somewhere else.

### The window

Either a duration ending now:

```bash
sonda new --from-prometheus http://localhost:9090 --query up \
  --range 30m --step 15s --out capture.csv
```

…or explicit bounds, as unix seconds or RFC 3339:

```bash
sonda new --from-prometheus http://localhost:9090 --query up \
  --start 2026-05-14T09:00:00Z --end 2026-05-14T10:00:00Z \
  --step 15s --out capture.csv
```

`--step` is the sample step of the query *and* the replay grid: one CSV row per
step, one replay tick per row.

### Credentials

Pass a bearer token through the environment, never a flag — a flag value lands in
shell history, process listings and CI logs:

```bash
SONDA_PROM_TOKEN=… sonda new --from-prometheus https://prom.example.com \
  --query up --range 1h --step 15s --out capture.csv
```

For anything else, `--header` (repeatable):

```bash
sonda new --from-prometheus https://prom.example.com \
  --header 'X-Scope-OrgID: tenant-7' \
  --query up --range 1h --step 15s --out capture.csv
```

Both can be set at once. An explicit `--header Authorization:` wins over
`SONDA_PROM_TOKEN`, and says so on stderr rather than quietly dropping one of the
two. Neither ever reaches the emitted CSV or YAML.

---

## Aggregated queries need `--metric-name`

A query matching more than 20 series is refused — past that a capture stops being
a scenario anyone can read. The usual way under the cap is to aggregate, and
**PromQL aggregations drop `__name__`**, so the emitted scenario would have no
metric name to use. Supply one:

```bash
sonda new --from-prometheus http://localhost:9090 \
  --query 'sum by (job) (rate(http_errors_total[5m]))' \
  --metric-name http_errors_per_second \
  --range 1h --step 15s --out errors.csv -o errors.yaml
```

`--metric-name` fills in only the series that lack a name; one the query kept is
never overwritten.

## Replaying faster than real time

`--timescale 4` replays a one-hour capture in fifteen minutes. It moves the rate,
the duration and the gap windows together, so the accelerated scenario describes
one consistent timeline:

```bash
sonda new --from-prometheus http://localhost:9090 --query up \
  --range 1h --step 15s --timescale 4 --out capture.csv -o fast.yaml
```

## Gaps are data

A grid point the database had no sample for becomes a blank cell in the CSV and a
[`gap_windows:`](../build/scheduling.md) entry in the scenario, so the replay goes
silent exactly where the original did. A scrape gap that made an alert fire is
part of the incident, and a capture that filled it in would not reproduce it.

Values are never interpolated, averaged or carried forward. A `NaN` the database
reported is a sample; a point it reported nothing for is a gap. They are
different facts and the capture keeps them apart.

---

## What this is not

Three fences worth stating plainly, because each one is a thing people reasonably
expect and none of them is here.

**Not a PromQL engine.** Sonda sends your query to Prometheus and reads the
result. It does not parse, rewrite, optimise or validate PromQL — a syntax error
comes back from the server, in the server's words. Anything your Prometheus can
answer, this can capture; anything it cannot, neither can this.

**Not backfill.** This reads *out* of a TSDB and writes files. It never writes
into one. If you need historical samples loaded into Prometheus, that is
`promtool tsdb create-blocks-from` or your vendor's import path, not Sonda.

**Not decomposition.** A capture is not analysed. Nothing here classifies the
shape of a signal, fits a generator to it, or hands you tunable parameters —
there is no `--fit`. You get the recorded numbers back.

If you want the *pattern* rather than the recording — a portable scenario with
knobs you can turn, that runs without the original file — that is
[`sonda new --from <csv>`](from-csv.md), which does classify and does fit. The
two answer different questions:

| | `--from-prometheus` | `--from <csv>` |
|---|---|---|
| Output | the recorded values | a fitted generator |
| Needs the data file at run time | yes | no |
| Parameterised afterwards | no | yes |
| Reproduces an incident exactly | yes | approximately |

---

## What gets written

The CSV carries one timestamp column and one column per series, with the label
set in the header:

```text title="incident.csv"
timestamp,"{__name__=""up"", job=""api""}"
1747213200.000,1
1747213215.000,1
1747213230.000,
1747213245.000,1
```

The scenario points at it, one entry per group of columns that share an absence
pattern and carry different metric names:

```yaml title="incident.yaml"
version: 2
kind: runnable
tags: []
scenarios:
- id: capture_0
  signal_type: metrics
  name: capture_0
  rate: 0.06666666666666667
  duration: 3615s
  generator:
    type: csv_replay
    file: incident.csv
    columns:
    - index: 1
      name: up
      labels:
        job: api
    repeat: false
  gap_windows:
  - at: 22.5s
    for: 15s
```

`repeat: false` is always written: a capture is a single pass, and looping one
that declares `gap_windows:` is refused by the engine.

One metric across several label sets — `up{job="api"}` and `up{job="db"}`, the
ordinary result of a range query — becomes one entry each, because a column name
becomes a scenario name and those must be unique.
