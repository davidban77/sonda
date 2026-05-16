# Grafana CSV Export Replay

You have a Grafana dashboard showing a production incident. You want to replay those exact metric values through your pipeline -- same shapes, same labels, same timing -- to verify alert rules, test recording rules, or validate a new ingest path. Sonda can replay a Grafana CSV export with zero manual column mapping, and the replay preserves the original sample interval automatically.

---

## Export From Grafana

Open the panel you want to replay, then extract the data as CSV.

1. Click the panel title and select **Inspect** (or press `e` then switch to the **Inspect** tab).
2. Switch to the **Data** tab.
3. In the **Data options** dropdown, select **Series joined by time**.
4. Click **Download CSV**.

!!! warning "Use 'Series joined by time'"
    The default per-series view exports one CSV file per series. The "Series joined by time" option
    produces a single file with one time column and one data column per series -- this is the format
    Sonda's auto-discovery expects.

The exported CSV looks like this:

```csv title="grafana-export.csv"
"Time","{__name__=""up"", instance=""localhost:9090"", job=""prometheus""}","{__name__=""up"", instance=""localhost:9100"", job=""node""}"
1704067200000,1,1
1704067215000,1,1
1704067230000,0,1
1704067245000,1,0
1704067260000,1,1
```

Each column header encodes the metric name and labels in `{key="value"}` syntax. Sonda parses these automatically.

---

## Replay Speed Is Driven By The CSV, Not By `rate:`

Sonda reads the first column of the CSV as a timestamp series, measures the median interval between samples, and uses that to compute the replay rate. The `rate:` field on a `csv_replay` scenario is **always overridden** by this derived value -- it does not matter what you put in YAML.

This is the most common point of confusion when moving from earlier releases. Before, you had to set `rate: 0.1` by hand to match a 10-second Grafana scrape interval; if you set the wrong rate, a 5-minute incident would play back in 30 seconds.

```csv title="Grafana export with 15s scrape interval"
"Time","{__name__=""cpu"", instance=""prod-01""}"
1704067200000,42.1
1704067215000,43.5
1704067230000,45.8
```

```yaml title="examples/csv-replay-grafana-auto.yaml"
defaults:
  rate: 1      # ignored for csv_replay -- the CSV's 15s step wins
scenarios:
  - signal_type: metrics
    name: incident_replay
    generator:
      type: csv_replay
      file: examples/grafana-export.csv
```

```text title="Startup banner shows the derived rate"
[1/1] ▶ cpu  signal_type: metrics | rate: 0.1/s | ...
```

The displayed `0.1/s` is the rounded view of `1 / 15` (about 0.0667 samples per second), computed from the CSV. The actual emission cadence matches the 15-second step exactly, not the rounded display value, so the 5-minute incident replays in 5 minutes.

The scenario `name: incident_replay` is replaced with `cpu` because each CSV column expands into its own scenario named after the column's `__name__`. See [Replay With Auto-Discovery](#replay-with-auto-discovery) below for details.

!!! info "How the derivation works"
    Sonda reads column 0 as a timestamp series, parses each cell as a number, and computes the **median** of consecutive differences across up to the first 100 data rows. Values larger than `1e12` are treated as epoch milliseconds; smaller values are treated as epoch seconds. Both Grafana (ms) and VictoriaMetrics (s) exports are covered. The derived rate is `timescale / median_delta`.

### Speeding up or slowing down with `timescale`

Use `timescale:` to play the recording faster or slower without rewriting the CSV.

| `timescale` | Effect | Use case |
|-------------|--------|----------|
| `1.0` (default) | Play at the original speed | Fidelity replay -- 1h of source data plays in 1h |
| `2.0` | Play 2x faster | Replay 1h in 30min for faster alert-rule iteration |
| `10.0` | Play 10x faster | Squash an overnight incident into a 5-minute test |
| `0.5` | Play 2x slower | Stretch a 1-minute event over 2 minutes for visual inspection |

```yaml title="Replay 1 hour of production data in 5 minutes"
scenarios:
  - signal_type: metrics
    name: chaos_replay
    generator:
      type: csv_replay
      file: production-incident.csv
      timescale: 12.0      # 60 min CSV / 12 = 5 min replay
```

`timescale` must be a positive finite number. `timescale: 0` or a negative value is rejected at config load with `csv_replay: 'timescale' must be a positive finite number, got 0`.

