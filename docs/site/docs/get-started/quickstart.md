---
title: Getting started with Sonda
description: Install Sonda, stream your first synthetic metric, and send it to a real backend in under five minutes.
hide:
  - toc
---

<div class="sonda-section-hero" markdown>

<span class="sonda-section-hero__eyebrow">Quickstart · ~5 minutes</span>

<h1 class="sonda-section-hero__title">Get started with Sonda</h1>

<p class="sonda-section-hero__subtitle">Install Sonda, stream your first synthetic metric to stdout, then point the sink at a real Prometheus, Loki, or OTLP backend. No YAML to hand-write — <code>sonda new</code> scaffolds it.</p>

</div>

## Installation

=== "Install script (Linux/macOS)"

    ```bash
    curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
    ```

    Pin a version with `SONDA_VERSION=v1.9.0` before the pipe.

=== "Cargo"

    ```bash
    cargo install sonda
    ```

=== "Docker"

    ```bash
    docker pull ghcr.io/davidban77/sonda:latest
    docker run --rm \
      -v "$PWD":/work -w /work \
      ghcr.io/davidban77/sonda:latest \
      run my-scenario.yaml
    ```

    The default entrypoint runs `sonda-server`, but dispatches to the `sonda` CLI
    when the first argument is a known subcommand (`run`, `list`, `show`, `new`).

=== "From source"

    ```bash
    git clone https://github.com/davidban77/sonda.git
    cd sonda
    cargo build --release -p sonda
    ```

    Binary lands at `target/release/sonda`.

Check it works: `sonda --version` should print the installed version.

## Your first metric

