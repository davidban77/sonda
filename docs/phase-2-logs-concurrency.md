# Phase 2 — Logs, Bursts & Concurrency Implementation Plan

**Goal:** Expand signal types to include log generation, add burst window scheduling, and introduce
multi-scenario concurrency via threads and channels.

**Prerequisite:** Phase 1 complete — multiple encoders and sinks are stable, factory pattern proven,
scenario runner handles gaps.

**Final exit criteria:** Sonda runs multiple concurrent scenarios (metrics and logs) with burst windows,
each targeting independent encoder/sink combinations, from a single multi-scenario YAML config.

---

## Slice 2.1 — Log Event Model

### Input state
- Phase 1 passes all gates.

### Specification

**Files to create:**
- `sonda-core/src/model/log.rs`:
  ```rust
  #[derive(Debug, Clone)]
  pub struct LogEvent {
      pub timestamp: SystemTime,
      pub severity: Severity,
      pub message: String,
      pub fields: BTreeMap<String, String>,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
  #[serde(rename_all = "lowercase")]
  pub enum Severity { Trace, Debug, Info, Warn, Error, Fatal }

  impl LogEvent {
      pub fn new(severity: Severity, message: String, fields: BTreeMap<String, String>) -> Self;
      pub fn with_timestamp(...) -> Self;  // for deterministic testing
  }
  ```

**Files to modify:**
- `sonda-core/src/model/mod.rs` — uncomment `pub mod log`.
- `sonda-core/src/lib.rs` — re-export `LogEvent`, `Severity`.
- `sonda-core/src/encoder/mod.rs` — add default `encode_log()` to `Encoder` trait:
  ```rust
  fn encode_log(&self, _event: &LogEvent, _buf: &mut Vec<u8>) -> Result<(), SondaError> {
      Err(SondaError::Encoder("log encoding not supported by this encoder".into()))
  }
  ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/model/log.rs` | new |
| `sonda-core/src/model/mod.rs` | modified |
| `sonda-core/src/lib.rs` | modified |
| `sonda-core/src/encoder/mod.rs` | modified |

### Test criteria
- `LogEvent::new()` creates event with current timestamp.
- `LogEvent::with_timestamp()` uses exact provided timestamp.
- `Severity` serializes to lowercase JSON: `"info"`, `"error"`, etc.
- Existing encoders' default `encode_log()` returns appropriate error.

### Review criteria
- `Severity` has both `Deserialize` and `Serialize`.
- Default trait method for `encode_log()` — does not break existing encoders.
- `BTreeMap` for fields (sorted keys).

### UAT criteria
- N/A (model-only, no binary change).

---

## Slice 2.2 — Log Generators

### Input state
- Slice 2.1 passes all gates.

### Specification

**Files to create:**
- `sonda-core/src/generator/log_template.rs`:
  ```rust
  pub struct LogTemplateGenerator {
      templates: Vec<TemplateEntry>,
      severity_weights: Vec<(Severity, f64)>,
      seed: u64,
  }

  struct TemplateEntry {
      message: String,  // "Request from {ip} to {endpoint}"
      field_pools: HashMap<String, Vec<String>>,  // {ip: ["10.0.0.1", ...]}
  }
  ```
  - `generate(tick: u64) -> LogEvent`:
    - Select template (round-robin or weighted).
    - Resolve `{placeholder}` from field pools using deterministic hash of (seed, tick, field_name).
    - Select severity from weights using deterministic hash.
    - Return `LogEvent` with resolved message and fields.

- `sonda-core/src/generator/log_replay.rs`:
  ```rust
  pub struct LogReplayGenerator {
      lines: Vec<String>,
  }
  ```
  - `LogReplayGenerator::from_file(path: &Path) -> Result<Self, SondaError>`.
  - `generate(tick: u64) -> LogEvent`:
    - `line = lines[tick % lines.len()]`.
    - Return `LogEvent` with severity=Info, message=line, empty fields.

