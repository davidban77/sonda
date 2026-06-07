# Log CSV Replay

This page covers `log_csv_replay`. You point it at a structured CSV, and Sonda replays each row as a `LogEvent` at the cadence recorded in the file. It is the log-side counterpart of [`csv_replay` for metrics](grafana-exports.md).

A common case: you captured a 10-minute window around an incident from Loki with `logcli`. You want to send the same lines back through your pipeline with the same severities, fields, and timing.

The replay rate is derived from the CSV's `timestamp` column. The `rate:` you set in YAML is ignored. A 10-minute window in the CSV replays over 10 minutes of wall clock time with no manual rate tuning.

---

## CSV format

`log_csv_replay` expects a CSV with three semantic columns plus any number of free-form field columns:

```csv title="sample-logs.csv"
timestamp,severity,message,user_id
1700000000,info,GET /api/v1/health returned 200,u-42
1700000003,info,GET /api/v1/metrics returned 200,u-17
1700000006,warn,GET /api/v1/users returned 200 with high latency,u-91
1700000009,info,POST /api/v1/events returned 201,u-42
1700000012,error,POST /api/v1/events returned 500: upstream timeout,u-19
```

| Column | Role | Required | Behavior |
|--------|------|----------|----------|
| `timestamp` | Drives the replay rate via median Δt | yes | Epoch seconds, or epoch milliseconds when the value is greater than 1e12. |
| `severity` | Maps to `LogEvent::severity` | no | Falls back to `default_severity` when missing or unparseable. |
| `message` | Maps to `LogEvent::message` | no | An empty cell becomes an empty string. |
| everything else | Becomes an entry in `LogEvent::fields` | no | The column name is the field key. Empty cells are omitted. |

Sonda detects each role from the header row, case-insensitive. The aliases below are matched in order:

- `timestamp` / `ts` / `time` → timestamp role
- `severity` / `level` → severity role
- `message` / `msg` / `log` → message role

Any header that does not match one of these becomes a free-form field column. Headers like `user_id`, `trace_id`, or `pod` are written as a `fields` entry on every emitted event.

When the header names don't match the conventions, use [explicit `columns:`](#explicit-column-mapping) to map Sonda to the right columns by name.

---

## The minimal scenario

```yaml title="examples/log-csv-replay.yaml"
version: 2
kind: runnable

defaults:
  duration: 60s
  encoder:
    type: json_lines
  sink:
    type: stdout

scenarios:
  - signal_type: logs
    name: app_logs_csv_replay
    rate: 1
    log_generator:
      type: csv_replay
      file: examples/sample-logs.csv
      default_severity: info
      repeat: true
```

```bash
sonda -q run examples/log-csv-replay.yaml --duration 11s
```

```text title="Output (first three events)"
{"timestamp":"2026-05-15T18:37:55.791Z","severity":"info","message":"GET /api/v1/health returned 200","labels":{},"fields":{"user_id":"u-42"}}
{"timestamp":"2026-05-15T18:37:55.791Z","severity":"info","message":"GET /api/v1/metrics returned 200","labels":{},"fields":{"user_id":"u-17"}}
{"timestamp":"2026-05-15T18:37:55.791Z","severity":"warn","message":"GET /api/v1/users returned 200 with high latency","labels":{},"fields":{"user_id":"u-91"}}
```

The CSV has a 3-second step between timestamps. Sonda derives `rate = 1 / 3 ≈ 0.333 events/s` and emits one event every three seconds. The `rate: 1` in YAML is replaced. A `tracing::warn!` records the override:

```text title="Override warning on stderr"
WARN log_csv_replay 'app_logs_csv_replay': overriding rate=1 with derived rate=0.3333333333333333 samples/s (CSV Δt=3s, timescale=1)
```

The `timestamp` field on each emitted event is the wall-clock time at emission, not the CSV row's timestamp. The CSV column is only used to derive the cadence. Severity, message, and field values are taken from the CSV without changes.

---

## CSV-derived rate overrides YAML `rate:`

`log_csv_replay` behaves like the metrics-side `csv_replay`. The scenario's `rate:` is always replaced by `timescale / median_delta_t`. The value `median_delta_t` is the median interval between consecutive timestamps in the `timestamp` column. Setting `rate:` in YAML has no effect on emission cadence.

