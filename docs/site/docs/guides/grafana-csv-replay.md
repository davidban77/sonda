# Grafana CSV Export Replay

You have a Grafana dashboard showing a production incident. You want to replay those exact metric
values through your pipeline -- same shapes, same labels, same timing -- to verify alert rules,
test recording rules, or validate a new ingest path. Sonda can replay a Grafana CSV export with
zero manual column mapping.

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

Each column header encodes the metric name and labels in `{key="value"}` syntax. Sonda parses
these automatically.

---

## Replay With Auto-Discovery

Point Sonda at the exported CSV. When `columns` is omitted, Sonda reads the header row,
auto-detects it as a header (non-numeric fields), extracts metric names and labels from each
column, and creates independent metric streams.

```yaml title="examples/csv-replay-grafana-auto.yaml"
name: grafana_replay
rate: 1
duration: 60s

generator:
  type: csv_replay
  file: examples/grafana-export.csv

labels:
  env: production

encoder:
  type: prometheus_text
sink:
  type: stdout
```

```bash
sonda metrics --scenario examples/csv-replay-grafana-auto.yaml
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
name: system_metrics
rate: 1
duration: 60s

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

encoder:
  type: prometheus_text
sink:
  type: stdout
```

```bash
sonda metrics --scenario examples/csv-replay-explicit-labels.yaml
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

Sonda recognizes five header formats. The first two are what Grafana produces; the others
support hand-authored CSV files.

| Format | Example header | Metric name | Labels |
|--------|---------------|-------------|--------|
| 1. `__name__` inside braces | `{__name__="up", instance="host", job="prom"}` | `up` | `instance`, `job` |
| 2. Name before braces | `up{instance="host", job="prom"}` | `up` | `instance`, `job` |
| 3. Labels only (no name) | `{instance="host", job="prom"}` | none (error with auto-discovery) | `instance`, `job` |
| 4. Plain metric name | `cpu_percent` | `cpu_percent` | none |
| 5. Simple word | `prometheus` | `prometheus` | none |

Format 1 is what Grafana exports by default when you use **Series joined by time**. Format 2
appears when a Grafana panel has a custom `legendFormat` that puts the metric name outside the
braces.

!!! info "Format 3 requires explicit columns"
    Headers with labels but no `__name__` cannot be used with auto-discovery because there is no
    metric name to extract. Use `columns:` with an explicit `name:` for each column instead.

??? tip "Grafana legendFormat and header format"
    If your Grafana panel has a custom `legendFormat` (e.g., `{{instance}}`), the CSV headers
    will reflect that format instead of the raw `{__name__=...}` syntax. If the headers no
    longer include the metric name, switch to `columns:` with explicit names, or clear
    `legendFormat` before exporting.

---

## Quick Reference

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | -- | Path to the CSV file. |
| `columns` | list | no | -- | Explicit column specs. When absent, columns are auto-discovered from the header. |
| `columns[].index` | integer | yes | -- | Zero-based column index in the CSV file. |
| `columns[].name` | string | yes | -- | Metric name for the expanded scenario. |
| `columns[].labels` | map | no | none | Per-column labels merged with scenario-level labels. Column labels override on conflict. |
| `repeat` | boolean | no | `true` | Cycle back to start or hold last value. |

For the full CSV replay parameter reference, see
[Generators: csv_replay](../configuration/generators.md#csv_replay).