**Files to create:**
- `sonda-core/src/generator/log_mod.rs` (or integrate into existing mod.rs):
  ```rust
  pub trait LogGenerator: Send + Sync {
      fn generate(&self, tick: u64) -> LogEvent;
  }

  #[derive(Debug, Clone, Deserialize)]
  #[serde(tag = "type")]
  pub enum LogGeneratorConfig {
      #[serde(rename = "template")]
      Template { templates: Vec<TemplateConfig>, severity_weights: Option<HashMap<String, f64>>, seed: Option<u64> },
      #[serde(rename = "replay")]
      Replay { file: String },
  }

  pub fn create_log_generator(config: &LogGeneratorConfig) -> Result<Box<dyn LogGenerator>, SondaError>;
  ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/generator/log_template.rs` | new |
| `sonda-core/src/generator/log_replay.rs` | new |
| `sonda-core/src/generator/mod.rs` | modified (add LogGenerator trait + factory) |

### Test criteria
- **Template**: seeded generator → deterministic message and severity for same tick.
- **Template**: placeholder resolution from pool — all resolved values come from the pool.
- **Severity weights**: over 10,000 ticks with seed, info=0.7/warn=0.2/error=0.1 → distribution within 5%.
- **Replay**: 5-line file, tick 0-4 → lines 0-4, tick 5 → wraps to line 0.
- **Replay**: empty file → error at construction time.
- **Factory**: each config variant creates correct generator type.

### Review criteria
- `LogGenerator` trait is `&self` only (stateless, deterministic with seed).
- Template placeholder resolution is hash-based (not stateful RNG).
- Replay reads file once at construction, not per-tick.
- File I/O errors wrapped in `SondaError`.

### UAT criteria
- N/A (no CLI support yet for logs).

---

## Slice 2.3 — Log Encoders

### Input state
- Slice 2.2 passes all gates.

### Specification

**Files to modify:**
- `sonda-core/src/encoder/json.rs` — implement real `encode_log()`:
  ```json
  {"timestamp":"2026-03-20T12:00:00.000Z","severity":"info","message":"Request from 10.0.0.1","fields":{"ip":"10.0.0.1","endpoint":"/api"}}
  ```

**Files to create:**
- `sonda-core/src/encoder/syslog.rs`:
  ```rust
  pub struct Syslog;
  ```
  - RFC 5424 format: `<priority>1 timestamp hostname app-name procid msgid [SD] message\n`
  - Priority: calculated from severity (facility=1 user-level).
  - Hostname and app-name configurable (defaults: "sonda", "sonda").

**Files to modify:**
- `sonda-core/src/encoder/mod.rs`:
  - Add `pub mod syslog`.
  - Add `Syslog` variant to `EncoderConfig`.
  - Wire into `create_encoder()`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/encoder/json.rs` | modified |
| `sonda-core/src/encoder/syslog.rs` | new |
| `sonda-core/src/encoder/mod.rs` | modified |

### Test criteria
- JSON `encode_log()`: valid JSON, all fields present, severity lowercase.
- JSON log roundtrip: encode → parse → verify fields.
- Syslog: valid RFC 5424 format. Priority calculated correctly for each severity level.
- Syslog: message with special characters handled.
- Prometheus `encode_log()`: still returns "not supported" error.

### Review criteria
- JSON uses `serde_json::to_writer()` into buffer.
- Syslog priority math is correct (facility * 8 + severity).
- No new dependencies for syslog (manual formatting).

### UAT criteria
- N/A (no CLI log subcommand yet).

---

## Slice 2.4 — Burst Windows

### Input state
- Slices 2.1-2.3 pass all gates (or can run in parallel).

### Specification

**Files to modify:**
- `sonda-core/src/schedule/mod.rs` — add:
  ```rust
  pub fn is_in_burst(elapsed: Duration, burst: &BurstWindow) -> Option<f64> {
      // Returns Some(multiplier) during burst, None otherwise
  }

  pub fn time_until_burst_end(elapsed: Duration, burst: &BurstWindow) -> Duration
  ```

- `sonda-core/src/schedule/runner.rs` — update main loop:
  - Each tick: check gap first (gap wins over burst), then check burst.
  - During burst: `effective_interval = base_interval / multiplier`.
  - Recalculate sleep dynamically on burst state change.

- `sonda-core/src/config/mod.rs` — add to `ScenarioConfig`:
  ```rust
  #[serde(default)]
  pub bursts: Option<BurstConfig>,
  ```
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct BurstConfig {
      pub every: String,
      pub r#for: String,
      pub multiplier: f64,
  }
  ```

