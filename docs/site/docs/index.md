---
title: Sonda — synthetic telemetry generator
description: Synthetic metrics, logs, histograms, and summaries shaped like real telemetry — in a single static binary.
hide:
  - navigation
  - toc
---

<div class="sonda-hero" markdown>

<span class="sonda-hero__badge">Synthetic telemetry · v1.9</span>

<h1 class="sonda-hero__title">Telemetry that looks real, on demand.</h1>

<p class="sonda-hero__subtitle">Metrics, logs, histograms, and summaries shaped like real telemetry — in a single static binary. Test your pipelines, your alerts, and your dashboards before they break in production.</p>

<div class="sonda-hero__ctas" markdown>
[Get started in 5 minutes](get-started/quickstart.md){ .md-button .md-button--primary }
[See what you can test](test/index.md){ .md-button }
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
<a href="https://github.com/davidban77/sonda/blob/main/LICENSE-MIT"><img alt="License" src="https://img.shields.io/crates/l/sonda.svg?style=for-the-badge&color=f97316"></a>
</p>

## Try it

Two commands. No YAML to write yourself yet — `sonda new --template` generates a runnable starter file, and `sonda run` runs it.

```bash
sonda new --template -o hello.yaml
sonda run hello.yaml --duration 3s
```

```text title="stdout (Prometheus exposition)"
example_metric 1 1777243958972
example_metric 1 1777243959978
example_metric 1 1777243960981
```

Each line is Prometheus exposition format (Prometheus's plain-text format for emitting metrics — see the [glossary](reference/glossary.md#prometheus-exposition-format)): `metric_name value timestamp_ms`.

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

Sonda ships as both a CLI (`sonda`) and an optional long-running HTTP server (`sonda-server`). The CLI is enough for laptop and CI use; the server lets you POST scenarios over a REST API. The same file runs from your laptop, from CI, or [posted to `sonda-server`](deploy/server.md) over HTTP. For a guided walkthrough — including pushing to a real Prometheus or Loki backend — see [Getting Started](get-started/quickstart.md).

## Why Sonda

<div class="grid cards" markdown>

-   :material-flash: __Fast and self-contained__

    A single 5 MB static musl binary, no runtime, no JVM, no Python. Boots in under
    50 ms; runs anywhere a Linux container can. Same binary for your laptop, your
    CI runner, and your Kubernetes Job.

-   :material-shape-outline: __Signals shaped like real ones__

    Metrics with sine/sawtooth/step/spike shapes; logs with bursty distributions;
    histograms (distributions across buckets) with realistic latency tails. Match
    the schema, the cardinality (the number of unique label-value combinations —
    see the [glossary](reference/glossary.md#cardinality)), and the failure modes —
    not just the volume.

-   :material-connection: __Speaks your pipeline's protocols__

    Encoders translate to formats like Prometheus text, InfluxDB line protocol,
    JSON, syslog, OTLP, and Prometheus [remote-write](reference/glossary.md#remote_write).
    Sinks deliver to destinations like stdout, files, TCP/UDP, HTTP, Kafka, Loki,
    and OTLP collectors. Pick an [encoder](reference/glossary.md#encoder) and a
    [sink](reference/glossary.md#sink) — no custom shim per stack.

-   :material-source-branch: __YAML-first, code-second__

    Scenarios are YAML files you can check into git, diff, review, and template.
    Override any field from the CLI or `SONDA_*` env vars when you want to without
    forking the file.

</div>

## Where to next

<div class="grid cards" markdown>

-   :material-rocket-launch: __[Get started in 5 minutes](get-started/quickstart.md)__

    Install Sonda, stream your first metric, and push to a real backend.

-   :material-bookshelf: __[Author your own scenarios](build/catalogs-and-packs.md)__

    Organize a catalog directory of runnable scenarios and composable packs;
    discover them with `sonda list --catalog <dir>` and run with
    `sonda run @name`.

-   :material-bell-alert: __[Test your alert rules](test/alert-testing.md)__

    Trigger, resolve, and validate alert rules with the right metric shape —
    thresholds, correlation, cardinality, histograms, recording rules.

-   :material-server-network: __[Run as a server](deploy/server.md)__

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
