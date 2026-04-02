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

| Sink | Batching | Default Threshold | Configurable? | Unit |
|------|----------|-------------------|---------------|------|
| `stdout` | OS-level (`BufWriter`) | ~8 KB | No | bytes |
| `file` | OS-level (`BufWriter`) | ~8 KB | No | bytes |
| `tcp` | OS-level (`BufWriter`) | ~8 KB | No | bytes |
| `udp` | None (immediate) | -- | -- | -- |
| `http_push` | Application-level | 64 KiB | Yes | bytes |
| `kafka` | Application-level | 64 KiB | No | bytes |
| `loki` | Application-level | 100 entries | Yes | entries |
| `remote_write` | Application-level | 100 entries | Yes | entries |

### OS-level buffering (stdout, file, tcp)

These sinks wrap their output in Rust's `BufWriter` with a default ~8 KB buffer. Encoded metric
lines accumulate in memory and are written to the underlying destination when the buffer fills up
or when Sonda explicitly flushes at the end of the scenario.

This is why you see stdout output appear in bursts -- the terminal receives a chunk of lines each
time the buffer flushes, rather than one line per event.

### Application-level batching (http_push, kafka, loki, remote_write)

These sinks manage their own internal buffer. Each call to `write()` appends data to the buffer.
When the buffer reaches the configured threshold, the entire batch is sent as a single HTTP POST
or Kafka record.

This means data does not appear at the destination until either:

1. The batch fills up and triggers an automatic flush, or
2. The scenario completes and Sonda flushes the remaining partial batch.

### No batching (udp)

The UDP sink sends each encoded event as an individual datagram immediately. There is no
buffering.

## Configuring batch size

Three sinks let you tune the batch threshold via the `batch_size` field in the sink config.

=== "http_push"

    `batch_size` is in **bytes**. Default: `65536` (64 KiB).

    ```yaml title="Smaller batches for lower latency"
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
      batch_size: 1024  # 1 KiB -- more frequent sends
    ```

=== "remote_write"

    `batch_size` is in **TimeSeries entries**. Default: `100`.

    ```yaml title="Smaller remote write batches"
    encoder:
      type: remote_write
    sink:
      type: remote_write
      url: "http://localhost:8428/api/v1/write"
      batch_size: 10  # flush every 10 time series
    ```

=== "loki"

    `batch_size` is in **log entries**. Default: `100`.

    ```yaml title="Smaller Loki batches"
    sink:
      type: loki
      url: "http://localhost:3100"
      batch_size: 20  # flush every 20 log lines
    ```

??? tip "Choosing a batch size"
    Smaller batches mean data appears at the destination sooner, but each batch incurs HTTP or
    network overhead. For debugging and development, use small batches (e.g., `batch_size: 1`
    for http_push) to see data arrive immediately. For load testing, keep the defaults or
    increase them to reduce request volume.

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

**Network sinks have delivery delay.** With `http_push` at the default 64 KiB threshold, a
scenario producing small metrics (~100 bytes each) needs roughly 650 events to fill a batch.
At 10 events/second, that is about 65 seconds before the first HTTP POST goes out.

**Short scenarios may send only one batch.** If your scenario runs for 5 seconds at 10
events/second (50 events total), the entire output is likely sent as a single flush at the end,
not during execution.

For more details on configuring each sink, see [Sinks](sinks.md).
