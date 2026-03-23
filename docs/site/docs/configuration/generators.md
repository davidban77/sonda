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
sonda metrics --name up --rate 2 --duration 2s
```

```text title="Output"
up 0 1774279693496
up 0 1774279694001
up 0 1774279694501
```

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

### csv_replay

Replays numeric values from a CSV file. Use it to reproduce real production metric patterns
captured from monitoring systems.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `file` | string | yes | -- | Path to the CSV file. |
| `column` | integer | no | `0` | Zero-based column index to read. |
| `has_header` | boolean | no | `true` | Whether to skip the first row as a header. |
| `repeat` | boolean | no | `true` | When true, cycles back to the start. When false, holds the last value. |

```yaml title="CSV replay generator"
generator:
  type: csv_replay
  file: examples/sample-cpu-values.csv
  column: 1
  has_header: true
  repeat: true
```

**Shape:** Follows the exact pattern recorded in the CSV file -- the values are replayed verbatim,
one per tick.

!!! note
    The CSV file path is relative to the working directory where you run `sonda`, not
    relative to the scenario file.

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
