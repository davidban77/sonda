---
title: Sinks
description: Reference for every Sonda sink — stdout, file, and network destinations like remote_write, Loki, and OTLP, with parameters and YAML examples.
---

# Sinks

A sink delivers encoded bytes to a destination. Set the destination with the `sink.type` field. The default is `stdout`.

Most sinks buffer data before delivery, which affects when output appears. For the full model, see [Sink Batching](sink-batching.md).

!!! tip "Choosing the right `url:`"
    Network sinks (`http_push`, `remote_write`, `loki`, `otlp_grpc`) accept a URL resolved inside the process that runs the scenario. The same YAML run from your host CLI versus POSTed to a containerized `sonda-server` reaches different hosts. See [Endpoints and networking](../deploy/server.md) for when to use `localhost`, Compose service names, or Kubernetes Service DNS.

## stdout

Writes events to standard output through a buffered writer. This is the default sink.

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

The `-o` flag on `sonda run` is a shortcut for `--sink file --endpoint <path>`:

```bash title="cpu-scenario.yaml"
sonda run cpu-scenario.yaml -o /tmp/metrics.txt
```

## tcp

Writes events over a persistent TCP connection. The connection opens when the scenario starts.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `address` | string | yes | Remote address in `host:port` format. |

```yaml title="TCP sink"
sink:
  type: tcp
  address: "127.0.0.1:9999"
```

## udp

Sends each encoded event as a single UDP datagram. There is no persistent connection. An ephemeral local port is bound and each event goes out through `send_to`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `address` | string | yes | Remote address in `host:port` format. |

```yaml title="UDP sink"
sink:
  type: udp
  address: "127.0.0.1:9999"
```

## memory

In-process buffer sink for tests and benchmarks. Encoded bytes accumulate in a `Vec<u8>` inside the running process; no I/O happens, no destination is contacted. Use this when you want to drive a scenario through Sonda's pipeline (generator → encoder → sink) and either ignore the output or inspect it from a test harness that holds a `MemorySink` directly.

When `capture: true`, each write records the `Instant` it landed alongside the bytes, keeping a ring of the most recent `max_events` entries (default `1_000_000`). The ring drops the oldest entry on overflow, so a benchmark that runs longer than the cap allows still ends up with the latest events for drift or rate analysis.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `capture` | boolean | no | `false` | Record `(Instant, bytes)` per write. |
| `max_events` | integer | no | `1_000_000` | Cap on retained captured events when `capture: true`. |

```yaml title="Memory sink (capture disabled)"
sink:
  type: memory
```

```yaml title="Memory sink with capture for drift measurement"
sink:
  type: memory
  capture: true
  max_events: 100000
```

!!! warning
    This sink is for in-process inspection only — there is no destination to deliver to, so it should never be used in production scenarios. Pair it with a Rust test or benchmark harness that holds the `MemorySink` instance and reads `MemorySink::captured()` after the run.

## http_push

!!! note
    This sink requires the `http` Cargo feature flag. Pre-built release binaries include this feature. To build from source: `cargo build --features http -p sonda`.

Buffers encoded events and delivers them by HTTP POST. Events accumulate until the batch size is reached, then the buffer is flushed as a single POST.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes | -- | Target URL for HTTP POST requests. |
| `content_type` | string | no | `application/octet-stream` | Value for the `Content-Type` header. |
| `batch_size` | integer | no | `4096` (4 KiB) | Size flush threshold in bytes. Raise this for high-rate scenarios. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |
| `headers` | map | no | none | Extra HTTP headers sent with every request. |

```yaml title="HTTP push sink"
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
  batch_size: 32768
```

**CLI override** — `sonda run` accepts `--sink http_push` and `--endpoint` to override the file's sink type and URL on a one-off run. Settings like `content_type`, `batch_size`, and custom `headers:` live in the YAML:

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

Batches metrics as Prometheus remote write requests. Pair it with the `remote_write` encoder, which produces length-prefixed protobuf `TimeSeries` bytes. The sink accumulates entries and, on flush, wraps them in a `WriteRequest`, applies snappy compression, and HTTP POSTs with the correct protocol headers.

!!! note
    This sink requires the `remote-write` Cargo feature flag. Pre-built release binaries include this feature. To build from source: `cargo build --features remote-write -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes | -- | Remote write endpoint URL. |
| `batch_size` | integer | no | `5` | Size flush threshold in number of TimeSeries entries. Raise this for high-rate scenarios. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |

```yaml title="Remote write sink"
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
  batch_size: 5
```

**CLI override** — `sonda run` accepts `--encoder remote_write`, `--sink remote_write`, and `--endpoint <url>` to override the file's sink and encoder on a one-off run:

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

Batches events and publishes them to a Kafka topic. Uses a pure-Rust Kafka client (`rskafka`) for static binary compatibility. No C dependencies or OpenSSL are required.

!!! note
    This sink requires the `kafka` Cargo feature flag. Pre-built release binaries include this feature. To build from source: `cargo build --features kafka -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `brokers` | string | yes | -- | Comma-separated broker addresses (e.g. `"broker1:9092,broker2:9092"`). |
