# Sink Batching

When you run a Sonda scenario, you might notice that metrics appear in chunks on stdout, or that
data shows up in VictoriaMetrics in bursts rather than one point at a time. This is batching at
work -- and it is intentional.

## Why batching exists

Sending each metric event individually would mean one syscall (for stdout/file/TCP) or one HTTP
request (for network sinks) per event. At high rates, that overhead dominates. Batching collects
events in a buffer and sends them together, trading a small delay for significantly better
throughput.

## How each sink batches

Sonda has two kinds of batching depending on the sink type:

| Sink | Batching | Size Threshold | Time Threshold | Unit |
|------|----------|----------------|----------------|------|
| `stdout` | OS-level (`BufWriter`) | ~8 KB (fixed) | -- | bytes |
| `file` | OS-level (`BufWriter`) | ~8 KB (fixed) | -- | bytes |
| `tcp` | OS-level (`BufWriter`) | ~8 KB (fixed) | -- | bytes |
| `udp` | None (immediate) | -- | -- | -- |
| `http_push` | Application-level | 4 KiB (configurable) | `5s` (configurable) | bytes |
| `kafka` | Application-level | 64 KiB (fixed) | `5s` (configurable) | bytes |
| `loki` | Application-level | 5 entries (configurable) | `5s` (configurable) | entries |
| `remote_write` | Application-level | 5 entries (configurable) | `5s` (configurable) | entries |
| `otlp_grpc` | Application-level | 5 entries (configurable) | `5s` (configurable) | entries |

### OS-level buffering (stdout, file, tcp)

These sinks wrap their output in Rust's `BufWriter` with a default ~8 KB buffer. Encoded metric
lines accumulate in memory and are written to the underlying destination when the buffer fills up
or when Sonda explicitly flushes at the end of the scenario.

This is why you see stdout output appear in bursts -- the terminal receives a chunk of lines each
time the buffer flushes, rather than one line per event.

### Application-level batching (http_push, kafka, loki, remote_write, otlp_grpc)

These sinks manage their own internal buffer. Each call to `write()` appends data to the buffer.
When the buffer reaches the configured threshold, the entire batch is sent as a single HTTP POST,
Kafka record, or gRPC call.

This means data does not appear at the destination until one of these happens:

