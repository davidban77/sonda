# Sonda

*Synthetic telemetry generator for the people who run the pipeline -- metrics, logs, histograms, and summaries shaped like the real thing, in a single static binary.*

[![crates.io](https://img.shields.io/crates/v/sonda.svg?logo=rust)](https://crates.io/crates/sonda)
[![MSRV](https://img.shields.io/badge/MSRV-1.75-blue.svg)](https://github.com/davidban77/sonda/blob/main/Cargo.toml)
[![CI](https://github.com/davidban77/sonda/actions/workflows/ci.yml/badge.svg)](https://github.com/davidban77/sonda/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/sonda.svg)](https://github.com/davidban77/sonda/blob/main/LICENSE-MIT)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
```

Other install paths (Cargo, Docker, source) live in
[Getting Started](getting-started.md#installation).

## A taste

Two commands. No YAML to hand-author yet — `sonda new --template` scaffolds a runnable starter file, and `sonda run` plays it back:

```bash title="hello.yaml"
sonda new --template -o hello.yaml
sonda run hello.yaml --duration 3s
```

```text title="stdout (Prometheus exposition)"
example_metric 1 1777243958972
example_metric 1 1777243959978
example_metric 1 1777243960981
```

Edit `hello.yaml` to shape the signal — swap `constant` for `sine`, add labels, point the sink at a real backend:

```yaml title="hello.yaml (edited)"
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
      period_secs: 4
```

```text title="Output"
cpu_usage{host="web-01"} 50 1777243958972
cpu_usage{host="web-01"} 85.35533905932738 1777243959525
cpu_usage{host="web-01"} 100 1777243959982
cpu_usage{host="web-01"} 85.35533905932738 1777243960481
```

Same file runs from your laptop, from CI, or [posted to `sonda-server` over HTTP](deployment/sonda-server.md). For a guided walkthrough — including pushing to a real Prometheus, Loki, or OTLP backend — see [Getting Started](getting-started.md).

## Where to next

<div class="grid cards" markdown>

-   :material-rocket-launch: __[Get started in 5 minutes](getting-started.md)__

    Install Sonda, stream your first metric, and push to a real backend.

-   :material-bookshelf: __[Author your own scenarios](guides/scenarios.md)__

    Organize a catalog directory of runnable scenarios and composable packs;
    discover them with `sonda list --catalog <dir>` and run with `sonda run @name`.

-   :material-file-document-outline: __[v2 scenario files](configuration/v2-scenarios.md)__

    The canonical file shape: `version: 2`, `kind: runnable`, shared `defaults:`,
    inline packs, `after:` temporal chains, and env-var interpolation.

-   :material-database-import: __[CSV import](guides/csv-import.md)__

    Turn Grafana exports into portable, parameterized scenarios -- one
    `sonda new --from <csv>` away.

</div>