```yaml title="Speed up a 10-minute log window into 1 minute"
scenarios:
  - signal_type: logs
    name: incident_replay
    rate: 1               # ignored -- CSV Δt and timescale drive the rate
    log_generator:
      type: csv_replay
      file: incident-2026-05-12.csv
      timescale: 10.0     # play back 10x faster
```

`timescale` must be a positive finite number (`> 0`). The default is `1.0`.

!!! info "How the derivation works"
    Sonda reads the timestamp column for up to 100 data rows. It parses each cell as a number and computes the median of consecutive differences. Values larger than `1e12` are read as epoch milliseconds. Smaller values are read as epoch seconds. The derived rate is `timescale / median_delta`. Run `sonda --verbose --dry-run` to confirm the value, or check the startup banner.

---

## Pulling a CSV out of Loki with `logcli`

A typical workflow: you have a real log stream in Loki. You want to extract a window of it as a CSV that `log_csv_replay` can read.

Loki's official CLI `logcli` produces JSON output by default. The format you want for `log_csv_replay` is `timestamp,severity,message,...fields`. The conversion is a small `jq` pipeline.

### Step 1: query the window from Loki

```bash
logcli query \
  --from="2026-05-12T14:00:00Z" \
  --to="2026-05-12T14:10:00Z" \
  --output=jsonl \
  '{app="api-gateway"}' > raw-logs.jsonl
```

Each line is one log entry:

```json
{"timestamp":"2026-05-12T14:00:00.123Z","labels":{"app":"api-gateway","level":"info","pod":"api-7c4f9"},"line":"GET /api/v1/health returned 200"}
```

### Step 2: project the columns you need

Use `jq` to flatten each entry into a CSV row. The `timestamp` comes from the entry timestamp. The `severity` comes from the labels. The `message` is `line`. Other values become field columns.

```bash
jq -r '
  [
    (.timestamp | fromdate),
    (.labels.level // "info"),
    .line,
    .labels.pod,
    .labels.trace_id
  ] | @csv
' raw-logs.jsonl > body.csv

echo "timestamp,severity,message,pod,trace_id" > incident.csv
cat body.csv >> incident.csv
```

You now have `incident.csv`:

```csv
timestamp,severity,message,pod,trace_id
1715522400,info,"GET /api/v1/health returned 200","api-7c4f9","abc123"
1715522401,warn,"GET /api/v1/users high latency","api-7c4f9","abc124"
...
```

### Step 3: replay

```yaml title="loki-replay.yaml"
version: 2
kind: runnable

defaults:
  duration: 10m
  encoder:
    type: json_lines
  sink:
    type: loki
    endpoint: http://localhost:3100/loki/api/v1/push
    labels:
      source: replay
      job: api-gateway-replay

scenarios:
  - signal_type: logs
    name: api_gateway_replay
    log_generator:
      type: csv_replay
      file: incident.csv
      default_severity: info
      repeat: false
```

```bash
sonda run loki-replay.yaml
```

Sonda derives the replay rate from the timestamps in `incident.csv`. A 10-minute window replays over 10 minutes. Each event is sent to Loki tagged with `source="replay"`, so you can query the originals and the replay side by side.

!!! tip "Add a tag label, not a CSV column"
    Use the sink's `labels:` block to add `source=replay` or `run_id=...` to every event. Adding the value in the CSV would create a redundant `LogEvent.fields` entry on every row.

---

## Explicit column mapping

When the CSV header uses names that don't match the auto-discovery aliases, declare them with `columns:`. Examples: `ts` instead of `timestamp`, `sev` instead of `severity`, `text` instead of `message`.

```yaml title="Explicit column mapping"
scenarios:
  - signal_type: logs
    name: custom_headers
    log_generator:
      type: csv_replay
      file: weird-headers.csv
      columns:
        timestamp: ts
        severity: sev
        message: text
```

Explicit `columns:` overrides auto-discovery. Any column not listed and not auto-matched becomes a field column.

---

## Failure modes

