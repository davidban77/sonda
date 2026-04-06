# Generators

Generators produce values for each tick of a scenario. For metrics, they produce `f64` values. For
logs, they produce structured log events. You select a generator with the `generator.type` field.

## Metric generators

### constant

Returns the same value on every tick. Use it for baseline testing or known-value verification
(e.g. recording rule validation).

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `value` | float | yes | -- | The fixed value emitted on every tick. |

```yaml title="Constant generator"
generator:
  type: constant
  value: 42.0
```

**Shape:** A flat horizontal line at the configured value.

```bash
sonda metrics --name up --rate 2 --duration 2s --value 1
```

```text title="Output"
up 1 1774279693496
up 1 1774279694001
up 1 1774279694501
```

Use `--value` from the CLI to set the constant value directly.

When no generator is configured, the default is `constant` with `value: 0.0`.

### sine

Produces a sine wave that oscillates between `offset - amplitude` and `offset + amplitude`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `amplitude` | float | yes | -- | Half the peak-to-peak swing. |
| `period_secs` | float | yes | -- | Duration of one full cycle in seconds. |
| `offset` | float | yes | -- | Vertical midpoint of the wave. |

```yaml title="Sine generator"
generator:
  type: sine
  amplitude: 50.0
  period_secs: 60
  offset: 50.0
```

**Shape:** Oscillates smoothly between 0 and 100 with a 60-second period. At tick 0, the value
equals the offset.

```bash
sonda metrics --name cpu --rate 2 --duration 2s \
  --value-mode sine --amplitude 50 --period-secs 4 --offset 50
```

```text title="Output"
cpu 50 1774279696105
cpu 85.35533905932738 1774279696610
cpu 100 1774279697110
```

### sawtooth

Linearly ramps from `min` to `max` and resets to `min` at the start of each period.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `min` | float | yes | -- | Value at the start of each period. |
| `max` | float | yes | -- | Value approached at the end (never reached). |
| `period_secs` | float | yes | -- | Duration of one full ramp in seconds. |

```yaml title="Sawtooth generator"
generator:
  type: sawtooth
  min: 0.0
  max: 100.0
  period_secs: 60.0
```

**Shape:** A linear ramp from 0 to 100 over 60 seconds, then snaps back to 0.

```bash
sonda metrics --name ramp --rate 2 --duration 2s \
  --value-mode sawtooth --min 0 --max 100 --period-secs 4
```

```text title="Output"
ramp 0 1774279701394
ramp 12.5 1774279701898
ramp 25 1774279702399
```

### uniform

Produces uniformly distributed random values in the range `[min, max]`. Deterministic when
seeded.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `min` | float | yes | -- | Lower bound (inclusive). |
| `max` | float | yes | -- | Upper bound (inclusive). |
| `seed` | integer | no | `0` | RNG seed for deterministic replay. |

```yaml title="Uniform generator"
generator:
  type: uniform
  min: 10.0
  max: 90.0
  seed: 42
```

**Shape:** Random values scattered between 10 and 90. Same seed produces same sequence.

```bash
sonda metrics --name noise --rate 2 --duration 2s \
  --value-mode uniform --min 10 --max 90 --seed 42
```

```text title="Output"
noise 69.32519030174588 1774279698726
noise 68.2543018631486 1774279699231
noise 27.068700996215277 1774279699731
```

### sequence

Steps through an explicit list of values. Use it for modeling specific incident patterns like
threshold crossings.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `values` | list of floats | yes | -- | The ordered values to step through. Must not be empty. |
| `repeat` | boolean | no | `true` | When true, cycles back to the start. When false, holds the last value. |

```yaml title="Sequence generator"
generator:
  type: sequence
  values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
  repeat: true
```

**Shape:** Steps through the list one value per tick. With `repeat: true`, wraps around after the
last value. With `repeat: false`, the last value is emitted for all subsequent ticks.

```bash
sonda metrics --scenario examples/sequence-alert-test.yaml --duration 5s
```

