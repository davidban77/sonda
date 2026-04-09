# CLI Reference

Sonda provides subcommands for generating metrics, logs, histograms, and summaries, running
multi-scenario files, browsing a library of built-in scenario patterns, importing CSV data
into parameterized scenarios, and interactively scaffolding new scenario files.

## Global options

```
sonda [--quiet | --verbose] [--dry-run] [--scenario-path <DIR>] [--pack-path <DIR>] <COMMAND>
```

| Flag | Short | Description |
|------|-------|-------------|
| `--quiet` | `-q` | Suppress start/stop banners and live progress. Errors still print to stderr. |
| `--verbose` | `-v` | Print resolved scenario config at startup, then run normally. Mutually exclusive with `--quiet`. |
| `--dry-run` | -- | Parse and validate the scenario config, print it, then exit without emitting events. |
| `--scenario-path <DIR>` | -- | Directory containing scenario YAML files. Overrides `SONDA_SCENARIO_PATH` and default paths. |
| `--pack-path <DIR>` | -- | Directory containing metric pack YAML files. Overrides `SONDA_PACK_PATH` and default paths. |
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
sonda 0.11.0
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

### Live progress

Between the start and stop banners, Sonda shows live progress for each running scenario.
The display updates in place and cleans up before the stop banner prints, so your terminal
stays tidy.

Each progress line shows the event count, bytes emitted, current rate versus configured rate,
and elapsed time. When a gap, burst, or cardinality spike window is active, a colored tag
appears at the end of the line.

**Interactive terminal (TTY):**

Progress lines update in place every 200ms using ANSI cursor control:

```text
  ~ cpu_usage  events: 1,234 | rate: 98.5/s | bytes: 12.3 KB | elapsed: 5.2s
```

The `~` indicator changes color to reflect the scenario state:

| Color | Meaning |
|-------|---------|
| Green | Normal operation |
| Yellow | Gap window active -- events paused |
| Magenta | Burst window active -- elevated rate |

Window state tags also appear when active:

```text
  ~ cpu_usage  events: 1,234 | rate: 0.0/s | bytes: 12.3 KB | elapsed: 5.2s [gap]
  ~ cpu_usage  events: 1,234 | rate: 500.0/s | bytes: 12.3 KB | elapsed: 7.1s [burst]
  ~ cpu_usage  events: 1,234 | rate: 98.5/s | bytes: 12.3 KB | elapsed: 9.0s [spike]
```

**Non-TTY (piped or redirected stderr):**

When stderr is not a terminal, Sonda emits a static `[progress]`-prefixed line every 5 seconds
instead of using ANSI escape sequences. This avoids flooding CI logs while still showing that
the scenario is alive:

```text
[progress] cpu_usage  events: 1234 | rate: 98.5/s | bytes: 12.3 KB | elapsed: 5.1s
```

**Suppression:**

Use `--quiet` / `-q` to suppress all progress output along with banners. Use Ctrl+C at any
time for a clean shutdown -- progress lines are erased and no dangling ANSI sequences are left
behind.

!!! tip
    For short runs (under 5 seconds), you may not see progress lines in non-TTY mode because
    the first update fires at the 5-second mark. TTY mode shows progress within the first 200ms.

### Multi-scenario numbering

When running multiple scenarios (via `sonda run` or a multi-scenario built-in), each banner
includes a `[N/total]` prefix showing its position in the run:

```text
[1/3] ▶ interface_oper_state  signal_type: metrics | rate: 1/s | ...
[2/3] ▶ interface_in_octets   signal_type: metrics | rate: 1/s | ...
[3/3] ▶ interface_errors      signal_type: metrics | rate: 1/s | ...
...
[1/3] ■ interface_oper_state  completed in 3.0s | events: 4 | bytes: 500 B | errors: 0
[2/3] ■ interface_in_octets   completed in 3.0s | events: 4 | bytes: 528 B | errors: 0
[3/3] ■ interface_errors      completed in 3.0s | events: 4 | bytes: 484 B | errors: 0
━━ run complete  scenarios: 3 | events: 12 | bytes: 1.5 KB | errors: 0 | elapsed: 3.0s
```

Stop banners always print in launch order regardless of which scenario finishes first.

### Color behavior

Colors are automatic and require no configuration:

