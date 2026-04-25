# Sinks

So far everything has gone to stdout. In production testing, you need data flowing to
real backends -- over HTTP, TCP, or directly into Kafka or Loki. Sinks are the
delivery layer.

Sonda supports nine sinks:

| Sink | Description | CLI flag |
|------|-------------|----------|
| `stdout` | Print to standard output | _(default)_ |
| `file` | Write to a file | `--output path` |
| `tcp` | Stream to a TCP listener | YAML only |
| `udp` | Send to a UDP endpoint | YAML only |
| `http_push` | POST batches to an HTTP endpoint | `--sink http_push --endpoint <url>` |
| `loki` | Push logs to Grafana Loki | `--sink loki --endpoint <url>` |
| `kafka` | Publish to a Kafka topic | `--sink kafka --brokers <addr> --topic <t>` |
| `remote_write` | Prometheus remote write protocol | `--sink remote_write --endpoint <url>` |
| `otlp_grpc` | OTLP/gRPC to an OpenTelemetry Collector | `--sink otlp_grpc --endpoint <url> --signal-type <s>` |

## stdout (default)

No flags needed -- `stdout` is the default sink. Pipe output to any tool:

```bash
sonda metrics --name up --rate 10 --duration 5s | wc -l
```

## file

Write to a file with `--output`:

```bash
sonda metrics --name up --rate 10 --duration 5s --output /tmp/metrics.txt
```

## http_push

POST batched data to any HTTP endpoint. This is the most universal network sink -- it
works with any backend that accepts HTTP imports. Use CLI flags for quick ad-hoc runs:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink http_push --endpoint http://localhost:9090/api/v1/push \
  --content-type "text/plain; version=0.0.4"
```

Or use a scenario file for full control (including custom headers):

```bash
sonda metrics --scenario examples/http-push-sink.yaml
```

```yaml title="examples/http-push-sink.yaml (key fields)"
sink:
  type: http_push
  url: "http://localhost:9090/api/v1/push"
  content_type: "text/plain; version=0.0.4"
  batch_size: 65536
```

The key sink fields are `url`, `content_type`, and `batch_size` (bytes buffered before
each POST).

## tcp and udp

Stream raw encoded bytes over a socket. Both are YAML-only.

??? example "TCP sink setup"
    Start a listener in another terminal:

    ```bash
    nc -lk 9999
    ```

    Then run:

    ```bash
    sonda metrics --scenario examples/tcp-sink.yaml
    ```

    ```yaml title="examples/tcp-sink.yaml"
    version: 2

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
    sonda metrics --scenario examples/udp-sink.yaml
    ```

    ```yaml title="examples/udp-sink.yaml"
    version: 2

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

Push JSON logs to Grafana Loki. The fastest way is a single CLI command:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --sink loki --endpoint http://localhost:3100 \
  --label job=sonda --label env=dev
```

For richer logs with field pools and severity weights, use a scenario file:

```bash
sonda logs --scenario examples/loki-json-lines.yaml
```

??? example "Full Loki scenario file"

    ```yaml title="examples/loki-json-lines.yaml"
    version: 2

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

Publish to a Kafka topic. Use CLI flags for a quick test:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink kafka --brokers 127.0.0.1:9092 --topic sonda-metrics
```

Or use a scenario file for full control:

```bash
sonda metrics --scenario examples/kafka-sink.yaml
```

??? example "Full Kafka scenario file"

    ```yaml title="examples/kafka-sink.yaml (key fields)"
    sink:
      type: kafka
      brokers: "localhost:9094"
      topic: sonda-metrics
    ```

    See `examples/kafka-sink.yaml` for the complete file with generator and encoder
    config.

## remote_write

Prometheus remote write protocol -- native compatibility with VictoriaMetrics, Prometheus,
Thanos Receive, and Cortex/Mimir. The encoder and sink must both be `remote_write`:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Or use a scenario file:

```bash
sonda metrics --scenario examples/remote-write-vm.yaml
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

Push to an OpenTelemetry Collector via gRPC. Use CLI flags for a quick test:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317 --signal-type metrics
```

For logs, `--signal-type` defaults to `logs` automatically:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317
```

Or use a scenario file:

```bash
sonda metrics --scenario examples/otlp-metrics.yaml
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
