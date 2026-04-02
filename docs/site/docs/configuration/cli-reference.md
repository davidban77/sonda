# CLI Reference

Sonda provides three subcommands: `metrics` for metric generation, `logs` for log generation, and
`run` for concurrent multi-scenario execution.

## Global options

```
sonda [--quiet | --verbose] [--dry-run] <COMMAND>
```

| Flag | Short | Description |
|------|-------|-------------|
| `--quiet` | `-q` | Suppress start/stop status banners. Errors still print to stderr. |
| `--verbose` | `-v` | Print resolved scenario config at startup, then run normally. Mutually exclusive with `--quiet`. |
| `--dry-run` | -- | Parse and validate the scenario config, print it, then exit without emitting events. |
| `--help` | `-h` | Print help information. |
| `--version` | `-V` | Print version. |

Global flags go **before** the subcommand:

```bash
sonda -q metrics --name up --rate 1 --duration 5s
sonda --verbose metrics --name up --rate 1 --duration 5s
sonda --dry-run run --scenario examples/multi-scenario.yaml
```

```bash
sonda --version
```

```text title="Output"
sonda 0.3.0
```

## Status output

Sonda prints colored lifecycle banners to stderr when running scenarios. These banners show you
what is running and how it performed, without interfering with data output on stdout.

### Start banner

Printed when a scenario begins:

```text
▶ cpu_usage  signal_type: metrics | rate: 1000/s | encoder: prometheus_text | sink: stdout | duration: 30s
```

### Stop banner

Printed when a scenario completes:

```text
■ cpu_usage  completed in 30.0s | events: 30000 | bytes: 1.2 MB | errors: 0
```

### Color behavior

Colors are automatic and require no configuration:

