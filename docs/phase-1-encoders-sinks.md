# Phase 1 — Encoders & Sinks Implementation Plan

**Goal:** Expand Sonda beyond Prometheus-to-stdout. Add Influx Line Protocol and JSON Lines encoders,
and file, TCP/UDP, HTTP remote-write, and Kafka sinks.

**Prerequisite:** Phase 0 complete — encoder/sink traits stable, factory pattern proven, scenario runner
handles gaps, CLI works end-to-end.

**Final exit criteria:** A single scenario can target any combination of
`{prometheus_text, influx_lp, json_lines}` × `{stdout, file, tcp, udp, http_push, kafka}` via YAML config.

---

## Slice 1.1 — Influx Line Protocol Encoder

### Input state
- Phase 0 passes all gates.
- `Encoder` trait and factory exist in `sonda-core/src/encoder/mod.rs`.

### Specification

**Files to create:**
- `sonda-core/src/encoder/influx.rs`:
  ```rust
  pub struct InfluxLineProtocol { field_key: String }
  ```
  - Format: `measurement,tag1=val1,tag2=val2 field_key=value timestamp_ns\n`
  - Constructor: `new(field_key: Option<String>)` — default field key is `"value"`.
  - Tags sorted by key (InfluxDB best practice).
  - Timestamp in **nanoseconds** since epoch.
  - Escaping: measurement name and tag keys/values escape `,`, ` `, `=`.

**Files to modify:**
- `sonda-core/src/encoder/mod.rs`:
  - Add `pub mod influx`.
  - Add variant to `EncoderConfig`:
    ```rust
    #[serde(rename = "influx_lp")]
    InfluxLineProtocol { field_key: Option<String> },
    ```
  - Wire into `create_encoder()`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/encoder/influx.rs` | new |
| `sonda-core/src/encoder/mod.rs` | modified |

### Test criteria
- Basic metric with two labels → correct line protocol string with sorted tags.
- Metric with no labels → measurement name, space, field, space, timestamp.
- Escaping: measurement name with space/comma → escaped correctly.
- Tag value with `=` → escaped to `\=`.
- Timestamp is nanoseconds (13+ digits, not milliseconds).
- Regression anchor: hardcoded input → exact expected byte string.

### Review criteria
- Zero per-event allocations.
- Tags sorted by key.
- Escaping rules are Influx-specific (different from Prometheus).
- `field_key` is pre-stored, not re-computed.

### UAT criteria
- `sonda metrics --name cpu --rate 10 --duration 3s --encoder influx_lp --value-mode constant --offset 42 --label host=srv1` → valid Influx line protocol to stdout.
- Output accepted by `influx write` CLI if available.

---

## Slice 1.2 — JSON Lines Encoder

### Input state
- Slice 1.1 passes all gates (or can run in parallel with 1.1).

### Specification

**Files to create:**
- `sonda-core/src/encoder/json.rs`:
  ```rust
  pub struct JsonLines;
  ```
  - Format: `{"name":"metric","value":1.0,"labels":{"k":"v"},"timestamp":"2026-03-20T12:00:00.000Z"}\n`
  - Timestamp in RFC 3339 / ISO 8601 with millisecond precision.
  - Use `serde_json::to_writer()` writing directly into buffer.
  - Labels as flat JSON object.

**Files to modify:**
- `sonda-core/Cargo.toml` — add `serde_json` workspace dependency.
- `sonda-core/src/encoder/mod.rs`:
  - Add `pub mod json`.
  - Add `JsonLines` variant to `EncoderConfig`.
  - Wire into `create_encoder()`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/encoder/json.rs` | new |
| `sonda-core/src/encoder/mod.rs` | modified |
| `sonda-core/Cargo.toml` | modified |

### Test criteria
- Basic metric → valid JSON parseable by `serde_json::from_str()`.
- Roundtrip: encode → parse → verify all fields match original event.
- Empty labels → `"labels":{}`.
- Timestamp format: RFC 3339 with milliseconds.
- Each line ends with `\n`.
- Regression anchor: hardcoded input → exact JSON string.

### Review criteria
- Uses `serde_json::to_writer()` into buffer (not `to_string()` + copy).
- Timestamp formatting without pulling in `chrono` if possible (manual or `time` crate).
- JSON field order is consistent.

### UAT criteria
- `sonda metrics --name http_requests --rate 10 --duration 3s --encoder json_lines --value-mode constant --offset 100 --label endpoint=/api` → valid JSON per line.
- Each line parseable by `jq`.

---

## Slice 1.3 — File Sink

### Input state
- At least one new encoder (1.1 or 1.2) passes all gates.

### Specification

**Files to create:**
- `sonda-core/src/sink/file.rs`:
  ```rust
  pub struct FileSink { writer: BufWriter<File> }
  ```
  - `FileSink::new(path: &Path) -> Result<Self, SondaError>`.
  - Create parent directories if missing (`std::fs::create_dir_all`).
  - `write()` and `flush()` delegate to BufWriter.