```text title="Output"
cpu_spike_test{instance="server-01",job="node"} 10 1774279704026
cpu_spike_test{instance="server-01",job="node"} 10 1774279705031
cpu_spike_test{instance="server-01",job="node"} 10 1774279706031
cpu_spike_test{instance="server-01",job="node"} 10 1774279707031
cpu_spike_test{instance="server-01",job="node"} 10 1774279708031
cpu_spike_test{instance="server-01",job="node"} 95 1774279709031
```

### step

Produces a monotonically increasing counter value: `start + tick * step_size`. With `max` set,
the value wraps around using modular arithmetic, simulating a counter reset. This is the go-to
generator for testing PromQL `rate()` and `increase()` queries.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `start` | float | no | `0.0` | Initial value at tick 0. |
| `step_size` | float | yes | -- | Increment applied per tick. |
| `max` | float | no | none | Wrap-around threshold. When set and greater than `start`, the value resets to `start` upon reaching `max`. |

```yaml title="Step generator"
generator:
  type: step
  start: 0
  step_size: 1.0
  max: 1000
```

**Shape:** A linear ramp from `start`, incrementing by `step_size` each tick. Without `max`, it
grows without bound. With `max`, it wraps back to `start` when it reaches the threshold.

```bash
sonda metrics --scenario examples/step-counter.yaml --duration 3s
```

```text title="Output"
request_count{instance="web-01",job="app"} 0 1775192670938
request_count{instance="web-01",job="app"} 1 1775192671439
request_count{instance="web-01",job="app"} 2 1775192671939
request_count{instance="web-01",job="app"} 3 1775192672443
request_count{instance="web-01",job="app"} 4 1775192672943
```

!!! tip "Simulating counter resets"
    Set `max` to a low value to see wrap-around behavior. For example, `start: 0`, `step_size: 1`,
    `max: 5` produces `0, 1, 2, 3, 4, 0, 1, 2, ...` -- useful for verifying that your `rate()`
    queries handle counter resets correctly.

### spike

Outputs a constant baseline value with periodic spikes. During a spike window the value is
`baseline + magnitude`; outside the window the value is `baseline`. Use it for testing alert
thresholds and anomaly detection rules that trigger on sudden value changes.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `baseline` | float | yes | -- | The normal output value between spikes. |
| `magnitude` | float | yes | -- | The amount added to baseline during a spike. Negative values create dips below baseline. |
| `duration_secs` | float | yes | -- | How long each spike lasts in seconds. |
| `interval_secs` | float | yes | -- | Time between spike starts in seconds. Must be greater than 0. |

```yaml title="Spike generator"
generator:
  type: spike
  baseline: 50.0
  magnitude: 200.0
  duration_secs: 10
  interval_secs: 60
```

**Shape:** Holds at 50 for most of the 60-second cycle, then jumps to 250 for 10 seconds.

```bash
sonda metrics --scenario examples/spike-alert-test.yaml --duration 5s
```

```text title="Output"
cpu_spike_test{instance="server-01",job="node"} 250 1775195158883
cpu_spike_test{instance="server-01",job="node"} 250 1775195159888
cpu_spike_test{instance="server-01",job="node"} 250 1775195160888
cpu_spike_test{instance="server-01",job="node"} 250 1775195161888
cpu_spike_test{instance="server-01",job="node"} 250 1775195162888
```

!!! tip "Negative magnitude for dip testing"
    Set `magnitude` to a negative value to create periodic dips below the baseline. For example,
    `baseline: 100.0` with `magnitude: -50.0` produces values that drop from 100 to 50 during
    the spike window -- useful for testing low-threshold alerts.

### csv_replay

Replays numeric values from a CSV file. Use it to reproduce real production metric patterns
captured from monitoring systems -- including Grafana CSV exports with embedded labels. For a
step-by-step walkthrough of the Grafana export workflow, see the
[Grafana CSV Replay](../guides/grafana-csv-replay.md) guide.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | -- | Path to the CSV file. |
| `columns` | list | no | -- | Explicit column specs. Each entry: `{index, name}` with optional `labels`. When absent, columns are auto-discovered from the header. |
| `repeat` | boolean | no | `true` | When true, cycles back to the start. When false, holds the last value. |

