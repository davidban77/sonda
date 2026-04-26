# Sonda

*Synthetic telemetry generator for the people who run the pipeline -- metrics, logs, histograms, and summaries shaped like the real thing, in a single static binary.*

[![crates.io](https://img.shields.io/crates/v/sonda.svg?logo=rust)](https://crates.io/crates/sonda)
[![MSRV](https://img.shields.io/badge/MSRV-1.75-blue.svg)](https://github.com/davidban77/sonda/blob/main/Cargo.toml)
[![CI](https://github.com/davidban77/sonda/actions/workflows/ci.yml/badge.svg)](https://github.com/davidban77/sonda/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/sonda.svg)](https://github.com/davidban77/sonda/blob/main/LICENSE-MIT)

!!! tip "New in 1.2.0 -- env-var interpolation in v2 scenarios"
    Reference `${VAR}` and `${VAR:-default}` directly in scenario YAML. One file
    runs from your laptop on the defaults and from a containerized `sonda-server`
    on the overrides -- no `sed`, no per-environment fork. See
    [Environment variable interpolation](configuration/v2-scenarios.md#environment-variable-interpolation).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
```

Other install paths (Cargo, Docker, source) live in
[Getting Started](getting-started.md#installation).

## A taste

```bash
sonda metrics --name cpu_usage --rate 2 --duration 2s \
  --value-mode sine --amplitude 50 --offset 50 --period-secs 4 \
  --label host=web-01
```

```text title="stdout (Prometheus exposition)"
cpu_usage{host="web-01"} 50 1777243958972
cpu_usage{host="web-01"} 85.35533905932738 1777243959525
cpu_usage{host="web-01"} 100 1777243959982
cpu_usage{host="web-01"} 85.35533905932738 1777243960481
cpu_usage{host="web-01"} 50.00000000000001 1777243960974
```

One command, shaped values, labeled output -- now wire it once in a v2 scenario file
and replay it from CI, your laptop, or `sonda-server`.

## Where to next

<div class="grid cards" markdown>

-   :material-rocket-launch: __[Get started in 5 minutes](getting-started.md)__

    Install Sonda, stream your first metric, and push to a real backend.

-   :material-bookshelf: __[Built-in scenarios](guides/scenarios.md)__

    Run curated patterns instantly -- `sonda metrics --scenario @cpu-spike`.
    Browse the catalog, pin one, customize from there.

-   :material-file-document-outline: __[v2 scenario files](configuration/v2-scenarios.md)__

    The canonical file shape: `version: 2`, shared `defaults:`, inline packs,
    `after:` temporal chains, and env-var interpolation.

-   :material-database-import: __[CSV import](guides/csv-import.md)__

    Turn Grafana exports into portable, parameterized scenarios -- one
    `sonda import` away.

</div>