- `sonda-core/src/config/validate.rs` — add burst validation:
  - `multiplier > 0.0`
  - `burst.for < burst.every`

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/schedule/mod.rs` | modified |
| `sonda-core/src/schedule/runner.rs` | modified |
| `sonda-core/src/config/mod.rs` | modified |
| `sonda-core/src/config/validate.rs` | modified |

### Test criteria
- **`is_in_burst`**: at 0s with burst_every=10s, burst_for=2s → None. At 0.5s → Some(multiplier). At 2.5s → None.
- **Gap + burst overlap**: gap at same time as burst → gap wins (no output).
- **Rate during burst**: integration test with MemorySink, rate=100, burst multiplier=5, burst_for=1s → ~500 events in that second.
- **Validation**: multiplier=0 → Err. burst_for > burst_every → Err.

### Review criteria
- Gap always takes priority over burst.
- Rate recalculation is correct and doesn't accumulate drift.
- No busy-waiting during normal (non-burst) periods.

### UAT criteria
- `sonda metrics --name up --rate 100 --duration 10s --burst-every 5s --burst-for 1s --burst-multiplier 10` → visible rate spike in output density.
- Pipe to `wc -l` with timing → burst second has ~10x lines compared to normal second.

---

## Slice 2.5 — CLI Logs Subcommand

### Input state
- Slices 2.1-2.3 pass all gates (or can run in parallel).

### Specification

**Files to modify:**
- `sonda/src/cli.rs`:
  - Add `Logs(LogsArgs)` variant to `Commands` enum.
  - `LogsArgs` struct with: `--scenario`, `--mode {template|replay}`, `--file <path>`, `--rate`, `--duration`, `--encoder`, `--label`, gap/burst flags.

- `sonda/src/config.rs`:
  - Add `pub fn load_log_config(args: &LogsArgs) -> Result<LogScenarioConfig>`.

- `sonda/src/main.rs`:
  - Match `Commands::Logs(args)` → load config → call log runner.

**Files to create:**
- `sonda-core/src/schedule/log_runner.rs`:
  ```rust
  pub fn run_logs(config: &LogScenarioConfig) -> Result<(), SondaError>
  ```
  - Same loop structure as metric runner, but uses `LogGenerator` and `encode_log()`.

- `sonda-core/src/config/mod.rs` — add `LogScenarioConfig`:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct LogScenarioConfig {
      pub name: String,
      pub rate: f64,
      pub duration: Option<String>,
      pub generator: LogGeneratorConfig,
      pub gaps: Option<GapConfig>,
      pub bursts: Option<BurstConfig>,
      pub encoder: EncoderConfig,
      pub sink: SinkConfig,
  }
  ```

**Files to create:**
- `examples/log-template.yaml`
- `examples/log-replay.yaml`

### Output files
| File | Status |
|------|--------|
| `sonda/src/cli.rs` | modified |
| `sonda/src/config.rs` | modified |
| `sonda/src/main.rs` | modified |
| `sonda-core/src/schedule/log_runner.rs` | new |
| `sonda-core/src/config/mod.rs` | modified |
| `examples/log-template.yaml` | new |
| `examples/log-replay.yaml` | new |

