# Scenario Fields

This page is the per-entry field reference. It describes every field you can set on a
`scenarios:` entry inside a [v2 scenario file](v2-scenarios.md) -- generators, schedules,
labels, encoders, sinks, and multi-scenario timing controls.

!!! info "Start with the v2 guide"
    For the file shape (`version: 2`, `defaults:`, `scenarios:`), catalog metadata,
    pack-backed entries, and `after:` temporal chains, see
    [**v2 Scenario Files**](v2-scenarios.md). Every field below sits inside a v2
    `scenarios:` entry -- Sonda only accepts `version: 2` YAML.

## Complete example

A single v2 entry touching every available field:

```yaml title="full-example.yaml"
version: 2

defaults:
  rate: 100
  duration: 30s
  encoder:
    type: prometheus_text
    precision: 2          # optional: limit values to 2 decimal places
  sink:
    type: stdout

scenarios:
  - id: cpu_usage
    signal_type: metrics
    name: cpu_usage

    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60
      offset: 50.0

    gaps:
      every: 2m
      for: 20s

    bursts:
      every: 10s
      for: 2s
      multiplier: 5.0

    cardinality_spikes:
      - label: pod_name
        every: 2m
        for: 30s
        cardinality: 500
        strategy: counter
        prefix: "pod-"

    dynamic_labels:
      - key: hostname
        prefix: "host-"
        cardinality: 10

    labels:
      zone: us-east-1

    jitter: 2.5
    jitter_seed: 42

    phase_offset: "5s"
    clock_group: alert-test
```

```bash
sonda run full-example.yaml
```

## Field reference

