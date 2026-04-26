# Getting Started

Install Sonda and stream your first synthetic metrics and logs to stdout.

## Installation

=== "Install script (Linux/macOS)"

    ```bash
    curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
    ```

    Pin a version with `SONDA_VERSION=v1.0.1` before the pipe.

=== "Cargo"

    ```bash
    cargo install sonda
    ```

=== "Docker"

    ```bash
    docker pull ghcr.io/davidban77/sonda:latest
    docker run --rm ghcr.io/davidban77/sonda:latest \
      metrics --name cpu_usage --rate 2 --duration 5s
    ```

    The default entrypoint runs `sonda-server`, but dispatches to the `sonda` CLI
    when the first argument is a subcommand (`metrics`, `logs`, `run`, `catalog`, ...).

=== "From source"

    ```bash
    git clone https://github.com/davidban77/sonda.git
    cd sonda
    cargo build --release -p sonda
    ```

    Binary lands at `target/release/sonda`.

Check it works: `sonda --version` should print the installed version.

## Your first metric

Generate a constant metric at 2 events per second for 5 seconds:

```bash
sonda metrics --name cpu_usage --rate 2 --duration 5s
```

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

Each stdout line is Prometheus exposition format: `metric_name value timestamp_ms`.
Banners go to stderr; pipe stdout and only data flows through. Long runs show a live
progress line between the banners (see
[CLI Reference -- Live progress](configuration/cli-reference.md#live-progress)).

!!! tip "Suppress banners"
    `sonda -q metrics ...` (or `--quiet`) silences the banners.

The default generator is `constant` at `0.0`. Shape the signal with a sine wave:

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

The wave oscillates between 0 and 100 with a 10-second period. The
[Tutorial -- Generators](guides/tutorial-generators.md) covers all eight generators.

## Using a scenario file

Repeatable runs live in a [v2 YAML file](configuration/v2-scenarios.md):

```yaml title="basic-metrics.yaml"
version: 2

defaults:
  rate: 1000
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    hostname: t0-a1
    zone: eu1

scenarios:
  - id: interface_oper_state
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: sine
      amplitude: 5.0
      period_secs: 30
      offset: 10.0
    gaps:
      every: 2m
      for: 20s
```

Run it:

```bash
sonda run --scenario basic-metrics.yaml --duration 3s
```

```text title="Output"
interface_oper_state{hostname="t0-a1",zone="eu1"} 10 1774277944133
interface_oper_state{hostname="t0-a1",zone="eu1"} 10.00104719754354 1774277944134
interface_oper_state{hostname="t0-a1",zone="eu1"} 10.002094395041146 1774277944135
...
```

## Generating logs

Structured log events work the same way:

```bash
sonda logs --mode template --rate 2 --duration 3s
```

```json title="Output"
{"timestamp":"2026-03-23T14:59:04.840Z","severity":"info","message":"synthetic log event","labels":{},"fields":{}}
{"timestamp":"2026-03-23T14:59:05.345Z","severity":"info","message":"synthetic log event","labels":{},"fields":{}}
{"timestamp":"2026-03-23T14:59:05.845Z","severity":"info","message":"synthetic log event","labels":{},"fields":{}}
...
```

Field pools, severity weights, and multiple templates are in the
[Tutorial -- Generating logs](guides/tutorial-logs.md).

## Sending to a backend

Push directly from the CLI with `--sink` and `--endpoint` -- no YAML required:

```bash
# Push metrics to VictoriaMetrics / Prometheus via remote write
sonda metrics --name cpu_usage --rate 10 --duration 30s \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write

# Push logs to Grafana Loki
sonda logs --mode template --rate 10 --duration 30s \
  --sink loki --endpoint http://localhost:3100 --label app=myservice
```

See [Tutorial -- Sinks](guides/tutorial-sinks.md) for every sink type.

## What next

The **[Tutorial](guides/tutorial.md)** walks through every generator, encoder, sink, and
advanced feature step by step. Skip the YAML grind:

- **`sonda init`** -- interactive wizard, or non-interactive with flags like `--situation`
  and `--from @builtin` ([CLI Reference](configuration/cli-reference.md#sonda-init)).
- **[Built-in Scenarios](guides/scenarios.md)** -- run curated patterns instantly:
  `sonda metrics --scenario @cpu-spike`.
- **[Metric Packs](guides/metric-packs.md)** -- bundles for Telegraf SNMP and node_exporter
  that match real-world schemas.
- **[CSV Import](guides/csv-import.md)** -- turn existing CSV data into a portable scenario.

Reference pages:

- [**v2 Scenario Files**](configuration/v2-scenarios.md) -- file shape, defaults, `after:` chains, v1 migration
- [**Scenario Fields**](configuration/scenario-fields.md) -- per-entry field reference
- [**CLI Reference**](configuration/cli-reference.md) -- every flag for `metrics`, `logs`, `run`
- [**Docker**](deployment/docker.md) -- containers and Compose
- [**Troubleshooting**](guides/troubleshooting.md) -- common issues
