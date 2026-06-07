---
title: Send to a real backend
description: Point Sonda at Prometheus remote_write or Loki instead of stdout.
---

# Send to a real backend

This page shows how to send a Sonda scenario to Prometheus or Loki running locally in Docker.

Stdout confirms that your YAML parses and produces data. To test an alert rule like "fire when latency crosses 200 ms", you need the data in a real backend. Two backends cover most first-time setups: Prometheus (using the `remote_write` protocol) and Loki.

Two Sonda fields decide where the data goes:

- `encoder:` — converts values into the wire format the backend expects.
- `sink:` — sends the encoded bytes to the backend over the network.

The pattern is the same for every backend. You change the `encoder:` and `sink:` blocks in your YAML, set the `url:`, and run.

!!! tip "About the `url:` field"
    The `url:` is resolved by the process that runs the scenario. If you POST a YAML to a containerised `sonda-server`, `http://localhost:8428` resolves to the container itself, not your host machine. The loopback address `localhost` always points to the current process's own machine, so a container sees its own internal network. See [Networking](../deploy/server.md#networking) for the full table.

For Kafka or other sinks, see [Sinks](../build/sinks.md). The OTLP encoder is not included in the pre-built binaries from the install script or Docker image. To use OTLP, you need a custom build of Sonda — see [Encoders — `otlp`](../build/encoders.md#otlp) for the details.

=== "Prometheus remote_write"

    [VictoriaMetrics](../reference/glossary.md#victoriametrics) is the easiest Prometheus-compatible backend to run locally. It is a single container with no config file. It accepts Prometheus [`remote_write`](../reference/glossary.md#remote_write) with no extra setup.

    Start it:

    ```bash
    docker run -d --name vm -p 8428:8428 \
      victoriametrics/victoria-metrics:latest
    ```

    Now update your scenario's encoder and sink.

    !!! info "Why both are called `remote_write`"
        The encoder and the sink share the name because they are two sides of the same protocol. The encoder produces the payload format Prometheus expects: Protocol Buffers compressed with Snappy. The sink sends the payload to Prometheus over HTTP. You pick both together when targeting a Prometheus `remote_write` endpoint.

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

    Run the scenario, then query VictoriaMetrics:

    ```bash
    sonda run cpu-remote-write.yaml

    # Wait ~5s for ingestion, then verify
    curl -s "http://localhost:8428/api/v1/query?query=cpu_usage" | jq '.data.result'
    ```

    See [Sinks — `remote_write`](../build/sinks.md#remote_write) for the full backend matrix: Prometheus, vmagent, Thanos, Cortex, and Mimir. See [Encoders — `remote_write`](../build/encoders.md#remote_write) for the feature-flag note.

=== "Loki"

    [Loki](../reference/glossary.md#loki) is Grafana's log aggregation backend. It indexes logs by labels rather than full text. Start it:

    ```bash
    docker run -d --name loki -p 3100:3100 \
      grafana/loki:latest
    ```

    Update your scenario to a `logs` signal. Use the JSON Lines encoder and the Loki sink:

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

    Run the scenario, then query Loki:

    ```bash
    sonda run app-logs-loki.yaml

    # Wait ~5s, then verify the stream exists
    curl -s "http://localhost:3100/loki/api/v1/labels" | jq
    curl -s -G "http://localhost:3100/loki/api/v1/query_range" \
      --data-urlencode 'query={job="sonda"}' | jq '.data.result | length'
    ```

    See [Sinks — `loki`](../build/sinks.md#loki) for the stream model, per-flush cardinality cap, and dynamic-label rotations.

## Clean up

Stop and remove the containers when you are done:

```bash
docker rm -f vm loki
```

## Where to next

- [Run as a server](../deploy/server.md) — keep `sonda-server` running and POST scenarios over HTTP.
- [Sinks](../build/sinks.md) — every sink type, full parameter reference.
- [Test pipelines](../test/index.md) — test alert rules, recording rules, and full pipelines with the data you are now pushing.
