<p align="center">
  <img src=".github/assets/sonda-banner.svg" alt="Sonda — synthetic telemetry generator" width="640">
</p>

<p align="center">
  <a href="https://crates.io/crates/sonda"><img alt="crates.io" src="https://badgen.net/crates/v/sonda?icon=rust"></a>
  <a href="https://crates.io/crates/sonda-core"><img alt="sonda-core on crates.io" src="https://badgen.net/crates/v/sonda-core?label=sonda-core"></a>
  <a href="https://github.com/davidban77/sonda/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/davidban77/sonda/ci.yml?branch=main&color=1e40af"></a>
  <a href="https://github.com/davidban77/sonda/blob/main/Cargo.toml"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.75-3b82f6"></a>
  <a href="https://github.com/davidban77/sonda/blob/main/LICENSE-MIT"><img alt="License" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-f97316"></a>
</p>

# Sonda

Sonda is a synthetic telemetry generator written in Rust. It produces metrics and logs shaped like the real signals that break observability pipelines — gaps, micro-bursts, cardinality spikes, value sequences — for testing ingest paths, alert rules, and dashboards.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
```

Or with Cargo: `cargo install sonda`.

## Quick start

```yaml title="hello.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 3s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: cpu_usage
    signal_type: metrics
    name: cpu_usage
    generator: { type: sine, amplitude: 50.0, offset: 50.0, period_secs: 10 }
    labels: { host: web-01 }
```

```bash
sonda run hello.yaml
```

```text
cpu_usage{host="web-01"} 50 1779724001981
cpu_usage{host="web-01"} 79.38926261462366 1779724001982
cpu_usage{host="web-01"} 97.55282581475768 1779724002984
```

## Documentation

Full documentation — concepts, configuration, deployment, and guides — lives at **<https://davidban77.github.io/sonda/>**.

## Library usage

The generation engine ships as a separate crate: [`sonda-core`](https://crates.io/crates/sonda-core). Heavy dependencies (HTTP, Kafka, OTLP, remote-write) are gated behind Cargo feature flags.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions and the pull request process, or open an issue on [GitHub](https://github.com/davidban77/sonda/issues).

## License

Licensed under either of the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or the MIT license ([LICENSE-MIT](LICENSE-MIT)) at your option.