Header rows are auto-detected: if any non-time field on the first data line is non-numeric,
the line is treated as a header and skipped.

When `columns` is omitted, Sonda reads the CSV header and auto-discovers column names and
labels. If the CSV has no header (all-numeric first row), you must provide explicit `columns`.

=== "Auto-discovery (default)"

    When `columns` is absent, Sonda reads the header row and creates one metric stream per
    data column. This works with both plain headers and Grafana-style label-aware headers.

    ```yaml title="Auto-discovered columns"
    generator:
      type: csv_replay
      file: examples/grafana-export.csv
    ```

=== "Explicit columns"

    ```yaml title="Multi-column CSV replay"
    name: ignored_when_columns_set  # each column entry provides its own metric name
    rate: 1
    generator:
      type: csv_replay
      file: examples/sample-multi-column.csv
      columns:
        - index: 1
          name: cpu_percent
        - index: 2
          name: mem_percent
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

    This expands into three independent metric streams -- `cpu_percent`, `mem_percent`, and
    `disk_io_mbps` -- all sharing the same `labels`, `rate`, `sink`, and other scenario fields.

=== "Per-column labels"

    Each column entry can carry its own `labels` map. Per-column labels are merged with
    scenario-level labels, and column labels override on key conflict.

    ```yaml title="Per-column labels"
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

    `cpu_percent` gets `{core="0", instance="prod-server-42", job="node"}`.
    `disk_io_mbps` gets only the scenario-level labels.

**Shape:** Follows the exact pattern recorded in the CSV file -- the values are replayed verbatim,
one per tick.

!!! note
    The CSV file path is relative to the working directory where you run `sonda`, not
    relative to the scenario file.

??? tip "Supported header formats for auto-discovery"
    Sonda recognizes five column header formats:

    | Format | Example | Metric name | Labels |
    |--------|---------|-------------|--------|
    | `__name__` inside braces | `{__name__="up", job="prom"}` | `up` | `job` |
    | Name before braces | `up{job="prom"}` | `up` | `job` |
    | Labels only | `{job="prom"}` | none | `job` |
    | Plain name | `cpu_percent` | `cpu_percent` | none |
    | Simple word | `prometheus` | `prometheus` | none |

    Formats 1 and 2 are produced by Grafana. Format 3 (labels only, no metric name) is not
    compatible with auto-discovery and requires explicit `columns:` instead.

## Histogram and summary generators

The metric generators above produce a single number per tick -- one value, one time series, one
line. That works for counters ("how many requests?") and gauges ("what's the CPU usage?"), but
it cannot answer distribution questions: "how fast are requests?" or "what latency do 99% of
users experience?"

That is the problem histograms and summaries solve. Instead of recording a single value, they
observe many individual measurements (e.g., request durations) and produce **multiple time
series per tick** that describe the shape of those measurements: where the values cluster, how
they spread, and where the tail ends.

Think of it this way: a counter tells you *how many* requests happened. A histogram tells you
*how long* each of them took -- broken down into ranges so you can compute percentiles.

!!! info "How real systems work"
    When you instrument an HTTP handler with a histogram in a Prometheus client library, every
    request duration is "observed" into the histogram. The client doesn't store each individual
    duration. Instead, it maintains cumulative counters for predefined bucket boundaries (e.g.,
    "how many requests took <= 100ms?"). Prometheus scrapes these counters, and you use
    `histogram_quantile()` to estimate percentiles from the bucket distribution.

    Sonda's histogram generator does the same thing: it samples synthetic observations from a
    distribution, updates cumulative bucket counters, and emits the result in Prometheus format.
    The output is indistinguishable from a real instrumented service.

Histogram and summary generators use dedicated subcommands (`sonda histogram`, `sonda summary`)
and their own top-level scenario format -- not the `generator.type` field used by metric generators.
For a hands-on walkthrough of testing latency alerts, see the
[Histograms, Summaries, and Latency Alerts](../guides/histogram-alerts.md) guide.

### histogram

A histogram answers the question: **"what is the distribution of observed values?"** It does
this by sorting observations into buckets -- ranges with upper boundaries you define. Each
bucket counts how many observations fell at or below that boundary.

