# CSV Import

After last week's incident, you exported the CPU and memory series from Grafana to
attach to the postmortem. The CSV sits in a Drive folder. The next time someone tries
to reproduce the alert pattern in CI, they will not find the file -- and even if they
do, raw replay locks them to the original timestamps and the original rate.

What you actually want is the *shape* of that incident as a reusable scenario:
"a CPU spike like the one from the May 14 outage" parameterized on rate and duration,
checked into the repo next to the alert rules. `sonda new --from <csv>` does that
conversion in one step. It scans each column, classifies the pattern (steady, spike,
leak, sawtooth, flap, step), and emits a v2 scenario YAML wired to a generator with
the right knobs.

---

## Why import instead of replay?

The [csv_replay](grafana-csv-replay.md) generator plays back raw CSV values verbatim. It
preserves the original sample interval automatically (the replay rate is derived from the CSV
timestamps, not from `rate:`) and supports labels-only Grafana exports via
`default_metric_name:`. Use it when you need bit-for-bit fidelity tied to a specific file.

`sonda new --from <csv>` takes a different approach:

- **Portable** -- the generated YAML uses generators (`steady`, `spike_event`, `leak`, `flap`,
  `sawtooth`, `step`), so it runs without the original CSV file.
- **Parameterized** -- you can tune rate, duration, and generator parameters after import.
- **Shareable** -- the YAML is self-contained. Drop it into a repo, CI pipeline, or Helm chart.

Use `csv_replay` when you need bit-for-bit fidelity. Use `sonda new --from <csv>` when you
need the *shape* of the data as a reusable scenario that does not depend on the original CSV.

---

## The workflow

`sonda new --from <csv>` writes a v2 scenario YAML to stdout (or to a file with `-o`). Run the
result with `sonda run` once you are happy with it:

```text
CSV file  -->  sonda new --from data.csv  -->  scenario.yaml  -->  sonda run
              (pattern detection)             (tunable knobs)
```

### Generate a scenario

```bash
sonda new --from examples/sample-multi-column.csv -o scenario.yaml
```

```text title="stderr"
wrote scenario to scenario.yaml
```

The generated file is a valid [v2 scenario YAML](../configuration/v2-scenarios.md), ready for
`sonda run`:

```yaml title="scenario.yaml"
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

Each numeric column in the CSV gets its own `scenarios:` entry. The generator alias is chosen
by pattern detection, so you can edit the output and tune the parameters rather than starting
from a blank file.

!!! info "Output is always v2"
    `sonda new --from <csv>` emits v2 YAML regardless of column count. Single-column CSVs
    produce a one-entry `scenarios:` list; multi-column CSVs produce one entry per column.
    Shared `rate`, `duration`, `encoder`, and `sink` live in `defaults:`.

### Preview to stdout

Omit `-o` to print the generated YAML to stdout without writing a file — useful for quick
inspection or piping into other tools:

```bash
sonda new --from examples/sample-cpu-values.csv
```

### Run the result

Once the YAML looks right, run it like any other scenario:

```bash
sonda -q run scenario.yaml --duration 3s
```

```text title="Output"
cpu_percent 41.44404065390504 1775712694328
cpu_percent 46.07410906869991 1775712695333
cpu_percent 50.131242022026555 1775712696330
cpu_percent 55.42337922089686 1775712697333
```

---

## Grafana CSV exports

`sonda new --from <csv>` understands Grafana's "Series joined by time" CSV format. It parses
the `{__name__="...", key="value"}` headers to extract metric names and labels automatically.

```bash
sonda new --from examples/grafana-export.csv -o grafana-scenario.yaml
```

Labels are preserved in the generated YAML:

```yaml title="grafana-scenario.yaml (first entry)"
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
    The detector uses statistical thresholds (linear regression, IQR outlier detection,
    k-means clustering) to classify patterns. With very short time series (fewer than 10 data
    points), detection accuracy decreases. For best results, export at least 20-30 data points.

---

## Customizing generated scenarios

The generated YAML is a starting point. After import, you can:

- **Change the sink** -- replace `stdout` with `remote_write`, `loki`, or any other sink.
- **Adjust parameters** -- tune `amplitude`, `period`, or `baseline` to match your needs.
- **Add scheduling** -- add `gaps:`, `bursts:`, or `cardinality_spike:` blocks.
- **Override rate and duration at run time:**

```bash
sonda run scenario.yaml --rate 10 --duration 5m
```

---

## CLI reference

```
sonda new [--template] [--from <CSV>] [-o <PATH>]
```

| Flag | Description |
|------|-------------|
| (no flags) | Interactive flow. Walks through signal type → generator → rate → duration → sink type → output path. |
| `--template` | Print a minimal valid YAML to stdout and exit. No prompts. |
| `--from <CSV>` | Seed the scaffold from a CSV file. Runs pattern detection on each numeric column. |
| `-o <PATH>` | Write the result to a file instead of stdout. |

See [CLI Reference: sonda new](../configuration/cli-reference.md#sonda-new) for the full
reference.

---

## Replaying log streams from CSV

`sonda new --from <csv>` is scoped to metric series. If your CSV is a structured log export
-- for example a `timestamp,severity,message,trace_id` dump from Loki via `logcli` -- use the
`log_csv_replay` generator instead. It replays each row as a `LogEvent`, derives the emission
rate from the timestamp column the same way `csv_replay` does for metrics, and routes
free-form columns into `LogEvent::fields`. See the [Log CSV Replay](log-csv-replay.md) guide
for the end-to-end workflow.