1. The batch fills up and triggers a size-based flush,
2. A non-empty batch ages past its time threshold and triggers a [time-based flush](#time-based-flushing) (all five application-level sinks), or
3. The scenario completes and Sonda flushes the remaining partial batch.

### No batching (udp)

The UDP sink sends each encoded event as an individual datagram immediately. There is no
buffering.

## Configuring batch size

Four sinks let you tune the batch threshold via the `batch_size` field in the sink config.

=== "http_push"

    `batch_size` is in **bytes**. Default: `4096` (4 KiB).

    ```yaml title="Larger batches for high-rate scenarios"
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
      batch_size: 65536  # 64 KiB -- fewer requests at thousands of events/s
    ```

=== "remote_write"

    `batch_size` is in **TimeSeries entries**. Default: `5`.

    ```yaml title="Larger remote write batches for high-rate scenarios"
    encoder:
      type: remote_write
    sink:
      type: remote_write
      url: "http://localhost:8428/api/v1/write"
      batch_size: 100  # fewer requests at thousands of events/s
    ```

=== "loki"

    `batch_size` is in **log entries**. Default: `5`.

    ```yaml title="Larger Loki batches for high-rate scenarios"
    sink:
      type: loki
      url: "http://localhost:3100"
      batch_size: 100  # fewer requests at thousands of events/s
    ```

=== "otlp_grpc"

    `batch_size` is in **data points / log records**. Default: `5`.

    ```yaml title="Larger OTLP batches for high-rate scenarios"
    encoder:
      type: otlp
    sink:
      type: otlp_grpc
      endpoint: "http://localhost:4317"
      signal_type: metrics
      batch_size: 100  # fewer requests at thousands of events/s
    ```

??? tip "Choosing a batch size"
    Smaller batches mean data appears at the destination sooner, but each batch incurs HTTP or
    network overhead. For debugging and development, use small batches (e.g., `batch_size: 1`
    for http_push) to see data arrive immediately. For load testing, keep the defaults or
    increase them to reduce request volume.

## Time-based flushing

`batch_size` alone has a blind spot: a low-rate scenario. If you generate one log line every 20 seconds and `batch_size` is 5 entries, the buffer takes over a minute and a half to fill -- and nothing reaches the backend until it does. To anyone watching Loki or VictoriaMetrics, that looks like a broken pipeline.

`max_buffer_age` closes that gap. It is a *time* threshold that complements the *size* threshold: a non-empty batch is flushed once it has been buffered longer than `max_buffer_age`, in addition to the existing size-triggered and shutdown flushes. Whichever threshold trips first wins -- the batch can never get larger than `batch_size` or staler than `max_buffer_age`.

`max_buffer_age` is supported by all five application-level sinks -- `http_push`, `loki`, `remote_write`, `otlp_grpc`, and `kafka`. It accepts a duration string -- `"5s"`, `"500ms"`, `"2m"` -- and **defaults to `5s`** when omitted, so low-rate scenarios get prompt first delivery with zero configuration.

```yaml title="Low-rate scenario with explicit time threshold"
version: 2

defaults:
  rate: 0.05  # one event every 20 seconds
  encoder:
    type: json_lines

scenarios:
  - signal_type: logs
    name: slow_audit_logs
    log_generator:
      type: template
      templates:
        - message: "user {user} performed {action}"
          field_pools:
            user: ["alice", "bob"]
            action: ["login", "logout"]
    sink:
      type: loki
      url: "http://localhost:3100"
      batch_size: 100        # size threshold -- rarely reached at this rate
      max_buffer_age: "30s"  # time threshold -- flush a partial batch every 30s
    labels:
      job: sonda
      env: dev
```

### Disabling time-based flushing

Set `max_buffer_age: "0s"` to turn time-based flushing off. The sink reverts to size-and-shutdown-only flushing -- the behavior you get from `batch_size` by itself. This is the opt-out for high-rate streams that fill a batch in well under five seconds anyway and do not need the extra flush path.

```yaml title="Disable time-based flushing for a high-rate stream"
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
  batch_size: 65536
  max_buffer_age: "0s"  # size-and-shutdown flushing only
```

!!! info "The age is checked on write"
    `max_buffer_age` is evaluated each time an event is written to the sink. If a sink stops receiving writes entirely -- for example during a long scenario `gap` -- a partially-full batch will not flush until the next write arrives or the scenario stops. This is expected: the timer is driven by writes, not by a background clock.

## Flush on exit

When a scenario completes -- whether it reaches its configured `duration`, runs out of events,
or receives a Ctrl+C (SIGINT/SIGTERM) -- Sonda always calls `flush()` on the sink. This sends
any data remaining in the buffer, so you never lose a partial batch under normal circumstances.

!!! warning "SIGKILL bypasses flush"
    If you terminate Sonda with `kill -9` (SIGKILL), the process is killed immediately with no
    chance to flush. Any data sitting in the buffer is lost. Use Ctrl+C or `kill` (SIGTERM)
    instead to allow a clean shutdown.

## Practical implications

**Stdout appears chunky at low rates.** If you run a scenario at 1 event/second, you might not
see output for several seconds while the ~8 KB buffer fills. This is normal. The data will appear
all at once when the buffer flushes, or when the scenario ends.

**Network sinks have delivery delay.** With `http_push` at the default 4 KiB threshold, a
scenario producing small metrics (~100 bytes each) needs roughly 40 events to fill a batch.
At 10 events/second, that is about 4 seconds before the first HTTP POST goes out. Raise
`batch_size` for high-rate scenarios to cut request volume.

**Short scenarios may send only one batch.** If your scenario runs for 5 seconds at 10
events/second (50 events total), the entire output is likely sent as a single flush at the end,
not during execution.

For more details on configuring each sink, see [Sinks](sinks.md).
