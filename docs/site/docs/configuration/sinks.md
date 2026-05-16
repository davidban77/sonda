# Sinks

Sinks deliver encoded bytes to their destination. You select a sink with the `sink.type` field. If
omitted, the default is `stdout`.

Most sinks buffer data before delivering it, which affects when you see output. For a full
explanation of how this works, see [Sink Batching](sink-batching.md).

!!! tip "Choosing the right `url:`"
    Network sinks (`http_push`, `remote_write`, `loki`, `otlp_grpc`) accept a URL that is
    resolved inside the process running the scenario. The same YAML run from your host CLI
    vs. POSTed to a containerized `sonda-server` reaches different hosts. See
    [Endpoints & networking](../deployment/endpoints.md) for when to use `localhost`,
    Compose service names, or Kubernetes Service DNS.

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

You can also use the `-o` CLI flag on `sonda run` as a shorthand for `--sink file --endpoint <path>`:

```bash title="cpu-scenario.yaml"
sonda run cpu-scenario.yaml -o /tmp/metrics.txt
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
| `batch_size` | integer | no | `4096` (4 KiB) | Size flush threshold in bytes. Raise for high-rate scenarios. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once it has been buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |
| `headers` | map | no | none | Extra HTTP headers sent with every request. |

```yaml title="HTTP push sink"
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
  batch_size: 32768
```

**CLI override** -- `sonda run` accepts `--sink http_push` and `--endpoint` to override the file's
sink type and URL on a one-off run. Settings like `content_type`, `batch_size`, and custom
`headers:` live in the YAML:

```bash title="cpu-scenario.yaml"
sonda run cpu-scenario.yaml \
  --sink http_push --endpoint http://localhost:8428/api/v1/import/prometheus
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
| `batch_size` | integer | no | `5` | Size flush threshold in number of TimeSeries entries. Raise for high-rate scenarios. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once it has been buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |

```yaml title="Remote write sink"
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
  batch_size: 5
```

**CLI override** -- `sonda run` accepts `--encoder remote_write`, `--sink remote_write`, and
`--endpoint <url>` to override the file's sink and encoder on a one-off run:

```bash title="cpu-scenario.yaml"
sonda run cpu-scenario.yaml \
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

Batches events and publishes them to a Kafka topic. Uses a pure-Rust Kafka client (`rskafka`) for
static binary compatibility -- no C dependencies or OpenSSL required.

!!! note
    This sink requires the `kafka` Cargo feature flag. Pre-built release binaries include this
    feature. If building from source: `cargo build --features kafka -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `brokers` | string | yes | -- | Comma-separated broker addresses (e.g. `"broker1:9092,broker2:9092"`). |
