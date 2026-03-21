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
- **Burst windows** — recurring high-rate periods that simulate micro-bursts and traffic spikes.
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
  logs     Generate synthetic log events and write them to the configured sink
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

      --burst-every <BURST_EVERY>
          Burst recurrence interval (e.g. "10s").
          Together with --burst-for and --burst-multiplier, defines a recurring
          high-rate period. All three --burst-* flags must be provided together.

      --burst-for <BURST_FOR>
          Burst duration within each cycle (e.g. "2s").
          Must be strictly less than --burst-every.

      --burst-multiplier <BURST_MULTIPLIER>
          Rate multiplier applied during each burst window (e.g. "5.0").
          Effective rate during burst = base rate × multiplier.
          Must be strictly positive.

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

### `sonda logs`

```
Usage: sonda logs [OPTIONS]

Options:
      --scenario <SCENARIO>
          Path to a YAML log scenario file.
          When provided, loaded first; CLI flags override file values.

      --mode <MODE>
          Log generator mode.
          Accepted values: template, replay.
          Required when no --scenario file is provided.

      --file <FILE>
          Path to a log file for use with --mode replay.
          Lines are replayed in order, cycling back to the start when exhausted.

      --rate <RATE>
          Target event rate in events per second.
          Must be strictly positive. Defaults to 10.0 when no scenario file is provided.

      --duration <DURATION>
          Total run duration (e.g. "30s", "5m", "1h", "100ms").
          When absent the scenario runs indefinitely until Ctrl+C.

      --encoder <ENCODER>
          Output encoder format.
          Accepted values: json_lines, syslog. Default: json_lines.

      --output <OUTPUT>
          Write output to a file at this path instead of stdout.
          Shorthand for sink: file in a YAML scenario.

      --label <key=value>
          Static label attached to every emitted event (repeatable).
          Format: key=value.
          Example: --label hostname=t0-a1 --label zone=eu1

      --message <MESSAGE>
          A single static message template for use with --mode template.
          Overrides any templates in the scenario file.

      --severity-weights <WEIGHTS>
          Comma-separated severity=weight pairs (e.g. "info=0.7,warn=0.2,error=0.1").
          Used with --mode template.

      --seed <SEED>
          RNG seed for deterministic template resolution.
          When absent a seed of 0 is used.

      --replay-file <REPLAY_FILE>
          Alias for --file. Path to the log file for --mode replay.

      --gap-every <GAP_EVERY>
          Gap recurrence interval (e.g. "2m").
          Together with --gap-for, defines a recurring silent period.

      --gap-for <GAP_FOR>
          Gap duration within each cycle (e.g. "20s").
          Must be strictly less than --gap-every.

      --burst-every <BURST_EVERY>
          Burst recurrence interval (e.g. "5s").
          Together with --burst-for and --burst-multiplier, defines a recurring high-rate period.

      --burst-for <BURST_FOR>
          Burst duration within each cycle (e.g. "1s").
          Must be strictly less than --burst-every.

      --burst-multiplier <BURST_MULTIPLIER>
          Rate multiplier during burst periods (e.g. 10.0 for 10x the base rate).

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

bursts:
  every: 10s
  for: 2s
  multiplier: 5.0

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

### Metric generator types

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
| `syslog` | `hostname: string` (optional), `app_name: string` (optional) | RFC 5424 syslog format. Log events only — not supported for metrics. |

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
| `kafka` | `brokers: string`, `topic: string` | Publish batches of encoded events to a Kafka topic (requires `kafka` feature). `brokers` is a comma-separated list of `host:port` addresses. |

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

# Publish batches to a Kafka topic (requires the `kafka` feature)
sink:
  type: kafka
  brokers: "127.0.0.1:9092"
  topic: sonda-metrics
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

### Burst windows

A burst window defines a recurring high-rate period. During a burst the effective event rate is
`rate × multiplier`, which increases the emission frequency for the burst duration. Bursts are useful
for simulating traffic spikes, micro-burst patterns, and ingest pipeline stress.

```yaml
bursts:
  every: 10s      # one burst every 10 seconds
  for: 2s         # each burst lasts 2 seconds
  multiplier: 5.0 # 5x the base rate during the burst
```

`for` must be strictly less than `every`. `multiplier` must be strictly positive.

When a gap and a burst would overlap, the gap takes priority and no events are emitted.

---

## Log Scenario Files

Log scenarios use a different config structure from metric scenarios. Run with `sonda logs --scenario <file.yaml>`.

```yaml
name: app_logs_template
rate: 10
duration: 60s

