# Sonda

[![crates.io](https://img.shields.io/crates/v/sonda.svg)](https://crates.io/crates/sonda)
[![crates.io](https://img.shields.io/crates/v/sonda-core.svg)](https://crates.io/crates/sonda-core)

Sonda is a synthetic telemetry generator written in Rust. It produces realistic observability
signals -- metrics and logs -- for testing pipelines, validating ingest paths, and simulating
failure scenarios. Unlike pure-random noise generators, Sonda models the patterns that actually
break real pipelines: gaps, micro-bursts, cardinality spikes, and shaped value sequences.

## Features at a glance

| Category | Options |
|----------|---------|
| **Generators** | constant, sine, sawtooth, uniform random, sequence, step, spike, CSV replay |
| **Encoders** | Prometheus text, InfluxDB line protocol, JSON lines, syslog, Prometheus remote write, OTLP |
| **Sinks** | stdout, file, TCP, UDP, HTTP push, Prometheus remote write, Kafka, Loki, OTLP/gRPC |
| **Scheduling** | configurable rate, duration, gap windows, burst windows, cardinality spikes, dynamic labels, jitter |
| **Signals** | metrics (gauge, histogram, summary), logs (template and replay modes) |
| **CSV import** | `sonda new --from <csv>` analyzes CSV files, detects time-series patterns (steady, spike, leak, flap, sawtooth, step), generates portable scenario YAML |
| **Interactive scaffolding** | `sonda new` walks signal type → generator → rate → duration → sink and writes commented v2 YAML |
| **Catalogs** | author your own catalog directory of `kind: runnable` scenarios and `kind: composable` packs; discover with `sonda list --catalog <dir>` and run with `@name` references |
| **Deployment** | static binary, Docker, Kubernetes (Helm chart) |

## Quick install

**Install script** (Linux and macOS):

```bash
curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
```

**Cargo:**

```bash
cargo install sonda
```

**Docker:**

```bash
docker run --rm -v "$PWD":/work -w /work \
  ghcr.io/davidban77/sonda:latest run my-scenario.yaml
```

See the [Getting Started](https://davidban77.github.io/sonda/getting-started/) guide for all installation options.

## Your first scenario

Sonda runs YAML scenario files. Scaffold one with `sonda new --template`, save it, and run it:

```bash
sonda new --template -o hello.yaml
sonda run hello.yaml --duration 5s
```

```text
example_metric 1 1775518552355
example_metric 1 1775518553360
example_metric 1 1775518554360
```

Each stdout line is Prometheus exposition format: `metric_name value timestamp_ms`. The
template gives you a one-line constant value at 1 event per second; edit the `generator:`
block to swap in a sine wave, labels, and any of the eight built-in generators:

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

Push to any sink (HTTP, Loki, Kafka, OTLP, Prometheus remote-write) by editing the
`sink:` block, or override at the command line with `--sink`, `--endpoint`, and `--encoder`:

```bash
sonda run cpu-sine.yaml \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

The [Tutorial](https://davidban77.github.io/sonda/guides/tutorial/) walks through every generator,
encoder, sink, and scheduling option step by step.

## Catalogs and the `@name` shorthand

Organize your scenarios into a directory and Sonda discovers them as a catalog. Each file
carries a `kind: runnable` (a scenario you run) or `kind: composable` (a metric pack you
reference from other scenarios with `pack: <name>`).

```bash
sonda --catalog ./my-catalog list                # browse the catalog
sonda --catalog ./my-catalog show @cpu-spike     # view the YAML
sonda --catalog ./my-catalog run @cpu-spike      # run a scenario
```

See the [Catalogs](https://davidban77.github.io/sonda/guides/scenarios/) guide for the
directory layout and authoring conventions.

## Metric packs

Metric packs are reusable bundles of metric names and label schemas that expand into
multi-metric scenarios. Each pack models a real exporter (Telegraf SNMP, node_exporter) so
generated data matches the exact schema your dashboards and alert rules expect.

Author a pack as a `kind: composable` YAML file in your catalog directory and reference it
from any runnable scenario:

```yaml title="snmp-edge.yaml"
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
  - signal_type: metrics
    pack: telegraf_snmp_interface
    labels:
      device: rtr-edge-01
      ifName: GigabitEthernet0/0/0
      ifIndex: "1"
```

```bash
sonda --catalog ./my-catalog run snmp-edge.yaml
```

Override the generator for specific metrics without editing the pack definition:

```yaml
scenarios:
  - signal_type: metrics
    pack: node_exporter_cpu
    labels:
      instance: web-01:9100
    overrides:
      node_cpu_seconds_total:
        generator:
          type: spike
          baseline: 0.1
          spike_value: 0.95
          spike_duration: 5
          spike_interval: 30
```

## CSV import

Turn any CSV file into a parameterized scenario. `sonda new --from <csv>` analyzes
time-series data, detects dominant patterns, and generates portable YAML using generators
instead of raw CSV replay:

```bash
sonda new --from data.csv -o scenario.yaml         # generate a scenario file
sonda new --from data.csv                          # preview the YAML on stdout
```

Works with Grafana "Series joined by time" exports -- metric names and labels are extracted
from headers automatically. Six patterns are detected: steady, spike, climb/leak, sawtooth,
flap, and step.

See the [CSV Import](https://davidban77.github.io/sonda/guides/csv-import/) guide for the
full walkthrough.

## Interactive scaffolding

Don't want to write YAML by hand? `sonda new` walks you through building a scenario with
guided prompts. It uses operational vocabulary -- "spike event", "leak", "flap" -- instead
of raw generator types:

```bash
sonda new -o my-scenario.yaml
```

```text
? Signal type › metrics
? Scenario id › example
? Generator › sine
? Events per second › 1
? Duration (e.g. 60s, 5m) › 60s
? Sink › stdout

wrote my-scenario.yaml
```

The generated YAML is immediately runnable. Pass `-o <path>` to write to a file
(omit it to preview on stdout), `--template` to skip prompts and dump a minimal
file, or `--from <csv>` to scaffold from time-series data. See the
[CLI Reference](https://davidban77.github.io/sonda/configuration/cli-reference/#sonda-new)
for every prompt and flag.

## Multi-signal temporal scenarios

Multi-signal scenarios with temporal causality are expressed directly as
[v2 scenario files](https://davidban77.github.io/sonda/configuration/v2-scenarios/): define
several entries and use `after:` clauses to express when one signal starts relative to another
-- Sonda resolves the timing into concrete `phase_offset` values at compile time. See the
[Network Device Telemetry](https://davidban77.github.io/sonda/guides/network-device-telemetry/)
guide for a worked link-failover cascade.

## Documentation

Full documentation is available at **https://davidban77.github.io/sonda/**.

- [**Getting Started**](https://davidban77.github.io/sonda/getting-started/) -- installation, first scenario, first log scenario
- [**Tutorial**](https://davidban77.github.io/sonda/guides/tutorial/) -- generators, encoders, sinks, gaps, bursts, multi-scenario runs
- [**Configuration Reference**](https://davidban77.github.io/sonda/configuration/scenario-fields/) -- scenario files, generators, encoders, sinks
- [**CLI Reference**](https://davidban77.github.io/sonda/configuration/cli-reference/) -- every flag for `run`, `list`, `show`, `new`
- [**Deployment**](https://davidban77.github.io/sonda/deployment/docker/) -- Docker, Kubernetes, sonda-server HTTP API
- [**Guides**](https://davidban77.github.io/sonda/guides/alert-testing/) -- alert testing, pipeline validation, CSV replay, example catalog

## Library usage

Add `sonda-core` to use the generation engine programmatically:

```toml
[dependencies]
sonda-core = "0.8"
```

Heavy dependencies (HTTP, remote write, Kafka, OTLP) are gated behind Cargo feature flags so you
only pay for what you use. See the [sonda-core docs on docs.rs](https://docs.rs/sonda-core) for
API details and feature flag reference.

## Contributing

```text
sonda/
├── sonda-core/     library crate: generators, encoders, schedulers, sinks
├── sonda/          binary crate: CLI
├── sonda-server/   binary crate: HTTP API control plane
└── examples/       YAML scenario files
```

```bash
cargo build --workspace && cargo test --workspace
```

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, coding
conventions, and the pull request process.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