| `topic` | string | yes | -- | Kafka topic name. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once it has been buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |
| `tls` | object | no | none | TLS encryption settings. See [TLS](#kafka-tls). |
| `sasl` | object | no | none | SASL authentication settings. See [SASL](#kafka-sasl). |

```yaml title="Kafka sink (plaintext)"
sink:
  type: kafka
  brokers: "127.0.0.1:9092"
  topic: sonda-metrics
```

Kafka brokers and topic live in the YAML; `sonda run` does not expose flags for them. Override
the sink type from the command line with `--sink kafka` and define the rest in the file:

```bash title="kafka-cpu.yaml"
sonda run kafka-cpu.yaml
```

Events are buffered and published as a single Kafka record to partition 0 of the configured topic. The size threshold is a fixed 64 KiB internal buffer -- it is not user-tunable -- while `max_buffer_age` (default `5s`) is the configurable knob: a non-empty batch is flushed once it has been buffered longer than that. Broker-side auto-topic-creation is supported: the sink retries metadata lookups, giving the broker time to create the topic if `auto.create.topics.enable=true`.

### TLS { #kafka-tls }

Most managed Kafka services (Confluent Cloud, AWS MSK, Aiven) require TLS-encrypted connections.
Add a `tls` block to enable encryption.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `tls.enabled` | boolean | yes | `false` | Set to `true` to connect over TLS. |
| `tls.ca_cert` | string | no | Mozilla bundled roots | Path to a PEM-encoded CA certificate file. Use this for self-signed or internal CAs. |

```yaml title="Kafka with TLS"
sink:
  type: kafka
  brokers: "broker.example.com:9093"
  topic: sonda-metrics
  tls:
    enabled: true
```

When `ca_cert` is omitted, Sonda trusts Mozilla's bundled root certificates (via `webpki-roots`).
This works out of the box for any broker whose certificate is signed by a public CA.

For brokers with self-signed or internal CA certificates, point `ca_cert` to the PEM file:

```yaml title="Kafka with custom CA"
sink:
  type: kafka
  brokers: "kafka-internal.corp:9093"
  topic: sonda-metrics
  tls:
    enabled: true
    ca_cert: /etc/ssl/certs/internal-ca.pem
```

!!! tip
    TLS uses `rustls` (pure Rust) -- there is no dependency on OpenSSL. This keeps the static
    musl binary fully self-contained.

### SASL { #kafka-sasl }

SASL authenticates your client to the Kafka broker. Sonda supports three mechanisms:

| Mechanism | When to use |
|-----------|-------------|
| `PLAIN` | Confluent Cloud, Aiven, most SaaS Kafka services. |
| `SCRAM-SHA-256` | AWS MSK (serverless and provisioned with SCRAM enabled). |
| `SCRAM-SHA-512` | Self-managed Kafka clusters preferring stronger hashing. |

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `sasl.mechanism` | string | yes | `"PLAIN"`, `"SCRAM-SHA-256"`, or `"SCRAM-SHA-512"`. |
| `sasl.username` | string | yes | SASL username (API key for Confluent Cloud). |
| `sasl.password` | string | yes | SASL password (API secret for Confluent Cloud). |

```yaml title="Kafka with SASL PLAIN"
sink:
  type: kafka
  brokers: "broker.example.com:9093"
  topic: sonda-metrics
  tls:
    enabled: true
  sasl:
    mechanism: PLAIN
    username: sonda
    password: changeme
```

!!! warning
    SASL can be used without TLS, but Sonda prints a warning because credentials are sent in
    plaintext over the network. Always enable TLS alongside SASL in production.

### Common configurations { #kafka-examples }

=== "Confluent Cloud"

    Confluent Cloud uses TLS + SASL PLAIN. Your API key is the username, API secret is the password.

    ```yaml title="confluent-cloud.yaml"
    sink:
      type: kafka
      brokers: "pkc-xxxxx.us-east-1.aws.confluent.cloud:9092"
      topic: sonda-metrics
      tls:
        enabled: true
      sasl:
        mechanism: PLAIN
        username: YOUR_API_KEY
        password: YOUR_API_SECRET
    ```

=== "AWS MSK (SCRAM)"

    AWS MSK with SASL/SCRAM authentication uses port 9096.

    ```yaml title="aws-msk-scram.yaml"
    sink:
      type: kafka
      brokers: "b-1.mycluster.xxxxx.kafka.us-east-1.amazonaws.com:9096"
      topic: sonda-metrics
      tls:
        enabled: true
      sasl:
        mechanism: SCRAM-SHA-256
        username: msk-user
        password: msk-password
    ```

=== "Internal CA"

    Self-managed Kafka cluster with an internal CA certificate and SCRAM auth.

    ```yaml title="internal-kafka.yaml"
    sink:
      type: kafka
      brokers: "kafka-01.internal:9093,kafka-02.internal:9093"
      topic: sonda-metrics
      tls:
        enabled: true
        ca_cert: /etc/ssl/certs/kafka-ca.pem
      sasl:
        mechanism: SCRAM-SHA-512
        username: sonda
        password: s3cret
    ```

=== "TLS only (no auth)"

    Some clusters use TLS for encryption but rely on network-level access control instead of SASL.

    ```yaml title="tls-only.yaml"
    sink:
      type: kafka
      brokers: "kafka.private:9093"
      topic: sonda-metrics
      tls:
        enabled: true
    ```

!!! info
    TLS and SASL are configured via scenario YAML only. There are no CLI flags for these options --
    use a scenario file with `--scenario`.

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
| `batch_size` | integer | no | `5` | Size flush threshold in number of log entries. Raise for high-rate scenarios. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once it has been buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |

```yaml title="Loki sink with top-level labels"
version: 2

defaults:
  rate: 10
  sink:
    type: loki
    url: "http://localhost:3100"
    batch_size: 50

scenarios:
  - signal_type: logs
    name: app_logs_loki
    labels:
      job: sonda
      env: dev
```

**CLI override** -- `sonda run` accepts `--sink loki`, `--endpoint <url>`, and `--label k=v`
(repeatable) for stream labels:

```bash title="app-logs.yaml"
sonda run app-logs.yaml \
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
| `batch_size` | integer | no | `5` | Size flush threshold in number of data points or log records. Raise for high-rate scenarios. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once it has been buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |

```yaml title="OTLP gRPC sink (metrics)"
encoder:
  type: otlp
sink:
  type: otlp_grpc
  endpoint: "http://localhost:4317"
  signal_type: metrics
  batch_size: 5
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

**CLI override** -- `sonda run` accepts `--encoder otlp`, `--sink otlp_grpc`, and `--endpoint <url>`.
The OTLP `signal_type` is derived from the scenario's `signal_type:`, so it does not need its own
CLI flag:

```bash title="cpu.yaml"
sonda run cpu.yaml \
  --encoder otlp --sink otlp_grpc --endpoint http://localhost:4317
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

Retry settings live in the scenario YAML — there is no CLI shortcut for them. Edit the
`retry:` block inside the sink definition to tune attempts and backoff.