### Core fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | yes | -- | Metric name. Must match `[a-zA-Z_:][a-zA-Z0-9_:]*`. |
| `rate` | float | yes | -- | Events per second. Must be positive. Fractional values allowed (e.g. `0.5`). |
| `duration` | string | no | runs forever | Total run time. Supports `ms`, `s`, `m`, `h` units. |
| `generator` | object | yes | -- | Value generator configuration. Core types and [operational aliases](generators.md#operational-aliases). See [Generators](generators.md). |
| `encoder` | object | no | `prometheus_text` | Output format. See [Encoders](encoders.md). |
| `sink` | object | no | `stdout` | Output destination. See [Sinks](sinks.md). |
| `dynamic_labels` | list | no | none | Rotating labels that cycle through values on every tick. See [Dynamic labels](#dynamic-labels). |
| `labels` | map | no | none | Static key-value labels attached to every event. |
| `jitter` | float | no | none | Noise amplitude. Adds uniform noise in `[-jitter, +jitter]` to every generated value. See [Generators - Jitter](generators.md#jitter). |
| `jitter_seed` | integer | no | `0` | Seed for deterministic jitter noise. Different seeds produce different noise sequences. |
| `on_sink_error` | string | no | `warn` | Behavior when the sink returns an error mid-run: `warn` (log + drop batch + keep running) or `fail` (propagate and exit the runner). Overrides `defaults.on_sink_error`. See [Sink-error policy](v2-scenarios.md#sink-error-policy). |

### Gap window

Gaps create recurring silent periods where no events are emitted. Both fields must be provided
together.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `gaps.every` | string | yes (if gaps used) | Recurrence interval (e.g. `"2m"`). |
| `gaps.for` | string | yes (if gaps used) | Duration of each gap. Must be less than `every`. |

```yaml title="Gap example"
gaps:
  every: 2m
  for: 20s
```

This emits events for 1m40s, then goes silent for 20s, then repeats.

### Burst window

Bursts create recurring high-rate periods. All three fields must be provided together. If a gap
and burst overlap, the gap takes priority.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `bursts.every` | string | yes (if bursts used) | Recurrence interval (e.g. `"10s"`). |
| `bursts.for` | string | yes (if bursts used) | Duration of each burst. Must be less than `every`. |
| `bursts.multiplier` | float | yes (if bursts used) | Rate multiplier during burst. Must be positive. |

```yaml title="Burst example"
bursts:
  every: 10s
  for: 2s
  multiplier: 5.0
```

At a base rate of 100 events/sec, this produces 500 events/sec for 2 seconds out of every 10.

### Cardinality spike window

Cardinality spikes create recurring windows that inject dynamic label values, simulating the label
explosions that break real pipelines. During a spike window, a configured label key is added with
one of `cardinality` unique values. Outside the window, the label is absent.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `cardinality_spikes[].label` | string | yes | -- | Label key to inject during the spike. |
| `cardinality_spikes[].every` | string | yes | -- | Recurrence interval (e.g. `"2m"`). |
| `cardinality_spikes[].for` | string | yes | -- | Duration of each spike. Must be less than `every`. |
| `cardinality_spikes[].cardinality` | integer | yes | -- | Number of unique label values. Must be > 0. |
| `cardinality_spikes[].strategy` | string | no | `counter` | Value generation strategy: `counter` or `random`. |
| `cardinality_spikes[].prefix` | string | no | `"{label}_"` | Prefix for generated label values. |
| `cardinality_spikes[].seed` | integer | no | `0` | RNG seed for the `random` strategy. |

```yaml title="Cardinality spike example"
cardinality_spikes:
  - label: pod_name
    every: 2m
    for: 30s
    cardinality: 500
    strategy: counter
    prefix: "pod-"
```

**Strategies:**

- **`counter`** -- Generates sequential values: `{prefix}0`, `{prefix}1`, ..., `{prefix}{cardinality-1}`, then wraps around. Deterministic without a seed.
- **`random`** -- Generates hash-like hex values via SplitMix64: `{prefix}{hex}`. Produces exactly `cardinality` unique values. Requires a `seed` for reproducibility.

!!! note
    Gap windows take priority over spikes. If a gap and spike overlap, the gap suppresses all
    output including spike labels.

### Dynamic labels

Dynamic labels attach a rotating label value to **every** emitted event. They simulate a stable
fleet of N distinct sources -- hostnames, pod names, regions -- without a time window. Unlike
[cardinality spikes](#cardinality-spike-window), the label is always present, not just during a
spike window.

This lets you test dashboards that aggregate by label (e.g., `sum by (hostname)`) and exercise
high-cardinality query paths in Prometheus or VictoriaMetrics.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `dynamic_labels[].key` | string | yes | -- | Label key to attach. Must be a valid Prometheus label key. |
| `dynamic_labels[].prefix` | string | no | `"{key}_"` | Prefix for counter strategy values (e.g., `"host-"` produces `host-0`, `host-1`). |
| `dynamic_labels[].cardinality` | integer | yes (counter) | -- | Number of unique values in the cycle. Must be > 0. |
| `dynamic_labels[].values` | list | yes (values list) | -- | Explicit list of label values to cycle through. |

**Two strategies**, chosen by which fields you provide:

=== "Counter"

    Provide `prefix` and `cardinality`. Values cycle as `{prefix}0`, `{prefix}1`, ...,
    `{prefix}{cardinality-1}`, then wrap around.

    ```yaml title="examples/dynamic-labels-fleet.yaml"
    dynamic_labels:
      - key: hostname
        prefix: "host-"
        cardinality: 10
    ```

    ```
    node_cpu_usage{hostname="host-0",...} 50 1712345678000
    node_cpu_usage{hostname="host-1",...} 50.4 1712345678100
    ...
    node_cpu_usage{hostname="host-9",...} 53.7 1712345678900
    node_cpu_usage{hostname="host-0",...} 54.1 1712345679000
    ```

    If you omit `prefix`, it defaults to `"{key}_"` (e.g., `hostname_0`, `hostname_1`).

=== "Values list"

    Provide `values` -- an explicit list of strings. The label cycles through the list in order.

    ```yaml title="examples/dynamic-labels-regions.yaml"
    dynamic_labels:
      - key: region
        values: [us-east-1, us-west-2, eu-west-1]
    ```

    ```
    api_latency{region="us-east-1",...} 0.42 1712345678000
    api_latency{region="us-west-2",...} 1.23 1712345678200
    api_latency{region="eu-west-1",...} 0.87 1712345678400
    api_latency{region="us-east-1",...} 0.31 1712345678600
    ```

You can combine multiple dynamic labels in the same scenario. Each label cycles independently
based on the tick counter:

```yaml title="examples/dynamic-labels-multi.yaml"
dynamic_labels:
  - key: hostname
    prefix: "web-"
    cardinality: 3
  - key: region
    values: [us-east-1, eu-west-1]
```

```
request_count{hostname="web-0",region="us-east-1",...} 0
request_count{hostname="web-1",region="eu-west-1",...} 1
request_count{hostname="web-2",region="us-east-1",...} 2
request_count{hostname="web-0",region="eu-west-1",...} 3
```

!!! tip "Dynamic labels vs. cardinality spikes"
    Use **dynamic labels** when you want a label to be present on every event (fleet simulation,
    multi-region testing). Use **cardinality spikes** when you want a label to appear only during
    recurring time windows (simulating label explosions that come and go).

!!! info "Label merge behavior"
    Dynamic labels are merged with static `labels:` on every tick. If a dynamic label key
    collides with a static label key, the dynamic value wins. Dynamic labels work identically
    for both metric and log scenarios.

### Temporal fields

These fields control when and how entries coordinate inside a multi-entry v2 file (including
bodies POSTed to [`POST /scenarios`](../deployment/sonda-server.md#start-a-scenario)).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `id` | string | no | auto | Entry identifier. `after:` and explicit `clock_group:` references target other entries by `id`. Defaults to the entry's `name` when omitted. |
| `phase_offset` | string | no | none | Explicit delay before starting this scenario. Supports `ms`, `s`, `m`, `h`. Mutually exclusive with `after:` (the compiler computes `phase_offset` from `after:`). |
| `clock_group` | string | no | none | Entries with the same clock group share a start-time reference. Auto-assigned when you use `after:`. |
| `after` | object | no | none | Start this entry when another entry's generator crosses a threshold. See [Temporal chains](v2-scenarios.md#temporal-chains-with-after). |

See [Multi-signal files](#multi-signal-files) below for a working example.

### Duration format

All duration fields (`duration`, `gaps.every`, `gaps.for`, `bursts.every`, `bursts.for`,
`phase_offset`) accept the same format:

| Unit | Example | Description |
|------|---------|-------------|
| `ms` | `100ms` | Milliseconds |
| `s` | `30s` | Seconds |
| `m` | `5m` | Minutes |
| `h` | `1h` | Hours |

Fractional values are supported in all units. For example, `1.5s` means 1500 milliseconds
and `0.5m` means 30 seconds.

## Log entries

A log entry uses `signal_type: logs` and puts the generator configuration under `log_generator:`
(not `generator:`). The default encoder is `json_lines`, but any encoder that accepts log events
works.

```yaml title="log-scenario.yaml"
version: 2

defaults:
  rate: 10
  duration: 60s
  encoder:
    type: json_lines
  sink:
    type: stdout
  labels:
    job: sonda
    env: dev

scenarios:
  - id: app_logs
    signal_type: logs
    name: app_logs
    log_generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint}"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2"]
            endpoint: ["/api", "/health"]
      severity_weights:
        info: 0.7
        warn: 0.2
        error: 0.1
      seed: 42
```

```bash
sonda run log-scenario.yaml
```

The `labels` / `dynamic_labels` fields work the same way as for metric entries. Static labels
attach a fixed key-value to every event; dynamic labels rotate values per tick. Both appear in
JSON Lines output and become Loki stream labels when the sink is `loki`.

## Multi-signal files

Each entry in a v2 `scenarios:` list declares its own `signal_type`. The compiler routes the
entry to the matching generator family at compile time.

| `signal_type` | Description | Body shape |
|---------------|-------------|------------|
| `metrics` | Gauge / counter metrics via a [generator](generators.md#metric-generators) or [operational alias](generators.md#operational-aliases) | `generator:` + standard fields |
| `logs` | Structured log events | `log_generator:` (`template` or `replay`) |
| `histogram` | Prometheus-style histogram (bucket, count, sum) | `distribution:` + histogram fields |
| `summary` | Prometheus-style summary (quantile, count, sum) | `distribution:` + summary fields |

Two metric entries correlated with `phase_offset` + a shared `clock_group:`:

```yaml title="multi-scenario.yaml"
version: 2

defaults:
  rate: 1
  duration: 120s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    instance: server-01
    job: node

scenarios:
  - id: cpu_usage
    signal_type: metrics
    name: cpu_usage
    phase_offset: "0s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
      repeat: true

  - id: memory_usage
    signal_type: metrics
    name: memory_usage_percent
    phase_offset: "3s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
      repeat: true
```

```bash
sonda run multi-scenario.yaml
```

The `phase_offset` on `memory_usage` delays it by 3 seconds, so CPU spikes first and memory
follows. Both entries share the `alert-test` clock group for synchronized timing. For
declarative chains, use [`after:`](v2-scenarios.md#temporal-chains-with-after) instead of
hand-tuned offsets.

### Mixing all four signal types

```yaml title="mixed-signals.yaml"
version: 2

defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: http_requests_total
    signal_type: metrics
    name: http_requests_total
    rate: 10
    generator:
      type: step
      start: 0
      step_size: 1.0
    labels:
      job: api

  - id: http_request_duration_seconds
    signal_type: histogram
    name: http_request_duration_seconds
    distribution:
      type: exponential
      rate: 10.0
    observations_per_tick: 100
    seed: 42
    labels:
      job: api

  - id: rpc_duration_seconds
    signal_type: summary
    name: rpc_duration_seconds
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    observations_per_tick: 100
    labels:
      service: auth

  - id: app_logs
    signal_type: logs
    name: app_logs
    rate: 5
    encoder:
      type: json_lines
    log_generator:
      type: template
      templates:
        - message: "Request processed in {duration}ms"
          field_pools:
            duration: ["12", "45", "120", "500"]
```

```bash
sonda run mixed-signals.yaml
```

!!! info "Histogram and summary entries use different fields"
    Histogram and summary entries do not have a `generator:` block. Instead, they use
    `distribution:`, `buckets:` / `quantiles:`, and `observations_per_tick:` on the entry.
    See [Generators -- histogram and summary](generators.md#histogram-and-summary-generators)
    for the full field reference.

## Pack-backed entries

A `scenarios:` entry with `pack: <name>` replaces the `name:` + `generator:` combo with a
reference to a [metric pack](../guides/metric-packs.md). The compiler expands the pack into
one entry per metric at compile time:

```yaml title="pack-scenario.yaml"
version: 2

defaults:
  rate: 1
  duration: 10s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: edge_router_snmp
    signal_type: metrics
    pack: telegraf_snmp_interface
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifIndex: "1"
```

```bash
sonda run pack-scenario.yaml
```

Any `labels`, `rate`, `duration`, `encoder`, or `sink` you set on the entry applies to every
expanded metric. Per-metric `overrides:` let you tune individual metrics inside the pack --
see the [Metric Packs guide](../guides/metric-packs.md) for the full reference.

## CLI overrides

Any of the common knobs (`rate`, `duration`, `sink`, `endpoint`, `encoder`, `label`,
`on-sink-error`) can be overridden from the command line. CLI flags always take precedence
over YAML values:

```bash title="scenario.yaml"
sonda run scenario.yaml --duration 5s --rate 2
```

This loads the file but overrides `duration` and `rate` (applied to every entry) with the CLI
values. Encoder-specific knobs like `precision` and pack-specific overrides live in the YAML —
see [CLI Reference: sonda run](cli-reference.md#sonda-run) for the full override list.

## What next

- [**v2 Scenario Files**](v2-scenarios.md) -- file shape, `defaults:`, `after:` chains, catalog
  metadata, and migration notes.
- [**CLI Reference -- sonda run**](cli-reference.md#sonda-run) -- the unified entry point for
  v2 scenario files.
- [**Metric Packs**](../guides/metric-packs.md) -- reusable metric name + label schemas you can
  reference via `pack:`.
