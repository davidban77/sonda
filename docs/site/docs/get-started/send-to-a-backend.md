---
title: Send to a real backend
description: Point Sonda at Prometheus remote_write, Loki, or an OTLP collector instead of stdout.
---

# Send to a real backend

Stdout is useful for "does this YAML work?". For "does my alert rule fire when latency crosses 200 ms?" you need data in a real backend. This page walks the three backends most readers want first — Prometheus (`remote_write`), Loki, and an OTLP collector — with a 30-second local Docker setup and a `curl` verification for each.

The shape is the same every time: swap the `sink:` (and sometimes the `encoder:`) in your YAML, point it at the backend, and run.

!!! tip "Networking gotcha"
    The `url:` field is resolved inside the process that runs the scenario. If you POST a YAML to a containerized `sonda-server`, `http://localhost:8428` resolves to the container's loopback, not your host's. See [Networking](../deploy/server.md#networking) for the full table.

=== "Prometheus remote_write"

    [VictoriaMetrics](../reference/glossary.md#victoriametrics) is the easiest Prometheus-compatible backend to spin up — single container, no config file, accepts `remote_write` (Prometheus's protocol for pushing samples to a [TSDB](../reference/glossary.md#tsdb) over HTTP with protobuf + Snappy compression — see the [glossary](../reference/glossary.md#remote_write)).

    Start it:

    ```bash
    docker run -d --name vm -p 8428:8428 \
      victoriametrics/victoria-metrics:latest
    ```

    Update your scenario's encoder and sink:

    ```yaml title="cpu-remote-write.yaml"
    version: 2
    kind: runnable
    defaults:
      rate: 10
      duration: 30s
      encoder:
        type: remote_write
      sink:
        type: remote_write
        url: "http://localhost:8428/api/v1/write"
    scenarios:
      - id: cpu
        signal_type: metrics
        name: cpu_usage
        labels:
          host: web-01
        generator:
          type: sine
          amplitude: 50.0
          offset: 50.0
          period_secs: 60
    ```

    Run, then query:

    ```bash
    sonda run cpu-remote-write.yaml

    # Wait ~5s for ingestion, then verify
    curl -s "http://localhost:8428/api/v1/query?query=cpu_usage" | jq '.data.result'
    ```

    See [Sinks — `remote_write`](../build/sinks.md#remote_write) for the full backend matrix (Prometheus, vmagent, Thanos, Cortex, Mimir) and [Encoders — `remote_write`](../build/encoders.md#remote_write) for the feature-flag note.

=== "Loki"

    [Loki](../reference/glossary.md#loki) is Grafana's log aggregation backend — stores logs indexed by labels rather than full text. Start it:

    ```bash
    docker run -d --name loki -p 3100:3100 \
      grafana/loki:latest
    ```

    Update your scenario to a `logs` signal with the JSON Lines encoder and Loki sink:

    ```yaml title="app-logs-loki.yaml"
    version: 2
    kind: runnable
    defaults:
      rate: 5
      duration: 30s
      encoder:
        type: json_lines
      sink:
        type: loki
        url: "http://localhost:3100"
        batch_size: 10
    scenarios:
      - id: app_logs
        signal_type: logs
        name: app_logs
        labels:
          job: sonda
          env: dev
        log_generator:
          type: template
          templates:
            - message: "Request handled in 47ms"
    ```

    Run, then query:

    ```bash
    sonda run app-logs-loki.yaml

    # Wait ~5s, then verify the stream exists
    curl -s "http://localhost:3100/loki/api/v1/labels" | jq
    curl -s -G "http://localhost:3100/loki/api/v1/query_range" \
      --data-urlencode 'query={job="sonda"}' | jq '.data.result | length'
    ```

    See [Sinks — `loki`](../build/sinks.md#loki) for the stream model, per-flush cardinality cap, and dynamic-label rotations.

=== "OTLP"

    [OTLP](../reference/glossary.md#otlp) is the OpenTelemetry Protocol — the wire format used by OpenTelemetry collectors and SDKs to ship traces, metrics, and logs. Start a Collector with the default OTLP receiver enabled:

    ```bash
    docker run -d --name otel -p 4317:4317 \
      otel/opentelemetry-collector-contrib:latest
    ```

    The OTLP encoder and `otlp_grpc` sink are gated behind a Cargo feature flag — pre-built release binaries do **not** include them. You'll need to build from source:

    ```bash
    cargo build --release --features otlp -p sonda
    ```

    Update your scenario:

    ```yaml title="cpu-otlp.yaml"
    version: 2
    kind: runnable
    defaults:
      rate: 10
      duration: 30s
      encoder:
        type: otlp
      sink:
        type: otlp_grpc
        endpoint: "http://localhost:4317"
        signal_type: metrics
    scenarios:
      - id: cpu
        signal_type: metrics
        name: cpu_usage
        generator:
          type: sine
          amplitude: 50.0
          offset: 50.0
          period_secs: 60
    ```

    Run:

    ```bash
    ./target/release/sonda run cpu-otlp.yaml
    ```

    Verify by tailing the Collector's stdout (it logs every batch it receives with the default `logging` exporter):

    ```bash
    docker logs -f otel
    ```

    See [Sinks — `otlp_grpc`](../build/sinks.md#otlp_grpc) for the compatible receiver matrix (Collector, Grafana Alloy, Datadog Agent, Elastic APM).

## Tear down

```bash
docker rm -f vm loki otel
```

## Where to next

- [Run as a server](../deploy/server.md) — keep `sonda-server` running and POST scenarios over HTTP.
- [Sinks](../build/sinks.md) — every sink type, full parameter reference.
- [Test pipelines](../test/index.md) — exercise alert rules, recording rules, and full pipelines with the data you're now pushing.