**Files to modify:**
- `sonda-core/src/sink/mod.rs`:
  - Add `pub mod file`.
  - Add `File` variant to `SinkConfig`: `File { path: String }`.
  - Wire into `create_sink()`.
- `sonda/src/cli.rs` — add `--output <path>` flag as shorthand for file sink.
- `sonda/src/config.rs` — if `--output` provided, override sink to `File { path }`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/sink/file.rs` | new |
| `sonda-core/src/sink/mod.rs` | modified |
| `sonda/src/cli.rs` | modified |
| `sonda/src/config.rs` | modified |

### Test criteria
- Write to temp file → read back, verify exact contents.
- Parent dir creation: write to `/tmp/sonda-test/subdir/out.txt` → dirs created.
- Flush on drop: data appears in file after sink is dropped.
- Permission error → `SondaError::Sink(...)` with path in message.

### Review criteria
- BufWriter wraps File (not raw writes).
- Parent dir creation uses `create_dir_all`.
- Path comes from config, not hardcoded.

### UAT criteria
- `sonda metrics --name up --rate 10 --duration 3s --output /tmp/sonda-test.txt` → file created with ~30 lines.
- `sonda metrics --name up --rate 10 --duration 3s --output /tmp/sonda/nested/test.txt` → nested dirs created.

---

## Slice 1.4 — TCP and UDP Sinks

### Input state
- Slice 1.3 passes all gates.

### Specification

**Files to create:**
- `sonda-core/src/sink/tcp.rs`:
  ```rust
  pub struct TcpSink { writer: BufWriter<TcpStream> }
  ```
  - `TcpSink::new(addr: &str) -> Result<Self, SondaError>` — connect on construction.
  - `write()` → `write_all` on buffered stream.
  - `flush()` → flush buffered stream.

- `sonda-core/src/sink/udp.rs`:
  ```rust
  pub struct UdpSink { socket: UdpSocket, target: SocketAddr }
  ```
  - `UdpSink::new(addr: &str) -> Result<Self, SondaError>` — bind ephemeral port, set target.
  - `write()` → `send_to` as single datagram.
  - `flush()` → no-op.

**Files to modify:**
- `sonda-core/src/sink/mod.rs`:
  - Add `pub mod tcp` and `pub mod udp`.
  - Add variants to `SinkConfig`: `Tcp { address: String }`, `Udp { address: String }`.
  - Wire into `create_sink()`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/sink/tcp.rs` | new |
| `sonda-core/src/sink/udp.rs` | new |
| `sonda-core/src/sink/mod.rs` | modified |

### Test criteria
- **TCP**: start `TcpListener` on localhost, connect sink, write data, accept + read → matches.
- **UDP**: bind `UdpSocket`, send via sink, recv → matches.
- **Connection refused**: connect to unused port → `SondaError::Sink(...)`.
- **Address parsing**: "127.0.0.1:9999" → Ok. "not-a-host" → Err.

### Review criteria
- TCP uses BufWriter around TcpStream.
- UDP handles datagram size limits (return error if data > 65507 bytes).
- Error messages include the address that failed.

### UAT criteria
- Start `nc -l 9999` in background → `sonda metrics --name up --rate 10 --duration 3s` with tcp sink to localhost:9999 → nc receives data.
- Same test with UDP: `nc -u -l 9999`.

---

## Slice 1.5 — HTTP Push Sink (Remote Write)

### Input state
- Slice 1.4 passes all gates.

### Specification

**Files to create:**
- `sonda-core/src/sink/http.rs`:
  ```rust
  pub struct HttpPushSink {
      client: ureq::Agent,
      url: String,
      content_type: String,
      batch: Vec<u8>,
      batch_size: usize,
  }
  ```
  - `new(url: &str, content_type: &str, batch_size: usize) -> Result<Self, SondaError>`.
  - `write()` appends to batch buffer. When batch reaches `batch_size`, auto-flush.
  - `flush()` POSTs batch contents to URL. Clears batch.
  - Response handling: 2xx → Ok, 4xx → log + continue, 5xx → retry once then error.

**Files to modify:**
- `sonda-core/Cargo.toml` — add `ureq = { version = "2", features = ["tls"] }`.
  - Verify: `ureq` with `rustls` (default TLS) stays musl-compatible. **Do not use `native-tls`.**
