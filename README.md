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
| **Built-in scenarios** | 11 curated patterns (cpu-spike, memory-leak, interface-flap, log-storm, and more) |
| **Metric packs** | 3 reusable metric bundles (telegraf-snmp-interface, node-exporter-cpu, node-exporter-memory) |
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
docker run --rm --entrypoint /sonda ghcr.io/davidban77/sonda:latest metrics --name up --rate 5 --duration 10s
```

See the [Getting Started](https://davidban77.github.io/sonda/getting-started/) guide for all installation options.

## Your first metric

Emit a constant value -- the simplest signal for health-check or baseline testing:

```bash
sonda metrics --name up --rate 1 --duration 5s --value 1
```

```text
up 1 1775518552355
up 1 1775518553360
up 1 1775518554360
```

Shape the signal with a sine wave, labels, and any of the eight built-in generators:

```bash
sonda metrics --name cpu_usage --rate 2 --duration 5s \
  --value-mode sine --amplitude 50 --period-secs 10 --offset 50 \
  --label host=web-01
```

Push directly to a backend -- no scenario file needed:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Define complex scenarios in YAML for repeatable runs with `sonda metrics --scenario config.yaml`.
The [Tutorial](https://davidban77.github.io/sonda/guides/tutorial/) walks through every generator,
encoder, sink, and scheduling option step by step.

## Built-in scenarios

Sonda ships with 11 pre-built patterns you can run instantly -- no YAML needed:

```bash
sonda scenarios list                       # browse the catalog
sonda scenarios run cpu-spike              # run a pattern directly
sonda scenarios show memory-leak           # view the YAML to customize it
sonda metrics --scenario @cpu-spike        # @name shorthand in any subcommand
```

See the [Built-in Scenarios](https://davidban77.github.io/sonda/guides/scenarios/) guide for the
full catalog and customization workflow.

## Built-in metric packs

Metric packs are reusable bundles of metric names and label schemas that expand into multi-metric
scenarios. Each pack models a real exporter (Telegraf SNMP, node_exporter) so generated data
matches the exact schema your dashboards and alert rules expect:

```bash
sonda packs list                                     # browse the catalog
sonda packs show telegraf_snmp_interface              # view the raw YAML definition
sonda packs run telegraf_snmp_interface --rate 1 \
  --duration 60s --label device=rtr-edge-01           # run a pack directly
```

Override the generator for specific metrics without editing the pack definition:

```yaml
# scenario.yaml
pack: node_exporter_cpu
rate: 1
duration: 60s
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

## Documentation

Full documentation is available at **https://davidban77.github.io/sonda/**.

- [**Getting Started**](https://davidban77.github.io/sonda/getting-started/) -- installation, first metric, first log scenario
- [**Tutorial**](https://davidban77.github.io/sonda/guides/tutorial/) -- generators, encoders, sinks, gaps, bursts, multi-scenario runs
- [**Configuration Reference**](https://davidban77.github.io/sonda/configuration/scenario-file/) -- scenario files, generators, encoders, sinks
- [**CLI Reference**](https://davidban77.github.io/sonda/configuration/cli-reference/) -- every flag for `metrics`, `logs`, `histogram`, `summary`, and `run`
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