For a metric named `http_request_duration_seconds` with buckets at 0.1, 0.25, and 0.5, each
tick produces something like:

```text
http_request_duration_seconds_bucket{le="0.1"}   60   # 60 requests were <= 100ms
http_request_duration_seconds_bucket{le="0.25"}  85   # 85 requests were <= 250ms
http_request_duration_seconds_bucket{le="0.5"}   97   # 97 requests were <= 500ms
http_request_duration_seconds_bucket{le="+Inf"}  100  # all 100 requests
http_request_duration_seconds_count              100  # total observations
http_request_duration_seconds_sum                15.2 # total seconds across all requests
```

Buckets are **cumulative** -- the `le="0.25"` count includes all observations that are also in
`le="0.1"`. They are also **counters**, so they only increase over time. This is what makes
`rate()` and `histogram_quantile()` work: Prometheus computes per-second rates from the
counter deltas, then interpolates between bucket boundaries to estimate any percentile you ask
for.

??? tip "Choosing bucket boundaries"
    Bucket boundaries determine the resolution of your percentile estimates. If your SLO is
    "p99 latency under 500ms" but you have no bucket boundary near 500ms, the estimate will be
    coarse. The default Prometheus buckets (`0.005` to `10.0`) work for general HTTP latency.
    For tighter SLOs, add boundaries near your threshold:

    ```yaml
    buckets: [0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5]
    ```

    More buckets means more time series (one per bucket per label combination), so there is a
    cardinality tradeoff. For most services, 10-15 buckets is a reasonable starting point.