generator:
  type: template
  templates:
    - message: "Request from {ip} to {endpoint} returned {status}"
      field_pools:
        ip:
          - "10.0.0.1"
          - "10.0.0.2"
          - "10.0.0.3"
        endpoint:
          - "/api/v1/health"
          - "/api/v1/metrics"
        status:
          - "200"
          - "404"
          - "500"
  severity_weights:
    info: 0.7
    warn: 0.2
    error: 0.1
  seed: 42

gaps:
  every: 2m
  for: 20s

bursts:
  every: 5s
  for: 1s
  multiplier: 10.0

encoder:
  type: json_lines
sink:
  type: stdout
```

Run it with:

```bash
sonda logs --scenario examples/log-template.yaml
```

### Log generator types

| `type` | Parameters | Description |
|--------|-----------|-------------|
| `template` | `templates: list`, `severity_weights: map` (optional), `seed: u64` (optional) | Generates structured log events from message templates with field pools. Placeholders like `{ip}` are resolved from the matching pool entry using a deterministic hash of the seed and tick. |
| `replay` | `file: string` | Replays lines from a file at the configured rate, cycling back to the start when exhausted. Each line becomes a log event with severity `info`. |

### `LogScenarioConfig` YAML schema

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | `string` | required | Scenario name (used for identification). |
| `rate` | `f64` | required | Target event rate in events per second. Must be strictly positive. |
| `duration` | `string` | none (indefinite) | Total run duration (e.g. `"30s"`, `"5m"`). |
| `generator` | `object` | required | Log generator configuration. See log generator types above. |
| `gaps` | `object` | none | Optional gap window: `every` and `for` duration strings. |
| `bursts` | `object` | none | Optional burst window: `every`, `for`, and `multiplier`. |
| `encoder` | `object` | `{type: json_lines}` | Output encoder. Accepted values: `json_lines`, `syslog`. |
| `sink` | `object` | `{type: stdout}` | Output sink. Any sink type supported by metric scenarios. |

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

### `examples/kafka-sink.yaml`

Constant metric published in batches to a local Kafka broker (requires `kafka` feature):

```bash
# Start a local Kafka broker first (e.g. via Docker)
sonda metrics --scenario examples/kafka-sink.yaml
```

### `examples/influx-file.yaml`

Sawtooth ramp in InfluxDB line protocol written to `/tmp/sonda-influx-output.txt`:

```bash
sonda metrics --scenario examples/influx-file.yaml
cat /tmp/sonda-influx-output.txt
```

Output looks like:

```
disk_io_bytes,device=sda,host=storage-01 bytes=0.0 1742500000000000000
disk_io_bytes,device=sda,host=storage-01 bytes=20000.0 1742500000020000000
...
```

### `examples/burst-metrics.yaml`

A sine wave at 100 events/sec that bursts to 500 events/sec for 2 seconds out of every 10 seconds:

```bash
sonda metrics --scenario examples/burst-metrics.yaml
```

Count lines during a burst second to see the rate spike:

```bash
sonda metrics --scenario examples/burst-metrics.yaml | pv -l > /dev/null
```

### `examples/json-tcp.yaml`

HTTP request duration sine wave streamed as JSON Lines over TCP (start a listener first):

```bash
nc -l 9999 &
sonda metrics --scenario examples/json-tcp.yaml
```

Output looks like:

```json
{"name":"http_request_duration_ms","value":150.0,"labels":{"method":"GET","service":"api-gateway","status":"200"},"timestamp":"2026-03-20T12:00:00.000Z"}
```

### `examples/prometheus-http-push.yaml`

Prometheus text exposition format POSTed in batches to an HTTP endpoint. Compatible with
VictoriaMetrics, vmagent, and any endpoint that accepts the Prometheus text format over HTTP:

```bash
# Quick test with netcat
nc -l 9090 &
sonda metrics --scenario examples/prometheus-http-push.yaml

# Against VictoriaMetrics
# Edit the url in the YAML to: http://localhost:8428/api/v1/import/prometheus
sonda metrics --scenario examples/prometheus-http-push.yaml
```

### `examples/log-template.yaml`

Template-based log generation at 10 events/sec for 60 seconds. Emits JSON Lines to stdout with
varied messages, field values, and severity levels (70% info, 20% warn, 10% error):

```bash
sonda logs --scenario examples/log-template.yaml
```

Output looks like:

```json
{"timestamp":"2026-03-21T12:00:00.000Z","severity":"info","message":"Request from 10.0.0.2 to /api/v1/health returned 200","fields":{"endpoint":"/api/v1/health","ip":"10.0.0.2","status":"200"}}
{"timestamp":"2026-03-21T12:00:00.100Z","severity":"warn","message":"Service ingest processed 100 events in 47ms","fields":{"count":"100","duration_ms":"47","service":"ingest"}}
```

### `examples/log-replay.yaml`

Replay lines from an existing log file at 5 events/sec for 30 seconds. Lines cycle when the file
is exhausted. Update the `file:` path in the YAML to point to a real log file:

```bash
sonda logs --scenario examples/log-replay.yaml
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

