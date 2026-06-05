---
title: Send to a real backend
description: Point Sonda at Prometheus remote_write or Loki instead of stdout.
---

# Send to a real backend

Stdout is useful for "does this YAML work?". For "does my alert rule fire when latency crosses 200 ms?" you need data in a real backend. This page walks the two backends most readers want first — Prometheus (`remote_write`) and Loki — with a 30-second local Docker setup and a `curl` verification for each.

The shape is the same every time: swap the `sink:` (and sometimes the `encoder:`) in your YAML, point it at the backend, and run.

!!! tip "Networking gotcha"
    The `url:` field is resolved inside the process that runs the scenario. If you POST a YAML to a containerized `sonda-server`, `http://localhost:8428` resolves to the container's loopback, not your host's. See [Networking](../deploy/server.md#networking) for the full table.

=== "Prometheus remote_write"

    [VictoriaMetrics](../reference/glossary.md#victoriametrics) is the easiest Prometheus-compatible backend to spin up: single container, no config file, accepts Prometheus [`remote_write`](../reference/glossary.md#remote_write) out of the box.

    Start it:

    ```bash
    docker run -d --name vm -p 8428:8428 \
      victoriametrics/victoria-metrics:latest
    ```

    Update your scenario's encoder and sink:

    !!! info "Why both are called `remote_write`"
        The encoder and the sink share the name `remote_write` because they are two sides of the same protocol: the encoder produces the protobuf+snappy payload, the sink delivers it over HTTP. You pick both together when targeting Prometheus's remote-write endpoint.

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

!!! info "What about OTLP?"
    The OTLP encoder and `otlp_grpc` sink are gated behind a Cargo feature flag — pre-built release binaries do **not** include them. To use them you build Sonda from source with `cargo build --release --features otlp -p sonda`. See [Encoders — `otlp`](../build/encoders.md#otlp) and [Sinks — `otlp_grpc`](../build/sinks.md#otlp_grpc) for the wire format, sink shape, and the compatible receiver matrix.

## Tear down

```bash
docker rm -f vm loki
```

## Where to next

- [Run as a server](../deploy/server.md) — keep `sonda-server` running and POST scenarios over HTTP.
- [Sinks](../build/sinks.md) — every sink type, full parameter reference.
- [Test pipelines](../test/index.md) — exercise alert rules, recording rules, and full pipelines with the data you're now pushing.