| Error message | Cause | Fix |
|---------------|-------|-----|
| `csv_replay: file "..." has fewer than 2 data rows; cannot derive replay rate` | The CSV only has a header and zero or one data rows. | At least two data rows are needed to measure the sample interval. Export a wider window. |
| `csv_replay: non-monotonic timestamps in "..." (row N value X <= previous Y)` | A timestamp goes backward or repeats. Common with concatenated exports or paused recordings. | Sort the CSV by timestamp, deduplicate, or split it at the discontinuity. |
| `csv_replay: 'timescale' must be a positive finite number, got 0` | `timescale: 0`, a negative value, or `NaN` / `Inf`. | Set `timescale` to a positive number, or remove it to use the default `1.0`. |
| `log_csv_replay: CSV content is empty` | The file is empty or contains only comments. | Re-export the window with actual data rows. |
| `log_csv_replay: column "X" not found in CSV header` | An explicit `columns:` mapping references a header name that does not exist. | Check the header row of the CSV. Column name matching is case-insensitive. |

### Severity fallback (soft-fail)

When a row's severity cell is empty or contains an unrecognised value like `bogus`, Sonda does **not** error. Instead, the row falls back to `default_severity`, which is `Info` by default. At expand time, Sonda emits one summary warn line counting how many rows used the fallback:

```text
WARN log_csv_replay 'incident_replay': 7 row(s) used default_severity due to missing or unparseable severity values
```

This is a per-scenario summary, not per-event. If the fallback count is high relative to the row count, check the severity column in your CSV. Typos in custom severity values often appear this way.

Valid severity strings: `trace`, `debug`, `info`, `warn` (or `warning`), `error`, `fatal`. Case-insensitive.

### Empty messages and fields

- An empty `message` cell produces an empty string in the emitted event. It is not an error.
- An empty field cell is **omitted** from the row's `fields` map. A row with `user_id,,trace_id=abc` produces `{"trace_id":"abc"}`, with no `user_id` key, rather than `{"user_id":""}`.

---

## Quick reference

```yaml title="All fields"
log_generator:
  type: csv_replay
  file: path/to/logs.csv
  timescale: 1.0           # optional, default 1.0, must be > 0
  default_severity: info   # optional, default 'info'
  repeat: true             # optional, default true
  columns:                 # optional -- when omitted, auto-discover from header
    timestamp: timestamp
    severity: severity
    message: message
```

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | -- | Path to the CSV file, relative to the working directory where you run `sonda`. |
| `timescale` | float | no | `1.0` | Replay speed multiplier. `2.0` is 2x faster, `0.5` is 2x slower. Must be strictly positive. |
| `default_severity` | string | no | `info` | Fallback severity when the severity column is missing, empty, or unparseable. One of `trace`, `debug`, `info`, `warn`, `error`, `fatal`. |
| `repeat` | boolean | no | `true` | When true, cycles back to the start of the CSV. When false, holds the last row for all later ticks. |
| `columns` | object | no | auto-discover | Explicit name-based column mapping. Sub-fields: `timestamp`, `severity`, `message`. Any column not named here and not auto-matched becomes a field column. |

!!! note "The scenario's `rate:` is always replaced"
    For `log_csv_replay`, `rate:` is computed from the CSV's `timestamp` column and `timescale`. Any value you set in YAML is replaced. Run `sonda --verbose --dry-run` to confirm the derived rate, or check the startup banner.

For the full generator reference, see [Generators: log csv_replay](../build/generators.md#csv_replay_1). For the metrics-side workflow, see [Grafana CSV Replay](grafana-exports.md).

---

## Coming from `type: replay`?

Earlier versions included a `type: replay` log generator that cycled lines from a plain text file at a hand-tuned `rate:`. It hardcoded severity to `Info`, had no field support, and ignored timestamps. **It has been removed.**

If you have an existing YAML that uses `type: replay`, the migration is:

1. Convert your text log file into a CSV with at least a `timestamp` column. The simplest CSV is `timestamp,message` with synthetic timestamps spaced at your old replay rate. For example, 200ms apart for `rate: 5`.
2. Change `type: replay` to `type: csv_replay`.
3. Remove the YAML `rate:`. The rate is derived from the CSV now.

```yaml title="Before (no longer works)"
log_generator:
  type: replay
  file: app.log
```

```yaml title="After"
log_generator:
  type: csv_replay
  file: app.csv
  default_severity: info
```

If your source data is unstructured text and you cannot convert it to CSV, raw-file replay is planned. It will live in a separate `sonda-integrations` adapter. Track [issue #347](https://github.com/davidban77/sonda/issues/347) for status.
