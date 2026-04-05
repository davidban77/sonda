# Sinks

Sinks deliver encoded bytes to their destination. You select a sink with the `sink.type` field. If
omitted, the default is `stdout`.

Most sinks buffer data before delivering it, which affects when you see output. For a full
explanation of how this works, see [Sink Batching](sink-batching.md).

## stdout

Writes events to standard output via a buffered writer. This is the default sink.

No additional parameters.

```yaml title="Stdout sink"
sink:
  type: stdout
```

## file

Writes events to a file. Parent directories are created automatically.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | yes | Filesystem path to write to. |

```yaml title="File sink"
sink:
  type: file
  path: /tmp/sonda-output.txt
```

You can also use the `--output` CLI flag as a shorthand:

```bash
sonda metrics --name cpu --rate 10 --duration 5s --output /tmp/metrics.txt
```

## tcp

Writes events over a persistent TCP connection. The connection is established when the scenario
starts.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `address` | string | yes | Remote address in `host:port` format. |

```yaml title="TCP sink"
sink:
  type: tcp
  address: "127.0.0.1:9999"
```

## udp

Sends each encoded event as a single UDP datagram. No connection is established -- an ephemeral
local port is bound and each event is sent via `send_to`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `address` | string | yes | Remote address in `host:port` format. |

```yaml title="UDP sink"
sink:
  type: udp
  address: "127.0.0.1:9999"
```

## http_push

!!! note
    This sink requires the `http` Cargo feature flag. Pre-built release binaries include this
    feature. If building from source: `cargo build --features http -p sonda`.

Batches encoded events and delivers them via HTTP POST. Events accumulate in a buffer until the
batch size is reached, then the buffer is flushed as a single POST request.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes | -- | Target URL for HTTP POST requests. |
| `content_type` | string | no | `application/octet-stream` | Value for the `Content-Type` header. |
| `batch_size` | integer | no | `65536` (64 KiB) | Flush threshold in bytes. |
| `headers` | map | no | none | Extra HTTP headers sent with every request. |

```yaml title="HTTP push sink"
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
  batch_size: 32768
```

**CLI equivalent** -- use `--sink http_push` with `--endpoint`, `--content-type`, and
optionally `--batch-size`:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink http_push --endpoint http://localhost:8428/api/v1/import/prometheus \
  --content-type "text/plain"
```

### Pushing to VictoriaMetrics

```yaml title="VictoriaMetrics via HTTP push"
encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

### Custom headers

Use the `headers` map for protocols that require specific headers:

```yaml title="HTTP push with custom headers"
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/write"
  headers:
    Content-Type: "application/x-protobuf"
    Content-Encoding: "snappy"
    X-Prometheus-Remote-Write-Version: "0.1.0"
```

## remote_write

Batches metrics as Prometheus remote write requests. Designed to be paired with the
`remote_write` encoder, which produces length-prefixed protobuf `TimeSeries` bytes. The sink
accumulates entries and, on flush, wraps them in a `WriteRequest`, snappy-compresses it, and
HTTP POSTs with the correct protocol headers.

!!! note
    This sink requires the `remote-write` Cargo feature flag. Pre-built release binaries include
    this feature. If building from source: `cargo build --features remote-write -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes | -- | Remote write endpoint URL. |
| `batch_size` | integer | no | `100` | Flush threshold in number of TimeSeries entries. |

```yaml title="Remote write sink"
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
  batch_size: 100
```

**CLI equivalent** -- use `--encoder remote_write --sink remote_write --endpoint`:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Compatible endpoints:

| Backend | URL |
|---------|-----|
| VictoriaMetrics | `http://host:8428/api/v1/write` |
| vmagent | `http://host:8429/api/v1/write` |
| Prometheus | `http://host:9090/api/v1/write` |
| Thanos Receive | `http://host:19291/api/v1/receive` |
| Cortex / Mimir | `http://host:9009/api/v1/push` |

## kafka

Batches events and publishes them to a Kafka topic. Uses a pure-Rust Kafka client for static
binary compatibility.

!!! note
    This sink requires the `kafka` Cargo feature flag. Pre-built release binaries include this
    feature. If building from source: `cargo build --features kafka -p sonda`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `brokers` | string | yes | Comma-separated broker addresses (e.g. `"broker1:9092,broker2:9092"`). |
| `topic` | string | yes | Kafka topic name. |

```yaml title="Kafka sink"
sink:
  type: kafka
  brokers: "127.0.0.1:9092"
  topic: sonda-metrics
```

**CLI equivalent** -- use `--sink kafka --brokers --topic`:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink kafka --brokers 127.0.0.1:9092 --topic sonda-metrics
```

Events are buffered until 64 KiB is accumulated, then published as a single Kafka record to
partition 0 of the configured topic. Broker-side auto-topic-creation is supported: the sink
retries metadata lookups, giving the broker time to create the topic if
`auto.create.topics.enable=true`.

## loki

!!! note
    This sink requires the `http` Cargo feature flag. Pre-built release binaries include this
    feature. If building from source: `cargo build --features http -p sonda`.

Batches log lines and delivers them to Grafana Loki via HTTP POST. Each call to write appends one
log line, and the batch is flushed when it reaches the configured size.

Stream labels are configured at the **top-level** `labels` field of the scenario, not inside the
sink block. This is consistent with how labels work for all other signal types. The scenario-level
labels are used as Loki stream labels in the push API envelope.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes | -- | Base URL of the Loki instance. |
| `batch_size` | integer | no | `100` | Flush threshold in number of log entries. |

```yaml title="Loki sink with top-level labels"
name: app_logs_loki
rate: 10
labels:
  job: sonda
  env: dev
