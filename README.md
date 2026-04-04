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

Emit a constant value -- the simplest signal for health-check or baseline testing:

```bash
sonda metrics --name up --rate 1 --duration 5s --value 1
```

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

## Sending to a backend

Push metrics via Prometheus remote write -- no scenario file needed:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Send to an OpenTelemetry Collector via OTLP/gRPC:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317 --signal-type metrics
```

Send logs to Grafana Loki:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --sink loki --endpoint http://localhost:3100 --label app=myservice
```

All complex sinks (`http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`) are available via
`--sink` and their companion flags. See the
[CLI Reference](https://davidban77.github.io/sonda/configuration/cli-reference/) for the full list.

## Simulating a fleet with dynamic labels

Dynamic labels rotate through a fixed set of values on every tick, simulating
a fleet of N distinct sources. Unlike cardinality spikes, they are always on --
no time window required.

```yaml
# Simulate 10 hosts emitting the same metric
name: cpu_usage
rate: 100
duration: 60s
generator:
  type: sine
  amplitude: 50
  period_secs: 30
  offset: 50
dynamic_labels:
  - key: hostname
    prefix: "host-"
    cardinality: 10
labels:
  env: production
encoder:
  type: prometheus_text
sink:
  type: stdout
```

You can also cycle through an explicit list of values:

```yaml
dynamic_labels:
  - key: region
    values: [us-east-1, us-west-2, eu-west-1]
```

## CLI global flags

| Flag | Short | Description |
|------|-------|-------------|
| `--quiet` | `-q` | Suppress all status banners. Errors still go to stderr. |
| `--verbose` | `-v` | Show the resolved config at startup, then run normally. Mutually exclusive with `--quiet`. |
| `--dry-run` | | Parse and validate the config, print it, and exit without emitting events. |

Validate a scenario file without emitting any events:

```bash
sonda --dry-run metrics --scenario examples/basic-metrics.yaml
```

```text
[config] Resolved scenario config:

  name:       interface_oper_state
  signal:     metrics
  rate:       1000/s
  duration:   30s
  generator:  sine (amplitude: 5, period: 30s, offset: 10)
  encoder:    prometheus_text (precision: 2)
  sink:       stdout
  labels:     hostname=t0-a1, zone=eu1
  gaps:       every 2m, for 20s

Validation: OK
```

Show the resolved config at startup, then run the scenario normally:

```bash
sonda --verbose metrics --name cpu_usage --rate 2 --duration 2s --label host=web-01
```

Combine `--dry-run` with CLI-only flags to verify what would run:

```bash
sonda --dry-run metrics --name my_metric --rate 100 --duration 1m --value-mode sine \
  --amplitude 50 --period-secs 60 --offset 50 --label env=staging
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
| `config` | yes | `serde_yaml_ng` | `Deserialize` impls on config types for YAML parsing |
| `http` | no | `ureq` (rustls) | HTTP push and Loki sinks |
| `remote-write` | no | `prost`, `snap`, `ureq` | Prometheus remote write encoder and sink |
| `kafka` | no | `rskafka`, `tokio`, `chrono` | Kafka sink |
| `otlp` | no | `tonic`, `prost`, `tokio` | OTLP protobuf encoder and gRPC sink |

Generators, encoders, and the stdout/file/TCP/UDP/memory/channel sinks are always
available with no optional dependencies. Library consumers who build configs in code
can disable the `config` feature to avoid pulling in `serde_yaml_ng`.

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