Each tick, the generator samples `observations_per_tick` values from a configurable distribution,
updates cumulative bucket counters, and emits one line per bucket plus `+Inf`, `_count`, and
`_sum`. Bucket counts never decrease -- they follow counter semantics.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `name` | string | yes | -- | Base metric name. Sonda appends `_bucket`, `_count`, `_sum` automatically. |
| `rate` | float | yes | -- | Ticks per second. Each tick produces one full histogram sample. |
| `duration` | string | no | runs forever | Total run time. |
| `distribution` | object | yes | -- | Observation distribution model. See [Distribution models](#distribution-models). |
| `buckets` | list of floats | no | Prometheus defaults | Sorted upper boundaries. Default: `[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]`. |
| `observations_per_tick` | integer | no | `100` | Number of observations sampled per tick. |
| `mean_shift_per_sec` | float | no | `0.0` | Linear drift applied to the distribution center per second. Simulates latency degradation. |
| `seed` | integer | no | `0` | RNG seed for deterministic output. Same seed produces the same bucket counts. |
| `labels` | map | no | none | Static labels attached to every series. |
| `encoder` | object | no | `prometheus_text` | Output format. |
| `sink` | object | no | `stdout` | Output destination. |

```yaml title="examples/histogram.yaml"
name: http_request_duration_seconds
rate: 1
duration: 10s

distribution:
  type: exponential
  rate: 10.0

observations_per_tick: 100
seed: 42

labels:
  method: GET
  handler: /api/v1/query

encoder:
  type: prometheus_text

sink:
  type: stdout
```

**Shape:** N+3 time series per tick (N bucket boundaries + `+Inf` + `_count` + `_sum`). With
default buckets, that is 14 series per tick. All bucket counters are cumulative and monotonically
increasing across ticks.

```bash
sonda histogram --scenario examples/histogram.yaml
```

```text title="Output (first tick, abbreviated)"
http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.005",method="GET"} 3 1775409497421
http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.01",method="GET"} 11 1775409497421
http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.025",method="GET"} 26 1775409497421
...
http_request_duration_seconds_bucket{handler="/api/v1/query",le="+Inf",method="GET"} 100 1775409497421
http_request_duration_seconds_count{handler="/api/v1/query",method="GET"} 100 1775409497421
http_request_duration_seconds_sum{handler="/api/v1/query",method="GET"} 9.505 1775409497421
```

!!! tip "Simulating latency degradation"
    Set `mean_shift_per_sec` to a positive value to make the distribution drift higher over time.
    This causes more observations to land in higher buckets, raising percentile estimates and
    eventually triggering latency alerts. See the
    [alert testing walkthrough](../guides/histogram-alerts.md#test-a-histogram_quantile-alert-with-sonda)
    for a complete example.

### summary

Where a histogram stores raw bucket counts and lets Prometheus estimate percentiles server-side,
a summary does the math upfront: it computes the actual percentile values on the client and
reports them directly. The p50 *is* 98ms. The p99 *is* 148ms. No estimation, no bucket
interpolation.

The tradeoff is flexibility. With a histogram, you can compute *any* percentile after the fact
from the stored buckets. With a summary, you only get the specific quantiles you configured. And
critically, you **cannot aggregate summary quantiles across instances** -- averaging the p99 of
ten pods does not give you the fleet-wide p99. If you need cross-instance percentiles (and in
Kubernetes, you almost always do), use histograms.

Each tick, the generator samples observations, sorts them, and computes quantile values using
the nearest-rank method.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `name` | string | yes | -- | Base metric name. Sonda appends `_count`, `_sum` for those series. |
| `rate` | float | yes | -- | Ticks per second. |
| `duration` | string | no | runs forever | Total run time. |
| `distribution` | object | yes | -- | Observation distribution model. See [Distribution models](#distribution-models). |
| `quantiles` | list of floats | no | `[0.5, 0.9, 0.95, 0.99]` | Quantile targets in `(0, 1)`. |
| `observations_per_tick` | integer | no | `100` | Number of observations sampled per tick. |
| `mean_shift_per_sec` | float | no | `0.0` | Linear drift applied to the distribution center per second. |
| `seed` | integer | no | `0` | RNG seed for deterministic output. |
| `labels` | map | no | none | Static labels attached to every series. |
| `encoder` | object | no | `prometheus_text` | Output format. |
| `sink` | object | no | `stdout` | Output destination. |

```yaml title="examples/summary.yaml"
name: rpc_duration_seconds
rate: 1
duration: 10s

distribution:
  type: normal
  mean: 0.1
  stddev: 0.02

observations_per_tick: 100
seed: 42

labels:
  service: auth
  method: GetUser

encoder:
  type: prometheus_text

sink:
  type: stdout
```

**Shape:** Q+2 time series per tick (Q quantile targets + `_count` + `_sum`). With default
quantiles, that is 6 series per tick. Quantile values are fresh per-tick snapshots computed from
that tick's observations. `_count` and `_sum` are cumulative.

```bash
sonda summary --scenario examples/summary.yaml
```

```text title="Output (first tick)"
rpc_duration_seconds{method="GetUser",quantile="0.5",service="auth"} 0.098 1775409507904
rpc_duration_seconds{method="GetUser",quantile="0.9",service="auth"} 0.128 1775409507904
rpc_duration_seconds{method="GetUser",quantile="0.95",service="auth"} 0.136 1775409507904
rpc_duration_seconds{method="GetUser",quantile="0.99",service="auth"} 0.148 1775409507904
rpc_duration_seconds_count{method="GetUser",service="auth"} 100 1775409507904
rpc_duration_seconds_sum{method="GetUser",service="auth"} 9.802 1775409507904
```

!!! warning "Summaries are not aggregatable"
    You cannot meaningfully combine quantile values across multiple instances. If you need
    percentiles across a fleet, use histograms instead -- `histogram_quantile()` works on
    summed bucket counters.

### Distribution models

Both histogram and summary generators require a `distribution` block that controls how
observations are sampled. The distribution you choose determines the *shape* of the data --
whether observations cluster tightly around a center, skew toward fast values with a long tail,
or spread evenly across a range.

Pick the distribution that matches the real-world metric you are simulating. For HTTP request
latency, exponential is almost always the right choice: most requests are fast, but some take
much longer. For RPC durations in a healthy service with predictable behavior, normal gives you
a symmetric bell curve. Uniform is mainly useful for stress-testing bucket boundaries, since
real metrics rarely distribute evenly.

| Distribution | YAML type | Parameters | Typical use |
|-------------|-----------|------------|-------------|
| Exponential | `exponential` | `rate` (lambda; mean = 1/rate) | Request latency with long tail |
| Normal | `normal` | `mean`, `stddev` | Symmetric metrics (RPC duration) |
| Uniform | `uniform` | `min`, `max` | Even spread for bucket boundary testing |

=== "Exponential"

    ```yaml
    distribution:
      type: exponential
      rate: 10.0
    ```

    Models latency where most requests are fast but some have long tails. Mean = 1/rate = 0.1s.

=== "Normal"

    ```yaml
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    ```

    Symmetric bell curve centered at `mean`. Good for metrics with consistent spread.

=== "Uniform"

    ```yaml
    distribution:
      type: uniform
      min: 0.05
      max: 0.15
    ```

    Every value in `[min, max]` is equally likely.

## Log generators

Log generators produce structured log events instead of numeric values. They are used with the
`sonda logs` subcommand.

### template

Generates log events from message templates with randomized field values.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `templates` | list | yes | -- | One or more template entries (round-robin selection). |
| `templates[].message` | string | yes | -- | Message template. Use `{field}` for placeholders. |
| `templates[].field_pools` | map | no | `{}` | Maps placeholder names to value lists. |
| `severity_weights` | map | no | info only | Severity distribution. Keys: `trace`, `debug`, `info`, `warn`, `error`, `fatal`. |
| `seed` | integer | no | `0` | RNG seed for deterministic field and severity selection. |

```yaml title="Template log generator"
generator:
  type: template
  templates:
    - message: "Request from {ip} to {endpoint} returned {status}"
      field_pools:
        ip: ["10.0.0.1", "10.0.0.2"]
        endpoint: ["/api", "/health"]
        status: ["200", "404", "500"]
  severity_weights:
    info: 0.7
    warn: 0.2
    error: 0.1
  seed: 42
```

Templates are selected round-robin by tick. Placeholders are resolved by randomly picking from
the corresponding field pool.

### replay

Replays lines from a log file, cycling back to the start when the file is exhausted.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | -- | Path to the log file to replay. |

```yaml title="Replay log generator"
generator:
  type: replay
  file: /var/log/app.log
```

Each line becomes the `message` field of a log event with `info` severity.

## Jitter

Jitter adds deterministic uniform noise to any metric generator's output. Instead of clean,
perfectly smooth values, you get realistic-looking fluctuations -- the kind you see in real
production metrics.

!!! info "Why jitter?"
    A sine wave is useful for testing alert thresholds, but real CPU metrics are never perfectly
    smooth. Adding jitter lets you verify that your alerting rules and dashboards handle noisy
    signals correctly.

Jitter is configured at the scenario level (a sibling of `generator`, not nested inside it)
because it wraps any generator transparently.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `jitter` | float | no | none | Noise amplitude. Adds uniform noise in `[-jitter, +jitter]` to every value. |
| `jitter_seed` | integer | no | `0` | Seed for deterministic noise. Same seed produces the same noise sequence. |

```yaml title="Sine wave with jitter"
name: cpu_usage_realistic
rate: 1
duration: 30s
generator:
  type: sine
  amplitude: 20
  period_secs: 120
  offset: 50
jitter: 3.0
jitter_seed: 42
labels:
  instance: server-01
  job: node
encoder:
  type: prometheus_text
sink:
  type: stdout
```

```bash
sonda metrics --scenario examples/jitter-sine.yaml --duration 3s
```

Without jitter, a sine wave with `offset: 50` outputs exactly `50.0` at tick 0. With
`jitter: 3.0`, the value lands somewhere in `[47.0, 53.0]` -- different each tick, but
reproducible across runs when `jitter_seed` is set.

!!! tip "Works with every metric generator"
    Jitter wraps the generator's output, so it works with `constant`, `sine`, `sawtooth`,
    `uniform`, `sequence`, `step`, `spike`, and `csv_replay`. It does not apply to log generators.

??? tip "When to skip `jitter_seed`"
    If you omit `jitter_seed`, it defaults to `0`. Two scenarios with the same `jitter` value
    and no explicit seed produce identical noise sequences. Set different seeds when you need
    independent noise on multiple scenarios.
