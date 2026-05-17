---
title: Sonda — synthetic telemetry generator
description: Synthetic metrics, logs, histograms, and summaries shaped like the real thing — in a single static binary.
hide:
  - navigation
  - toc
---

<div class="sonda-hero" markdown>

<span class="sonda-hero__badge">Synthetic telemetry · v1.9</span>

<h1 class="sonda-hero__title">Telemetry that looks real, on demand.</h1>

<p class="sonda-hero__subtitle">Metrics, logs, histograms, and summaries shaped like the production thing — in a single static binary. Test your pipelines, your alerts, and your dashboards before production tests them for you.</p>

<div class="sonda-hero__ctas" markdown>
[Get started in 5 minutes](getting-started.md){ .md-button .md-button--primary }
[Browse the guides](guides/index.md){ .md-button }
[See it on GitHub](https://github.com/davidban77/sonda){ .md-button }
</div>

<div class="sonda-hero__install">curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh</div>

</div>

<p style="text-align:center; margin: -1rem 0 2.5rem;">
<a href="https://crates.io/crates/sonda"><img alt="crates.io" src="https://img.shields.io/crates/v/sonda.svg?logo=rust&style=for-the-badge&color=1e3a8a"></a>
&nbsp;
<a href="https://github.com/davidban77/sonda/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/davidban77/sonda/ci.yml?branch=main&style=for-the-badge&color=1e40af"></a>
&nbsp;
<a href="https://github.com/davidban77/sonda/blob/main/Cargo.toml"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.75-3b82f6?style=for-the-badge"></a>
&nbsp;
<a href="https://github.com/davidban77/sonda/blob/main/LICENSE-MIT"><img alt="License" src="https://img.shields.io/crates/l/sonda.svg?style=for-the-badge&color=84cc16"></a>
</p>

## A taste

Two commands. No YAML to hand-author yet — `sonda new --template` scaffolds a runnable starter file, and `sonda run` plays it back.

```bash
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

Same file runs from your laptop, from CI, or [posted to `sonda-server`](deployment/sonda-server.md) over HTTP. For a guided walkthrough — including pushing to a real Prometheus, Loki, or OTLP backend — see [Getting Started](getting-started.md).

## Why Sonda

<div class="grid cards" markdown>

-   :material-flash: __Fast and self-contained__

    A single 5 MB static musl binary, no runtime, no JVM, no Python. Boots in under
    50 ms; runs anywhere a Linux container can. Same binary for your laptop, your
    CI runner, and your Kubernetes Job.

-   :material-shape-outline: __Signals shaped like real ones__

    Metrics with sine/sawtooth/step/spike shapes; logs with bursty distributions;
    histograms with realistic latency tails. Match the schema, the cardinality, and
    the failure modes — not just the volume.

-   :material-connection: __Speaks your pipeline's protocols__

    Prometheus text, remote-write, InfluxDB line protocol, JSON, syslog, OTLP/gRPC,
    Kafka, Loki, raw TCP/UDP — pick the encoder and sink, point at the backend,
    done. No custom shim per stack.

-   :material-source-branch: __YAML-first, code-second__

    Scenarios are YAML files you can check into git, diff, review, and template.
    Override any field from the CLI or `SONDA_*` env vars when you want to without
    forking the file.

</div>

## Where to next

<div class="grid cards" markdown>

-   :material-rocket-launch: __[Get started in 5 minutes](getting-started.md)__

    Install Sonda, stream your first metric, and push to a real backend.

-   :material-bookshelf: __[Author your own scenarios](guides/scenarios.md)__

    Organize a catalog directory of runnable scenarios and composable packs;
    discover them with `sonda list --catalog <dir>` and run with
    `sonda run @name`.

-   :material-file-document-outline: __[v2 scenario files](configuration/v2-scenarios.md)__

    The canonical file shape: `version: 2`, `kind: runnable`, shared `defaults:`,
    inline packs, `after:` temporal chains, and env-var interpolation.

-   :material-database-import: __[CSV import](guides/csv-import.md)__

    Turn Grafana exports into portable, parameterized scenarios — one
    `sonda new --from <csv>` away.

-   :material-bell-alert: __[Test your alert rules](guides/alert-testing.md)__

    Trigger, resolve, and validate alert rules with the right metric shape —
    thresholds, correlation, cardinality, histograms, recording rules.

-   :material-server-network: __[Run as a server](deployment/sonda-server.md)__

    Run `sonda-server` as a long-lived HTTP control plane and submit scenarios
    over the REST API. Great for CI and synthetic-monitoring fleets.

</div>

## Part of the Modern Network Observability ecosystem

<div class="grid cards" markdown>

-   :material-book-open-variant: __[Modern Network Observability](https://network-observability.github.io/)__

    The hub: book, workshops, lab, and the broader project Sonda plugs into.

-   :material-school: __[The AutoCon5 workshop](https://network-observability.github.io/workshops/)__

    One workday on the new on-call rotation — telemetry, dashboards, alerts,
    AI-assisted ops, in four hours.

</div>