- `sonda-core/src/sink/mod.rs`:
  - Add `pub mod http`.
  - Add variant: `HttpPush { url: String, content_type: Option<String>, batch_size: Option<usize> }`.
  - Wire into `create_sink()`. Default batch_size: 64KB.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/sink/http.rs` | new |
| `sonda-core/src/sink/mod.rs` | modified |
| `sonda-core/Cargo.toml` | modified |

### Test criteria
- Mock HTTP server (TcpListener accepting one request): send batch → verify body matches.
- Batch accumulation: 3 writes of 100 bytes each with batch_size=1000 → no HTTP call yet. 10 writes → flush triggered.
- Flush on explicit call: remaining data sent.
- Connection refused → `SondaError::Sink(...)`.

### Review criteria
- Uses `ureq` (sync HTTP, no async, pure Rust TLS).
- Batch buffer is pre-allocated.
- Content-Type header set from encoder.
- Binary stays musl-compatible (no native-tls, no openssl).

### UAT criteria
- `sonda metrics --name up --rate 100 --duration 5s` with http_push sink to a local HTTP endpoint → data received.
- Test with VictoriaMetrics if available: data queryable after ingest.

---

## Slice 1.6 — Kafka Sink

### Input state
- Slice 1.5 passes all gates.

### Specification

**Files to create:**
- `sonda-core/src/sink/kafka.rs`:
  ```rust
  pub struct KafkaSink {
      topic: String,
      client: rskafka::client::partition::PartitionClient,
      buffer: Vec<u8>,
  }
  ```
  - `KafkaSink::new(brokers: &str, topic: &str) -> Result<Self, SondaError>` — connects to the
    broker(s) synchronously on construction. `brokers` is a comma-separated list of
    `host:port` pairs.
  - `write()` appends data to an internal buffer. When the buffer exceeds a reasonable threshold
    (64 KB), auto-flush.
  - `flush()` publishes the buffered bytes as a single Kafka record to the configured topic and
    clears the buffer. Returns `SondaError::Sink(...)` on delivery failure.
  - Uses `rskafka` (pure Rust, no C dependencies, musl-compatible). Do not use `rdkafka` (C
    bindings).

**Files to modify:**
- `sonda-core/Cargo.toml` — add `rskafka` as a workspace dependency.
- `sonda-core/src/sink/mod.rs`:
  - Add `pub mod kafka`.
  - Add variant to `SinkConfig`:
    ```rust
    #[serde(rename = "kafka")]
    Kafka { brokers: String, topic: String },
    ```
  - Wire into `create_sink()`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/sink/kafka.rs` | new |
| `sonda-core/src/sink/mod.rs` | modified |
| `sonda-core/Cargo.toml` | modified |

### Test criteria
- Integration tests use a docker-compose Kafka broker (Redpanda or plain Kafka).
- Write data via `KafkaSink` → consume from the topic via `rskafka` consumer → verify exact bytes
  match what was written.
- Multiple `write()` calls followed by `flush()` → all data arrives in a single record.
- Auto-flush at 64 KB threshold → record delivered before explicit `flush()` call.
- Connection to unreachable broker → `SondaError::Sink(...)` with broker address in message.
- Invalid topic name → `SondaError::Sink(...)`.

### Review criteria
- Uses `rskafka` (pure Rust). No `rdkafka` or any crate with C bindings.
- Confirmed musl-compatible: no C FFI in the transitive dependency tree.
- Buffer pre-allocated; no per-`write()` heap allocation beyond buffer growth.
- `flush()` is idempotent when the buffer is empty.
- Error messages include the broker address and topic name.

### UAT criteria
- `docker-compose up -d kafka` (or Redpanda) → `sonda metrics --name up --rate 100 --duration 5s`
  with kafka sink pointing at the local broker and topic `sonda-test`.
- After run: consume from `sonda-test` topic with `kcat` or `rpk` → data matches Sonda output.
- Verify no C library warnings or linker errors on musl build.

---

## Slice 1.7 — Encoder × Sink Matrix Validation

### Input state
- Slices 1.1–1.6 pass all gates.

### Specification

**Files to create:**
- `examples/influx-file.yaml` — Influx LP encoder → file sink.
- `examples/json-tcp.yaml` — JSON Lines encoder → TCP sink.
- `examples/prometheus-http-push.yaml` — Prometheus text → HTTP push sink.

**Files to modify:**
- `README.md` — add encoder and sink reference tables, new examples.

### Output files
| File | Status |
|------|--------|
| `examples/influx-file.yaml` | new |
| `examples/json-tcp.yaml` | new |
| `examples/prometheus-http-push.yaml` | new |
| `README.md` | modified |

### Test criteria
- All 18 combinations (3 encoders × 6 sinks) compile and produce output.
- Integration test that programmatically tests each combination with MemorySink.

### Review criteria
- Example YAMLs are realistic and documented.
- README encoder/sink tables are complete.
- No untested combinations.

### UAT criteria
- Run each example YAML → verify output at destination.
- `sonda metrics --help` shows all encoder and sink options.

---

## Dependency Graph

```
Slice 1.1 (influx encoder)  ──┐
Slice 1.2 (json encoder)    ──┤  (parallel)
                               ↓
Slice 1.3 (file sink)       ──┐
Slice 1.4 (tcp/udp sinks)   ──┤  (parallel, after at least one encoder)
Slice 1.5 (http sink)       ──┤
                               ↓
Slice 1.6 (kafka sink)
                               ↓
Slice 1.7 (matrix validation)
```