- **Interactive terminal** -- colors are enabled.
- **Piped output** (`sonda metrics ... | grep foo`) -- colors are disabled on the piped stream. Since banners go to stderr, they stay colored if stderr is still a terminal.
- **`NO_COLOR` environment variable** -- set `NO_COLOR=1` to disable colors everywhere. Sonda respects the [no-color.org](https://no-color.org) convention.
- **Non-TTY stderr** -- colors are disabled when stderr is redirected to a file or pipe.

### Suppressing banners and progress

Use `--quiet` / `-q` to suppress all status output including live progress. Only errors are
printed:

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
    [config] cpu

      name:          cpu
      signal:        metrics
      rate:          10/s
      duration:      30s
      generator:     sine (amplitude: 50, period: 60s, offset: 50)
      encoder:       prometheus_text
      sink:          stdout
      labels:        host=web-01

    Validation: OK (1 scenario)
    ```

=== "Logs"

    ```bash
    sonda --dry-run logs --mode template --rate 5 --duration 10s \
      --message "Connection timeout" \
      --severity-weights "info=0.7,warn=0.2,error=0.1"
    ```

    ```text title="Output"
    [config] logs

      name:          logs
      signal:        logs
      rate:          5/s
      duration:      10s
      generator:     template (1 template(s), severity: error=0.1/info=0.7/warn=0.2)
      encoder:       json_lines
      sink:          stdout

    Validation: OK (1 scenario)
    ```

=== "Run (multi-scenario)"

    ```bash
    sonda --dry-run run --scenario examples/multi-scenario.yaml
    ```

    ```text title="Output"
    [config] [1/2] cpu_usage

      name:          cpu_usage
      signal:        metrics
      rate:          100/s
      duration:      30s
      generator:     sine (amplitude: 50, period: 60s, offset: 50)
      encoder:       prometheus_text
      sink:          stdout

    ───
    [config] [2/2] app_logs

      name:          app_logs
      signal:        logs
      rate:          10/s
      duration:      30s
      generator:     template (1 template(s), severity: error=0.1/info=0.7/warn=0.2, seed: 42)
      encoder:       json_lines
      sink:          file: /tmp/sonda-logs.json

    Validation: OK (2 scenarios)
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
sonda 0.11.0 (http) — synthetic telemetry generator
[config] up

  name:          up
  signal:        metrics
  rate:          1/s
  duration:      2s
  generator:     constant (value: 0)
  encoder:       prometheus_text
  sink:          stdout

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
| Branding header | -- | -- | Yes | -- |
| Resolved config | -- | -- | Yes | Yes |
| Start banner | Yes | -- | Yes | -- |
| Live progress | Yes | -- | Yes | -- |
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
| `--scenario <FILE \| @name>` | path or `@name` | YAML scenario file, or a `@name` shorthand to load a [built-in scenario](../guides/scenarios.md) (e.g. `@cpu-spike`). CLI flags override file values. |
| `--name <NAME>` | string | Metric name. Required if no `--scenario`. |
| `--rate <RATE>` | float | Events per second. Required if no `--scenario`. |
| `--duration <DURATION>` | string | Run duration (e.g. `30s`, `5m`, `1.5s`). Fractional values supported. Omit for indefinite. |

### Generator

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--value-mode <MODE>` | string | `constant` | Generator type: `constant`, `uniform`, `sine`, `sawtooth`. |
| `--value <FLOAT>` | float | -- | Fixed value for the `constant` generator. Only valid when `--value-mode` is `constant` (the default). When omitted, defaults to `0.0`. |
| `--amplitude <FLOAT>` | float | `1.0` | Sine wave amplitude. |
| `--period-secs <FLOAT>` | float | `60.0` | Sine or sawtooth period in seconds. |
| `--offset <FLOAT>` | float | `0.0` | Sine wave vertical offset. Sets the midpoint around which the wave oscillates. Only valid when `--value-mode` is `sine`. |
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

### Jitter

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--jitter <FLOAT>` | float | none | Jitter amplitude. Adds uniform noise in `[-jitter, +jitter]` to every generated value. Must be non-negative. |
| `--jitter-seed <INT>` | integer | `0` | Seed for deterministic jitter noise. Different seeds produce different noise sequences. |

### Labels, encoder, output

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--label <KEY=VALUE>` | string | none | Static label (repeatable). |
| `--encoder <FORMAT>` | string | `prometheus_text` | Output format: `prometheus_text`, `influx_lp`, `json_lines`, `remote_write`, `otlp`. The last two require the `remote-write` and `otlp` Cargo features. |
| `--precision <INT>` | integer | full f64 | Decimal places for metric values (0--17). See [Encoders - Value precision](encoders.md#value-precision). |
| `--output <FILE>` | path | stdout | Write to file instead of stdout. Mutually exclusive with `--sink`. |

### Sink

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--sink <TYPE>` | string | none | Sink type: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`. Mutually exclusive with `--output`. |
| `--endpoint <URL>` | string | none | URL for `http_push`, `remote_write`, `loki`, and `otlp_grpc` sinks. Required for those types. |
| `--signal-type <TYPE>` | string | none | OTLP signal type: `metrics` or `logs`. Required for `--sink otlp_grpc` in the metrics subcommand. |
| `--batch-size <N>` | integer | varies | Batch size for batching sinks. Meaning varies by sink (bytes for `http_push`, entries for others). |
| `--content-type <TYPE>` | string | `application/octet-stream` | Content-Type header for `http_push`. |
| `--brokers <STRING>` | string | none | Comma-separated Kafka broker addresses. Required for `--sink kafka`. |
| `--topic <STRING>` | string | none | Kafka topic name. Required for `--sink kafka`. |

### Retry

Configure retry with exponential backoff for network sinks. All three flags must be provided
together, or none at all. CLI retry flags override any `retry:` block in the YAML scenario file.

| Flag | Type | Description |
|------|------|-------------|
| `--retry-max-attempts <N>` | integer | Retry attempts after initial failure. Total calls = N + 1. |
| `--retry-backoff <DURATION>` | string | Initial backoff duration (e.g. `100ms`, `1s`). |
| `--retry-max-backoff <DURATION>` | string | Maximum backoff cap (e.g. `5s`). Must be >= `--retry-backoff`. |

Applies to: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`, `tcp`.
Using retry flags with `stdout`, `file`, or `udp` produces a validation error.

For details on backoff behavior and error classification, see [Sinks - Retry with backoff](sinks.md#retry-with-backoff).

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

Send metrics to an HTTP endpoint:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink http_push --endpoint http://localhost:9090/api/v1/write
```

Send metrics via Prometheus remote write:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Send metrics to an OTLP collector:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317 --signal-type metrics
```

Retry on transient failures (up to 3 retries with exponential backoff):

```bash
sonda metrics --name cpu --rate 10 --duration 60s \
  --sink http_push --endpoint http://localhost:8428/api/v1/import/prometheus \
  --retry-max-attempts 3 --retry-backoff 100ms --retry-max-backoff 5s
```

## sonda logs

Generate synthetic log events and write them to the configured sink.

```bash
sonda logs [OPTIONS]
```

### Scenario and rate

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--scenario <FILE \| @name>` | path or `@name` | -- | YAML log scenario file, or a `@name` [built-in scenario](../guides/scenarios.md) (e.g. `@log-storm`). |
| `--mode <MODE>` | string | -- | Generator mode: `template` or `replay`. Required if no `--scenario`. |
| `--rate <RATE>` | float | `10.0` | Events per second. |
| `--duration <DURATION>` | string | indefinite | Run duration (e.g. `10s`, `1.5s`). Fractional values supported. |

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
| `--encoder <FORMAT>` | string | `json_lines` | Output format: `json_lines`, `syslog`, `otlp`. The last one requires the `otlp` Cargo feature. |
| `--precision <INT>` | integer | full f64 | Decimal places for numeric values (0--17). Only applies to `json_lines`. |
| `--output <FILE>` | path | stdout | Write to file instead of stdout. Mutually exclusive with `--sink`. |

### Sink

The same sink flags from `sonda metrics` are available for logs:
`--sink`, `--endpoint`, `--signal-type`, `--batch-size`, `--content-type`, `--brokers`, `--topic`.

When `--sink otlp_grpc` is used with the logs subcommand, `--signal-type` defaults to `logs`
automatically, so you typically do not need to specify it.

### Gaps and bursts

The same gap and burst flags from `sonda metrics` are available for logs:
`--gap-every`, `--gap-for`, `--burst-every`, `--burst-for`, `--burst-multiplier`.

### Cardinality spikes

The same cardinality spike flags from `sonda metrics` are available for logs:
`--spike-label`, `--spike-every`, `--spike-for`, `--spike-cardinality`,
`--spike-strategy`, `--spike-prefix`, `--spike-seed`.

### Jitter

The same jitter flags from `sonda metrics` are available for logs:
`--jitter`, `--jitter-seed`.

### Retry

The same retry flags from `sonda metrics` are available for logs:
`--retry-max-attempts`, `--retry-backoff`, `--retry-max-backoff`.

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

Send logs to Loki:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --sink loki --endpoint http://localhost:3100 \
  --label app=myservice --label env=staging
```

Send logs to an OTLP collector:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317
```

Send logs to Loki with retry:

```bash
sonda logs --mode template --rate 10 --duration 60s \
  --sink loki --endpoint http://localhost:3100 \
  --label app=myservice --label env=staging \
  --retry-max-attempts 5 --retry-backoff 200ms --retry-max-backoff 10s
```

## sonda histogram

Generate synthetic histogram metrics (cumulative bucket counts, `_count`, `_sum`) and write them
to the configured sink. Requires a scenario file -- histogram configuration is too complex for
inline CLI flags.

```bash
sonda histogram --scenario <FILE | @name>
```

| Flag | Type | Description |
|------|------|-------------|
| `--scenario <FILE \| @name>` | path or `@name` | YAML histogram scenario file, or a `@name` [built-in scenario](../guides/scenarios.md) (e.g. `@histogram-latency`). Required. |

The scenario file must contain a `distribution` block and may include `buckets`,
`observations_per_tick`, `mean_shift_per_sec`, and `seed`. See
[Generators -- histogram](generators.md#histogram) for the full field reference.

```bash
sonda histogram --scenario examples/histogram.yaml
```

Dry run to validate config:

```bash
sonda --dry-run histogram --scenario examples/histogram.yaml
```

```text title="Output"
[config] http_request_duration_seconds

  name:          http_request_duration_seconds
  signal:        histogram
  rate:          1/s
  duration:      10s
  buckets:       default (Prometheus)
  distribution:  Exponential { rate: 10.0 }
  obs/tick:      100
  encoder:       prometheus_text
  sink:          stdout
  labels:        handler=/api/v1/query, method=GET

Validation: OK (1 scenario)
```

!!! note
    The `histogram` subcommand only accepts `--scenario`. Unlike `sonda metrics`, it does not
    support inline generator flags. All histogram parameters must be defined in the YAML file.

## sonda summary

Generate synthetic summary metrics (pre-computed quantile values, `_count`, `_sum`) and write
them to the configured sink. Requires a scenario file.

```bash
sonda summary --scenario <FILE | @name>
```

| Flag | Type | Description |
|------|------|-------------|
| `--scenario <FILE \| @name>` | path or `@name` | YAML summary scenario file, or a `@name` [built-in scenario](../guides/scenarios.md). Required. |

The scenario file must contain a `distribution` block and may include `quantiles`,
`observations_per_tick`, `mean_shift_per_sec`, and `seed`. See
[Generators -- summary](generators.md#summary) for the full field reference.

```bash
sonda summary --scenario examples/summary.yaml
```

Dry run to validate config:

```bash
sonda --dry-run summary --scenario examples/summary.yaml
```

```text title="Output"
[config] rpc_duration_seconds

  name:          rpc_duration_seconds
  signal:        summary
  rate:          1/s
  duration:      10s
  quantiles:     default [0.5, 0.9, 0.95, 0.99]
  distribution:  Normal { mean: 0.1, stddev: 0.02 }
  obs/tick:      100
  encoder:       prometheus_text
  sink:          stdout
  labels:        method=GetUser, service=auth

Validation: OK (1 scenario)
```

!!! note
    Like `sonda histogram`, the `summary` subcommand only accepts `--scenario`. All summary
    parameters must be defined in the YAML file.

## sonda run

Run multiple scenarios concurrently from a multi-scenario YAML file.

```bash
sonda run --scenario <FILE | @name>
```

| Flag | Type | Description |
|------|------|-------------|
| `--scenario <FILE \| @name>` | path or `@name` | Multi-scenario YAML file, or a `@name` [built-in scenario](../guides/scenarios.md) (e.g. `@interface-flap`). Required. |

The file (or built-in) must have a top-level `scenarios:` list. Each entry includes a `signal_type`
field: `metrics`, `logs`, `histogram`, or `summary`. See
[Scenario Files - Multi-scenario files](scenario-file.md#multi-scenario-files).

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
and wall-clock elapsed time. Each individual scenario prints its own `[N/total]`-prefixed stop
banner in launch order before the aggregate line appears.

!!! tip
    Pipe the summary to a monitoring script to gate CI pipelines -- a non-zero `errors` count
    means at least one scenario encountered a write failure.

## sonda scenarios

Browse, inspect, and run [built-in scenario patterns](../guides/scenarios.md) discovered from
the filesystem. No network access needed.

```bash
sonda scenarios <COMMAND>
```

### scenarios list

List all available built-in scenarios in a formatted table.

```bash
sonda scenarios list [--category <CATEGORY>] [--json]
```

| Flag | Type | Description |
|------|------|-------------|
| `--category <CATEGORY>` | string | Filter by category: `infrastructure`, `network`, `application`, `observability`. |
| `--json` | flag | Output the list as a JSON array instead of a table. Each element contains `name`, `category`, `signal_type`, and `description` fields. |

```bash
sonda scenarios list
sonda scenarios list --category application
sonda scenarios list --json
```

### scenarios show

Print the raw YAML for a built-in scenario to stdout. Pipe to a file to create a customizable copy.

```bash
sonda scenarios show <NAME>
```

| Argument | Type | Description |
|----------|------|-------------|
| `<NAME>` | string | Kebab-case scenario name (e.g. `cpu-spike`). |

```bash
sonda scenarios show cpu-spike
sonda scenarios show memory-leak > my-memory-leak.yaml
```

### scenarios run

Execute a built-in scenario with optional overrides. Equivalent to running the scenario YAML
directly, but with a focused set of override flags.

```bash
sonda scenarios run <NAME> [OPTIONS]
```

| Argument / Flag | Type | Description |
|-----------------|------|-------------|
| `<NAME>` | string | Kebab-case scenario name (e.g. `cpu-spike`). |
| `--duration <DURATION>` | string | Override the run duration (e.g. `10s`, `2m`). |
| `--rate <RATE>` | float | Override events per second. |
| `--encoder <ENCODER>` | string | Override the encoder format (e.g. `prometheus_text`, `json_lines`). |
| `--sink <TYPE>` | string | Override the sink type (e.g. `stdout`, `http_push`). |
| `--endpoint <URL>` | string | Override the sink endpoint (required for network sinks). |

```bash
sonda scenarios run cpu-spike --duration 10s --rate 5
sonda scenarios run log-storm --sink loki --endpoint http://localhost:3100
sonda --dry-run scenarios run cpu-spike
```

!!! tip
    For the full set of subcommand-specific flags (e.g. `--label`, `--precision`, `--value`),
    use the `@name` shorthand with `metrics`, `logs`, or `histogram` instead:
    `sonda metrics --scenario @cpu-spike --label env=staging`

## sonda packs

Browse, inspect, and run [metric packs](../guides/metric-packs.md) discovered from the
filesystem. A metric pack is a reusable bundle of metric names and label schemas that expands
into a multi-metric scenario.

```bash
sonda packs <COMMAND>
```

### packs list

List all available built-in metric packs in a formatted table.

```bash
sonda packs list [--category <CATEGORY>] [--json]
```

| Flag | Type | Description |
|------|------|-------------|
| `--category <CATEGORY>` | string | Filter by category: `infrastructure`, `network`. |
| `--json` | flag | Output the list as a JSON array. Each element contains `name`, `category`, `metric_count`, and `description` fields. |

```bash
sonda packs list
sonda packs list --category network
sonda packs list --json
```

### packs show

Print the raw YAML definition for a built-in pack to stdout. Pipe to a file to create a
customizable copy.

```bash
sonda packs show <NAME>
```

| Argument | Type | Description |
|----------|------|-------------|
| `<NAME>` | string | Snake_case pack name (e.g. `telegraf_snmp_interface`). |

```bash
sonda packs show telegraf_snmp_interface
sonda packs show node_exporter_cpu > my-cpu-pack.yaml
```

### packs run

Execute a built-in pack with optional overrides. Expands the pack into one metric scenario per
metric and runs them concurrently.

```bash
sonda packs run <NAME> [OPTIONS]
```

| Argument / Flag | Type | Description |
|-----------------|------|-------------|
| `<NAME>` | string | Snake_case pack name (e.g. `telegraf_snmp_interface`). |
| `--rate <RATE>` | float | Events per second for each metric in the pack. |
| `--duration <DURATION>` | string | Run duration (e.g. `10s`, `2m`). |
| `--encoder <ENCODER>` | string | Override the encoder format (e.g. `prometheus_text`, `json_lines`). |
| `--sink <TYPE>` | string | Override the sink type (e.g. `stdout`, `http_push`). |
| `--endpoint <URL>` | string | Override the sink endpoint (required for network sinks). |
| `--label <KEY=VALUE>` | string | Add or override a label (repeatable). |

```bash
sonda packs run telegraf_snmp_interface \
  --rate 1 --duration 10s \
  --label device=rtr-edge-01 \
  --label ifName=GigabitEthernet0/0/0 \
  --label ifIndex=1

sonda packs run node_exporter_cpu \
  --rate 1 --duration 30s \
  --label instance=web-01

sonda --dry-run packs run node_exporter_memory \
  --rate 1 --duration 10s \
  --label instance=db-01
```

!!! tip
    For per-metric generator overrides, use a [pack scenario YAML file](../guides/metric-packs.md#per-metric-overrides)
    instead of `packs run`. The CLI does not support overrides directly -- those require the
    `overrides:` block in YAML.

## sonda import

Analyze a CSV file, detect time-series patterns, and generate a portable scenario YAML that uses
generators instead of `csv_replay`. For a detailed walkthrough with examples, see the
[CSV Import](../guides/csv-import.md) guide.

```bash
sonda import <FILE> [OPTIONS]
```

| Argument / Flag | Type | Default | Description |
|-----------------|------|---------|-------------|
| `<FILE>` | path | -- | CSV file to import. Supports Grafana "Series joined by time" exports and plain CSV with a header row. |
| `--analyze` | flag | -- | Print a read-only analysis of detected patterns. No file output. Conflicts with `-o` and `--run`. |
| `-o, --output <FILE>` | path | -- | Write the generated scenario YAML to this path. Conflicts with `--analyze` and `--run`. |
| `--run` | flag | -- | Generate the scenario and immediately execute it. No file output. Conflicts with `--analyze` and `-o`. |
| `--columns <INDICES>` | string | all non-timestamp | Comma-separated column indices (e.g., `1,3,5`). Column 0 is the timestamp. |
| `--rate <RATE>` | float | `1.0` | Events per second in the generated scenario. |
| `--duration <DURATION>` | string | `60s` | Duration of the generated scenario (e.g., `60s`, `5m`). |

Exactly one of `--analyze`, `-o`, or `--run` must be specified.

Analyze patterns in a CSV file:

```bash
sonda import data.csv --analyze
```

Generate a scenario YAML:

```bash
sonda import data.csv -o scenario.yaml --rate 10 --duration 5m
```

Generate and run immediately:

```bash
sonda import data.csv --run --duration 30s
```

Import only specific columns:

```bash
sonda import data.csv --columns 1,3,5 -o scenario.yaml
```

!!! tip
    `--run` integrates with global flags. Use `sonda --dry-run import data.csv --run` to
    validate the generated scenario without emitting events, or `sonda --verbose import data.csv --run`
    to see the resolved config at startup.

## sonda init

Create a new scenario YAML file. By default, `sonda init` walks you through an interactive
prompt flow -- signal type, domain, situation, parameters, labels, encoding, and sink -- and
writes a commented, immediately-runnable YAML file.

You can also supply CLI flags to skip prompts, pre-fill values from a built-in scenario or
CSV file, or run fully non-interactively.

```bash
sonda init [OPTIONS]
```

### Flags

| Flag | Short | Type | Description |
|------|-------|------|-------------|
| `--from <SOURCE>` | -- | string | Pre-fill values from a built-in scenario (`@name`) or CSV file (`path.csv`). See [Pre-filling with --from](#pre-filling-with-from). |
| `--signal-type <TYPE>` | -- | string | Signal type: `metrics` or `logs`. |
| `--domain <DOMAIN>` | -- | string | Domain category: `infrastructure`, `network`, `application`, `custom`. |
| `--situation <ALIAS>` | -- | string | Operational situation: `steady`, `spike_event`, `flap`, `leak`, `saturation`, `degradation`. |
| `--metric <NAME>` | -- | string | Metric name. |
| `--pack <NAME>` | -- | string | Use a metric pack instead of a single metric. Mutually exclusive with `--metric` and `--situation`. |
| `--rate <RATE>` | -- | float | Events per second. |
| `--duration <DURATION>` | -- | string | Run duration (e.g. `60s`, `5m`). |
| `--encoder <FORMAT>` | -- | string | Encoder: `prometheus_text`, `influx_lp`, `json_lines`, `syslog`. |
| `--sink <TYPE>` | -- | string | Sink: `stdout`, `http_push`, `file`, `remote_write`, `loki`, `otlp_grpc`, `kafka`, `tcp`, `udp`. |
| `--endpoint <URL>` | -- | string | Sink endpoint (URL, file path, or `host:port`). |
| `--output <PATH>` | `-o` | path | Output file path for the generated YAML. |
| `--label <KEY=VALUE>` | -- | string | Static label (repeatable). |

All flags are optional. When a flag is provided, its corresponding interactive prompt is
skipped. When **all** required fields are supplied via flags, `sonda init` runs fully
non-interactively -- no terminal interaction needed.

### Non-interactive mode

Supply enough flags to skip every prompt and `sonda init` generates the YAML without asking
any questions. This is useful in scripts, CI pipelines, or when you already know what you want.

```bash
sonda init \
  --signal-type metrics \
  --domain infrastructure \
  --metric node_cpu_seconds \
  --situation steady \
  --rate 1 --duration 60s \
  --encoder prometheus_text \
  --sink stdout \
  -o ./scenarios/node-cpu.yaml
```

Partial flags work too -- Sonda prompts only for the missing values. For example, if you
supply `--signal-type` and `--domain` but nothing else, the wizard starts at step 3:

```bash
sonda init --signal-type metrics --domain network
```

### Pre-filling with --from

The `--from` flag loads default values from an existing source and uses them as prompt
defaults. You can override any pre-filled value with an explicit flag.

=== "--from @builtin"

    Load a built-in scenario by name. Sonda extracts the signal type, domain, metric name,
    generator type, rate, duration, encoder, and sink from the scenario YAML and uses them
    as defaults:

    ```bash
    sonda init --from @cpu-spike
    ```

    This pre-fills the prompts with the `cpu-spike` scenario's configuration. You can
    override individual fields:

    ```bash
    sonda init --from @cpu-spike --rate 5 --duration 2m --sink http_push \
      --endpoint http://localhost:9090/api/v1/write
    ```

    !!! tip
        Use `sonda scenarios list` to see available built-in scenario names.

=== "--from CSV"

    Point at a CSV file to detect the dominant time-series pattern and use it as the
    situation default. Sonda reads the first numeric column, runs pattern detection
    (the same engine as `sonda import`), and maps the result to an operational situation:

    ```bash
    sonda init --from metrics.csv
    ```

    Detected patterns map to situations: Steady becomes `steady`, Spike becomes
    `spike_event`, Climb becomes `leak`, Sawtooth becomes `saturation`, and Flap becomes
    `flap`. The first column name is used as the default metric name.

    Combine with flags to override:

    ```bash
    sonda init --from metrics.csv --metric custom_name --rate 10
    ```

When `--from` is active, Sonda prints a summary of pre-filled values before starting the
prompts so you can see what was loaded:

```text
  Starting from: @cpu-spike
    signal_type: metrics
    domain:      infrastructure
    metric:      cpu_spike
    situation:   spike_event
    rate:        1
    duration:    60s
    encoder:     prometheus_text
    sink:        stdout
```

### Interactive flow

| Step | Prompt | Options |
|------|--------|---------|
| 1 | Signal type | `metrics`, `logs` |
| 2 | Domain | `infrastructure`, `network`, `application`, `custom` |
| 3 | Approach (metrics only) | Single metric, or use a [metric pack](../guides/metric-packs.md) |
| 4a | Metric details (single) | Name, situation, situation parameters, labels |
| 4b | Pack details | Pack selection (filtered by domain), fill in required shared labels, extra labels |
| 4c | Log details | Name, message template, severity distribution, labels |
| 5 | Delivery | Rate, duration, encoder, sink (primary or [advanced](#advanced-sinks)), endpoint |
| 6 | Output path | Defaults to `./scenarios/<name>.yaml` |
| 7 | Run now | Execute the scenario immediately, or exit with instructions |

### Situations (operational vocabulary)

Instead of asking for raw generator types, `sonda init` presents operational situations
that map to generator configurations under the hood:

| Situation | What it models | Key parameters |
|-----------|---------------|----------------|
| `steady` | Stable value with gentle oscillation and noise | center, amplitude, period |
| `spike_event` | Baseline with periodic spikes (anomaly testing) | baseline, spike height, spike duration, spike interval |
| `flap` | Value toggling between two states (up/down) | up value, down value, up duration, down duration |
| `leak` | Gradual climb to a ceiling (memory leak) | baseline, ceiling, time to ceiling |
| `saturation` | Repeating fill-and-reset cycles | baseline, ceiling, time to saturate |
| `degradation` | Slow ramp with increasing noise | baseline, ceiling, time to degrade, noise |

### Pack filtering by domain

When you choose "Use a metric pack" at step 3, the pack list is filtered to show only packs
whose category matches your selected domain. For example, choosing the `network` domain shows
only network packs (like `telegraf_snmp_interface`), while `infrastructure` shows packs like
`node_exporter_cpu` and `node_exporter_memory`.

If no packs match the selected domain, Sonda falls back to showing all available packs so you
are never stuck with an empty list.

!!! tip
    Packs are loaded from the [pack search path](#sonda-packs). Use `sonda packs list` to see
    what is available, or `--pack-path` to point at a custom directory.

### Advanced sinks

The sink prompt shows three common options first -- `stdout`, `http_push`, and `file`. To
access protocol-specific sinks, select **Advanced...** to open a second menu with six
additional sinks:

| Sink | Protocol | Prompted fields |
|------|----------|-----------------|
| `remote_write` | Prometheus remote write (protobuf + snappy) | Endpoint URL |
| `loki` | Grafana Loki HTTP push | Loki base URL |
| `otlp_grpc` | OpenTelemetry Collector gRPC | Endpoint URL, signal type (`metrics` or `logs`) |
| `kafka` | Apache Kafka producer | Broker address(es), topic name |
| `tcp` | Raw TCP socket | Address (`host:port`) |
| `udp` | Raw UDP socket | Address (`host:port`) |

Each advanced sink prompts for the connection details specific to its protocol.

!!! warning "Encoder auto-override"
    Some sinks require a specific wire format. When you select one of these sinks, Sonda
    automatically overrides your encoder choice and prints a note explaining the change:

    | Sink | Required encoder |
    |------|-----------------|
    | `remote_write` | `remote_write` |
    | `otlp_grpc` | `otlp` |

    For example, if you chose `prometheus_text` as your encoder but then selected the
    `remote_write` sink, the encoder is silently switched to `remote_write`:

    ```text
    ? Output encoding format prometheus_text
    ? Where should output be sent? Advanced...
    ? Which advanced sink? remote_write - Prometheus remote write (protobuf + snappy)
    ? Remote write endpoint URL http://localhost:8428/api/v1/write
      Encoder overridden to 'remote_write' (required by the remote_write sink).
    ```

    All other sinks work with any encoder you choose.

!!! warning "Feature-gated sinks"
    The `remote_write` and `otlp_grpc` sinks require Cargo feature flags when building from
    source. Pre-built binaries include `remote_write` by default. See
    [Sinks](sinks.md#remote_write) for build details.

### Immediate execution

After writing the YAML file, `sonda init` offers to run the scenario immediately:

```text
? Run it now? [Y/n]
```

Pressing Enter (or typing `Y`) executes the scenario using the same pipeline as
`sonda run --scenario`. Typing `n` exits with the file path and run command printed
so you can execute it later.

This lets you go from zero to running telemetry in a single `sonda init` invocation --
no need to copy-paste a follow-up command.

### Example session

=== "Single metric with advanced sink"

    ```text
    sonda init — guided scenario scaffolding
    Answer the prompts to generate a runnable scenario YAML.
    Every prompt has a default — press Enter to accept it.

    ── [1/4] Signal ─────────────────────────────

    ? What type of signal? metrics
    ? What domain? infrastructure

    ── [2/4] Metric ─────────────────────────────

    ? How would you like to define metrics? Single metric
    ? Metric name node_cpu_usage_percent
    ? What situation should this metric simulate? spike_event - baseline with periodic spikes
    ? Baseline value (between spikes) 35
    ? Spike height (amount added during spike) 60
    ? Spike duration 10s
    ? Spike interval (time between spikes) 30s
    ? Add a label (key=value, empty to finish) instance=web-01
    ? Add a label (key=value, empty to finish)

    ── [3/4] Delivery ───────────────────────────

    ? Events per second (rate) 1
    ? Duration (e.g., 30s, 5m, 1h) 60s
    ? Output encoding format prometheus_text
    ? Where should output be sent? Advanced...
      Advanced sinks may require feature flags at compile time.
    ? Which advanced sink? remote_write - Prometheus remote write (protobuf + snappy)
    ? Remote write endpoint URL http://localhost:8428/api/v1/write
      Encoder overridden to 'remote_write' (required by the remote_write sink).

    ── Preview ──────────────────────────────────

      # ...YAML preview...

    ── [4/4] Output ─────────────────────────────

    ? Output file path ./scenarios/node-cpu-usage-percent.yaml

    ✔ Scenario created

      name:  node_cpu_usage_percent
      type:  metrics
      file:  ./scenarios/node-cpu-usage-percent.yaml

      Run it with:
        sonda metrics --scenario ./scenarios/node-cpu-usage-percent.yaml
        sonda run --scenario ./scenarios/node-cpu-usage-percent.yaml

    ? Run it now? Yes
      Running scenario...
    ▶ node_cpu_usage_percent  signal_type: metrics | rate: 1/s | ...
    ```

=== "Metric pack (domain-filtered)"

    ```text
    sonda init — guided scenario scaffolding
    Answer the prompts to generate a runnable scenario YAML.
    Every prompt has a default — press Enter to accept it.

    ── [1/4] Signal ─────────────────────────────

    ? What type of signal? metrics
    ? What domain? network

    ── [2/4] Metric ─────────────────────────────

    ? How would you like to define metrics? Use a metric pack
      Showing packs for domain: network
    ? Which metric pack? telegraf_snmp_interface - SNMP interface metrics (5 metrics)
    ? Value for label 'agent_host' my-agent-host
    ? Add a label (key=value, empty to finish)

    ── [3/4] Delivery ───────────────────────────

    ? Events per second (rate) 10
    ? Duration (e.g., 30s, 5m, 1h) 5m
    ? Output encoding format prometheus_text
    ? Where should output be sent? stdout

    ── [4/4] Output ─────────────────────────────

    ? Output file path ./scenarios/telegraf-snmp-interface.yaml

    ✔ Scenario created

      name:  telegraf_snmp_interface
      type:  metrics (pack)
      file:  ./scenarios/telegraf-snmp-interface.yaml

      Run it with:
        sonda run --scenario ./scenarios/telegraf-snmp-interface.yaml

    ? Run it now? No
    ```

=== "Non-interactive (full)"

    All prompts skipped -- no terminal interaction:

    ```bash
    sonda init \
      --signal-type metrics \
      --domain infrastructure \
      --metric node_memory_used_bytes \
      --situation leak \
      --rate 1 --duration 5m \
      --encoder prometheus_text \
      --sink stdout \
      --label instance=db-01 \
      -o ./scenarios/memory-leak.yaml
    ```

=== "--from @builtin with overrides"

    Start from the built-in `cpu-spike` scenario, override the sink and rate:

    ```bash
    sonda init --from @cpu-spike \
      --rate 5 \
      --sink http_push --endpoint http://localhost:9090/api/v1/write \
      -o ./scenarios/cpu-spike-fast.yaml
    ```

    Pre-filled values from the built-in are shown before prompts begin. Only
    fields not covered by `--from` or explicit flags are prompted interactively.

=== "--from CSV"

    Detect patterns from a Grafana CSV export and use them as defaults:

    ```bash
    sonda init --from metrics.csv --rate 10 --duration 2m
    ```

    Sonda reads the first numeric column, detects the dominant pattern (e.g. spike,
    steady), and maps it to a situation. The column name becomes the default metric name.

The generated YAML includes inline comments and scenario metadata, so it appears in
`sonda scenarios list` automatically.

## Precedence rules

Configuration values are resolved in this order (highest priority wins):

1. **CLI flags** -- always win when provided.
2. **YAML scenario file** -- base configuration loaded from disk.

If neither is provided for a required field, Sonda exits with an error.

For example, a YAML file sets `rate: 100` and the CLI passes `--rate 500`. The effective rate
is 500.
