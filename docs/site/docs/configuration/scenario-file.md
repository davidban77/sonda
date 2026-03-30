# Scenario Files

Scenario files define everything about a Sonda run: what to generate, how to encode it, and where
to send it. They are YAML files that you pass with `--scenario`.

## Complete example

This scenario touches every available field:

```yaml title="full-example.yaml"
name: cpu_usage
rate: 100
duration: 30s

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

labels:
  hostname: web-01
  zone: us-east-1

encoder:
  type: prometheus_text
  precision: 2          # optional: limit values to 2 decimal places

sink:
  type: stdout

phase_offset: "5s"
clock_group: alert-test
```

```bash
sonda metrics --scenario full-example.yaml
```

## Field reference

### Core fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | yes | -- | Metric name. Must match `[a-zA-Z_:][a-zA-Z0-9_:]*`. |
| `rate` | float | yes | -- | Events per second. Must be positive. Fractional values allowed (e.g. `0.5`). |
| `duration` | string | no | runs forever | Total run time. Supports `ms`, `s`, `m`, `h` units. |
| `generator` | object | yes | -- | Value generator configuration. See [Generators](generators.md). |
| `encoder` | object | no | `prometheus_text` | Output format. See [Encoders](encoders.md). |
| `sink` | object | no | `stdout` | Output destination. See [Sinks](sinks.md). |
| `labels` | map | no | none | Static key-value labels attached to every event. |

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

### Multi-scenario fields

These fields are only meaningful in multi-scenario mode (via `sonda run`). They control temporal
correlation between scenarios.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `phase_offset` | string | no | none | Delay before starting this scenario. Supports `ms`, `s`, `m`, `h`. |
| `clock_group` | string | no | none | Scenarios with the same clock group share a start time reference. |

See [Multi-scenario files](#multi-scenario-files) below for a working example.

### Duration format

All duration fields (`duration`, `gaps.every`, `gaps.for`, `bursts.every`, `bursts.for`,
`phase_offset`) accept the same format:

| Unit | Example | Description |
|------|---------|-------------|
| `ms` | `100ms` | Milliseconds |
| `s` | `30s` | Seconds |
| `m` | `5m` | Minutes |
| `h` | `1h` | Hours |

## Log scenario files

Log scenarios use a different generator section but share the same structure for gaps, bursts,
encoder, and sink. The key differences:

- The `generator` uses log-specific types (`template` or `replay`).
- There is no `labels` field on log scenarios.
- The default encoder is `json_lines` instead of `prometheus_text`.

```yaml title="log-scenario.yaml"
name: app_logs
rate: 10
duration: 60s

generator:
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

encoder:
  type: json_lines
sink:
  type: stdout
```

```bash
sonda logs --scenario log-scenario.yaml
```

## Multi-scenario files

Run multiple scenarios concurrently from a single file using `sonda run`. Each entry in the
`scenarios` list must include a `signal_type` field (`metrics` or `logs`).

```yaml title="multi-scenario.yaml"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 1
    duration: 120s
    phase_offset: "0s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
      repeat: true
    labels:
      instance: server-01
      job: node
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  - signal_type: metrics
    name: memory_usage_percent
    rate: 1
    duration: 120s
    phase_offset: "3s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
      repeat: true
    labels:
      instance: server-01
      job: node
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

```bash
sonda run --scenario multi-scenario.yaml
```

The `phase_offset` on `memory_usage_percent` delays it by 3 seconds, so CPU spikes first and
memory follows. Both scenarios share the `alert-test` clock group for synchronized timing.

## CLI overrides

Any field in the scenario file can be overridden from the command line. CLI flags always take
precedence over YAML values:

```bash
sonda metrics --scenario scenario.yaml --duration 5s --rate 2
```

This loads the file but overrides `duration` and `rate` with the CLI values.

Encoder options like `--precision` also work as overrides. You can add precision to a YAML
scenario without editing the file:

```bash
sonda metrics --scenario examples/basic-metrics.yaml --precision 2
```