### Test criteria
- Config from YAML: log-template.yaml → valid `LogScenarioConfig`.
- Config from flags: `--mode template` with required fields → valid config.
- Log runner integration test: MemorySink, rate=10, duration=1s → ~10 encoded log lines.

### Review criteria
- Log runner shares gap/burst logic with metric runner (no duplication).
- CLI crate remains thin — no log generation logic.
- `--help` for `sonda logs` is complete.

### UAT criteria
- `sonda logs --scenario examples/log-template.yaml` → JSON log lines with varied messages and severities.
- `sonda logs --scenario examples/log-replay.yaml` → replayed lines from file.
- `sonda logs --mode template --rate 10 --duration 3s --encoder json_lines` → ~30 valid JSON log lines.
- Error cases: `--mode replay` without `--file` → clear error.

---

## Slice 2.6 — Loki Sink & Log E2e Tests

### Input state
- Slice 2.5 passes all gates (`LokiSink` needs the `LogEvent` model and the `ureq` dependency
  already present from `HttpPushSink`).

### Specification

**Why a sink rather than an encoder:** Loki's push API wraps multiple log lines into a single JSON
batch envelope (`{"streams": [...]}`). This batching boundary belongs in the sink, not the encoder.
The `JsonLines` encoder continues to handle per-event serialization. `LokiSink` accumulates encoded
lines and POSTs the batch when it is full or flushed.

**Files to create:**
- `sonda-core/src/sink/loki.rs`:
  ```rust
  pub struct LokiSink {
      url: String,
      labels: HashMap<String, String>,
      batch_size: usize,
      batch: Vec<(String, String)>,  // (unix_nano_timestamp, log_line)
  }

  impl LokiSink {
      pub fn new(url: String, labels: HashMap<String, String>, batch_size: usize) -> Self;
      fn flush_batch(&mut self) -> Result<(), SondaError>;  // POSTs current batch
  }

  impl Sink for LokiSink {
      fn write(&mut self, data: &[u8]) -> Result<(), SondaError>;  // accumulates; auto-flushes at batch_size
      fn flush(&mut self) -> Result<(), SondaError>;               // flushes remaining
  }
  ```
  - Each call to `write()` appends one line to the batch. When `batch.len() == batch_size`, calls
    `flush_batch()` automatically.
  - `flush_batch()` builds the Loki JSON envelope and POSTs to `{url}/loki/api/v1/push`.
  - Loki JSON push format:
    ```json
    {
      "streams": [{
        "stream": { "label1": "value1", ... },
        "values": [["<unix_nanoseconds>", "<log_line>"]]
      }]
    }
    ```
  - Timestamps in the batch come from the current wall clock at write time (nanoseconds since Unix
    epoch as a decimal string).
  - HTTP errors from ureq are wrapped in `SondaError::Sink`.
  - Uses the existing `ureq` dependency — no new HTTP client.

**Files to modify:**
- `sonda-core/src/sink/mod.rs`:
  - Add `pub mod loki`.
  - Add `Loki` variant to `SinkConfig`:
    ```rust
    #[serde(rename = "loki")]
    Loki {
        url: String,
        #[serde(default)]
        labels: HashMap<String, String>,
        #[serde(default)]
        batch_size: Option<usize>,
    },
    ```
  - Wire into `create_sink()`: `SinkConfig::Loki { url, labels, batch_size }` →
    `LokiSink::new(url, labels, batch_size.unwrap_or(100))`.

**Files to create (examples):**
- `examples/loki-json-lines.yaml`:
  ```yaml
  name: app_logs_loki
  rate: 10
  duration: 60s
  generator:
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
  encoder:
    type: json_lines
  sink:
    type: loki
    url: http://localhost:3100
    labels:
      job: sonda
      env: dev
    batch_size: 50
  ```