- **Interactive terminal** -- colors are enabled.
- **Piped output** (`sonda metrics ... | grep foo`) -- colors are disabled on the piped stream. Since banners go to stderr, they stay colored if stderr is still a terminal.
- **`NO_COLOR` environment variable** -- set `NO_COLOR=1` to disable colors everywhere. Sonda respects the [no-color.org](https://no-color.org) convention.
- **Non-TTY stderr** -- colors are disabled when stderr is redirected to a file or pipe.

### Suppressing banners

Use `--quiet` / `-q` to suppress all status output. Only errors are printed:

```bash
# No banners, just data on stdout
sonda -q metrics --name up --rate 5 --duration 5s

# Useful in scripts and CI pipelines
sonda -q metrics --name up --rate 5 --duration 5s > /tmp/data.txt
```

!!! note
    Status banners go to stderr, data goes to stdout. Even without `--quiet`, you can
    safely redirect stdout to a file or pipe it to another program -- banners never mix
    with your data.

### Dry run

Use `--dry-run` to validate a scenario without emitting any events. Sonda parses the
configuration, prints the resolved settings, and exits. This is useful for catching YAML
errors and confirming what Sonda *would* do before committing to a long run.

=== "Metrics"

    ```bash
    sonda --dry-run metrics --name cpu --rate 10 --duration 30s \
      --value-mode sine --amplitude 50 --offset 50 --label host=web-01
    ```

    ```text title="Output"
    [config] Resolved scenario config:

      name:       cpu
      signal:     metrics
      rate:       10/s
      duration:   30s
      generator:  sine (amplitude: 50, period: 60s, offset: 50)
      encoder:    prometheus_text
      sink:       stdout
      labels:     host=web-01

    Validation: OK
    ```

=== "Logs"

    ```bash
    sonda --dry-run logs --mode template --rate 5 --duration 10s \
      --message "Connection timeout" \
      --severity-weights "info=0.7,warn=0.2,error=0.1"
    ```

    ```text title="Output"
    [config] Resolved scenario config:

      name:       logs
      signal:     logs
      rate:       5/s
      duration:   10s
      generator:  template (1 template(s), severity: error=0.1/info=0.7/warn=0.2)
      encoder:    json_lines
      sink:       stdout

    Validation: OK
    ```

=== "Run (multi-scenario)"

    ```bash
    sonda --dry-run run --scenario examples/multi-scenario.yaml
    ```

    ```text title="Output"
    [config] Resolved scenario config:

      name:       cpu_usage
      signal:     metrics
      rate:       100/s
      duration:   30s
      generator:  sine (amplitude: 50, period: 60s, offset: 50)
      encoder:    prometheus_text
      sink:       stdout

    [config] Resolved scenario config:

      name:       app_logs
      signal:     logs
      rate:       10/s
      duration:   30s
      generator:  template (1 template(s), severity: error=0.1/info=0.7/warn=0.2, seed: 42)
      encoder:    json_lines
      sink:       file: /tmp/sonda-logs.json

    Validation: OK
    ```

`--dry-run` works with scenario files too -- handy for validating YAML before deploying:

```bash
sonda --dry-run metrics --scenario examples/basic-metrics.yaml
```

!!! tip
    `--dry-run` is orthogonal to `--quiet` and `--verbose`. It always prints the resolved
    config regardless of other flags, since its whole purpose is to show you what was parsed.

### Verbose mode

Use `--verbose` / `-v` to print the resolved scenario config at startup, then continue
running normally. This gives you the same config dump as `--dry-run`, followed by the
regular start banner, events, and stop banner.

```bash
sonda --verbose metrics --name up --rate 1 --duration 2s
```

```text title="Output (stderr)"
[config] Resolved scenario config:

  name:       up
  signal:     metrics
  rate:       1/s
  duration:   2s
  generator:  constant (value: 0)
  encoder:    prometheus_text
  sink:       stdout

▶ up  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 2s
■ up  completed in 2.0s | events: 3 | bytes: 57 B | errors: 0
```

`--verbose` is mutually exclusive with `--quiet`. If you pass both, Sonda exits with an error:

```text
error: the argument '--verbose' cannot be used with '--quiet'
```

### Verbosity comparison

| Output | Default | `--quiet` | `--verbose` | `--dry-run` |
|--------|---------|-----------|-------------|-------------|
| Resolved config | -- | -- | Yes | Yes |
| Start banner | Yes | -- | Yes | -- |
| Event data | Yes | Yes | Yes | -- |
| Stop banner | Yes | -- | Yes | -- |
| Errors | Yes | Yes | Yes | Yes |

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

### Cardinality spikes

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--spike-label <KEY>` | string | -- | Target label key for the spike. All four required flags must be provided together. |
| `--spike-every <DURATION>` | string | -- | Spike recurrence interval (e.g. `2m`). |
| `--spike-for <DURATION>` | string | -- | Spike duration within each cycle (e.g. `30s`). Must be less than `--spike-every`. |
| `--spike-cardinality <INT>` | integer | -- | Number of unique label values during the spike. |
| `--spike-strategy <STRATEGY>` | string | `counter` | Value generation strategy: `counter` or `random`. |
| `--spike-prefix <PREFIX>` | string | `{label}_` | Prefix for generated spike label values. |
| `--spike-seed <INT>` | integer | `0` | RNG seed for the `random` strategy. |

### Labels, encoder, output

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--label <KEY=VALUE>` | string | none | Static label (repeatable). |
| `--encoder <FORMAT>` | string | `prometheus_text` | Output format: `prometheus_text`, `influx_lp`, `json_lines`. |
| `--precision <INT>` | integer | full f64 | Decimal places for metric values (0--17). See [Encoders - Value precision](encoders.md#value-precision). |
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

Limit metric values to 2 decimal places:

```bash
sonda metrics --name cpu --rate 2 --duration 2s \
  --value-mode sine --amplitude 50 --period-secs 10 --offset 50 \
  --label host=web-01 --precision 2
```

Override precision on an existing YAML scenario:

```bash
sonda metrics --scenario examples/basic-metrics.yaml --precision 2 --duration 5s
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
| `--precision <INT>` | integer | full f64 | Decimal places for numeric values (0--17). Only applies to `json_lines`. |
| `--output <FILE>` | path | stdout | Write to file instead of stdout. |

### Gaps and bursts

The same gap and burst flags from `sonda metrics` are available for logs:
`--gap-every`, `--gap-for`, `--burst-every`, `--burst-for`, `--burst-multiplier`.

### Cardinality spikes

The same cardinality spike flags from `sonda metrics` are available for logs:
`--spike-label`, `--spike-every`, `--spike-for`, `--spike-cardinality`,
`--spike-strategy`, `--spike-prefix`, `--spike-seed`.

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

### Aggregate summary

After all scenarios finish, `sonda run` prints a summary line that aggregates totals across
every scenario in the file:

```text
━━ run complete  scenarios: 2 | events: 3302 | bytes: 174.9 KB | errors: 0 | elapsed: 30.0s
```

The summary includes the scenario count, total events emitted, total bytes written, error count,
and wall-clock elapsed time. Each individual scenario still prints its own stop banner before
the aggregate line appears.

!!! tip
    Pipe the summary to a monitoring script to gate CI pipelines -- a non-zero `errors` count
    means at least one scenario encountered a write failure.

## Precedence rules

Configuration values are resolved in this order (highest priority wins):

1. **CLI flags** -- always win when provided.
2. **YAML scenario file** -- base configuration loaded from disk.

If neither is provided for a required field, Sonda exits with an error.

For example, a YAML file sets `rate: 100` and the CLI passes `--rate 500`. The effective rate
is 500.