sink:
  type: loki
  url: "http://localhost:3100"
  batch_size: 50
```

**CLI equivalent** -- use `--sink loki --endpoint` and `--label` for stream labels:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --sink loki --endpoint http://localhost:3100 \
  --label job=sonda --label env=dev
```

The sink POSTs to `{url}/loki/api/v1/push`.

## otlp_grpc

Batches OTLP protobuf data and delivers it via gRPC to an OpenTelemetry Collector. Designed to
be paired with the `otlp` encoder, which produces length-prefixed protobuf `Metric` or `LogRecord`
bytes. The sink accumulates entries and, on flush or when `batch_size` is reached, wraps them in
an `ExportMetricsServiceRequest` or `ExportLogsServiceRequest` and sends via gRPC unary call.

!!! warning "Feature flag and build requirement"
    This sink requires the `otlp` Cargo feature flag. Pre-built release binaries and Docker
    images do **not** include this feature. You must build from source:
    `cargo build --features otlp -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `endpoint` | string | yes | -- | gRPC endpoint URL of the OTEL Collector (e.g. `"http://localhost:4317"`). |
| `signal_type` | string | yes | -- | `"metrics"` or `"logs"` -- must match the scenario signal type. |
| `batch_size` | integer | no | `100` | Flush threshold in number of data points or log records. |

```yaml title="OTLP gRPC sink (metrics)"
encoder:
  type: otlp
sink:
  type: otlp_grpc
  endpoint: "http://localhost:4317"
  signal_type: metrics
  batch_size: 100
```

```yaml title="OTLP gRPC sink (logs)"
encoder:
  type: otlp
sink:
  type: otlp_grpc
  endpoint: "http://localhost:4317"
  signal_type: logs
  batch_size: 50
```

**CLI equivalents:**

```bash
# Metrics
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317 --signal-type metrics

# Logs (--signal-type defaults to "logs" automatically)
sonda logs --mode template --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317
```

Scenario-level `labels` are automatically converted to OTLP `Resource` attributes, so they appear
as resource metadata in the Collector's output.

Compatible receivers:

| Backend | Default gRPC port |
|---------|-------------------|
| OpenTelemetry Collector | `4317` |
| Grafana Alloy | `4317` |
| Datadog Agent (OTLP) | `4317` |
| Elastic APM Server | `8200` |

## Retry with backoff

Network sinks can encounter transient failures -- connection resets, HTTP 5xx responses, broker
unavailability. By default, Sonda does **not** retry, which is ideal for CI pipelines where you want
fast failure feedback. For long-running soak tests or Kubernetes deployments where brief interruptions
are expected, you can add a `retry:` block to any network sink.

Retry applies to these sinks: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`, `tcp`.
It does **not** apply to `stdout`, `file`, or `udp`.

### Configuration

Add the `retry:` block inside any network sink definition:

```yaml title="Retry configuration"
sink:
  type: http_push
  url: "http://victoriametrics:8428/api/v1/import/prometheus"
  retry:
    max_attempts: 3          # retries after initial failure (total = 4 attempts)
    initial_backoff: 100ms   # first retry delay
    max_backoff: 5s          # backoff cap
```

All three fields are required when `retry:` is present:

| Field | Type | Description |
|-------|------|-------------|
| `max_attempts` | integer | Number of retry attempts after the initial failure. Must be at least 1. Total calls = `max_attempts + 1`. |
| `initial_backoff` | duration string | Delay before the first retry (e.g. `100ms`, `1s`). |
| `max_backoff` | duration string | Upper bound on any single backoff delay. Must be >= `initial_backoff`. |

### How it works

Sonda uses **exponential backoff with full jitter**:

```text
base  = min(max_backoff, initial_backoff * 2^attempt)
sleep = random(0, base)
```

The jitter prevents synchronized retries when multiple sinks retry at the same time ("thundering
herd"). Each retry attempt is logged to stderr:

```text
sonda: retry 1/3 after 127ms (error: connection refused)
sonda: retry 2/3 after 312ms (error: connection refused)
sonda: all 3 retries exhausted (last error: connection refused)
```

If all retries are exhausted, the batch is **discarded**. This prevents unbounded buffer growth --
since Sonda generates synthetic data, losing a batch is acceptable.

### Error classification

Not all errors trigger retries. Each sink classifies errors as retryable (transient) or permanent:

| Sink | Retryable | Not retried |
|------|-----------|-------------|
| `http_push`, `remote_write`, `loki` | HTTP 5xx, 429 (rate limit), transport errors | HTTP 4xx (except 429) |
| `otlp_grpc` | UNAVAILABLE, DEADLINE_EXCEEDED, RESOURCE_EXHAUSTED | INVALID_ARGUMENT, UNAUTHENTICATED |
| `kafka` | All produce errors (typically transient broker/network issues) | -- |
| `tcp` | Connection reset, broken pipe (reconnects automatically) | -- |

!!! warning
    Permanent errors (like a 401 Unauthorized or a malformed request returning 400) are never
    retried. Sonda logs a warning and discards the batch immediately.

### CLI flags

You can also configure retry from the command line with three flags. All three must be provided
together (all-or-nothing):

```bash
sonda metrics --name cpu --rate 10 --duration 60s \
  --sink http_push --endpoint http://localhost:8428/api/v1/import/prometheus \
  --retry-max-attempts 3 --retry-backoff 100ms --retry-max-backoff 5s
```

CLI retry flags override any `retry:` block in the YAML scenario file. See
[CLI Reference](cli-reference.md#retry) for details.
