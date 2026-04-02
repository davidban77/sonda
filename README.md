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
| **Generators** | constant, sine, sawtooth, uniform random, sequence, CSV replay |
| **Encoders** | Prometheus text, InfluxDB line protocol, JSON lines, syslog, Prometheus remote write |
| **Sinks** | stdout, file, TCP, UDP, HTTP push, Prometheus remote write, Kafka, Loki |
| **Scheduling** | configurable rate, duration, gap windows, burst windows, cardinality spikes |
| **Signals** | metrics, logs (template and replay modes) |
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

Generate a sine wave metric with labels, 2 samples/sec for 2 seconds:

```bash
sonda metrics \
  --name cpu_usage \
  --rate 2 \
  --duration 2s \
  --value-mode sine \
  --amplitude 50 \
  --period-secs 4 \
  --offset 50 \
  --label host=web-01
```

```text
cpu_usage{host="web-01"} 50 1774872730347
cpu_usage{host="web-01"} 85.35533905932738 1774872730852
cpu_usage{host="web-01"} 100 1774872731351
cpu_usage{host="web-01"} 85.35533905932738 1774872731848
```

The value oscillates as a sine wave between 0 and 100, encoded in Prometheus text format.

## Using a scenario file

Define everything in YAML and run it:

```yaml
# examples/basic-metrics.yaml
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
  precision: 2          # optional: limit metric values to 2 decimal places
sink:
  type: stdout
```

```bash
sonda metrics --scenario examples/basic-metrics.yaml
```

## Documentation

Full documentation is available at **https://davidban77.github.io/sonda/**.

- [**Getting Started**](https://davidban77.github.io/sonda/getting-started/) -- installation, first metric, first log scenario
- [**Configuration Reference**](https://davidban77.github.io/sonda/configuration/scenario-file/) -- scenario files, generators, encoders, sinks, CLI flags
- [**Deployment**](https://davidban77.github.io/sonda/deployment/docker/) -- Docker, Kubernetes, sonda-server HTTP API
- [**Guides**](https://davidban77.github.io/sonda/guides/alert-testing/) -- alert testing, pipeline validation, recording rules, example catalog

## Library usage

Add `sonda-core` as a dependency to use the generation engine programmatically:

```toml
[dependencies]
sonda-core = "0.3"
```

Heavy dependencies are gated behind Cargo features so library consumers only pay
for what they use:

| Feature | Default | Dependencies | What it enables |
|---------|---------|-------------|-----------------|
| `config` | yes | `serde_yaml` | `Deserialize` impls on config types for YAML parsing |
| `http` | no | `ureq` (rustls) | HTTP push and Loki sinks |
| `remote-write` | no | `prost`, `snap`, `ureq` | Prometheus remote write encoder and sink |
| `kafka` | no | `rskafka`, `tokio`, `chrono` | Kafka sink |

Generators, encoders, and the stdout/file/TCP/UDP/memory/channel sinks are always
available with no optional dependencies. Library consumers who build configs in code
can disable the `config` feature to avoid pulling in `serde_yaml`.

See the [sonda-core docs on docs.rs](https://docs.rs/sonda-core) for API details.

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
