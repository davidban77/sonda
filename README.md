# Sonda

Sonda is a synthetic telemetry generator written in Rust. It produces realistic observability signals
— metrics, logs, traces, and flows — for use in lab environments, pipeline validation, load testing,
and incident simulation.

Its purpose is not to produce perfectly regular data or pure random noise, but to model the kinds of
failure patterns that actually break real observability pipelines: gaps, micro-bursts, cardinality
changes, and pattern-driven value sequences.

**The core library (`sonda-core`) is the product.** The CLI is a delivery mechanism built on top of it.

---

## Features

- **Multiple value generators** — constant, uniform random (seeded for deterministic replay), sine wave, sawtooth ramp.
- **Intentional gap windows** — recurring silent periods that test alert flap detection, gap-fill logic, and buffer sizing.
- **Prometheus text exposition format** — output is valid `text/plain 0.0.4` ready for scraping or piping.
- **Static binary** — statically linked for maximum portability: runs on bare metal, Docker, and CI without a runtime installation.
- **YAML scenario files** — all runtime behavior is defined in YAML; CLI flags override any value.
- **Zero C dependencies** — pure Rust throughout; compatible with `x86_64-unknown-linux-musl`.

---

## Installation

### Build from source

```bash
# Debug build (for development)
cargo build -p sonda

# Release build
cargo build --release -p sonda

# Fully static musl binary (requires musl target)
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p sonda
```

The resulting binary is at `target/release/sonda` (or `target/x86_64-unknown-linux-musl/release/sonda`
for the musl build).

---

## Quick Start

Generate 10 Prometheus metric lines per second for 5 seconds:

```bash
sonda metrics --name up --rate 10 --duration 5s
```

Example output:

```
up 1 1742500000123
up 1 1742500000223
up 1 1742500000323
...
```

Generate a sine wave with labels:

```bash
sonda metrics \
  --name cpu_usage \
  --rate 100 \
  --duration 30s \
  --value-mode sine \
  --amplitude 5 \
  --period-secs 30 \
  --offset 50 \
  --label hostname=t0-a1 \
  --label zone=eu1
```

Example output:

```
cpu_usage{hostname="t0-a1",zone="eu1"} 50 1742500000100
cpu_usage{hostname="t0-a1",zone="eu1"} 50.1045 1742500000110
...
```

Run from a YAML scenario file:

```bash
sonda metrics --scenario examples/basic-metrics.yaml
```

---

## CLI Reference

```
sonda <COMMAND>

Commands:
  metrics  Generate synthetic metrics and write them to the configured sink
  help     Print help information

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### `sonda metrics`

```
Usage: sonda metrics [OPTIONS]

Options:
      --scenario <SCENARIO>
          Path to a YAML scenario file.
          When provided, loaded first; CLI flags override file values.

      --name <NAME>
          Metric name emitted by this scenario.
          Must match [a-zA-Z_:][a-zA-Z0-9_:]*.
          Required when no --scenario file is provided.

      --rate <RATE>
          Target event rate in events per second.
          Must be strictly positive. Supports fractional values (e.g. 0.5).
          Required when no --scenario file is provided.

      --duration <DURATION>
          Total run duration (e.g. "30s", "5m", "1h", "100ms").
          When absent the scenario runs indefinitely until Ctrl+C.

      --value-mode <VALUE_MODE>
          Value generator mode.
          Accepted values: constant, uniform, sine, sawtooth.
          Default: constant.

      --amplitude <AMPLITUDE>
          Sine wave amplitude (half the peak-to-peak swing).
          Used with --value-mode sine. Default: 1.0.

      --period-secs <PERIOD_SECS>
          Sine wave or sawtooth period in seconds.
          Used with --value-mode sine or sawtooth. Default: 60.0.

      --offset <OFFSET>
          Sine wave midpoint, or the constant value for --value-mode constant.
          Default: 0.0.

      --min <MIN>
          Minimum value for the uniform generator.
          Used with --value-mode uniform. Default: 0.0.

      --max <MAX>
          Maximum value for the uniform generator.
          Used with --value-mode uniform. Default: 1.0.

      --seed <SEED>
          RNG seed for the uniform generator (enables deterministic replay).
          When absent a seed of 0 is used.

      --gap-every <GAP_EVERY>
          Gap recurrence interval (e.g. "2m").
          Together with --gap-for, defines a recurring silent period.
          Both --gap-every and --gap-for must be provided together.

      --gap-for <GAP_FOR>
          Gap duration within each cycle (e.g. "20s").
          Must be strictly less than --gap-every.

      --label <key=value>
          Static label attached to every emitted event (repeatable).
          Format: key=value. Keys must match [a-zA-Z_][a-zA-Z0-9_]*.
          Example: --label hostname=t0-a1 --label zone=eu1

      --encoder <ENCODER>
          Output encoder format.
          Accepted values: prometheus_text. Default: prometheus_text.

  -h, --help
          Print help
```

---

## YAML Scenario Files

All flags can be expressed in a YAML file. CLI flags override any value in the file.

```yaml
name: interface_oper_state
rate: 1000
duration: 30s

generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0

gaps:
  every: 2m
  for: 20s

labels:
  hostname: t0-a1
  zone: eu1

encoder:
  type: prometheus_text
sink:
  type: stdout