| `topic` | string | yes | -- | Kafka topic name. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |
| `tls` | object | no | none | TLS encryption settings. See [TLS](#kafka-tls). |
| `sasl` | object | no | none | SASL authentication settings. See [SASL](#kafka-sasl). |

```yaml title="Kafka sink (plaintext)"
sink:
  type: kafka
  brokers: "127.0.0.1:9092"
  topic: sonda-metrics
```

Kafka brokers and topic live in the YAML. `sonda run` does not expose flags for them. Override the sink type from the command line with `--sink kafka` and define the rest in the file:

```bash title="kafka-cpu.yaml"
sonda run kafka-cpu.yaml
```

Events are buffered and published as a single Kafka record to partition 0 of the configured topic. The size threshold is a fixed 64 KiB internal buffer and is not user-tunable. `max_buffer_age` (default `5s`) is the configurable knob: a non-empty batch is flushed once buffered longer than that. Broker-side auto-topic-creation is supported. The sink retries metadata lookups, which gives the broker time to create the topic if `auto.create.topics.enable=true`.

### TLS { #kafka-tls }

Most managed Kafka services (Confluent Cloud, AWS MSK, Aiven) require TLS-encrypted connections. Add a `tls` block to enable encryption.

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

When `ca_cert` is omitted, Sonda trusts Mozilla's bundled root certificates (via `webpki-roots`). This works by default for any broker whose certificate is signed by a public CA.

For brokers with self-signed or internal CA certificates, point `ca_cert` at the PEM file:

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
    TLS uses `rustls` (pure Rust). There is no dependency on OpenSSL. The static musl binary stays fully self-contained.

### SASL { #kafka-sasl }

SASL authenticates your client to the Kafka broker. Sonda supports three mechanisms:

| Mechanism | When to use |
|-----------|-------------|
| `PLAIN` | Confluent Cloud, Aiven, most SaaS Kafka services. |
| `SCRAM-SHA-256` | AWS MSK (serverless and provisioned with SCRAM enabled). |
| `SCRAM-SHA-512` | Self-managed Kafka clusters that prefer stronger hashing. |

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
    SASL can be used without TLS, but Sonda prints a warning because the credentials are sent in plaintext over the network. Always enable TLS alongside SASL in production.

### Common configurations { #kafka-examples }

=== "Confluent Cloud"

    Confluent Cloud uses TLS and SASL PLAIN. Your API key is the username; the API secret is the password.

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
    TLS and SASL are configured via scenario YAML only. There are no CLI flags for these options. Use a scenario file with `--scenario`.

## loki

!!! note
    This sink requires the `http` Cargo feature flag. Pre-built release binaries include this feature. To build from source: `cargo build --features http -p sonda`.

Batches log lines and delivers them to Grafana Loki by HTTP POST. A Loki **stream** is the unit Loki indexes by, identified by its label set. At each flush, entries are grouped by their combined label set (scenario-level `labels` merged with any per-event `dynamic_labels`) and sent as a single push containing one stream per unique combination.

Scenario-level `labels:` form the base stream labels. Per-event `dynamic_labels` join the stream label set, so a rotation through N values produces N distinct streams, each queryable in Grafana by label. See [Dynamic Labels — Loki sinks](scheduling.md#dynamic-labels-with-the-loki-sink) for the worked pattern.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `url` | string | yes | -- | Base URL of the Loki instance. |
| `batch_size` | integer | no | `5` | Size flush threshold in number of log entries. Raise this for high-rate scenarios. |
| `max_streams_per_push` | integer | no | `128` | Hard cap on unique streams per flush. A flush that would emit more streams than the cap fails with a message naming the offending count and the cap. High-cardinality `dynamic_labels` configurations surface as a config error instead of overloading the Loki ingester. Raise this if your ingester is sized for higher cardinality. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |

```yaml title="Loki sink with top-level labels"
version: 2
kind: runnable

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
    log_generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint} returned {status}"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2"]
            endpoint: ["/api", "/health"]
            status: ["200", "404", "500"]
      severity_weights:
        info: 0.8
        warn: 0.15
        error: 0.05
```

**CLI override** — `sonda run` accepts `--sink loki`, `--endpoint <url>`, and `--label k=v` (repeatable) for stream labels:

```bash title="app-logs.yaml"
sonda run app-logs.yaml \
  --sink loki --endpoint http://localhost:3100 \
  --label job=sonda --label env=dev
```

The sink POSTs to `{url}/loki/api/v1/push`.

## otlp_grpc

Batches OTLP protobuf data and delivers it by gRPC to an OpenTelemetry Collector. Pair it with the `otlp` encoder, which produces length-prefixed protobuf `Metric` or `LogRecord` bytes. The sink accumulates entries and, on flush or when `batch_size` is reached, wraps them in an `ExportMetricsServiceRequest` or `ExportLogsServiceRequest` and sends them by gRPC unary call.

!!! warning "Not included in the pre-built binaries"
    OTLP is an optional feature. The release binaries and the published Docker image do **not** include it. Two ways to get it:

    - **As a binary** — build from source: `cargo build --features otlp -p sonda`.
    - **As a container** — build the image yourself with OTLP compiled in. See [Building the image](../deploy/docker.md#building-the-image).

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `endpoint` | string | yes | -- | gRPC endpoint URL of the OTEL Collector (e.g. `"http://localhost:4317"`). |
| `signal_type` | string | yes | -- | `"metrics"` or `"logs"`. Must match the scenario signal type. |
| `batch_size` | integer | no | `5` | Size flush threshold in number of data points or log records. Raise this for high-rate scenarios. |
| `max_buffer_age` | duration string | no | `5s` | Time flush threshold. A non-empty batch is flushed once buffered longer than this. Set `"0s"` to disable. See [Sink Batching](sink-batching.md#time-based-flushing). |

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

**CLI override** — `sonda run` accepts `--encoder otlp`, `--sink otlp_grpc`, and `--endpoint <url>`. The OTLP `signal_type` is derived from the scenario's `signal_type:`, so it does not need a separate CLI flag:

```bash title="cpu.yaml"
sonda run cpu.yaml \
  --encoder otlp --sink otlp_grpc --endpoint http://localhost:4317
```

Scenario-level `labels` are converted to OTLP `Resource` attributes, so they appear as resource metadata in the Collector's output.

Compatible receivers:

| Backend | Default gRPC port |
|---------|-------------------|
| OpenTelemetry Collector | `4317` |
| Grafana Alloy | `4317` |
| Datadog Agent (OTLP) | `4317` |
| Elastic APM Server | `8200` |

## gnmi_target

Serves the scenario's metrics as a gNMI target, the way a network device does. Every other sink connects out to a destination. This one listens: a gNMI client dials the `listen` address, subscribes to paths, and receives the latest value of every series under them. Clients include Telegraf's `inputs.gnmi`, gnmic, or a collector.

Telegraf is the client. It dials Sonda and asks for paths; Sonda never opens a connection.

Pair this sink with the [`gnmi` encoder](encoders.md#gnmi), which renders each series as a gNMI `Notification` on an openconfig path. Each takes only the other, and it works for `metrics`, `histogram` and `summary` entries. Logs entries take neither.

!!! note
    This sink requires the `gnmi` Cargo feature flag. Pre-built release binaries and the published Docker image include this feature. To build from source: `cargo build --features gnmi -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `listen` | string | yes | -- | Address to listen on, for example `"0.0.0.0:57400"`. |
| `max_subscribers` | integer | no | `16` | Subscriptions served at once, at least 1. One more is refused with `RESOURCE_EXHAUSTED`. |
| `sample_floor` | duration string | no | `1s` | Shortest sample and heartbeat interval served. A SAMPLE subscription that asks for less, or for 0, is sampled at this interval, and a shorter heartbeat interval is raised to it. |
| `subscriber_buffer` | integer | no | `1024` | Changes an ON_CHANGE subscription may fall behind before it is resynced, 1 to 65536. |
| `tls` | object | no | -- | Serve over TLS: `cert` and `key` PEM files, and `client_ca` to require client certificates. See [TLS and credentials](#tls-and-credentials). |
| `credentials` | object | no | -- | Require a `username` and `password` on every RPC. Needs `tls`. See [TLS and credentials](#tls-and-credentials). |

Every entry on one address shares one listener and one table of latest values, so the scenarios of a file are one device to the client. Entries on one address must set `max_subscribers`, `sample_floor`, `subscriber_buffer`, `tls` and `credentials` alike. A series belongs to the run that publishes it: another run publishing the same path while the first is live is a configuration error.

The listener serves gRPC, over TLS when `tls` is set:

| Request | What the target does |
|---------|----------------------|
| `Capabilities` | Answers gNMI version `0.10.0` and the encodings `PROTO` and `JSON_IETF`. |
| `Get` | Answers with the latest value of every series a requested path matches, one notification per series, each series once, in the request's encoding. `ALL`, `STATE` and `OPERATIONAL` data are served alike; `CONFIG` gets an empty response, since the target holds no configuration. A request some of whose paths match nothing gets what the others matched; one none of whose paths matches a series is refused with `NOT_FOUND`, naming the first path. A request with no paths, or with a data type gNMI does not define, is refused with `INVALID_ARGUMENT`. |
| `Subscribe`, ONCE | Sends the latest value of every matching series and `sync_response`, then ends the stream. |
| `Subscribe`, POLL | Sends the latest value of every matching series and `sync_response`, then the same again for each `poll` the client sends: every matching series, changed or not, then `sync_response`. Nothing is sent between polls, deletes included. A poll is answered once the previous answer has been sent. `updates_only` skips the initial values only. When the client closes its side of the stream, the target answers the polls already sent, then ends the stream. |
| `Subscribe`, STREAM | Sends the latest value of every matching series, then `sync_response`, once for the whole request even when it mixes modes. After that each subscription is served by its mode. |
| SAMPLE | Resends every matching series each sample interval. Each distinct interval in the request runs on its own schedule. |
| SAMPLE, `suppress_redundant: true` | At each sample interval, sends a matching series only when its value changed since the subscription last sent it. The initial values count as sent; a series first seen at a sample is sent. Ignored on the other modes. |
| ON_CHANGE | Sends a matching series each time its value changes, including a series first published after the subscription started. A value that holds sends nothing. |
| TARGET_DEFINED | Decides per series from the kind of value its first publish carried. A series with a string value (the encoder's `string` and `enum` values) is served as ON_CHANGE, any other as SAMPLE at the subscription's sample interval. |
| `heartbeat_interval` | On ON_CHANGE, on TARGET_DEFINED's string series, and on SAMPLE with `suppress_redundant`, resends the latest value of every matching series at this interval, changed or not. `0` sends none. Not read for SAMPLE without `suppress_redundant`, where every sample already sends every series. |
| `updates_only: true` | Skips the initial values; the first message is `sync_response`. |
| Encoding `PROTO` or `JSON` | Served in `PROTO`. `JSON` is also what a request that sets no encoding carries. |
| Encoding `JSON_IETF` | Served in `JSON_IETF`: every value is a `json_ietf_val`. See [Encodings](#gnmi-encodings). |
| Encoding `BYTES` or `ASCII` | Refused with `UNIMPLEMENTED`, naming the encoding, `PROTO` and `JSON_IETF`. |

A series leaves the target when the last scenario publishing it stops (scenarios of one scenario file can publish the same path), and at no other time: a series that stops changing stays, with its latest value. A subscriber receives one delete for every path it was sent that is no longer served, whichever way it learns: a notification whose `delete` names the path, split into `prefix` and path as its updates are. This covers the series an ON_CHANGE subscription (or TARGET_DEFINED, for a string series) or a SAMPLE subscription with `suppress_redundant` matches; a request whose only such subscription is the latter learns at its next sample. A request with both sends the delete once. A plain SAMPLE subscription receives no delete: the series stops appearing. ONCE and POLL never receive one. A series published again on the same path later is a new series and is sent like any other, after the old series' delete.

A change is a new value: the update's encoded `TypedValue` differs from the series' previous one, and the notification's timestamp does not count. A client that reads more slowly than its series change gets each series' latest value, not every change: a newer change replaces one still waiting to be sent. An ON_CHANGE subscription that falls more than `subscriber_buffer` changes behind is resynced with the latest value of every series it matches, without a second `sync_response`. A change the initial values, a resync or a heartbeat already sent is not sent again.

A series matches when its path starts with a subscription's path, joined to the request's prefix. Within it, a key of `*`, or a key the subscription leaves out, matches any value: `/interfaces/interface/state/counters` matches every interface's counters. An empty origin or target in the subscription matches any. An element named `*` matches any one element, keeping its own keys: `/interfaces/*/state/counters` matches `/interfaces/interface[name=eth0]/state/counters/in-octets` but not a path with two elements in its place. An element named `...` matches any run of elements, none included: `/interfaces/.../in-octets` matches every interface's `in-octets` counter and nothing else, and `/interfaces/...` matches everything under `/interfaces`. A `...` element with keys is refused with `INVALID_ARGUMENT`.

A subscription ends with `UNAVAILABLE` when:

- **The run stops.** The status follows the updates already queued, and the listener closes.
- **The client stalls.** The client takes nothing for three times the longest interval its subscriptions are sampled at, and at least 10 seconds. Heartbeat intervals do not count, so a request with only ON_CHANGE subscriptions, and a ONCE or POLL request, has the 10 seconds. The time counts only while responses wait, so a POLL client that sends no poll does not stall. A client's HTTP/2 layer accepts data until its receive window is full, whether or not the application reads it, so that time counts from when the window is full.

The server pings a connection that has been silent for 30 seconds and closes it when no answer arrives within 20 seconds.

The example below is a router with four interfaces, served to Telegraf. Only the first interface is shown here; the complete scenario is [`examples/gnmi-telegraf.yaml`](https://github.com/davidban77/sonda/blob/main/examples/gnmi-telegraf.yaml):

```yaml title="examples/gnmi-telegraf.yaml (excerpt)"
# sonda:static - excerpt only; a gnmi_target listens for a client, which the browser playground cannot provide
version: 2
kind: runnable

defaults:
  rate: 1
  encoder:
    type: gnmi
    origin: openconfig-interfaces
    target_label: device
    path: "/interfaces/interface[name={ifName}]/state/counters/{name}"
    values:
      in_octets: uint
      out_octets: uint
      oper_status:
        enum: { 1: UP, 2: DOWN }
    paths:
      oper_status: "/interfaces/interface[name={ifName}]/state/oper-status"
  sink:
    type: gnmi_target
    listen: 0.0.0.0:57400

scenarios:
  - signal_type: metrics
    name: in_octets
    generator: { type: step, start: 0, step_size: 125000 }
    labels: { device: rtr-core-01, ifName: GigabitEthernet0/0/0 }

  - signal_type: metrics
    name: out_octets
    generator: { type: step, start: 0, step_size: 62500 }
    labels: { device: rtr-core-01, ifName: GigabitEthernet0/0/0 }

  - signal_type: metrics
    name: oper_status
    generator:
      type: flap
      up_duration: 60s
      down_duration: 5s
      enum: oper_state
    labels: { device: rtr-core-01, ifName: GigabitEthernet0/0/0 }
```

The Telegraf side samples the counters every 10 seconds. It subscribes to the oper-status ON_CHANGE, so it receives the oper-status when it changes and every 30 seconds while it holds:

```toml title="examples/telegraf/telegraf.conf (excerpt)"
[[inputs.gnmi]]
  addresses = ["sonda:57400"]
  encoding = "proto"
  redial = "10s"

  [[inputs.gnmi.subscription]]
    name = "ifcounters"
    origin = "openconfig-interfaces"
    path = "/interfaces/interface/state/counters"
    subscription_mode = "sample"
    sample_interval = "10s"

  [[inputs.gnmi.subscription]]
    name = "ifstate"
    origin = "openconfig-interfaces"
    path = "/interfaces/interface/state/oper-status"
    subscription_mode = "on_change"
    heartbeat_interval = "30s"
```

Each subscription's `name` becomes the measurement, the interface key becomes the `name` tag, and the address Telegraf dialed becomes `source`. The full `telegraf.conf` maps oper-status `UP`/`DOWN` to `1`/`2` with `processors.enum`, so Telegraf writes these lines:

```text title="What Telegraf emits"
ifcounters,name=GigabitEthernet0/0/0,source=sonda in_octets=1250000i 1792339482653260154
ifcounters,name=GigabitEthernet0/0/0,source=sonda out_octets=625000i 1792339482653530936
ifstate,name=GigabitEthernet0/0/0,source=sonda oper_status=1i 1792339473653348313
```

To run the pair, start the compose stack's `gnmi` profile. It runs Telegraf with that config and writes to VictoriaMetrics. Then start the scenario on the host:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml --profile gnmi up -d
sonda run examples/gnmi-telegraf.yaml
```

Without Telegraf, gnmic reads the same target directly: once, as each oper-status changes, with a `Get`, or each time you poll:

```bash
gnmic -a 127.0.0.1:57400 --insecure subscribe --path /interfaces --mode once
gnmic -a 127.0.0.1:57400 --insecure subscribe --path /interfaces/interface/state/oper-status \
  --stream-mode on-change
gnmic -a 127.0.0.1:57400 --insecure get --path /interfaces/interface/state/oper-status
gnmic -a 127.0.0.1:57400 --insecure subscribe --path /interfaces --mode poll
```

In poll mode gnmic prints the initial values, then prompts for the target and subscription to poll; each choice sends one `poll`.

### Encodings { #gnmi-encodings }

The request's `encoding` decides how the values of its subscriptions are sent. One target serves `PROTO` and `JSON_IETF` requests at once.

- **`PROTO`**, and `JSON` or no encoding: each value is the `TypedValue` field the [`gnmi` encoder](encoders.md#gnmi) wrote.
- **`JSON_IETF`**: each value is a `json_ietf_val` holding the value's JSON text, written as RFC 7951 writes it. The timestamp, prefix and path are those a `PROTO` request receives.

Deletes and `sync_response` carry no value and are the same in both.

| Encoder value | `PROTO` | `JSON_IETF` text |
|---------------|---------|------------------|
| `uint` | `uint_val` | A JSON string: `"18446744073709551615"`. |
| `int` | `int_val` | A JSON string: `"-42"`. |
| `double` | `double_val` | A JSON number in decimal notation: `1.5`, `2`. NaN and the infinities, which JSON has no number for, are the strings `"NaN"`, `"Infinity"` and `"-Infinity"`. |
| `bool` | `bool_val` | `true` or `false`. |
| `string`, `enum` | `string_val` | A JSON string, escaped as RFC 8259 requires: `"UP"`. |

Integers arrive as strings: RFC 7951 writes 64-bit integers as JSON strings, so a client that reads the text with a plain JSON parser gets a string, not a number. gnmic asks for `JSON_IETF` with `-e json_ietf`:

```bash
gnmic -a 127.0.0.1:57400 --insecure -e json_ietf subscribe --path /interfaces --mode once
```

### TLS and credentials

With `tls`, the target serves TLS from a certificate and key, and can require client certificates. With `credentials`, every RPC must carry a username and password:

```yaml title="examples/gnmi-telegraf-tls.yaml (sink)"
# sonda:static - excerpt only; the certificate files are made locally
sink:
  type: gnmi_target
  listen: 0.0.0.0:57401
  tls:
    cert: examples/telegraf/certs/server.pem
    key: examples/telegraf/certs/server-key.pem
  credentials:
    username: sonda
    password: "sonda-uat"
```

| Field | Required | Description |
|-------|----------|-------------|
| `tls.cert` | yes | PEM file holding the certificate chain the target presents, its own certificate first. |
| `tls.key` | yes | PEM file holding the private key of `cert`. |
| `tls.client_ca` | no | PEM file of CA certificates. When set, every client must present a certificate signed by one of them; when unset, any client may connect. |
| `credentials.username` | yes | The username every RPC must carry. |
| `credentials.password` | yes | The password every RPC must carry. Quote it: YAML reads an unquoted number as a number, and the target then compares its rewritten text (`0x1F` as `31`). |

- **TLS.** Every connection to the `listen` address is TLS, and a plaintext client is refused at the handshake. A connection that has not finished its handshake 10 seconds after it was accepted is closed. The files are read when the scenario is validated, so `--dry-run` reports a file that cannot be read, or holds no PEM certificate or private key, naming the field and the file. A key that is not `cert`'s is reported the same way. Relative paths resolve against the directory Sonda runs in.
- **Credentials need TLS.** A sink with `credentials` and no `tls` is refused, because the password would cross the wire in clear.
- **Every RPC is checked.** `Capabilities`, `Get` and `Subscribe` alike must carry the gRPC metadata `username` and `password`, equal to the configured pair. A request without them, or with another pair, is refused with `UNAUTHENTICATED` and the message `credentials required`. The target logs each refusal at warn with the client's address and the method, never with the values it was sent. The password appears in no log line, error message or stats field.
- **One address, one identity.** Every entry on one address names the same `tls` files and the same `credentials`; a different set is refused, naming the files and usernames that differ but no password.

gnmic trusts the target's CA with `--tls-ca` and sends the pair with `-u` and `-p`:

```bash
gnmic -a 127.0.0.1:57401 --tls-ca examples/telegraf/certs/ca.pem -u sonda -p sonda-uat \
  subscribe --path /interfaces --mode once
```

When the target sets `client_ca`, gnmic also presents its own certificate with `--tls-cert` and `--tls-key`.

Telegraf's `inputs.gnmi` turns TLS on with `tls_enable`, trusts the CA with `tls_ca`, and sends `username` and `password`. With `client_ca` on the target, it presents `tls_cert` and `tls_key` as well:

```toml title="examples/telegraf/telegraf-tls.conf (excerpt)"
[global_tags]
  gnmi_row = "tls"

[[inputs.gnmi]]
  addresses = ["sonda:57401"]
  encoding = "json_ietf"
  redial = "10s"
  tls_enable = true
  tls_ca = "/etc/telegraf/certs/ca.pem"
  username = "sonda"
  password = "sonda-uat"
```

This client asks for `JSON_IETF`, so the counters arrive as [JSON strings](#gnmi-encodings). Telegraf writes them as string fields (`in_octets="1250000"`), which VictoriaMetrics stores as numbers; a backend that does not needs Telegraf's `processors.converter` to turn them into integers first. The `gnmi_row="tls"` tag tells this client's metrics apart from those of `telegraf.conf`, which carry the same `source`.

The compose stack's `gnmi-tls` profile runs this Telegraf, with `examples/telegraf/certs/` mounted at `/etc/telegraf/certs`. The certificates are not committed: make them first with the `openssl` commands in the header of `examples/gnmi-telegraf-tls.yaml`, for the names `sonda`, `localhost` and `127.0.0.1`. Then start the profile:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml --profile gnmi-tls up -d
```

and, from the repository root, `sonda run examples/gnmi-telegraf-tls.yaml`. Without the certificates, that command and its `--dry-run` stop with `gnmi_target tls cert "examples/telegraf/certs/server.pem" cannot be read`.

## snmp_agent

Serves the scenario's metrics as an SNMP agent, the way a managed device does. Like `gnmi_target`, this sink listens: a poller such as Telegraf's `inputs.snmp`, `snmpwalk` or a network management system sends requests to the `listen` address over UDP and reads the latest value of each instance.

Pair this sink with the [`snmp` encoder](encoders.md#snmp), which renders each series as an instance OID and a typed value. Each takes only the other, and it works for `metrics` entries. Histogram, summary and logs entries take neither.

!!! note
    This sink requires the `snmp` Cargo feature flag. Pre-built release binaries and the published Docker image include this feature. To build from source: `cargo build --features snmp -p sonda`.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `listen` | string | yes | -- | Address to listen on, for example `"0.0.0.0:1161"`. See [Ports](#snmp-ports). |
| `community` | string | no | `public` | The community a request must carry. |
| `max_message_size` | integer | no | `1472` | The largest reply the agent sends, in bytes, 484 to 65507. 1472 is the UDP payload of an Ethernet frame. |
| `system` | object | no | see below | The values of the system group, `1.3.6.1.2.1.1`. |
| `versions` | list | no | `[v2c]` | The SNMP versions the agent answers: any of `v1`, `v2c` and `v3`, each once. See [SNMPv1](#snmp-v1) and [SNMPv3](#snmp-v3). |
| `users` | list | no | none | The SNMPv3 users, each name once. `v3` in `versions` needs at least one. See [SNMPv3](#snmp-v3). |
| `engine_id` | string | no | per host | The agent's SNMPv3 engine ID: 5 to 32 bytes written as hex, such as `"80001f8804736f6e6461"`, a leading `0x` allowed. A string: an unquoted value YAML reads as a number is refused with the field named. See [SNMPv3](#snmp-v3). |

| `system` field | MIB object | Default | Description |
|----------------|------------|---------|-------------|
| `descr` | `sysDescr` | `""` | A description of the device. |
| `object_id` | `sysObjectID` | `1.3.6.1.4.1.8072.3.2.10` | The device's vendor OID, in dotted decimal. A string: an unquoted value YAML reads as a number, such as `1.30`, is refused with the field named. |
| `contact` | `sysContact` | `""` | Who to contact about the device. |
| `name` | `sysName` | `""` | The device's name. |
| `location` | `sysLocation` | `""` | Where the device is. |
| `services` | `sysServices` | `72` | The layers the device serves, 0 to 127. |

```yaml title="An snmp_agent sink with its system group"
sink:
  type: snmp_agent
  listen: 0.0.0.0:1161
  community: public
  system:
    name: rtr-core-01
    descr: Sonda synthetic router
    contact: noc@example.com
    location: lab-1
```

Every entry on one address shares one agent, so the scenarios of a file are one device to the poller. Entries on one address must set `community`, `max_message_size`, `system`, `versions`, `users` and `engine_id` alike (`versions` and `users` in any order); a difference is refused, naming the fields that differ but never a community or a password. An instance belongs to the run that publishes it: another run publishing the same OID while the first is live is a configuration error.

The agent serves the system group and every instance its entries publish, in OID order, which is the order a walk visits them in. `sysUpTime` counts hundredths of a second from when the agent started listening. No entry may publish under `1.3.6.1.2.1.1`. A request reads:

| Request for | Answer |
|-------------|--------|
| An instance the agent holds | Its latest value. |
| An object an entry has published under, or an OID under one, that is not an instance: a column without an index, a scalar without its `.0`, an index nobody publishes, an instance that has left | `noSuchInstance` |
| Any other OID | `noSuchObject` |
| The next OID (`GETNEXT`, `GETBULK`) | The instances that follow it in OID order, the system group included. |

An instance leaves the agent when the last scenario publishing it stops (scenarios of one scenario file can publish the same OID), and at no other time: a series that stops changing keeps its latest value, as a stalled counter does on a device. Once it has left, a walk skips it and a request for it answers `noSuchInstance`.

A [label column](encoders.md#label-columns) is served the same way. Its instance, such as `ifName.1`, arrives with the first value of the first series that carries it and stays while any series carrying it is live: `ifName.1` stays while any metric of interface 1 is published and leaves with the last. A label column's label is a static label of each entry, so the series of one entry agree on its value; entries that give one index different values share the instance, and the last one written is served. Another run's label column is owned like any other instance.

### Requests { #snmp-requests }

The agent answers SNMPv2c, SNMPv1 when `versions` lists it, and SNMPv3 when `versions` lists it. Each SNMPv1 or SNMPv2c datagram is checked in turn: its version, then its community, then its operation. This table is SNMPv2c's; [SNMPv1](#snmp-v1) lists what differs, and [SNMPv3](#snmp-v3) what comes before the operation over SNMPv3.

| Received | Answer |
|----------|--------|
| `GET` | Each OID on its own: its value, `noSuchInstance` or `noSuchObject`, as above. No error. |
| `GETNEXT` | Each OID's next instance in OID order, or `endOfMibView` past the last. |
| `GETBULK` | The first non-repeaters OIDs as `GETNEXT`; then, for the others, up to max-repetitions rounds of next instances, one per OID per round, ending after a round in which every one is `endOfMibView`. A negative non-repeaters or max-repetitions counts as 0. |
| `SET` | `notWritable` at error index 1, the request's values echoed. Nothing changes. |
| A request with no OIDs | An empty answer, no error. |
| An `INFORM`, a trap, a `REPORT` or a response | Dropped. |
| Another community | Dropped. Logged at warn at most once a minute per sender IP, with the community's length and never its value. |
| A version `versions` does not list | Dropped, the community never compared. |
| Anything that is not an SNMP message | Dropped. |

Every answer fits in `max_message_size` bytes. A `GETBULK` answer ends before the first value that would not fit, and is sent with the values before it. A `GET`, `GETNEXT` or `SET` whose answer does not fit is answered `tooBig` with no values, and logged at warn at most once a minute, naming the request's first OID and the size. Answers go to the address and port the request came from, and leave from the address the request was sent to, also when `listen` is a wildcard such as `0.0.0.0` or `[::]` (on Linux and macOS), so a poller that accepts replies only from the address it polled, as Telegraf does, receives them.

### SNMPv1 { #snmp-v1 }

With `versions: [v1, v2c]`, or `[v1]`, the agent also answers SNMPv1, as RFC 1157 and RFC 3584 describe. An answer carries the version of its request. SNMPv1 has no `counter64` type and no exceptions in its values, so the answers differ from SNMPv2c's:

| Received over SNMPv1 | Answer |
|----------------------|--------|
| `GET` | Each OID's value. The first OID that has no instance (where SNMPv2c answers `noSuchInstance` or `noSuchObject`), or whose value is a `counter64`, makes the answer `noSuchName` (error status 2) with that OID's position, from 1, as the error index, and the request's OIDs and values unchanged. |
| `GETNEXT` | Each OID's next instance in OID order, passing over every `counter64` instance. The first OID with no instance after it (where SNMPv2c answers `endOfMibView`) makes the answer `noSuchName` at its position, the request's OIDs and values unchanged. |
| `GETBULK` | Dropped: SNMPv1 has none. |
| `SET` | `noSuchName` at error index 1, the request's values echoed. Nothing changes. |
| A trap | Dropped. |
| An answer that does not fit | `tooBig` with the request's OIDs and values unchanged, where SNMPv2c sends none; dropped when even that does not fit. An OID that makes the answer `noSuchName` decides it first. |

A walk over SNMPv1 therefore reads every instance but the `counter64` ones: `ifInErrors`, `ifOperStatus` and the `ifName` label column, not `ifHCInOctets`.

### SNMPv3 { #snmp-v3 }

With `v3` in `versions`, the agent also answers SNMPv3 with the User-based Security Model (RFC 3414), for the users it is given. SNMPv3 adds users in place of the community, authentication and privacy: each request of a user with `auth` carries an HMAC of the whole message made with a key derived from the user's password, so the agent knows who asked and that nothing changed on the way, and each request of a user with `priv` as well carries its operation encrypted with a second key, derived from the privacy password. The operations and their answers are SNMPv2c's.

```yaml title="An snmp_agent sink answering SNMPv3"
sink:
  type: snmp_agent
  listen: 0.0.0.0:1161
  versions: [v2c, v3]
  users:
    - name: telegraf           # authPriv
      auth: { protocol: sha256, password: lab-only-v3-password }
      priv: { protocol: aes128, password: lab-only-v3-privacy }
    - name: monitor            # authNoPriv
      auth: { protocol: sha256, password: lab-only-v3-password }
    - name: watcher            # noAuthNoPriv: no auth block
```

| `users` field | Required | Description |
|---------------|----------|-------------|
| `name` | yes | The user name, 1 to 32 bytes. |
| `auth.protocol` | with `auth` | `md5`, `sha1` (also read as `sha`), `sha224`, `sha256`, `sha384` or `sha512`, as Telegraf names them in lower case. |
| `auth.password` | with `auth` | At least 8 bytes (RFC 3414). A string: a YAML number is refused. Never printed: a config shown for debugging prints `***` in its place. |
| `priv.protocol` | with `priv` | One of the privacy protocols below. `priv` needs `auth`. |
| `priv.password` | with `priv` | At least 8 bytes, a string, never printed, as `auth.password`. |

A user without `auth` is `noAuthNoPriv`: its requests carry no HMAC. A user with `auth` is `authNoPriv`, and with `priv` as well `authPriv`. That level is the user's minimum and the only one its requests are answered at: an unauthenticated request in the name of a user with `auth`, or an unencrypted one in the name of a user with `priv`, is refused, and so is an encrypted request for a user without `priv`.

| `priv.protocol` | Cipher | Key extension | Telegraf `priv_protocol` | net-snmp `-x` |
|-----------------|--------|---------------|--------------------------|---------------|
| `des` | DES-CBC (RFC 3414) | none | `DES` | `DES` |
| `aes128`, also read as `aes` | AES-128-CFB (RFC 3826) | none | `AES` | `AES` |
| `aes192` | AES-192-CFB | Blumenthal | `AES192` | `AES-192` or `AES192` |
| `aes192c` | AES-192-CFB | Reeder | `AES192C` | `AES-192-C` or `AES192C` |
| `aes256` | AES-256-CFB | Blumenthal | `AES256` | `AES-256` or `AES256` |
| `aes256c` | AES-256-CFB | Reeder | `AES256C` | `AES-256-C` or `AES256C` |

The privacy key is made from `priv.password` with the user's authentication hash, so it is as long as that hash. AES-192 and AES-256 need more than MD5 (16 bytes) or SHA-1 (20 bytes) gives, and two ways of extending the key are in use: the Blumenthal extension (draft-blumenthal-aes-usm-04) and the Reeder extension (draft-reeder-snmpv3-usm-3desede-00), which Cisco devices use and which gosnmp and Telegraf call the `C` variants. They give different keys, so a poller and an agent that disagree fail with a decryption error. With a hash at least as long as the key, such as SHA-256 with AES-256, there is nothing to extend and both give the same key.

MD5 and DES are legacy: MD5 is broken and DES's key is 56 bits. Use `sha256` and `aes128` where the poller has them. A DES salt is the engine boots and a 32-bit counter, and the boots are 1 on every start under the same key, so a salt is unique within a run but across runs only because each run's counter starts at a random value: a run that encrypts N messages with DES repeats one of the previous run's salts with odds on the order of N in 2³².

The agent is the SNMPv3 engine the poller talks to. Its engine ID is `engine_id`, or, without one, made from the host's name and the address the agent bound: net-snmp's enterprise prefix `80001f88`, format `05` (administratively assigned), then the first 16 bytes of the SHA-256 of `sonda:<host name>:<address>`, 21 bytes in all (a `listen` on port 0 gives the port it was bound to). It is the same every time the agent starts on one host and address, so a poller keeps its engine across a restart of sonda, and it differs between hosts, so agents in several containers behind one poller are separate engines. Set `engine_id` by hand when two agents on different hosts must present as one device across restarts, or when the poller is already configured with an engine ID. Its engine boots are 1 every time it starts listening (boots are not kept between runs) and its engine time counts whole seconds from then. A poller whose cached engine time is more than 150 seconds from the restarted clock gets a `usmStatsNotInTimeWindows` report and takes the new time from it. Each user's keys are derived from its passwords and the engine ID once, when the agent starts listening.

A poller first learns the engine ID, boots and time from the agent: it sends a request with an empty engine ID and gets a report back. An SNMPv3 message is checked in this order, and the first check it fails is answered with a `Report` carrying that `usmStats` counter (`1.3.6.1.6.3.15.1.1`), counted, and not answered further:

| Check | Answer |
|-------|--------|
| The security model is not the User-based Security Model | Dropped as malformed. |
| The engine ID is empty, or another engine's | `usmStatsUnknownEngineIDs`, unauthenticated, carrying the agent's engine ID, boots and time. This is discovery. |
| No user has the name | `usmStatsUnknownUserNames`. Logged at warn at most once a minute per sender IP, naming the user. |
| The request is not at the user's level: authenticated for a user without `auth` or not for one with it, encrypted for a user without `priv` or not for one with it | `usmStatsUnsupportedSecLevels` |
| The HMAC is not the one the user's key makes, or not as long as the protocol's | `usmStatsWrongDigests`, unauthenticated. Logged at warn at most once a minute per sender IP, naming the user and never a key. net-snmp prints it as an authentication failure. |
| The boots are not the agent's, or the time is more than 150 seconds from the agent's | `usmStatsNotInTimeWindows`, authenticated with the user's key, carrying the agent's boots and time. A poller takes them and asks again. |
| The request is encrypted and does not decrypt, with the user's privacy key, to an operation for this engine: another privacy password or protocol, a DES ciphertext that is not whole 8-byte blocks | `usmStatsDecryptionErrors`, authenticated. net-snmp prints it as a decryption error. |

A report is sent only when the request asks for one (its reportable flag), as pollers' requests do, and is never encrypted. A request that passes every check is answered as SNMPv2c answers it, at its security level: authenticated with the user's key when it was, and encrypted, with a salt no other answer of the run has used, when it was. The answer fits in the smaller of `max_message_size` and the message size the request says the poller accepts, DES's padding included. SNMPv3 has no views here: every user reads everything the agent serves, and `SET` is still refused `notWritable`.

### Ports { #snmp-ports }

SNMP's standard port is 161. Binding a port below 1024 takes the `CAP_NET_BIND_SERVICE` capability (or root); without it the run stops with an error naming the port and the capability. Listen on a port above 1023, such as 1161, and point the poller at it, or grant the binary the capability with `setcap cap_net_bind_service=+ep`.

A `gnmi_target` and an `snmp_agent` may use the same port number: one listens on TCP, the other on UDP.

With the agent running, net-snmp's tools read it directly:

```bash
snmpget -v2c -c public 127.0.0.1:1161 1.3.6.1.2.1.1.5.0
snmpwalk -v2c -c public -On 127.0.0.1:1161 1.3.6.1.2.1
snmpwalk -v1 -c public -On 127.0.0.1:1161 1.3.6.1.2.1    # when versions lists v1
snmpget -v3 -l authPriv -u telegraf -a SHA-256 -A lab-only-v3-password \
  -x AES -X lab-only-v3-privacy -On 127.0.0.1:1161 1.3.6.1.2.1.1.5.0   # when versions lists v3
```

### Telegraf { #snmp-telegraf }

The example below is a router with four interfaces, polled by Telegraf's `inputs.snmp`. The [`telegraf_snmp_interface`](builtin-packs.md#snmp-hints) pack carries each metric's IF-MIB column and SNMP type, and serves `ifName` and `ifAlias` as ifXTable columns, so the encoder block names no OIDs. Only the first interface is shown here; the complete scenario is [`examples/snmp-telegraf.yaml`](https://github.com/davidban77/sonda/blob/main/examples/snmp-telegraf.yaml):

```yaml title="examples/snmp-telegraf.yaml (excerpt)"
# sonda:static - excerpt only; an snmp_agent listens for a poller, which the browser playground cannot provide
version: 2
kind: runnable

defaults:
  rate: 1
  encoder:
    type: snmp
    drop_labels: [job]
  sink:
    type: snmp_agent
    listen: 0.0.0.0:1161
    community: public
    versions: [v1, v2c, v3]
    users:
      - name: telegraf
        auth: { protocol: sha256, password: lab-only-v3-password }
        priv: { protocol: aes128, password: lab-only-v3-privacy }
      - name: monitor
        auth: { protocol: sha256, password: lab-only-v3-password }
    system:
      name: rtr-core-01
      descr: Sonda synthetic router
      contact: noc@example.com
      location: lab-1

scenarios:
  - signal_type: metrics
    pack: telegraf_snmp_interface
    labels: { device: rtr-core-01, ifIndex: 1, ifName: GigabitEthernet0/0/0 }
    overrides:
      ifOperStatus:
        generator:
          type: flap
          up_duration: 60s
          down_duration: 5s
          enum: oper_state
```

Every label is in the instance OID, is the agent label, is a label column, or is dropped: `ifIndex` is the index arc, `device` is the agent label, `ifName` and `ifAlias` are served at `ifName.1` and `ifAlias.1`, and the agent serves no OID for `job`, so the encoder drops it. Interface 1's `ifOperStatus` is up for 60 seconds and down for 5. The agent answers SNMPv1, SNMPv2c, and SNMPv3 for the user `telegraf` at `authPriv` and the user `monitor` at `authNoPriv`, whose passwords are lab values.

Telegraf polls every 2 seconds: it reads `sysUpTime` and `sysName` and walks the interface table with `GETBULK` over SNMPv2c, tagging each interface with its `ifName` and `ifAlias`. Every OID is numeric, so Telegraf loads no MIB:

```toml title="examples/telegraf/telegraf-snmp.conf (excerpt)"
[[inputs.snmp]]
  agents = ["udp://sonda:1161"]
  version = 2
  community = "public"
  agent_host_tag = "source"

  [[inputs.snmp.field]]
    name = "uptime"
    oid = ".1.3.6.1.2.1.1.3.0"

  [[inputs.snmp.field]]
    name = "hostname"
    oid = ".1.3.6.1.2.1.1.5.0"
    is_tag = true

  [[inputs.snmp.table]]
    name = "interface"
    index_as_tag = true

    [[inputs.snmp.table.field]]
      name = "ifName"
      oid = ".1.3.6.1.2.1.31.1.1.1.1"
      is_tag = true

    [[inputs.snmp.table.field]]
      name = "ifOperStatus"
      oid = ".1.3.6.1.2.1.2.2.1.8"

    [[inputs.snmp.table.field]]
      name = "ifHCInOctets"
      oid = ".1.3.6.1.2.1.31.1.1.1.6"
```

The table's `name` becomes the measurement, each row's index the `index` tag and its `ifName` the `ifName` tag, and `agent_host_tag` tags every metric with the host Telegraf polled as `source`. The full config also polls `ifInErrors`, `ifOutErrors` and `ifHCOutOctets`, tags each row with its `ifAlias` (the example sets none, and an empty tag is left off), and walks the same table again over SNMPv1 as `interface_v1`, with `GETNEXT` and the same tags. Telegraf writes to VictoriaMetrics over the InfluxDB line protocol, which stores one series per field and interface. The SNMPv1 table has no `ifHCInOctets` or `ifHCOutOctets` series, because the agent passes over `counter64` instances for an SNMPv1 poller:

```toml title="examples/telegraf/telegraf-snmp.conf (the SNMPv1 block, excerpt)"
[[inputs.snmp]]
  agents = ["udp://sonda:1161"]
  version = 1
  community = "public"
  agent_host_tag = "source"

  [[inputs.snmp.table]]
    name = "interface_v1"
    index_as_tag = true

    [[inputs.snmp.table.field]]
      name = "ifName"
      oid = ".1.3.6.1.2.1.31.1.1.1.1"
      is_tag = true

    [[inputs.snmp.table.field]]
      name = "ifOperStatus"
      oid = ".1.3.6.1.2.1.2.2.1.8"
```

A third block reads `sysUpTime` and walks the same table over SNMPv3 at `authPriv`, as the user `telegraf` with HMAC-SHA-256 and AES-128, writing `interface_v3` and, through its own measurement name, `snmp_v3_uptime`:

```toml title="examples/telegraf/telegraf-snmp.conf (the SNMPv3 block, excerpt)"
[[inputs.snmp]]
  name = "snmp_v3"
  agents = ["udp://sonda:1161"]
  version = 3
  sec_level = "authPriv"
  sec_name = "telegraf"
  auth_protocol = "SHA256"
  auth_password = "lab-only-v3-password"
  priv_protocol = "AES"
  priv_password = "lab-only-v3-privacy"
  agent_host_tag = "source"

  [[inputs.snmp.field]]
    name = "uptime"
    oid = ".1.3.6.1.2.1.1.3.0"

  [[inputs.snmp.table]]
    name = "interface_v3"
    index_as_tag = true
```

```text title="What VictoriaMetrics stores (excerpt)"
interface_ifOperStatus{db="telegraf",ifName="GigabitEthernet0/0/0",index="1",source="sonda"}
interface_ifHCInOctets{db="telegraf",ifName="GigabitEthernet0/0/0",index="1",source="sonda"}
interface_v1_ifOperStatus{db="telegraf",ifName="GigabitEthernet0/0/0",index="1",source="sonda"}
interface_v3_ifHCInOctets{db="telegraf",ifName="GigabitEthernet0/0/0",index="1",source="sonda"}
snmp_uptime{db="telegraf",hostname="rtr-core-01",source="sonda"}
snmp_v3_uptime{db="telegraf",source="sonda"}
```

To run the pair, start the compose stack's `snmp` profile. It runs Telegraf with that config. Then start the scenario on the host:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml --profile snmp up -d
sonda run examples/snmp-telegraf.yaml
```

The Telegraf image ships net-snmp, so the agent can be walked from inside the container too, at the address Telegraf polls:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml exec telegraf-snmp \
  snmpbulkwalk -v2c -c public -On sonda:1161 1.3.6.1.2.1
docker compose -f examples/docker-compose-victoriametrics.yml exec telegraf-snmp \
  snmpwalk -v1 -c public -On sonda:1161 1.3.6.1.2.1
docker compose -f examples/docker-compose-victoriametrics.yml exec telegraf-snmp \
  snmpget -v3 -l authPriv -u telegraf -a SHA-256 -A lab-only-v3-password \
  -x AES -X lab-only-v3-privacy -On sonda:1161 1.3.6.1.2.1.1.5.0
docker compose -f examples/docker-compose-victoriametrics.yml exec telegraf-snmp \
  snmpget -v3 -l authNoPriv -u monitor -a SHA-256 -A lab-only-v3-password \
  -On sonda:1161 1.3.6.1.2.1.1.5.0
```

## Retry with backoff

Network sinks can hit transient failures — connection resets, HTTP 5xx responses, broker outages. By default, Sonda does **not** retry, which is ideal for CI pipelines where fast failure feedback matters. For long-running soak tests or Kubernetes deployments where short interruptions are expected, add a `retry:` block to any network sink.

Retry applies to these sinks: `http_push`, `remote_write`, `loki`, `otlp_grpc`, `kafka`, `tcp`. It does **not** apply to `stdout`, `file`, or `udp`.

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
| `max_backoff` | duration string | Upper bound on any single backoff delay. Must be at least `initial_backoff`. |

### How it works

Sonda uses **exponential backoff with full jitter**:

```text
base  = min(max_backoff, initial_backoff * 2^attempt)
sleep = random(0, base)
```

The jitter prevents synchronized retries when several sinks retry at the same time ("thundering herd"). Each retry attempt is logged to stderr:

```text
sonda: retry 1/3 after 127ms (error: connection refused)
sonda: retry 2/3 after 312ms (error: connection refused)
sonda: all 3 retries exhausted (last error: connection refused)
```

If every retry is exhausted, the batch is **discarded**. Sonda generates synthetic data, so losing a batch is acceptable and prevents unbounded buffer growth.

### Error classification

Not every error triggers a retry. Each sink classifies errors as retryable (transient) or permanent:

| Sink | Retryable | Not retried |
|------|-----------|-------------|
| `http_push`, `remote_write`, `loki` | HTTP 5xx, 429 (rate limit), transport errors | HTTP 4xx (except 429) |
| `otlp_grpc` | UNAVAILABLE, DEADLINE_EXCEEDED, RESOURCE_EXHAUSTED | INVALID_ARGUMENT, UNAUTHENTICATED |
| `kafka` | All produce errors (typically transient broker or network issues) | -- |
| `tcp` | Connection reset, broken pipe (reconnects automatically) | -- |

!!! warning
    Permanent errors (a 401 Unauthorized or a malformed request that returns 400) are never retried. Sonda logs a warning and drops the batch immediately.

Retry settings live in the scenario YAML. There is no CLI shortcut for them. Edit the `retry:` block inside the sink definition to tune attempts and backoff.
