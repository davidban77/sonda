# Getting Started

This guide walks you through installing Sonda and generating your first metrics and logs.
By the end, you will have synthetic telemetry streaming to stdout.

## Installation

=== "Install script (Linux/macOS)"

    Download the latest pre-built binary for your platform:

    ```bash
    curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
    ```

    To pin a specific version:

    ```bash
    curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | SONDA_VERSION=v0.1.3 sh
    ```

=== "Cargo"

    If you have the Rust toolchain installed:

    ```bash
    cargo install sonda
    ```

=== "Docker"

    Pull the image from GitHub Container Registry:

    ```bash
    docker pull ghcr.io/davidban77/sonda:latest
    ```

    Run Sonda inside the container (the default entrypoint is `sonda-server`, so
    override it with `--entrypoint`):

    ```bash
    docker run --rm --entrypoint /sonda ghcr.io/davidban77/sonda:latest \
      metrics --name cpu_usage --rate 2 --duration 5s
    ```

=== "From source"

    Clone and build:

    ```bash
    git clone https://github.com/davidban77/sonda.git
    cd sonda
    cargo build --release -p sonda
    ```

    The binary is at `target/release/sonda`.

Verify the installation:

```bash
sonda --version
```

```text title="Output"
sonda 0.1.3
```

## Your first metric

Generate a constant metric at 2 events per second for 5 seconds:

```bash
sonda metrics --name cpu_usage --rate 2 --duration 5s
```

You will see a colored start banner on stderr, followed by data on stdout, then a stop banner:

```text title="stderr"
▶ cpu_usage  signal_type: metrics | rate: 2/s | encoder: prometheus_text | sink: stdout | duration: 5s
```

```text title="stdout"
cpu_usage 0 1774277933018
cpu_usage 0 1774277933522
cpu_usage 0 1774277934023
cpu_usage 0 1774277934523
...
```

```text title="stderr"
■ cpu_usage  completed in 5.0s | events: 10 | bytes: 240 B | errors: 0
```

Each line on stdout is Prometheus exposition format: `metric_name value timestamp_ms`.

!!! tip
    Status banners go to stderr, data goes to stdout. When you redirect or pipe stdout,
    only data flows through. Use `--quiet` / `-q` to suppress banners entirely:
    `sonda -q metrics --name cpu_usage --rate 2 --duration 5s`

The default generator is `constant` with a value of `0.0`. To make it more interesting, use a
sine wave:

```bash
sonda metrics --name cpu_usage --rate 2 --duration 5s \
  --value-mode sine --amplitude 50 --period-secs 10 --offset 50 \
  --label host=web-01
```

```text title="Output"
cpu_usage{host="web-01"} 50 1774277938576
cpu_usage{host="web-01"} 65.45084971874736 1774277939081
cpu_usage{host="web-01"} 79.38926261462366 1774277939580
cpu_usage{host="web-01"} 90.45084971874738 1774277940081
...
```

The sine wave oscillates between 0 and 100 (offset 50 +/- amplitude 50), completing one full
cycle every 10 seconds.

## Using a scenario file

For repeatable configurations, define a scenario in YAML. Here is `examples/basic-metrics.yaml`
from the repository:

```yaml title="basic-metrics.yaml"
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

Run it:

```bash
sonda metrics --scenario examples/basic-metrics.yaml --duration 3s
```

```text title="Output"
interface_oper_state{hostname="t0-a1",zone="eu1"} 10 1774277944133
interface_oper_state{hostname="t0-a1",zone="eu1"} 10.00104719754354 1774277944134
interface_oper_state{hostname="t0-a1",zone="eu1"} 10.002094395041146 1774277944135
...
```

You can override any field from the command line. CLI flags take precedence over the YAML file:

```bash
sonda metrics --scenario examples/basic-metrics.yaml --duration 5s --rate 2
```

## Generating logs

Sonda also generates structured log events. The simplest command:

```bash
sonda logs --mode template --rate 2 --duration 3s
```

```json title="Output"
{"timestamp":"2026-03-23T14:59:04.840Z","severity":"info","message":"synthetic log event","fields":{}}
{"timestamp":"2026-03-23T14:59:05.345Z","severity":"info","message":"synthetic log event","fields":{}}
{"timestamp":"2026-03-23T14:59:05.845Z","severity":"info","message":"synthetic log event","fields":{}}
...
```

For richer log output, use a scenario file with templates and field pools. The repository
includes `examples/log-template.yaml` with multiple message templates:

```yaml title="examples/log-template.yaml"
name: app_logs_template
rate: 10
duration: 60s

generator:
  type: template
  templates:
    - message: "Request from {ip} to {endpoint} returned {status}"
      field_pools:
        ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3", "192.168.1.10"]
        endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs", "/api/v1/users"]
        status: ["200", "201", "400", "404", "500"]
    - message: "Service {service} processed {count} events in {duration_ms}ms"
      field_pools:
        service: ["ingest", "transform", "export"]
        count: ["1", "10", "100", "1000"]
        duration_ms: ["5", "12", "47", "200"]
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
sonda logs --scenario examples/log-template.yaml --duration 3s
```

```json title="Output"
{"timestamp":"2026-03-23T14:59:08.405Z","severity":"info","message":"Request from 10.0.0.3 to /api/v1/users returned 404","fields":{"endpoint":"/api/v1/users","ip":"10.0.0.3","status":"404"}}
{"timestamp":"2026-03-23T14:59:08.510Z","severity":"error","message":"Service transform processed 100 events in 47ms","fields":{"count":"100","duration_ms":"47","service":"transform"}}
{"timestamp":"2026-03-23T14:59:08.610Z","severity":"error","message":"Request from 10.0.0.3 to /api/v1/metrics returned 500","fields":{"endpoint":"/api/v1/metrics","ip":"10.0.0.3","status":"500"}}
...
```

## What next

- [**Scenario Files**](configuration/scenario-file.md) -- full YAML reference for all scenario
  fields, gaps, bursts, and multi-scenario mode.
- [**Generators**](configuration/generators.md) -- all value generators (constant, sine, sawtooth,
  uniform, sequence, CSV replay) and log generators.
- [**Encoders**](configuration/encoders.md) -- output formats: Prometheus text, InfluxDB, JSON,
  syslog, remote write.
- [**Sinks**](configuration/sinks.md) -- output destinations: stdout, file, TCP, UDP, HTTP push,
  remote write, Kafka, Loki.
- [**CLI Reference**](configuration/cli-reference.md) -- every flag for `metrics`, `logs`, and
  `run` subcommands.
