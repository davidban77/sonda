---
title: Your first scenario
description: The four parts of a Sonda scenario — scenario file, generator, encoder, sink — with examples.
---

# Your first scenario

## How Sonda works

A Sonda **scenario** is a YAML file that describes the telemetry you want to generate. For example: a CPU metric that oscillates between 40% and 80%, a router emitting interface counters, or an application emitting JSON logs at 100 messages per second. Sonda reads the file and sends realistic data to the destinations you choose: stdout, a file, Prometheus remote-write, Loki, Kafka, or OTLP.

You point your dashboards, alert rules, and ingestion pipelines at this synthetic data and test them without production traffic.

A scenario has four parts:

- A **scenario file** — the YAML document `sonda run` reads.
- One or more **generators** — each one produces a value pattern.
- An **encoder** — converts values into the wire format (Prometheus text, JSON Lines, OTLP, and so on).
- A **sink** — the destination that receives the encoded data.

The next sections cover each part, starting with the minimal example.

## Scenario file

A **scenario file** is the YAML document `sonda run` reads. The file declares its format with `version: 2` and marks itself as runnable with `kind: runnable`. Shared values like rate, duration, encoder, and sink go under `defaults:`. One or more entries go under `scenarios:`, and each entry emits a single signal — one metric series or one log stream.

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

This file emits a metric named `demo_cpu` with value `42`, once per second, for thirty seconds. The output uses the Prometheus text exposition format and prints to stdout.

For the full file reference — `defaults:`, multi-entry layouts, environment variable interpolation, and `after:` chains — see [Scenario file format](../build/scenario-files.md).

## Generator

A **generator** produces the value for each tick of a scenario. Sonda includes eight numeric generators: `constant`, `sine`, `sawtooth`, `uniform`, `sequence`, `step`, `spike`, and `csv_replay`. Each one produces a different pattern. For example, `sine` produces cyclical signals, `step` produces level changes, and `spike` produces transient anomalies.

For logs, the `template` generator builds messages from templates and field pools. Choose the generator that matches the pattern you want to model.

!!! info "Shortcut generator names"
    Sonda also recognises six shortcut names for common combinations: `steady`, `flap`, `saturation`, `leak`, `degradation`, and `spike_event`. Each one is equivalent to one of the eight generators above with preset defaults. See [Generators — shortcuts](../build/generators.md#operational-aliases) for the mapping.

The example below produces a sine wave between 0 and 100, with a period of 10 seconds:

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

For the full generator catalog with parameter tables and value-pattern diagrams, see [Generators](../build/generators.md).

## Encoder

An **encoder** converts the value the generator produced into bytes on the wire. You can send the same `cpu_usage` metric as Prometheus text, JSON Lines, InfluxDB line protocol, OTLP protobuf, or syslog text. Only the encoder changes. JSON Lines means one JSON object per line. The default encoder for metrics is `prometheus_text`. The default for logs is `json_lines`.

The example below replaces the encoder with JSON Lines:

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

A **sink** delivers the encoded bytes to a destination. `stdout` is the default. `file` writes to disk. The `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`, `tcp`, and `udp` sinks send data to real backends. Each sink has its own parameters:

- **Batching** groups many events into one request. Tune it when the event rate is high enough that one HTTP call per event becomes expensive.
- **TLS and SASL** secure the connection. Configure them when the sink is reachable over the internet or requires broker authentication.

The example below replaces stdout with HTTP POST to a VictoriaMetrics import endpoint:

```yaml title="cpu-push.yaml (sink block)"
defaults:
  sink:
    type: http_push
    url: "http://localhost:8428/api/v1/import/prometheus"
    content_type: "text/plain"
```

`sonda run` accepts `--sink`, `--endpoint`, and `--encoder` flags. You can use them to send the same YAML to a different destination without editing the file. For the full sink catalog including retry, batching, TLS, and SASL, see [Sinks](../build/sinks.md).

## Putting it together

The four parts are independent. Combine them and you have everything Sonda does. The same `hello.yaml` you started with uses all four:

| Concept | `hello.yaml` field | What to change |
|---------|--------------------|----------------|
| Scenario file | `version`, `kind`, `defaults`, `scenarios` | Add entries, share defaults |
| Generator | `generator.type` | Value pattern over time |
| Encoder | `encoder.type` | Wire format |
| Sink | `sink.type`, `sink.url` | Destination |

You now know enough to read the rest of the docs. Next: [send your scenario to a real backend](send-to-a-backend.md) — Prometheus remote-write, Loki, or OTLP — without leaving your laptop.
