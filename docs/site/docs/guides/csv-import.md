# CSV Import

You have a CSV file -- maybe a Grafana export from a production incident, maybe a hand-recorded
dataset -- and you want to turn it into a portable, parameterized scenario that uses Sonda's
generators instead of replaying raw values. `sonda import` analyzes the data, detects dominant
patterns, and generates scenario YAML you can run, share, and customize.

---

## Why import instead of replay?

The [csv_replay](grafana-csv-replay.md) generator plays back raw CSV values verbatim. That is
useful for exact reproduction, but the output is tied to the original file. `sonda import`
takes a different approach:

- **Portable** -- the generated YAML uses generators (`steady`, `spike_event`, `leak`, `flap`,
  `sawtooth`, `step`), so it runs without the original CSV file.
- **Parameterized** -- you can tune rate, duration, and generator parameters after import.
- **Shareable** -- the YAML is self-contained. Drop it into a repo, CI pipeline, or Helm chart.

Use `csv_replay` when you need bit-for-bit fidelity. Use `sonda import` when you need the
*shape* of the data as a reusable scenario.

---

## The workflow

`sonda import` has three modes that form a natural pipeline:

```
CSV file  -->  --analyze  -->  -o scenario.yaml  -->  --run
              (understand)       (generate)          (execute)
```

### Step 1: Analyze

Start by understanding what the data looks like. `--analyze` is read-only -- it prints
detected patterns without generating any files.

```bash
sonda import examples/sample-multi-column.csv --analyze
```

```text title="Output"
CSV Import Analysis
============================================================

Column 1 (index 1): cpu_percent
  Data points: 20
  Range: [12.30, 96.10]  Mean: 46.27
  Detected pattern: steady (center=46.27, amplitude=41.90)

Column 2 (index 2): mem_percent
  Data points: 20
  Range: [45.20, 86.20]  Mean: 59.88
  Detected pattern: steady (center=59.88, amplitude=20.50)

Column 3 (index 3): disk_io_mbps
  Data points: 20
  Range: [5.00, 65.80]  Mean: 25.04
  Detected pattern: steady (center=25.04, amplitude=30.40)
```

Each column shows the metric name (from the header), basic statistics, and the detected
pattern with extracted parameters.

### Step 2: Generate

Once you know the patterns look right, generate a scenario YAML file:

```bash
sonda import examples/sample-multi-column.csv -o scenario.yaml
```

```text title="stderr"
wrote scenario to scenario.yaml
```

The generated file is a valid [v2 scenario YAML](../configuration/v2-scenarios.md), ready for
`sonda run --scenario`:

```yaml title="scenario.yaml (generated)"
version: 2

defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: cpu_percent
    signal_type: metrics
    name: cpu_percent
    generator:
      type: steady
      center: 46.27
      amplitude: 41.9
      period: "60s"

  - id: mem_percent
    signal_type: metrics
    name: mem_percent
    generator:
      type: steady
      center: 59.88
      amplitude: 20.5
      period: "60s"

  # ... (one entry per column)
```

!!! info "Output is always v2"
    `sonda import` emits v2 YAML regardless of column count. Single-column CSVs produce a
    one-entry `scenarios:` list; multi-column CSVs produce one entry per column. Shared
    `rate`, `duration`, `encoder`, and `sink` live in `defaults:`.

### Step 3: Run

If you just want to see the output without saving a file, `--run` generates the scenario in
memory and executes it immediately:

```bash
sonda -q import examples/sample-cpu-values.csv --run --duration 3s
```

```text title="Output"
cpu_percent 41.44404065390504 1775712694328
cpu_percent 46.07410906869991 1775712695333
cpu_percent 50.131242022026555 1775712696330
cpu_percent 55.42337922089686 1775712697333
```

---

## Grafana CSV exports

`sonda import` understands Grafana's "Series joined by time" CSV format. It parses the
`{__name__="...", key="value"}` headers to extract metric names and labels automatically.

```bash
sonda import examples/grafana-export.csv --analyze
```

```text title="Output"
CSV Import Analysis
============================================================

Column 1 (index 1): up
  Labels: {instance="localhost:9090", job="prometheus"}
  Data points: 10
  Range: [0.00, 1.00]  Mean: 0.80
  Detected pattern: sawtooth (min=0.00, max=1.00, period=4pts)

Column 2 (index 2): up
  Labels: {instance="localhost:9100", job="node"}
  Data points: 10
  Range: [0.00, 1.00]  Mean: 0.80
  Detected pattern: sawtooth (min=0.00, max=1.00, period=6pts)
```

