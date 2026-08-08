---
title: Sonda — synthetic telemetry generator
description: Sonda generates synthetic metrics and logs with realistic patterns from a single static binary.
hide:
  - navigation
  - toc
---

<div class="sonda-hero" markdown>

<span class="sonda-hero__badge">Synthetic telemetry · open source</span>

<h1 class="sonda-hero__title">Synthetic telemetry generator for testing observability pipelines.</h1>

<p class="sonda-hero__subtitle">Sonda generates metrics and logs with realistic patterns from a single static binary. Use it to test your pipelines, alerts, and dashboards before they break in production.</p>

<div class="sonda-hero__ctas" markdown>
[Get started in 5 minutes](get-started/quickstart.md){ .md-button .md-button--primary }
[Try it in your browser](playground/index.md){ .md-button }
[See it on GitHub](https://github.com/davidban77/sonda){ .md-button }
</div>

<div class="sonda-hero__install" markdown>

```bash
curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh
```

</div>

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

A Sonda **scenario** is a YAML file with three parts: a **generator** that produces values, an **encoder** that formats them, and a **sink** that sends them to a destination. Two commands set one up: `sonda new --template` creates a starter file, and `sonda run` runs it.

```bash
sonda new --template -o hello.yaml
sonda run hello.yaml --duration 3s
```

<div class="sonda-term" data-sonda-term aria-label="Terminal session showing sonda generating metrics">
<div class="sonda-term__bar" aria-hidden="true"><i></i><i></i><i></i><span>sonda — bash</span></div>
<pre class="sonda-term__screen"><code><span class="sonda-term__line" data-t="cmd">sonda new --template -o hello.yaml</span>
<span class="sonda-term__line" data-t="out">wrote hello.yaml</span>
<span class="sonda-term__line" data-t="cmd">sonda run hello.yaml --duration 3s</span>
<span class="sonda-term__line" data-t="out">example_metric 1 1777243958972</span>
<span class="sonda-term__line" data-t="out">example_metric 1 1777243959978</span>
<span class="sonda-term__line" data-t="out">example_metric 1 1777243960981</span>
<span class="sonda-term__line" data-t="out">■ example_metric  completed in 3.0s | events 3 | bytes 90 | errors 0</span></code></pre>
</div>

Each line uses the Prometheus exposition format: `metric_name value timestamp_ms`. See the [glossary](reference/glossary.md#prometheus-exposition-format) for the full definition.

Edit `hello.yaml` to change the signal pattern. You can swap `constant` for `sine`, add labels, or point the sink at a real backend.

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

Sonda includes a CLI (`sonda`) and an optional HTTP server (`sonda-server`). The CLI is enough for laptop and CI use. The server accepts scenarios over a REST API. The same scenario file runs from your laptop, from CI, or from [`sonda-server`](deploy/server.md). For a guided walkthrough that pushes to Prometheus or Loki, see [Getting Started](get-started/quickstart.md).

## No install required

The real Sonda engine runs in your browser, compiled to WebAssembly. Edit a scenario and watch the signal it produces — then run the same YAML unchanged with `sonda run`.

<div class="grid cards" markdown>

-   :material-chart-bell-curve: __[Scenario playground](playground/index.md)__

    Edit scenario YAML and see the exact values Sonda will emit — every
    generator, operational alias, and encoder, with real compile errors.

-   :material-bell-ring: __[Alert lab](playground/alert-lab.md)__

    Set a threshold and a `for:` duration against a failure pattern and watch
    the alert go pending → firing → resolved, in sync with the signal.

</div>

## What do you want to test?

Every one of these is a small YAML file. Pick the failure, not the framework.

=== "Alert rules"

    Trigger, resolve, and debounce alerts with signals shaped like real failures.

    ```yaml
    generator:
      type: spike_event
      baseline: 120.0
      spike_height: 400.0
      spike_duration: 5s
      spike_interval: 30s
    ```

    [Alert testing →](test/alert-testing.md)

=== "Cardinality"

    Inject label churn on a schedule and watch what your TSDB does under it.

    ```yaml
    cardinality_spikes:
      - label: pod
        every: 60s
        for: 10s
        cardinality: 500
        strategy: random
    ```

    [Capacity planning →](test/capacity-planning.md)

=== "Incident replay"

    Export a Grafana panel from a real outage and replay it at the original cadence.

    ```yaml
    generator:
      type: csv_replay
      file: incident-2026-05-18.csv
      timescale: 4.0   # replay 4x faster
    ```

    [Grafana exports →](import/grafana-exports.md)

=== "Fleet dashboards"

    Fill a demo dashboard with a believable fleet instead of one flat line.

    ```yaml
    generator: { type: steady, center: 45.0, noise: 3.0 }
    dynamic_labels:
      - key: host
        strategy: counter
        prefix: web-
        cardinality: 12
    ```

    [Send to a real backend →](get-started/send-to-a-backend.md)

## Why not a bash loop?

Honest answer: a bash loop piping to `curl` is fine for checking that the port
is open. It stops being fine the moment the *shape* of the data matters —
alert thresholds, rate() windows, cardinality limits, retention behavior.

| | bash + curl | Avalanche | flog | Sonda |
|---|---|---|---|---|
| Realistic value shapes (`sine`, `flap`, `leak`) | hand-rolled | — | — | ✓ |
| Correlated failures (`after:`, `while:`) | — | — | — | ✓ |
| Replay real incidents from CSV | — | — | — | ✓ |
| Metrics *and* logs, histograms, summaries | scripted | metrics only | logs only | ✓ |
| Remote-write, OTLP, Kafka, Loki, InfluxDB LP | curl-able only | remote-write | files/stdout | ✓ |
| Scheduled gaps, bursts, cardinality spikes | — | churn only | — | ✓ |
| Single static binary | n/a | ✓ | ✓ | ✓ |

Avalanche is excellent at raw cardinality volume and flog at log throughput —
if that's the whole job, use them. Sonda is for when the pipeline has to be
tested against telemetry that *behaves* like production.

## Why Sonda

<div class="grid cards" markdown>

-   :material-flash: __Fast and self-contained__

    A single 5 MB static binary. No runtime, no JVM, no Python. Starts in under
    50 ms and runs anywhere a Linux container runs. The same binary works on your
    laptop, your CI runner, and as a Kubernetes Job.

-   :material-shape-outline: __Realistic signal patterns__

    Metrics with sine, sawtooth, step, and spike value patterns. Logs with bursty
    distributions. Histograms with realistic latency tails. Match real production
    data on schema, [cardinality](reference/glossary.md#cardinality), and failure
    modes. Not only on volume.

-   :material-connection: __Supports your pipeline's protocols__

    Encoders write formats like Prometheus text, InfluxDB line protocol, JSON,
    syslog, OTLP, and Prometheus [remote-write](reference/glossary.md#remote_write).
    Sinks send data to stdout, files, TCP/UDP, HTTP, Kafka, Loki, and OTLP
    collectors. Pick an [encoder](reference/glossary.md#encoder) and a
    [sink](reference/glossary.md#sink). No extra setup needed per stack.

-   :material-source-branch: __Scenarios as YAML files__

    Scenarios are YAML files. Check them into git, review them in pull requests,
    and template them. Override any field with CLI flags or `SONDA_*` env vars.
    No need to fork the file.

</div>

## Where to next

<div class="grid cards" markdown>

-   :material-rocket-launch: __[Get started in 5 minutes](get-started/quickstart.md)__

    Install Sonda, stream your first metric, and send it to a real backend.

-   :material-bookshelf: __[Write your own scenarios](build/catalogs-and-packs.md)__

    Organize runnable scenarios and composable packs in a catalog directory.
    Discover them with `sonda list --catalog <dir>` and run them with
    `sonda run @name`.

-   :material-bell-alert: __[Test your alert rules](test/alert-testing.md)__

    Trigger, resolve, and validate alert rules with realistic metric patterns.
    Covers thresholds, correlation, cardinality, histograms, and recording rules.

-   :material-server-network: __[Run as a server](deploy/server.md)__

    Run `sonda-server` as a long-lived HTTP control plane and submit scenarios
    over the REST API. Useful for CI and synthetic-monitoring fleets.

</div>

## Part of the Modern Network Observability ecosystem

<div class="grid cards" markdown>

-   :material-book-open-variant: __[Modern Network Observability](https://network-observability.github.io/)__

    The hub: book, workshops, lab, and the broader project Sonda integrates with.

-   :material-school: __[The AutoCon5 workshop](https://network-observability.github.io/workshops/)__

    One workday on the new on-call rotation. Covers telemetry, dashboards,
    alerts, and AI-assisted ops in four hours.

</div>