---

## Replay With Auto-Discovery

Point Sonda at the exported CSV. When `columns` is omitted, Sonda reads the header row,
auto-detects it as a header (non-numeric fields), extracts metric names and labels from each
column, and creates independent metric streams.

```yaml title="examples/csv-replay-grafana-auto.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: grafana_replay
    generator:
      type: csv_replay
      file: examples/grafana-export.csv
    labels:
      env: production
```

```bash
sonda run examples/csv-replay-grafana-auto.yaml
```

```text title="Output"
up{env="production",instance="localhost:9090",job="prometheus"} 1 1775505698611
up{env="production",instance="localhost:9100",job="node"} 1 1775505698611
up{env="production",instance="localhost:9090",job="prometheus"} 1 1775505699621
up{env="production",instance="localhost:9100",job="node"} 1 1775505699621
```

Each CSV data column becomes its own scenario. The `name` field in your YAML is ignored --
Sonda uses the metric name extracted from each column header instead.

### How labels merge

Labels come from two sources:

- **Header labels** -- extracted from the CSV column header (e.g., `instance`, `job`).
- **Scenario labels** -- defined in the `labels:` block of your YAML (e.g., `env: production`).

Sonda merges both sets. If the same key appears in both, the **header label wins**. In this
example, the output includes `env="production"` (from the scenario) alongside `instance` and
`job` (from the header).

!!! tip "Adding context labels"
    Use scenario-level `labels:` to tag replayed data with metadata that was not in the original
    export -- environment, team, test run ID, or anything your pipeline needs for routing.

---

## Explicit Per-Column Labels

When you need more control -- custom metric names, extra labels per column, or you are working
with a hand-authored CSV that has plain headers -- use `columns:` with the `labels` sub-field.

```yaml title="examples/csv-replay-explicit-labels.yaml"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: system_metrics
    generator:
      type: csv_replay
      file: examples/sample-multi-column.csv
      columns:
        - index: 1
          name: cpu_percent
          labels:
            core: "0"
        - index: 2
          name: mem_percent
          labels:
            type: physical
        - index: 3
          name: disk_io_mbps
    labels:
      instance: prod-server-42
      job: node
```

```bash
sonda run examples/csv-replay-explicit-labels.yaml
```

```text title="Output"
cpu_percent{core="0",instance="prod-server-42",job="node"} 12.3 1775505711361
mem_percent{instance="prod-server-42",job="node",type="physical"} 45.2 1775505711361
disk_io_mbps{instance="prod-server-42",job="node"} 5.1 1775505711361
```

Per-column labels merge with scenario-level labels, and column labels override on conflict. The
`disk_io_mbps` column has no per-column labels, so it gets only the scenario-level ones.

---

## Supported Header Formats

Sonda recognizes five header formats. The first two are what Grafana produces; the others support hand-authored CSV files.

| Format | Example header | Metric name | Labels |
|--------|---------------|-------------|--------|
| 1. `__name__` inside braces | `{__name__="up", instance="host", job="prom"}` | `up` | `instance`, `job` |
| 2. Name before braces | `up{instance="host", job="prom"}` | `up` | `instance`, `job` |
| 3. Labels only (no name) | `{instance="host", job="prom"}` | from `default_metric_name` | `instance`, `job` |
| 4. Plain metric name | `cpu_percent` | `cpu_percent` | none |
| 5. Simple word | `prometheus` | `prometheus` | none |

