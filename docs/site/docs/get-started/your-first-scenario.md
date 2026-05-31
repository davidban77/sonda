---
title: Your first scenario
description: The four moving parts of a Sonda scenario — scenario file, generator, encoder, sink — with examples.
---

# Your first scenario

A Sonda scenario is a YAML file describing what to emit, how to shape it, what format to write it in, and where to send it. Four moving parts; one file. This page walks each one in order, with the smallest working example for each.

You've already seen the four parts in passing if you ran through the [Quickstart](quickstart.md). Now you'll learn the names for them and what each one lets you change.

## Scenario file

A **scenario file** is the YAML unit `sonda run` consumes. It declares its format with `version: 2`, marks itself as runnable with `kind: runnable`, sets shared defaults (rate, duration, encoder, sink) under `defaults:`, and lists one or more entries under `scenarios:`. Each entry emits exactly one signal — one metric series, one log stream — and shares the `defaults:` unless it overrides them.

```yaml title="hello.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cpu
    signal_type: metrics
    name: demo_cpu
    generator:
      type: constant
      value: 42
```

That file emits a metric named `demo_cpu`, value `42`, once per second, for thirty seconds, in Prometheus exposition format (Prometheus's plain-text metric format — see the [glossary](../reference/glossary.md#prometheus-exposition-format)), printed to stdout.

For the full file reference — `defaults:`, multi-entry layouts, environment variable interpolation, `after:` chains — see [Scenario file format](../build/scenario-files.md).

## Generator

A **generator** produces the value for each tick of a scenario. Sonda ships eight core metric generators (`constant`, `sine`, `sawtooth`, `uniform`, `sequence`, `step`, `spike`, `csv_replay`) and a handful of operational aliases (`steady`, `flap`, `saturation`, `leak`, `degradation`, `spike_event`) that desugar into the core eight. For logs, the `template` generator builds messages from templates and field pools. Pick the generator that matches the shape you're modelling.

A sine wave between 0 and 100, period of 10 seconds:

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
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: sine
      amplitude: 50.0
      offset: 50.0
      period_secs: 10
```

```text title="Output"
cpu_usage 50 1774277938576
cpu_usage 65.45084971874736 1774277939081
cpu_usage 79.38926261462366 1774277939580
cpu_usage 90.45084971874738 1774277940081
```

For the full generator catalog with parameter tables and shape diagrams, see [Generators](../build/generators.md).

## Encoder

An **encoder** turns the value the generator produced into bytes on the wire. The same `cpu_usage` metric can land in your backend as Prometheus text, JSON Lines, InfluxDB line protocol, OTLP protobuf, or syslog — only the encoder changes. Default for metrics is `prometheus_text`; default for logs is `json_lines`.

Swap the encoder to JSON Lines:

```yaml title="cpu-json.yaml (encoder block)"
defaults:
  encoder:
    type: json_lines
```

```json title="Output"
{"name":"cpu_usage","value":50.0,"labels":{},"timestamp":"2026-03-23T15:28:32.321Z"}
{"name":"cpu_usage","value":65.45,"labels":{},"timestamp":"2026-03-23T15:28:32.821Z"}
```

For the full encoder catalog including precision rules, feature flags, and `Content-Type` headers, see [Encoders](../build/encoders.md).

## Sink

A **sink** delivers the encoded bytes to a destination. `stdout` is the default; `file` writes to disk; `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`, `tcp`, and `udp` ship to real backends. Each sink has its own parameter shape — batching thresholds, URLs, headers, TLS, SASL.

Swap stdout for HTTP POST to a VictoriaMetrics import endpoint:

```yaml title="cpu-push.yaml (sink block)"
defaults:
  sink:
    type: http_push
    url: "http://localhost:8428/api/v1/import/prometheus"
    content_type: "text/plain"
```

`sonda run` accepts `--sink`, `--endpoint`, and `--encoder` flags so you can repoint the same YAML at a different destination without editing the file. For the full sink catalog including retry, batching, TLS and SASL, see [Sinks](../build/sinks.md).

## Putting it together

Each concept stands on its own; combine them and you have everything Sonda does. The same `hello.yaml` you started with mixes all four:

| Concept | `hello.yaml` field | What you change to alter behavior |
|---------|--------------------|------------------------------------|
| Scenario file | `version`, `kind`, `defaults`, `scenarios` | Add entries, share defaults |
| Generator | `generator.type` | Shape of the value over time |
| Encoder | `encoder.type` | Wire format |
| Sink | `sink.type`, `sink.url` | Destination |

Now you know enough to read the rest of the docs. Next up: [send your scenario to a real backend](send-to-a-backend.md) — Prometheus remote-write, Loki, or OTLP — without leaving your laptop.