```

Run it with:

```bash
sonda metrics --scenario examples/basic-metrics.yaml
```

Override the rate from the CLI:

```bash
sonda metrics --scenario examples/basic-metrics.yaml --rate 500
```

### Generator types

| `type` | Parameters | Description |
|--------|-----------|-------------|
| `constant` | `value: f64` | Emits a fixed value every tick. |
| `uniform` | `min: f64`, `max: f64`, `seed: u64` (optional) | Uniformly distributed random value in `[min, max]`. Seeded for deterministic replay. |
| `sine` | `amplitude: f64`, `period_secs: f64`, `offset: f64` | Sine wave: `offset + amplitude * sin(2π * tick / period_ticks)`. |
| `sawtooth` | `min: f64`, `max: f64`, `period_secs: f64` | Linear ramp from `min` to `max` that resets at the period boundary. |

### Encoder types

The `encoder` field selects the wire format. Use a mapping with a `type` key:

| `type` | Parameters | Description |
|--------|-----------|-------------|
| `prometheus_text` | _(none)_ | Prometheus text exposition format 0.0.4. |
| `influx_lp` | `field_key: string` (optional, default `"value"`) | InfluxDB line protocol. |
| `json_lines` | _(none)_ | JSON Lines (NDJSON), one object per line. |

```yaml
encoder:
  type: influx_lp
  field_key: requests
```

### Sink types

The `sink` field selects the output destination. Use a mapping with a `type` key:

| `type` | Parameters | Description |
|--------|-----------|-------------|
| `stdout` | _(none)_ | Write to standard output (buffered). Default. |
| `file` | `path: string` | Write to a file. Parent directories are created automatically. |
| `tcp` | `address: string` | Write over a persistent TCP connection (e.g. `"127.0.0.1:9999"`). |
| `udp` | `address: string` | Send each event as a UDP datagram (e.g. `"127.0.0.1:9999"`). |
| `http_push` | `url: string`, `content_type: string` (optional), `batch_size: usize` (optional) | POST batches of encoded events to an HTTP endpoint. Retries once on 5xx. |

```yaml
# Write to a file
sink:
  type: file
  path: /tmp/sonda-output.txt

# Send over TCP
sink:
  type: tcp
  address: "127.0.0.1:9999"

# Send over UDP
sink:
  type: udp
  address: "127.0.0.1:9999"

# POST batches to an HTTP endpoint
sink:
  type: http_push
  url: "http://localhost:9090/api/v1/otlp/metrics"
  content_type: "text/plain; version=0.0.4"
  batch_size: 65536
```

### Gap windows

A gap window defines a recurring silent period. No events are emitted during the gap; the scheduler
sleeps to avoid busy-waiting.

```yaml
gaps:
  every: 2m    # one gap every 2 minutes
  for: 20s     # each gap lasts 20 seconds
```

`for` must be strictly less than `every`.

---

## Example Scenarios

Example scenario files are included in the `examples/` directory.

### `examples/basic-metrics.yaml`

A 30-second sine wave at 1000 events/sec with labels and a recurring gap:

```bash
sonda metrics --scenario examples/basic-metrics.yaml
```

### `examples/simple-constant.yaml`

A 10-second constant `up=1` metric at 10 events/sec:

```bash
sonda metrics --scenario examples/simple-constant.yaml
```

### `examples/tcp-sink.yaml`

Sine wave sent over TCP (start a listener first with `nc -l 9999`):

```bash
nc -l 9999 &
sonda metrics --scenario examples/tcp-sink.yaml
```

### `examples/udp-sink.yaml`

Constant metric sent as UDP datagrams in JSON Lines format (listen with `nc -u -l 9998`):

```bash
nc -u -l 9998 &
sonda metrics --scenario examples/udp-sink.yaml
```

### `examples/file-sink.yaml`

Sawtooth wave written to a file in InfluxDB line protocol:

```bash
sonda metrics --scenario examples/file-sink.yaml
cat /tmp/sonda-output.txt
```

### `examples/http-push-sink.yaml`

Sine wave POSTed in batches to an HTTP endpoint (start a local receiver first):

```bash
# Listen with netcat (for testing)
nc -l 9090 &
sonda metrics --scenario examples/http-push-sink.yaml
```

---

## Output Format

Output is [Prometheus text exposition format](https://prometheus.io/docs/instrumenting/exposition_formats/)
(`text/plain 0.0.4`). Each line is one sample:

```
metric_name{label1="val1",label2="val2"} value timestamp_ms
```

- Labels are sorted alphabetically by key.
- Timestamp is milliseconds since Unix epoch.
- Label values are escaped (`\`, `"`, and newlines).
- When there are no labels, the `{}` is omitted.

Example:

```
cpu_usage{hostname="t0-a1",zone="eu1"} 50.523 1742500001000
up 1 1742500001000
```

---

## Piping and Integration

Count lines produced in 5 seconds at 100 events/sec:

```bash
sonda metrics --name up --rate 100 --duration 5s | wc -l
# expect ~500
```

Feed into a pipeline:

```bash
sonda metrics --scenario examples/basic-metrics.yaml | your-ingest-tool
```

---

## Workspace Layout

```
sonda/
├── sonda-core/     library crate: all engine logic (generators, encoder, scheduler, sinks)
├── sonda/          binary crate: CLI (thin wrapper over sonda-core)
├── sonda-server/   binary crate: HTTP API control plane (post-MVP)
├── examples/       example YAML scenario files
└── docs/           architecture doc, phase plans
```

`sonda-core` is the primary product and is designed to be reusable as a library dependency.

---

## Development

```bash
# Build everything
cargo build --workspace

# Run all tests
cargo test --workspace

# Lint
cargo clippy --workspace -- -D warnings

# Format check
cargo fmt --all -- --check

# Run the CLI in development
cargo run -p sonda -- metrics --name up --rate 10 --duration 5s
```

---

## License

See [LICENSE](LICENSE) for details.
