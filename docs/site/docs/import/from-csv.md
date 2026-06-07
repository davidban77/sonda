# CSV Import

This page covers `sonda new --from <csv>`. The command reads a CSV file, detects the pattern in each numeric column, and writes a Sonda scenario YAML. The result is a portable scenario you can check into a repository and run later.

A common case: last week, you exported the CPU and memory series from Grafana for a postmortem. The CSV now sits in a shared folder. Next time someone wants to reproduce the alert in CI, the file is hard to find. Even if they find it, raw replay locks them to the original timestamps and rate.

You want the *pattern* of that incident as a reusable scenario. For example: "a CPU spike like the May 14 outage", parameterised on rate and duration, stored next to the alert rules. `sonda new --from <csv>` does this conversion in one step.

---

## Why import instead of replay?

The [csv_replay](grafana-exports.md) generator plays back raw CSV values without changes. It preserves the original sample interval from the CSV timestamps, not from `rate:`. It also supports labels-only Grafana exports through `default_metric_name:`. Use it when you need an exact replay tied to a specific file.

`sonda new --from <csv>` is different:

- **Portable** — the generated YAML uses generator aliases (`steady`, `spike_event`, `leak`, `flap`, `sawtooth`, `step`), so it runs without the original CSV file.
- **Parameterised** — you can change rate, duration, and generator parameters after import.
- **Shareable** — the YAML is self-contained. Drop it into a repository, CI pipeline, or Helm chart.

Use `csv_replay` when you need an exact replay. Use `sonda new --from <csv>` when you want the *pattern* of the data as a reusable scenario. The result does not depend on the original CSV.

---

## The workflow

`sonda new --from <csv>` writes a scenario YAML to stdout, or to a file with `-o`. Run the result with `sonda run`:

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

The generated file is a valid [scenario YAML](../build/scenario-files.md), ready for `sonda run`:

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

Each numeric column in the CSV becomes its own entry under `scenarios:`. The generator alias comes from pattern detection. You edit the output and tune the parameters instead of starting from a blank file.

!!! info "One column, one entry"
    `sonda new --from <csv>` writes a scenario YAML for any column count. A single-column CSV produces a one-entry `scenarios:` list. A multi-column CSV produces one entry per column. The shared `rate`, `duration`, `encoder`, and `sink` values live in `defaults:`.

### Preview to stdout

Omit `-o` to print the generated YAML to stdout without writing a file. This is useful for quick inspection or piping into other tools:

```bash
sonda new --from examples/sample-cpu-values.csv
```

### Run the result

When the YAML looks right, run it like any other scenario:

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

`sonda new --from <csv>` reads Grafana's "Series joined by time" CSV format. It parses the `{__name__="...", key="value"}` headers and extracts metric names and labels automatically.

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

For details on exporting from Grafana, see the [Grafana CSV Export Replay](grafana-exports.md#export-from-grafana) guide.

---

## Detected patterns

The pattern detector uses statistical analysis to classify each column into one of six patterns. Each pattern maps to a Sonda generator or [operational vocabulary](../build/generators.md) alias.

| Pattern | What it looks like | Generator / alias | Key parameters |
|---------|-------------------|-------------------|----------------|
| **Steady** | Low variance around a center | `steady` | center, amplitude, period |
| **Spike** | Periodic outliers above a baseline | `spike_event` | baseline, spike_height, spike_duration, spike_interval |
| **Climb** | Monotonic upward trend | `leak` | baseline, ceiling, time_to_ceiling |
| **Sawtooth** | Repeating climb-reset cycles | `sawtooth` | min, max, period_secs |
| **Flap** | Bimodal toggle (up/down) | `flap` | up_value, down_value, up_duration, down_duration |
| **Step** | Constant-rate counter increments | `step` | start, step_size |

The detector tries these in priority order. When the data does not clearly match a more specific pattern, it falls back to **steady**.

!!! info "Pattern detection is heuristic"
    The detector uses statistical thresholds: linear regression, IQR outlier detection, and k-means clustering. With very short time series (fewer than 10 data points), accuracy decreases. For best results, export at least 20-30 data points.

---

## Customizing generated scenarios

The generated YAML is a starting point. After import, you can:

- **Change the sink** — replace `stdout` with `remote_write`, `loki`, or another sink.
- **Adjust parameters** — change `amplitude`, `period`, or `baseline` to match your needs.
- **Add scheduling** — add `gaps:`, `bursts:`, or `cardinality_spike:` blocks.
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
| (no flags) | Interactive flow. Asks for signal type, generator, rate, duration, sink type, and output path. |
| `--template` | Print a minimal valid YAML to stdout and exit. No prompts. |
| `--from <CSV>` | Generate a scenario from a CSV file. Runs pattern detection on each numeric column. |
| `-o <PATH>` | Write the result to a file instead of stdout. |

See [CLI Reference: sonda new](../reference/cli-flags.md#sonda-new) for the full reference.

---

## Replaying log streams from CSV

`sonda new --from <csv>` only handles metric series. If your CSV is a structured log export, for example a `timestamp,severity,message,trace_id` dump from Loki via `logcli`, use the `log_csv_replay` generator instead. It replays each row as a `LogEvent` and derives the emission rate from the timestamp column. Free-form columns become entries in `LogEvent::fields`. See the [Log CSV Replay](log-files.md) guide for the full workflow.
