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

The sink POSTs to `{url}/loki/api/v1/push`.