Format 1 is what Grafana exports by default when you use **Series joined by time**. Format 2 appears when a Grafana panel has a custom `legendFormat` that puts the metric name outside the braces. Format 3 is what you get when `legendFormat` strips the metric name entirely (e.g. `{{instance}}` only) -- this is covered by [`default_metric_name`](#labels-only-headers-default_metric_name).

### Labels-only headers: `default_metric_name`

When a Grafana panel uses a `legendFormat` that omits `__name__`, the export looks like this:

```csv title="labels-only export"
Time,"{instance=""prod-01"",job=""node""}","{instance=""prod-02"",job=""node""}"
1704067200000,42.1,38.5
1704067210000,43.2,39.0
```

Before, you had to hand-write a script to inject `__name__=metric` into every header before sonda could ingest the file. Now, set `default_metric_name:` on the generator and the missing name is filled in automatically.

```yaml title="Replay a labels-only Grafana export"
scenarios:
  - signal_type: metrics
    name: cpu_replay
    generator:
      type: csv_replay
      file: cpu-export.csv
      default_metric_name: node_cpu_usage
```

```text title="Output"
node_cpu_usage_1{instance="prod-01",job="node"} 42.1 1778847012268
node_cpu_usage_2{instance="prod-02",job="node"} 38.5 1778847012268
```

Naming rules:

- **One** column without `__name__` -- uses `default_metric_name` verbatim. `default_metric_name: node_cpu_usage` produces `node_cpu_usage`.
- **Multiple** columns without `__name__` -- each gets the fallback name suffixed with `_<column_index>` to keep series unique. `node_cpu_usage_1`, `node_cpu_usage_2`, and so on.
- Columns whose header already has `__name__` (or name-before-braces) are unaffected -- they keep their own name and only the nameless columns use the fallback.

??? tip "Grafana legendFormat and header format"
    If your Grafana panel has a custom `legendFormat` (e.g., `{{instance}}`), the CSV headers will reflect that format instead of the raw `{__name__=...}` syntax. You have three options: set `default_metric_name:` on the generator (recommended), clear `legendFormat` before exporting, or switch to `columns:` with explicit `name:` for each column.

---

## Failure modes

| Error message | Cause | Fix |
|---------------|-------|-----|
| `csv_replay: 'timescale' must be a positive finite number, got 0` | `timescale: 0`, a negative value, or `NaN`/`Inf`. | Set `timescale` to a positive number, or remove it to use the default `1.0`. |
| `csv_replay: file "..." has fewer than 2 data rows; cannot derive replay rate` | The CSV only has a header and one data row (or zero). | At least two data rows are required to measure the sample interval. Re-export with a wider time range. |
| `csv_replay: non-monotonic timestamps in "..." (row N value X <= previous Y)` | A timestamp goes backward or repeats. Common with concatenated exports or paused recordings. | Sort the file by timestamp, deduplicate, or split it at the discontinuity. |
| `csv_replay: column N has no metric name (header has labels only with no __name__); set 'default_metric_name' on the generator config` | Auto-discovery hit a `{labels...}` header without a metric name. | Add `default_metric_name:` to the generator, or switch to explicit `columns:`. |
| `generator error: cannot read file "..."` | The CSV path does not exist or is not readable. | Paths are relative to the directory where `sonda` is launched, not to the scenario file. |

---

## Quick Reference

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | -- | Path to the CSV file. |
| `columns` | list | no | -- | Explicit column specs. When absent, columns are auto-discovered from the header. |
| `columns[].index` | integer | yes | -- | Zero-based column index in the CSV file. |
| `columns[].name` | string | yes | -- | Metric name for the expanded scenario. |
| `columns[].labels` | map | no | none | Per-column labels merged with scenario-level and header-derived labels. Column labels override on conflict. |
| `repeat` | boolean | no | `true` | Cycle back to start or hold last value. |
| `timescale` | float | no | `1.0` | Replay speed multiplier. `2.0` plays 2x faster, `0.5` plays 2x slower. Must be strictly positive. |
| `default_metric_name` | string | no | -- | Fallback metric name for auto-discovered columns whose header has labels but no `__name__`. Suffixed with `_<column_index>` when multiple columns share the fallback. |

!!! note "The scenario's `rate:` is always overridden"
    For `csv_replay` scenarios, `rate:` is computed from the CSV's column-0 timestamps and `timescale`. Any value you set in YAML is replaced. Run `sonda --verbose --dry-run` to confirm the derived rate, or inspect the startup banner.

For the full CSV replay parameter reference, see [Generators: csv_replay](../configuration/generators.md#csv_replay).

!!! tip "Want portable scenarios instead of raw replay?"
    `csv_replay` plays back exact values from the file. If you want to extract the *pattern*
    from the data and generate a self-contained scenario YAML that does not depend on the
    original file, use [`sonda new --from`](csv-import.md) instead.

!!! tip "Replaying logs instead of metrics?"
    The same workflow works for log events with `log_csv_replay`. Export the window from Loki via `logcli`, run it through `jq` to produce a `timestamp,severity,message,...fields` CSV, and point a logs scenario at the file. The rate-derivation, `timescale`, and override-warn semantics described above apply identically. Walkthrough: [Log CSV Replay](log-csv-replay.md).