- `examples/kafka-json-logs.yaml`:
  ```yaml
  name: app_logs_kafka
  rate: 10
  duration: 60s
  generator:
    type: template
    templates:
      - message: "Event from {service} severity {level}"
        field_pools:
          service: ["auth", "api", "worker"]
          level: ["INFO", "WARN", "ERROR"]
    severity_weights:
      info: 0.7
      warn: 0.2
      error: 0.1
  encoder:
    type: json_lines
  sink:
    type: kafka
    brokers: ["localhost:9092"]
    topic: sonda-logs
  ```

**Docker-compose additions (`docker-compose.yml` or `docker/docker-compose.yml`):**
- Add `loki` service:
  ```yaml
  loki:
    image: grafana/loki:latest
    ports:
      - "3100:3100"
    command: -config.file=/etc/loki/local-config.yaml
  ```
- Add Loki as a Grafana datasource in the provisioning config (e.g.,
  `docker/grafana/provisioning/datasources/loki.yaml`):
  ```yaml
  apiVersion: 1
  datasources:
    - name: Loki
      type: loki
      access: proxy
      url: http://loki:3100
      isDefault: false
  ```

**Taskfile additions (`Taskfile.yml` or `Makefile`):**
- `run:loki` task: `sonda logs --scenario examples/loki-json-lines.yaml`
- Update `stack:up` (or equivalent) to print Loki URL: `Loki: http://localhost:3100`

**E2e verification:**
- After running `loki-json-lines.yaml`, query Loki to confirm logs arrived:
  ```
  GET http://localhost:3100/loki/api/v1/query_range?query={job="sonda"}&start=<epoch_ns>&end=<epoch_ns>
  ```
  Response should contain log streams with the configured labels.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/sink/loki.rs` | new |
| `sonda-core/src/sink/mod.rs` | modified |
| `examples/loki-json-lines.yaml` | new |
| `examples/kafka-json-logs.yaml` | new |
| `docker-compose.yml` (or equivalent) | modified |
| `docker/grafana/provisioning/datasources/loki.yaml` | new |
| `Taskfile.yml` (or equivalent) | modified |

### Test criteria
- **Loki push format**: `flush_batch()` produces valid JSON matching the Loki push API envelope.
- **Batch accumulation**: write 49 lines → no HTTP call. Write 50th line → HTTP call fires.
- **Auto-flush on drop / explicit flush**: `flush()` sends remaining < batch_size lines.
- **Labels in stream**: configured labels appear in `streams[0].stream` of the POST body.
- **Empty batch flush**: `flush()` with zero buffered lines does nothing (no HTTP call).
- **E2e (manual/integration)**: logs arrive in Loki and are queryable via `query_range`.
- **E2e (manual/integration)**: `kafka-json-logs.yaml` produces log events in the Kafka topic.

### Review criteria
- Uses existing `ureq` dependency — no new HTTP client introduced.
- Loki JSON envelope format matches the Loki push API spec exactly.
- Labels are configurable per scenario; `job` label is not hardcoded.
- `batch_size` defaults to 100 if not specified.
- HTTP 4xx/5xx responses are treated as errors (not silently swallowed).
- Documentation updated: README (if it lists sinks), sonda-core `CLAUDE.md`, `examples/` directory.

### UAT criteria
- `sonda logs --scenario examples/loki-json-lines.yaml` with a running Loki → logs queryable in
  Grafana/Loki UI under the `job="sonda"` label.
- Invalid Loki URL → clear error message (no panic).
- `sonda run --scenario examples/kafka-json-logs.yaml` with Kafka running → messages visible in topic.

---

## Slice 2.7 — Multi-Scenario Concurrency

### Input state
- Slices 2.4, 2.5, and 2.6 pass all gates.

### Specification

**Files to modify:**
- `sonda-core/src/config/mod.rs`:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct MultiScenarioConfig {
      pub scenarios: Vec<ScenarioEntry>,
  }

  #[derive(Debug, Clone, Deserialize)]
  #[serde(tag = "signal_type")]
  pub enum ScenarioEntry {
      #[serde(rename = "metrics")]
      Metrics(ScenarioConfig),
      #[serde(rename = "logs")]
      Logs(LogScenarioConfig),
  }
  ```

