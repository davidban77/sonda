# CLI Reference

Sonda provides three subcommands: `metrics` for metric generation, `logs` for log generation, and
`run` for concurrent multi-scenario execution.

## Global options

```
sonda [OPTIONS] <COMMAND>

Options:
  -h, --help     Print help
  -V, --version  Print version
```

```bash
sonda --version
```

```text title="Output"
sonda 0.1.3
```

## sonda metrics

Generate synthetic metrics and write them to the configured sink.

```bash
sonda metrics [OPTIONS]
```

### Scenario and identity

| Flag | Type | Description |
|------|------|-------------|
| `--scenario <FILE>` | path | YAML scenario file. CLI flags override file values. |
| `--name <NAME>` | string | Metric name. Required if no `--scenario`. |
| `--rate <RATE>` | float | Events per second. Required if no `--scenario`. |
| `--duration <DURATION>` | string | Run duration (e.g. `30s`, `5m`). Omit for indefinite. |

### Generator

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--value-mode <MODE>` | string | `constant` | Generator type: `constant`, `uniform`, `sine`, `sawtooth`. |
| `--amplitude <FLOAT>` | float | `1.0` | Sine wave amplitude. |
| `--period-secs <FLOAT>` | float | `60.0` | Sine or sawtooth period in seconds. |
| `--offset <FLOAT>` | float | `0.0` | Sine midpoint or constant value. |
| `--min <FLOAT>` | float | `0.0` | Uniform or sawtooth minimum. |
| `--max <FLOAT>` | float | `1.0` | Uniform or sawtooth maximum. |
| `--seed <INT>` | integer | `0` | RNG seed for deterministic uniform output. |

!!! note
    The `sequence` and `csv_replay` generators are only available via scenario files. They cannot
    be configured with CLI flags alone.

### Gaps and bursts

| Flag | Type | Description |
|------|------|-------------|
| `--gap-every <DURATION>` | string | Gap recurrence interval. Must pair with `--gap-for`. |
| `--gap-for <DURATION>` | string | Gap duration. Must be less than `--gap-every`. |
| `--burst-every <DURATION>` | string | Burst recurrence interval. Must pair with `--burst-for` and `--burst-multiplier`. |
| `--burst-for <DURATION>` | string | Burst duration. Must be less than `--burst-every`. |
| `--burst-multiplier <FLOAT>` | float | Rate multiplier during bursts. |

### Labels, encoder, output

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--label <KEY=VALUE>` | string | none | Static label (repeatable). |
| `--encoder <FORMAT>` | string | `prometheus_text` | Output format: `prometheus_text`, `influx_lp`, `json_lines`. |
| `--output <FILE>` | path | stdout | Write to file instead of stdout. |

### Examples

Simplest metric:

```bash
sonda metrics --name up --rate 1 --duration 5s
```

Sine wave with labels:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --value-mode sine --amplitude 50 --period-secs 60 --offset 50 \
  --label host=web-01 --label zone=us-east-1
```

InfluxDB format to file:

```bash
sonda metrics --name cpu --rate 10 --duration 10s \
  --encoder influx_lp --output /tmp/metrics.influx
```

Scenario file with overrides:

```bash
sonda metrics --scenario examples/basic-metrics.yaml --duration 5s --rate 2
```

## sonda logs

Generate synthetic log events and write them to the configured sink.

```bash
sonda logs [OPTIONS]
```

### Scenario and rate

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--scenario <FILE>` | path | -- | YAML log scenario file. |
| `--mode <MODE>` | string | -- | Generator mode: `template` or `replay`. Required if no `--scenario`. |
| `--rate <RATE>` | float | `10.0` | Events per second. |
| `--duration <DURATION>` | string | indefinite | Run duration. |

### Template mode

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--message <TEXT>` | string | `"synthetic log event"` | Static message template. |
| `--severity-weights <SPEC>` | string | `info=1.0` | Comma-separated severity weights (e.g. `info=0.7,warn=0.2,error=0.1`). |
| `--seed <INT>` | integer | `0` | RNG seed for deterministic output. |

### Replay mode

| Flag | Type | Description |
|------|------|-------------|
| `--file <PATH>` | path | Log file to replay lines from. |

### Labels, encoder, output

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--label <KEY=VALUE>` | string | none | Static label (repeatable). |
| `--encoder <FORMAT>` | string | `json_lines` | Output format: `json_lines`, `syslog`. |
| `--output <FILE>` | path | stdout | Write to file instead of stdout. |

### Gaps and bursts

The same gap and burst flags from `sonda metrics` are available for logs:
`--gap-every`, `--gap-for`, `--burst-every`, `--burst-for`, `--burst-multiplier`.

### Examples

Simple template log:

```bash
sonda logs --mode template --rate 5 --duration 10s
```

Custom message with severity weights:

```bash
sonda logs --mode template --rate 5 --duration 10s \
  --message "Connection timeout" \
  --severity-weights "info=0.7,warn=0.2,error=0.1"
```

Syslog format:

```bash
sonda logs --mode template --rate 5 --duration 5s --encoder syslog
```

Scenario file:

```bash
sonda logs --scenario examples/log-template.yaml --duration 5s
```

## sonda run

Run multiple scenarios concurrently from a multi-scenario YAML file.

```bash
sonda run --scenario <FILE>
```

| Flag | Type | Description |
|------|------|-------------|
| `--scenario <FILE>` | path | Multi-scenario YAML file. Required. |

The file must have a top-level `scenarios:` list. Each entry includes `signal_type: metrics` or
`signal_type: logs`. See [Scenario Files - Multi-scenario files](scenario-file.md#multi-scenario-files).

```bash
sonda run --scenario examples/multi-scenario.yaml
```

## Precedence rules

Configuration values are resolved in this order (highest priority wins):

1. **CLI flags** -- always win when provided.
2. **YAML scenario file** -- base configuration loaded from disk.

If neither is provided for a required field, Sonda exits with an error.

For example, a YAML file sets `rate: 100` and the CLI passes `--rate 500`. The effective rate
is 500.