Labels are preserved in the generated YAML:

```bash
sonda import examples/grafana-export.csv -o grafana-scenario.yaml
```

```yaml title="grafana-scenario.yaml (generated, first entry)"
version: 2

defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: up
    signal_type: metrics
    name: up
    generator:
      type: sawtooth
      min: 0.0
      max: 1.0
      period_secs: 4.0
    labels:
      instance: "localhost:9090"
      job: prometheus
```

For details on exporting from Grafana, see the
[Grafana CSV Export Replay](grafana-csv-replay.md#export-from-grafana) guide.

---

## Selecting columns

By default, all non-timestamp columns are imported. Use `--columns` to pick specific ones
by their zero-based index:

```bash
sonda import examples/sample-multi-column.csv --columns 1,3 --analyze
```

```text title="Output"
CSV Import Analysis
============================================================

Column 1 (index 1): cpu_percent
  Data points: 20
  Range: [12.30, 96.10]  Mean: 46.27
  Detected pattern: steady (center=46.27, amplitude=41.90)

Column 2 (index 3): disk_io_mbps
  Data points: 20
  Range: [5.00, 65.80]  Mean: 25.04
  Detected pattern: steady (center=25.04, amplitude=30.40)
```

Column 0 is always the timestamp and cannot be selected for import.

---

## Detected patterns

The pattern detector uses statistical analysis to classify each column into one of six
patterns. Each pattern maps to a Sonda generator or
[operational vocabulary](../configuration/generators.md) alias.

| Pattern | What it looks like | Generator / alias | Key parameters |
|---------|-------------------|-------------------|----------------|
| **Steady** | Low variance around a center | `steady` | center, amplitude, period |
| **Spike** | Periodic outliers above a baseline | `spike_event` | baseline, spike_height, spike_duration, spike_interval |
| **Climb** | Monotonic upward trend | `leak` | baseline, ceiling, time_to_ceiling |
| **Sawtooth** | Repeating climb-reset cycles | `sawtooth` | min, max, period_secs |
| **Flap** | Bimodal toggle (up/down) | `flap` | up_value, down_value, up_duration, down_duration |
| **Step** | Constant-rate counter increments | `step` | start, step_size |

The detector runs through these in priority order. When the data does not clearly match a
more specific pattern, it falls back to **steady**.

!!! info "Pattern detection is heuristic"
    The detector uses statistical thresholds (linear regression, IQR outlier detection, k-means
    clustering) to classify patterns. With very short time series (fewer than 10 data points),
    detection accuracy decreases. For best results, export at least 20-30 data points.

---

## Customizing generated scenarios

The generated YAML is a starting point. After import, you can:

- **Change the sink** -- replace `stdout` with `remote_write`, `loki`, or any other sink.
- **Adjust parameters** -- tune `amplitude`, `period`, or `baseline` to match your needs.
- **Add scheduling** -- add `gaps:`, `bursts:`, or `cardinality_spike:` blocks.
- **Override rate and duration** at generation time:

```bash
sonda import data.csv -o scenario.yaml --rate 10 --duration 5m
```

---

## CLI reference

```
sonda import <FILE> [OPTIONS]
```

| Argument / Flag | Type | Default | Description |
|-----------------|------|---------|-------------|
| `<FILE>` | path | -- | CSV file to import. Supports Grafana exports and plain CSV. |
| `--analyze` | flag | -- | Print detected patterns (read-only). Conflicts with `-o` and `--run`. |
| `-o, --output <FILE>` | path | -- | Write generated scenario YAML to this path. Conflicts with `--analyze` and `--run`. |
| `--run` | flag | -- | Generate and immediately execute the scenario. Conflicts with `--analyze` and `-o`. |
| `--columns <INDICES>` | string | all | Comma-separated column indices (e.g., `1,3,5`). Column 0 is the timestamp. |
| `--rate <RATE>` | float | `1.0` | Events per second in the generated scenario. |
| `--duration <DURATION>` | string | `60s` | Duration of the generated scenario (e.g., `60s`, `5m`). |

Exactly one of `--analyze`, `-o`, or `--run` must be specified.

!!! tip "Combine with global flags"
    `--dry-run`, `--verbose`, and `--quiet` work with `sonda import --run`, just like any
    other subcommand. Use `sonda --dry-run import data.csv --run` to see the resolved config
    without emitting events.