**Files to create:**
- `sonda-core/src/schedule/multi_runner.rs`:
  ```rust
  pub fn run_multi(config: MultiScenarioConfig, shutdown: Arc<AtomicBool>) -> Result<(), SondaError>
  ```
  - Spawn one `std::thread` per scenario entry.
  - Each thread runs `run()` or `run_logs()` with a clone of the shutdown flag.
  - Main thread joins all threads.
  - Any thread error → collect and return all errors.

- `sonda-core/src/sink/channel.rs` (optional shared sink):
  ```rust
  pub struct ChannelSink { tx: SyncSender<Vec<u8>> }
  ```
  - For scenarios targeting the same sink: send encoded data via bounded channel.
  - Receiver thread owns the real sink and drains.
  - Bounded capacity provides backpressure.

**Files to modify:**
- `sonda/src/cli.rs` — add `Run(RunArgs)` subcommand: `sonda run --scenario multi.yaml`.
- `sonda/src/main.rs` — wire `Commands::Run`.
- `sonda/src/config.rs` — detect multi-scenario YAML (has `scenarios:` key).

**Files to create:**
- `examples/multi-scenario.yaml`:
  ```yaml
  scenarios:
    - signal_type: metrics
      name: cpu_usage
      rate: 100
      duration: 30s
      generator: { type: sine, amplitude: 50, period_secs: 60, offset: 50 }
      encoder:
        type: prometheus_text
      sink:
        type: stdout
    - signal_type: logs
      name: app_logs
      rate: 10
      duration: 30s
      generator: { type: template, templates: [...] }
      encoder:
        type: json_lines
      sink:
        type: file
        path: /tmp/logs.json
  ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/schedule/multi_runner.rs` | new |
| `sonda-core/src/sink/channel.rs` | new |
| `sonda-core/src/config/mod.rs` | modified |
| `sonda/src/cli.rs` | modified |
| `sonda/src/main.rs` | modified |
| `sonda/src/config.rs` | modified |
| `examples/multi-scenario.yaml` | new |

### Test criteria
- Two scenarios concurrently: both produce output (MemorySink per scenario).
- Shutdown flag: set flag → all threads exit within 2 seconds.
- Thread error: one thread fails → error reported, other thread still runs to completion.
- ChannelSink: bounded(10), write 20 items fast → backpressure (blocks or errors, doesn't OOM).

### Review criteria
- Threads use `std::thread::spawn`, not tokio.
- Shutdown via `Arc<AtomicBool>`, not channels or mutexes.
- No data races (no `unsafe`, no `Mutex` on hot path).
- Errors from all threads collected and reported.

### UAT criteria
- `sonda run --scenario examples/multi-scenario.yaml` → both metric and log output produced concurrently.
- Ctrl+C → all scenarios stop cleanly within 2 seconds.
- 3 concurrent scenarios at 1000/sec each → combined ~3000/sec output.

---

## Dependency Graph

```
Slice 2.1 (log model) → Slice 2.2 (log generators) → Slice 2.3 (log encoders) → Slice 2.5 (CLI logs) → Slice 2.6 (Loki sink)
                                                                                                               ↓
Slice 2.4 (burst windows)  ──────────────────────────────────────────────────────────────────────────→ Slice 2.7 (concurrency)
```

2.1-2.3 (log pipeline) and 2.4 (bursts) are independent parallel tracks.
Slice 2.6 (Loki sink) depends on 2.5 (CLI logs) for `LogEvent` and the existing `ureq` dep.
Slice 2.7 (concurrency) depends on 2.4 (bursts), 2.5 (CLI logs), and 2.6 (Loki sink) all passing.

**Phase 2 contains 7 slices total** (2.1 – 2.7).