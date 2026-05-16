# Sinks

So far everything has gone to stdout. In production testing, you need data flowing to
real backends -- over HTTP, TCP, or directly into Kafka or Loki. Sinks are the
delivery layer.

Sonda supports nine sinks:

| Sink | Description | YAML `sink.type` |
|------|-------------|------------------|
| `stdout` | Print to standard output | `stdout` (default) |
| `file` | Write to a file | `file` |
| `tcp` | Stream to a TCP listener | `tcp` |
| `udp` | Send to a UDP endpoint | `udp` |
| `http_push` | POST batches to an HTTP endpoint | `http_push` |
| `loki` | Push logs to Grafana Loki | `loki` |
| `kafka` | Publish to a Kafka topic | `kafka` |
| `remote_write` | Prometheus remote write protocol | `remote_write` |
| `otlp_grpc` | OTLP/gRPC to an OpenTelemetry Collector | `otlp_grpc` |

Sinks live in the YAML's `sink:` block (under `defaults:` or per entry). Quick CLI overrides exist for the common knobs — `--sink`, `--endpoint`, `--encoder`, and `-o <path>` (shorthand for `--sink file --endpoint <path>`). Anything richer (custom headers, batch size, Kafka brokers, OTLP signal type) goes in the file.

The starter scenario below is reused by every sink section that follows — copy it once, then change the `sink:` block:

```yaml title="cpu-base.yaml"
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 30s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu
    generator:
      type: sine
      amplitude: 50.0
      offset: 50.0
      period_secs: 60
```

## stdout (default)

No edit needed; the starter above writes to stdout. Pipe the output anywhere:

```bash
sonda run cpu-base.yaml | wc -l
```

## file

Either set `sink.type: file` in the YAML or use the `-o` shortcut on the CLI:

```bash
sonda run cpu-base.yaml -o /tmp/metrics.txt
```

```yaml title="file sink (YAML)"
sink:
  type: file
  path: /tmp/metrics.txt
```

## http_push

POST batched data to any HTTP endpoint — the most universal network sink. Use CLI overrides for ad-hoc runs:

```bash
sonda run cpu-base.yaml \
  --sink http_push --endpoint http://localhost:9090/api/v1/push
```

Use a scenario file when you need custom headers or a tuned batch size:

```bash
sonda run examples/http-push-sink.yaml
```

```yaml title="examples/http-push-sink.yaml (key fields)"
sink:
  type: http_push
  url: "http://localhost:9090/api/v1/push"
  content_type: "text/plain; version=0.0.4"
  batch_size: 65536
```

## tcp and udp

Stream raw encoded bytes over a socket. Both are YAML-only.

??? example "TCP sink setup"
    Start a listener in another terminal:

    ```bash
    nc -lk 9999
    ```

    Then run:

    ```bash
    sonda run examples/tcp-sink.yaml
    ```

    ```yaml title="examples/tcp-sink.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 10
      duration: 5s
      encoder:
        type: prometheus_text
      sink:
        type: tcp
        address: "127.0.0.1:9999"

    scenarios:
      - id: cpu_usage
        signal_type: metrics
        name: cpu_usage
        generator:
          type: sine
          amplitude: 50.0
          period_secs: 10
          offset: 50.0
        labels:
          host: server-01
          region: us-east
    ```

??? example "UDP sink setup"
    Start a listener in another terminal:

    ```bash
    nc -lu 9998
    ```

    Then run:

    ```bash
    sonda run examples/udp-sink.yaml
    ```

    ```yaml title="examples/udp-sink.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 10
      duration: 5s
      encoder:
        type: json_lines
      sink:
        type: udp
        address: "127.0.0.1:9998"

    scenarios:
      - id: cpu_usage
        signal_type: metrics
        name: cpu_usage
        generator:
          type: constant
          value: 1.0
        labels:
          host: server-01
    ```

## loki

Push JSON logs to Grafana Loki. The full file gives you templates and field pools:

```bash
sonda run examples/loki-json-lines.yaml
```

??? example "Full Loki scenario file"

    ```yaml title="examples/loki-json-lines.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 10
      duration: 60s
      encoder:
        type: json_lines
      sink:
        type: loki
        url: http://localhost:3100
        batch_size: 50
      labels:
        job: sonda
        env: dev

    scenarios:
      - id: app_logs_loki
        signal_type: logs
        name: app_logs_loki
        log_generator:
          type: template
          templates:
            - message: "Request from {ip} to {endpoint}"
              field_pools:
                ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3"]
                endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs"]
          severity_weights:
            info: 0.7
            warn: 0.2
            error: 0.1
    ```

## kafka

Publish to a Kafka topic. CLI overrides work for the host/topic, but Kafka brokers and ACK mode live in the YAML:

```bash
sonda run examples/kafka-sink.yaml
```

??? example "Kafka scenario fields"

    ```yaml title="examples/kafka-sink.yaml (key fields)"
    sink:
      type: kafka
      brokers: "localhost:9094"
      topic: sonda-metrics
    ```

    See `examples/kafka-sink.yaml` for the full file including generator and encoder
    config.

## remote_write

Prometheus remote write protocol — native compatibility with VictoriaMetrics, Prometheus, Thanos Receive, and Cortex/Mimir. The encoder and sink must both be `remote_write`:

```bash
sonda run cpu-base.yaml \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Or run the canonical example file:

```bash
sonda run examples/remote-write-vm.yaml
```

```yaml title="examples/remote-write-vm.yaml (key fields)"
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
  batch_size: 100
```

## otlp_grpc

Push to an OpenTelemetry Collector via gRPC. The encoder must be `otlp` and the sink type `otlp_grpc`. The signal type defaults from the scenario's `signal_type:` but is settable in the file:

```bash
sonda run examples/otlp-metrics.yaml
```

```yaml title="examples/otlp-metrics.yaml (key fields)"
encoder:
  type: otlp
sink:
  type: otlp_grpc
  endpoint: "http://localhost:4317"
  signal_type: metrics
  batch_size: 100
```

!!! warning "Feature flag required"
    OTLP requires the `otlp` Cargo feature. Pre-built binaries do **not** include it --
    build from source with `cargo build --features otlp -p sonda`. See
    [Sinks -- otlp_grpc](../configuration/sinks.md#otlp_grpc) for the full reference.

## Network resolution gotcha

If you POST a scenario to a containerised `sonda-server`, `localhost` resolves inside
the server container, not your host. See [Endpoints & networking](../deployment/endpoints.md)
for the full per-environment URL table.

For full sink configuration details, see [Sinks](../configuration/sinks.md).

## Next

Metrics covered. Sonda also generates structured log events with their own generators.

[Continue to **Generating logs** -->](tutorial-logs.md)