Sonda runs YAML scenario files. A scenario file is the unit `sonda run` consumes — see the [glossary](../reference/glossary.md#scenario). Scaffold one with `sonda new --template`, save it, and run it:

```bash
sonda new --template -o hello.yaml
```

```yaml title="hello.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: example
    signal_type: metrics
    name: example_metric
    generator:
      type: constant
      value: 1.0
```

```bash
sonda run hello.yaml --duration 5s
```

```text title="stderr"
▶ example_metric  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 5s
```

```text title="stdout"
example_metric 1 1774277933018
example_metric 1 1774277934023
example_metric 1 1774277935023
example_metric 1 1774277936023
example_metric 1 1774277937023
■ example_metric  completed in 5.0s | events: 5 | bytes: 130 B | errors: 0
```

Each stdout line is Prometheus exposition format (Prometheus's plain-text metric format — see the [glossary](../reference/glossary.md#prometheus-exposition-format)): `metric_name value timestamp_ms`.
Banners go to stderr; pipe stdout and only data flows through. Long runs show a live
progress line between the banners (see
[CLI Reference -- Live progress](../reference/cli-flags.md#live-progress)).

!!! tip "Suppress banners"
    `sonda -q run hello.yaml` (or `--quiet`) silences the banners.

Shape the signal by swapping the `generator:` block (the value-pattern producer — see the [glossary](../reference/glossary.md#generator)) for a sine wave with labels (key-value tags attached to each event — see the [glossary](../reference/glossary.md#label)):

```yaml title="cpu-sine.yaml"
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 5s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    host: web-01
scenarios:
  - id: cpu_usage
    signal_type: metrics
    name: cpu_usage
    generator:
      type: sine
      amplitude: 50.0
      offset: 50.0
      period_secs: 10
```

```bash
sonda run cpu-sine.yaml
```

```text title="Output"
cpu_usage{host="web-01"} 50 1774277938576
cpu_usage{host="web-01"} 65.45084971874736 1774277939081
cpu_usage{host="web-01"} 79.38926261462366 1774277939580
cpu_usage{host="web-01"} 90.45084971874738 1774277940081
...
```

The wave oscillates between 0 and 100 with a 10-second period. The
[Tutorial -- Generators](../build/generators.md) covers all eight generators.

## A larger scenario

The same shape lets you share defaults across many entries and add scheduling like
gaps and bursts:

```yaml title="basic-metrics.yaml"
version: 2
kind: runnable

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

```bash
sonda run basic-metrics.yaml --duration 3s
```

```text title="Output"
interface_oper_state{hostname="t0-a1",zone="eu1"} 10 1774277944133
interface_oper_state{hostname="t0-a1",zone="eu1"} 10.00104719754354 1774277944134
interface_oper_state{hostname="t0-a1",zone="eu1"} 10.002094395041146 1774277944135
...
```

## Generating logs

Structured log events live on a `signal_type: logs` entry with a `log_generator:` block:

```yaml title="hello-logs.yaml"
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 3s
  encoder:
    type: json_lines
  sink:
    type: stdout
scenarios:
  - id: app_logs
    signal_type: logs
    name: app_logs
    log_generator:
      type: template
      templates:
        - message: "synthetic log event"
```

```bash
sonda run hello-logs.yaml
```

```json title="Output"
{"timestamp":"2026-03-23T14:59:04.840Z","severity":"info","message":"synthetic log event","labels":{},"fields":{}}
{"timestamp":"2026-03-23T14:59:05.345Z","severity":"info","message":"synthetic log event","labels":{},"fields":{}}
{"timestamp":"2026-03-23T14:59:05.845Z","severity":"info","message":"synthetic log event","labels":{},"fields":{}}
...
```

Field pools, severity weights, and multiple templates are in the
[Tutorial -- Generating logs](../build/generators.md).

## Sending to a backend

Edit the `sink:` block (the destination component — see the [glossary](../reference/glossary.md#sink)) in the YAML, or override at the CLI with `--sink`, `--endpoint`, and `--encoder` (the format converter that writes Prometheus text, JSON, OTLP, etc. — see the [glossary](../reference/glossary.md#encoder)), to push data anywhere. The example below uses Prometheus's `remote_write` protocol (its metric-push wire format, see the [glossary](../reference/glossary.md#remote_write)):

```yaml title="cpu-remote-write.yaml"
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
```

```bash
# Send the same scenario to a different remote-write endpoint without editing the file
sonda run cpu-remote-write.yaml \
  --endpoint http://victoriametrics:8428/api/v1/write
```

See [Tutorial -- Sinks](../build/sinks.md) for every sink type.

## Catalogs and `@name`

When you organize scenarios into a directory (a **catalog** — see the [glossary](../reference/glossary.md#catalog)), point `--catalog <dir>` at it and refer to entries with `@name`:

```bash title="./my-catalog"
sonda list --catalog ./my-catalog
sonda show @cpu-spike --catalog ./my-catalog
sonda run @cpu-spike --catalog ./my-catalog
```

The catalog is just a directory of scenario YAML files with `kind: runnable` (scenarios you can run) or `kind: composable` (**packs** — reusable bundles of metric specs that other scenarios reference with `pack: <name>`; see the [glossary](../reference/glossary.md#pack)). See [Author your own catalog](../build/catalogs-and-packs.md) for the layout.

## What next

**[Your first scenario](your-first-scenario.md)** walks through every generator, encoder, sink, and advanced feature step by step. Skip the YAML grind:

- **`sonda new`** -- interactive scaffolder for a starter scenario; non-interactive with
  `--template` or `--from <csv>` ([CLI Reference](../reference/cli-flags.md#sonda-new)).
- **[Author your own catalog](../build/catalogs-and-packs.md)** -- organize scenarios and composable
  packs so you can reference them with `@name`.
- **[Metric Packs](../build/catalogs-and-packs.md)** -- composable bundles for Telegraf SNMP and
  node_exporter that match real-world schemas.
- **[CSV Import](../import/from-csv.md)** -- turn existing CSV data into a portable scenario
  with `sonda new --from <csv>`.

Reference pages:

- [**Scenario Files**](../build/scenario-files.md) -- file shape, defaults, `after:` chains
- [**Scenario Fields**](../reference/scenario-fields.md) -- per-entry field reference
- [**CLI Reference**](../reference/cli-flags.md) -- every flag for `run`, `list`, `show`, `new`
- [**Docker**](../deploy/docker.md) -- containers and Compose
- [**Troubleshooting**](../reference/troubleshooting.md) -- common issues