## End-to-End Integration Tests

The `tests/e2e/` directory contains a docker-compose based test suite that validates sonda against
real observability backends and message brokers.

### Prerequisites

- [Docker](https://docs.docker.com/get-docker/) with the Compose v2 plugin (`docker compose`)
- [Task](https://taskfile.dev/) (optional — for convenient task runner commands)
- `curl` and `python3` in PATH
- Rust toolchain (for `cargo build`)

### Services

| Service | Port | Purpose |
|---------|------|---------|
| `victoriametrics` | 8428 | VictoriaMetrics single-node (push target and query endpoint) |
| `prometheus` | 9090 | Prometheus with remote write receiver enabled |
| `vmagent` | 8429 | vmagent that relays incoming pushes to VictoriaMetrics |
| `kafka` | 9094 | Kafka broker (KRaft mode, no Zookeeper) |
| `kafka-ui` | 8080 | Kafka UI for browsing topics and messages |
| `grafana` | 3000 | Grafana with VictoriaMetrics and Prometheus datasources pre-configured |

### Test scenarios

**VictoriaMetrics scenarios** (verified by querying `/api/v1/series`):

| Scenario file | Encoder | Sink target | Metric verified |
|---------------|---------|-------------|-----------------|
| `vm-prometheus-text.yaml` | `prometheus_text` | VictoriaMetrics `/api/v1/import/prometheus` | `sonda_e2e_vm_prom_text` |
| `vm-influx-lp.yaml` | `influx_lp` | VictoriaMetrics `/write` | `sonda_e2e_vm_influx_lp_value` |

**Kafka scenarios** (verified by consuming from topic):

| Scenario file | Encoder | Kafka topic | Metric verified |
|---------------|---------|-------------|-----------------|
| `kafka-prometheus-text.yaml` | `prometheus_text` | `sonda-e2e-metrics` | messages consumed > 0 |
| `kafka-json-lines.yaml` | `json_lines` | `sonda-e2e-json` | messages consumed > 0 |

### Using the Taskfile

The project includes a `Taskfile.yml` for common operations:

```bash
task stack:up       # Start the full stack (VM, Prometheus, Kafka, Grafana, Kafka UI)
task stack:down     # Stop and remove everything
task stack:status   # Show service status
task stack:logs     # Tail all service logs

task e2e            # Run automated e2e tests (starts/stops stack)
task demo           # Start stack + send a 30s sine wave demo to VM

task run:vm-prom    # Send Prometheus text metrics to VictoriaMetrics
task run:vm-influx  # Send InfluxDB LP metrics to VictoriaMetrics
task run:kafka      # Send metrics to Kafka

task check          # Full quality gate (build + test + lint)
```

### Exploring metrics visually

Start the stack and send some data:

```bash
task stack:up
task demo
```

Then open the dashboards:

- **Grafana** — http://localhost:3000 (anonymous access, VictoriaMetrics datasource pre-configured). Go to Explore, select VictoriaMetrics, and query `demo_sine_wave`.
- **Kafka UI** — http://localhost:8080. Browse topics `sonda-e2e-metrics` and `sonda-e2e-json` to see messages.
- **VictoriaMetrics** — http://localhost:8428/vmui for the built-in query UI.

### Running the automated tests

```bash
# Via Taskfile
task e2e

# Or directly
./tests/e2e/run.sh
```

The script starts the docker-compose stack, waits for all services to become healthy, builds sonda
in release mode, runs each scenario, verifies data arrived (VictoriaMetrics via series API, Kafka
via consumer), and tears everything down. Exits `0` if all pass, `1` if any fail.

### Running scenarios manually

```bash
# Start the stack
task stack:up

# Run individual scenarios
sonda metrics --scenario tests/e2e/scenarios/vm-prometheus-text.yaml
sonda metrics --scenario tests/e2e/scenarios/kafka-prometheus-text.yaml

# Verify VictoriaMetrics
curl "http://localhost:8428/api/v1/series?match[]={__name__=%22sonda_e2e_vm_prom_text%22}"

# Verify Kafka (consume from topic)
docker exec sonda-e2e-kafka kafka-console-consumer.sh \
    --bootstrap-server 127.0.0.1:9092 \
    --topic sonda-e2e-metrics \
    --from-beginning --timeout-ms 5000

# Tear down
task stack:down
```

---

## License

See [LICENSE](LICENSE) for details.
