# Phase 7 — Alert Triage Implementation Plan

**Goal:** Enable full-fidelity alert testing workflows with Prometheus remote write (protobuf),
CSV-based metric replay from production data, pre-built Grafana dashboards, and correlated
multi-metric scenarios.

**Prerequisite:** Phase 6 complete — documentation is accurate, sequence generator exists,
VictoriaMetrics integration is documented, scrape endpoint works, and the alert testing guide
is published.

**Final exit criteria:** Sonda can push metrics via Prometheus remote write protobuf (enabling
vmagent relay and Cortex/Thanos/Mimir ingestion), replay real production metric patterns from CSV
files, provide Grafana dashboards for visual validation, and run correlated multi-metric scenarios
for testing compound alert rules.

**Design principle — production fidelity:** Alert testing is only useful if the generated data
behaves like production data. This phase adds the tools needed to reproduce real-world metric
patterns and validate them through the same pipeline that production data flows through.

---

## Slice 7.0 — Prometheus Remote Write (Protobuf) Encoder

> **Implementation note:** The actual implementation diverged from the spec below. Instead of
> using the `http_push` sink with custom headers, a dedicated `remote_write` sink was created
> (`sonda-core/src/sink/remote_write.rs`). The `remote_write` encoder produces length-prefixed
> protobuf `TimeSeries` messages, and the `remote_write` sink batches them into a single
> `WriteRequest`, prost-encodes, snappy-compresses, and HTTP POSTs with the correct protocol
> headers automatically. This two-stage design solves the batching corruption problem:
> individually snappy-compressed protobuf chunks cannot be concatenated. By deferring
> compression to flush time, each HTTP POST contains exactly one valid snappy-compressed
> `WriteRequest`. The YAML config uses `encoder: {type: remote_write}` and
> `sink: {type: remote_write, url: "...", batch_size: 100}`.

### Motivation

The Prometheus remote write protocol is the standard for pushing metrics to TSDB backends:
Prometheus itself, Thanos, Cortex, Mimir, VictoriaMetrics (via vmagent), Grafana Cloud, and
others. Currently Sonda can push Prometheus text exposition format via `http_push`, which works
for direct VM ingestion but not for vmagent relay or any remote-write-native receiver. Adding
protobuf remote write unlocks the entire Prometheus-compatible ecosystem.

### Input state
- Phase 6 passes all gates.
- `sonda-core/src/encoder/mod.rs` exists with `Encoder` trait and `EncoderConfig` enum.
- `sonda-core/src/sink/http.rs` exists with `HttpPushSink` that POSTs arbitrary bytes.
- Existing encoders: prometheus, influx, json, syslog.

### Specification

**Dependencies to add:**

- `sonda-core/Cargo.toml`:
  - Add `prost = "0.13"` and `prost-types = "0.13"` behind a feature flag:
    ```toml
    [features]
    default = []
    remote-write = ["prost", "prost-types", "snap"]

    [dependencies]
    prost = { version = "0.13", optional = true }
    prost-types = { version = "0.13", optional = true }
    snap = { version = "0.1", optional = true }
    ```
  - The `snap` crate provides Snappy compression (required by the remote write spec).
  - The proto definitions are hand-written as Rust structs with `prost` derive macros, not
    compiled from `.proto` files. This avoids a `protoc` build dependency and keeps the
    build pure-Rust. The remote write wire format is simple enough (WriteRequest, TimeSeries,
    Sample, Label) that hand-written structs are maintainable.

**Files to create:**

1. `sonda-core/src/encoder/remote_write.rs`:
   ```rust
   /// Prometheus remote write protobuf encoder.
   ///
   /// Encodes MetricEvents into the Prometheus remote write wire format:
   /// WriteRequest -> TimeSeries -> (Labels + Samples). The output is
   /// Snappy-compressed protobuf, ready for POSTing to any remote write
   /// endpoint.
   ///
   /// Requires the `remote-write` feature flag.
   #[cfg(feature = "remote-write")]
   pub struct RemoteWriteEncoder;

   /// Prometheus remote write protobuf types.
   ///
   /// Hand-written prost structs matching the prometheus remote write proto:
   /// https://github.com/prometheus/prometheus/blob/main/prompb/remote.proto
   #[derive(Clone, PartialEq, prost::Message)]
   pub struct WriteRequest {
       #[prost(message, repeated, tag = "1")]
       pub timeseries: Vec<TimeSeries>,
   }

   #[derive(Clone, PartialEq, prost::Message)]
   pub struct TimeSeries {
       #[prost(message, repeated, tag = "1")]
       pub labels: Vec<Label>,
       #[prost(message, repeated, tag = "2")]
       pub samples: Vec<Sample>,
   }

   #[derive(Clone, PartialEq, prost::Message)]
   pub struct Label {
       #[prost(string, tag = "1")]
       pub name: String,
       #[prost(string, tag = "2")]
       pub value: String,
   }

   #[derive(Clone, PartialEq, prost::Message)]
   pub struct Sample {
       #[prost(double, tag = "1")]
       pub value: f64,
       #[prost(int64, tag = "2")]
       pub timestamp: i64,
   }
   ```

   The `Encoder` implementation:
   - `encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>)`:
     - Builds a `WriteRequest` containing one `TimeSeries` per event.
     - The `__name__` label is set to `event.name`.
     - All `event.labels` are converted to `Label` entries, sorted by name.
     - One `Sample` with `event.value` and `event.timestamp_ms`.
     - Serializes with `prost::Message::encode`.
     - Compresses the serialized bytes with Snappy (`snap::raw::Encoder`).
     - Writes the compressed bytes to `buf`.

2. `sonda-core/src/encoder/remote_write_types.rs` (optional, if types are large enough to
   warrant a separate file — otherwise inline in `remote_write.rs`).

**Files to modify:**

3. `sonda-core/src/encoder/mod.rs`:
   - Add `#[cfg(feature = "remote-write")] pub mod remote_write;`
   - Add `#[cfg(feature = "remote-write")] RemoteWrite` variant to `EncoderConfig`.
   - Add match arm in `create_encoder()` for the new variant.

4. `sonda-core/src/sink/http.rs`:
   - Add support for custom headers per request. The `HttpPushSink` currently sets a fixed
     Content-Type. For remote write, the required headers are:
     - `Content-Type: application/x-protobuf`
     - `Content-Encoding: snappy`
     - `X-Prometheus-Remote-Write-Version: 0.1.0`
   - Extend `SinkConfig::HttpPush` with an optional `headers: HashMap<String, String>` field.
   - When `headers` is provided, merge them with the default Content-Type.
   - The remote write encoder should set these headers automatically via a convention or
     the sink config.

5. `sonda-core/CLAUDE.md`:
   - Add `remote_write.rs` to the encoder section (with feature flag note).

6. `README.md`:
   - Add `remote_write` to the encoder types table (noting it requires the `remote-write`
     feature flag).
   - Add a note about building with the feature: `cargo build --features remote-write`.
   - Update the VictoriaMetrics section to mention vmagent relay now works with remote write.

7. `sonda/Cargo.toml`:
   - Add `remote-write` as a passthrough feature:
     ```toml
     [features]
     remote-write = ["sonda-core/remote-write"]
     ```

8. `sonda-server/Cargo.toml`:
   - Add `remote-write` as a passthrough feature:
     ```toml
     [features]
     remote-write = ["sonda-core/remote-write"]
     ```

**Files to create (example):**

9. `examples/remote-write-vm.yaml`:
   ```yaml
   name: cpu_usage_rw
   rate: 10
   duration: 60s

   generator:
     type: sine
     amplitude: 50
     period_secs: 60
     offset: 50

   labels:
     instance: server-01
     job: sonda

   encoder:
     type: remote_write

   sink:
     type: http_push
     url: "http://localhost:8428/api/v1/write"
     headers:
       Content-Type: "application/x-protobuf"
       Content-Encoding: "snappy"
       X-Prometheus-Remote-Write-Version: "0.1.0"
   ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/encoder/remote_write.rs` | new |
| `sonda-core/src/encoder/mod.rs` | modified |
| `sonda-core/src/sink/http.rs` | modified |
| `sonda-core/Cargo.toml` | modified |
| `sonda/Cargo.toml` | modified |
| `sonda-server/Cargo.toml` | modified |
| `sonda-core/CLAUDE.md` | modified |
| `README.md` | modified |
| `examples/remote-write-vm.yaml` | new |

### Test criteria
- `RemoteWriteEncoder` produces valid Snappy-compressed protobuf that can be deserialized
  back into a `WriteRequest`.
- The `__name__` label is correctly set to the metric name.
- Labels are sorted alphabetically by name.
- Timestamp is in milliseconds.
- `HttpPushSink` sends the correct Content-Type, Content-Encoding, and remote write version
  headers when configured.
- `cargo build -p sonda-core` compiles without `remote-write` feature (no dependency on prost).
- `cargo build -p sonda-core --features remote-write` compiles with the feature enabled.
- `EncoderConfig::RemoteWrite` deserializes correctly from YAML (when feature is enabled).
- All existing tests continue to pass with and without the feature flag.

### Review criteria
- Feature flag correctly gates all remote-write code (no unconditional prost dependency).
- Proto types are hand-written with prost derive macros, not compiled from .proto files.
- Snappy compression uses the raw (block) format, not the framed (streaming) format.
- The encoder does not allocate intermediate buffers beyond what prost requires.
- Custom headers on `HttpPushSink` do not break existing non-remote-write usage.
- No `protoc` or `prost-build` in the build pipeline.
- All new public items have doc comments.

### UAT criteria
- Build with `cargo build --features remote-write -p sonda`.
- Run the remote-write example against VictoriaMetrics:
  `sonda metrics --scenario examples/remote-write-vm.yaml --features remote-write`.
- Query VictoriaMetrics and verify the metric appears:
  `curl "http://localhost:8428/api/v1/series?match[]={__name__='cpu_usage_rw'}"`.
- Run the example against vmagent and verify vmagent relays to VictoriaMetrics.
- Build without the feature flag — binary works, `remote_write` encoder is not available.

---

## Slice 7.1 — CSV/File Replay Generator for Metrics

### Motivation

The sequence generator (Slice 6.1) handles short explicit patterns. For longer patterns — hours
of production data with hundreds or thousands of data points — manually typing values in YAML is
not practical. The CSV replay generator reads metric values from a file, enabling users to record
a production metric's values (via VM/Prometheus export or custom tooling) and replay them through
Sonda to reproduce exact production conditions.

### Input state
- Slice 7.0 passes all gates.
- `sonda-core/src/generator/mod.rs` exists with `ValueGenerator` trait, `GeneratorConfig` enum,
  and `create_generator` factory function.
- Existing generators: constant, uniform, sine, sawtooth, sequence.

### Specification

**Files to create:**

1. `sonda-core/src/generator/csv_replay.rs`:
   ```rust
   /// A value generator that replays numeric values from a CSV file.
   ///
   /// Reads a column of numeric values from a CSV file at construction time.
   /// When `repeat` is true (default), cycles through the values.
   /// When `repeat` is false, returns the last value for ticks beyond the file length.
   ///
   /// This enables recording real production metric values (via Prometheus/VM
   /// export) and replaying them through Sonda to reproduce exact conditions.
   pub struct CsvReplayGenerator {
       values: Vec<f64>,
       repeat: bool,
   }

   impl CsvReplayGenerator {
       /// Create a new CSV replay generator.
       ///
       /// Reads the specified column from the CSV file. Each row's value in that
       /// column is parsed as f64. Rows with unparseable values are skipped with
       /// a warning.
       ///
       /// # Arguments
       /// * `path` - Path to the CSV file.
       /// * `column` - Zero-based column index to read (default 0).
       /// * `repeat` - Whether to cycle values (default true).
       ///
       /// Header rows are auto-detected: if any non-time field on the first
       /// data line is non-numeric, the line is treated as a header and skipped.
       ///
       /// # Errors
       /// Returns `SondaError::Config` if:
       /// - The file cannot be opened or read.
       /// - No valid numeric values are found in the specified column.
       /// - The column index is out of bounds for every row.
       pub fn new(
           path: &str,
           column: usize,
           repeat: bool,
       ) -> Result<Self, SondaError> { ... }
   }

   impl ValueGenerator for CsvReplayGenerator {
       fn value(&self, tick: u64) -> f64 {
           // Same logic as SequenceGenerator: wrap or clamp based on repeat
       }
   }
   ```

**Files to modify:**

2. `sonda-core/src/generator/mod.rs`:
   - Add `pub mod csv_replay;`
   - Add `GeneratorConfig::CsvReplay { file: String, column: Option<usize>, columns: Option<Vec<CsvColumnSpec>>, repeat: Option<bool> }` variant.
   - Add match arm in `create_generator()`.
   - Re-export `CsvReplayGenerator`.

3. `sonda-core/CLAUDE.md`:
   - Add `csv_replay.rs` to the generator section.

4. `README.md`:
   - Add `csv_replay` to the "Metric generator types" table.
   - Reference the example file.

**Files to create (examples):**

5. `examples/sample-cpu-values.csv`:
   - A sample CSV with ~50 rows of realistic CPU usage values (simulating a production
     incident: normal → spike → recovery).
   - Header row: `timestamp,cpu_percent`
   - Values that tell a story: 15-20% baseline, spike to 85-95%, recovery back to 20%.

6. `examples/csv-replay-metrics.yaml`:
   ```yaml
   name: cpu_replay
   rate: 1
   duration: 60s

   generator:
     type: csv_replay
     file: examples/sample-cpu-values.csv
     columns:
       - index: 1
         name: cpu_replay

   labels:
     instance: prod-server-42
     job: node

   encoder:
     type: prometheus_text
   sink:
     type: stdout
   ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/generator/csv_replay.rs` | new |
| `sonda-core/src/generator/mod.rs` | modified |
| `sonda-core/CLAUDE.md` | modified |
| `README.md` | modified |
| `examples/sample-cpu-values.csv` | new |
| `examples/csv-replay-metrics.yaml` | new |

### Test criteria
- `CsvReplayGenerator::new` with a valid CSV file produces a generator with the correct number
  of values.
- `CsvReplayGenerator::new` with a non-existent file returns a config error.
- `CsvReplayGenerator::new` with a CSV where the column index is out of bounds returns a config
  error.
- `CsvReplayGenerator::new` with a CSV containing no parseable numbers returns a config error.
- With `repeat: true`, `value(tick)` wraps around at the end of the file.
- With `repeat: false`, `value(tick)` clamps to the last value.
- Header rows are auto-detected and correctly skipped.
- `GeneratorConfig::CsvReplay` deserializes correctly from YAML.
- The example YAML file loads and runs successfully.
- All existing tests continue to pass.

### Review criteria
- The entire CSV file is read into memory at construction time (not per-tick I/O).
- Unparseable rows are skipped with a warning, not a hard error (resilient to messy CSVs).
- No external CSV parsing crate dependency — use simple line splitting (CSV values are simple
  numbers, not quoted strings with commas). If a CSV parsing crate is used, it must be
  lightweight (e.g., `csv` crate is acceptable but should be optional).
- `CsvReplayGenerator` is `Send + Sync`.
- The `value()` method has no allocations (same pattern as SequenceGenerator).
- File path in the config is relative to the working directory, not the YAML file location.

### UAT criteria
- Export 100 CPU usage values from VictoriaMetrics to a CSV file.
- Create a scenario YAML pointing to that CSV.
- Run `sonda metrics --scenario` and verify the output values match the CSV.
- Values cycle correctly when the scenario runs longer than the CSV.

---

## Slice 7.2 — Pre-built Grafana Dashboards & Recording Rule Example

### Motivation

Seeing generated metrics in Grafana is the "aha moment" for most users. Currently users must
manually create dashboards. Pre-built dashboards that auto-provision when using the docker-compose
stack eliminate this friction and demonstrate Sonda's value immediately.

### Input state
- Slice 7.1 passes all gates.
- `examples/docker-compose-victoriametrics.yml` exists with Grafana service (from Slice 6.2).
- `tests/e2e/grafana/` exists with datasource provisioning.

### Specification

**Files to create:**

1. `docker/grafana/dashboards/sonda-overview.json`:
   - Grafana dashboard JSON model with:
     - **Panel 1: "Generated Metric Values"** — Time series graph showing all Sonda-generated
       metrics over time. Uses a `{job="sonda"}` selector.
     - **Panel 2: "Event Rate"** — Graph showing events per second (derived from rate of
       sample timestamps).
     - **Panel 3: "Active Scenarios"** — Stat panel showing count of distinct metric names.
     - **Panel 4: "Gap/Burst Indicators"** — Annotation-style indicators showing when gaps
       and bursts occur (derived from rate changes).
   - Dashboard title: "Sonda Overview".
   - Uses `-- Grafana --` as the datasource variable with VictoriaMetrics as default.
   - Compatible with both Prometheus and VictoriaMetrics datasources (uses PromQL).
   - Time range default: last 15 minutes with 10-second auto-refresh.

2. `docker/grafana/provisioning/dashboards.yml`:
   ```yaml
   apiVersion: 1
   providers:
     - name: "sonda"
       orgId: 1
       folder: "Sonda"
       type: file
       disableDeletion: false
       updateIntervalSeconds: 30
       options:
         path: /var/lib/grafana/dashboards
         foldersFromFilesStructure: false
   ```

3. `examples/recording-rule-test.yaml`:
   - Scenario config that pushes known values for testing a recording rule.
   - Constant generator with `value: 100` and name `http_requests_total`.
   - Includes labels: `method: GET`, `status: 200`, `job: api`.
   - Rate: 1 event/sec, duration: 120s.
   - Push to VictoriaMetrics via http_push.

4. `examples/recording-rule-prometheus.yml`:
   - Prometheus config snippet showing a recording rule that depends on the metric above:
     ```yaml
     groups:
       - name: sonda-test-rules
         rules:
           - record: job:http_requests_total:rate5m
             expr: sum(rate(http_requests_total[5m])) by (job)
     ```
   - Include comments explaining how to verify: push known values, wait, query the recording
     rule, check the result.

**Files to modify:**

5. `examples/docker-compose-victoriametrics.yml`:
   - Add volume mount for dashboard provisioning:
     `./docker/grafana/provisioning/dashboards.yml:/etc/grafana/provisioning/dashboards/dashboards.yml`
   - Add volume mount for dashboard JSON:
     `./docker/grafana/dashboards:/var/lib/grafana/dashboards`
   - Ensure Grafana service depends on VictoriaMetrics being healthy.

6. `README.md`:
   - Add "Pre-built Grafana Dashboards" subsection to the Docker Deployment section.
   - Mention that dashboards auto-provision when using the docker-compose stack.
   - Add the recording rule test example to the Example Scenarios section.

### Output files
| File | Status |
|------|--------|
| `docker/grafana/dashboards/sonda-overview.json` | new |
| `docker/grafana/provisioning/dashboards.yml` | new |
| `examples/recording-rule-test.yaml` | new |
| `examples/recording-rule-prometheus.yml` | new |
| `examples/docker-compose-victoriametrics.yml` | modified |
| `README.md` | modified |

### Test criteria
- `docker/grafana/dashboards/sonda-overview.json` is valid JSON.
- `docker/grafana/provisioning/dashboards.yml` is valid YAML.
- `examples/recording-rule-test.yaml` is valid YAML that deserializes into a `ScenarioConfig`.
- `examples/recording-rule-prometheus.yml` is valid Prometheus config YAML.
- Dashboard JSON contains all four specified panels.
- The compose file correctly mounts dashboard provisioning directories.
- All existing tests continue to pass.

### Review criteria
- Dashboard uses PromQL compatible with both Prometheus and VictoriaMetrics.
- Dashboard uses a datasource variable, not a hardcoded datasource name.
- Dashboard panels use appropriate visualization types (time series, stat, etc.).
- Dashboard has a sensible default time range and refresh interval.
- Recording rule example is mathematically sound (expected output can be calculated).
- No hardcoded IP addresses or hostnames in the dashboard (use service names).
- Volume mounts use relative paths from the compose file location.

### UAT criteria
- Start the compose stack: `docker compose -f examples/docker-compose-victoriametrics.yml up -d`.
- Open Grafana at http://localhost:3000.
- Navigate to Dashboards > Sonda > "Sonda Overview" — dashboard is auto-provisioned.
- POST a metrics scenario to sonda-server.
- After 30 seconds, the dashboard shows live metric values.
- The recording rule example produces expected output when Prometheus evaluates it.

---

## Slice 7.3 — Multi-Metric Correlation

### Motivation

Real alerts often depend on multiple metrics: "CPU > 90% AND memory > 85% for 5 minutes". Testing
these compound conditions requires generating correlated metrics with precise timing relationships.
Currently each Sonda scenario runs independently with no shared timing control. This slice adds
phase offsets and clock groups that enable controlled temporal correlation between scenarios.

### Input state
- Slice 7.2 passes all gates.
- `sonda-core/src/schedule/multi_runner.rs` exists with `run_multi` function.
- `sonda-core/src/config/mod.rs` exists with `MultiScenarioConfig` and `ScenarioEntry`.
- `ScenarioHandle` provides lifecycle management per scenario.

### Specification

**Files to modify in sonda-core:**

1. `sonda-core/src/config/mod.rs`:
   - Add optional fields to `ScenarioEntry`:
     ```rust
     /// Delay before starting this scenario, relative to the group start time.
     /// Enables temporal correlation: "metric A starts immediately, metric B
     /// starts 30s later".
     #[serde(default)]
     pub phase_offset: Option<Duration>,

     /// Clock group identifier. Scenarios in the same clock group share a
     /// common start time reference. When one scenario enters a burst or gap,
     /// other scenarios in the same group can be aware of the timing.
     #[serde(default)]
     pub clock_group: Option<String>,
     ```
   - `phase_offset` is a Duration string (e.g., `"30s"`, `"1m"`) parsed by serde.
   - `clock_group` is an opaque string identifier — scenarios with the same value are grouped.

2. `sonda-core/src/schedule/multi_runner.rs`:
   - Modify `run_multi` to respect `phase_offset`:
     - All scenarios are launched at the same wall-clock time (group start).
     - A scenario with `phase_offset: 30s` sleeps for 30 seconds before entering its event
       loop.
     - The sleep happens inside the spawned thread, before the runner begins.
   - Modify `run_multi` to support `clock_group`:
     - Create a shared `Arc<AtomicU64>` tick counter per clock group.
     - All scenarios in a group increment the same shared counter.
     - This does NOT change the `ValueGenerator` interface — generators still receive
       `tick: u64` from their own local counter. The shared counter is used to coordinate
       timing decisions (e.g., "are we in the same phase of the pattern?").
     - For MVP, the clock group provides a shared start time reference only. Advanced
       cross-scenario signaling (e.g., "trigger burst when partner scenario enters gap")
       is deferred to a future phase.

3. `sonda-core/src/schedule/launch.rs`:
   - Modify `launch_scenario` to accept an optional `phase_offset: Option<Duration>`.
   - When provided, the spawned thread sleeps for the offset duration before calling the
     runner function.
   - The `started_at` field on `ScenarioHandle` reflects the actual start time (after the
     offset), not the spawn time.

**Files to modify (docs):**

4. `sonda-core/CLAUDE.md`:
   - Update config and multi_runner descriptions to mention phase_offset and clock_group.

5. `README.md`:
   - Add `phase_offset` and `clock_group` to the `sonda run` documentation.
   - Add the multi-metric correlation example to the Example Scenarios section.

**Files to create (example):**

6. `examples/multi-metric-correlation.yaml`:
   ```yaml
   scenarios:
     - signal_type: metrics
       name: cpu_usage
       rate: 1
       duration: 120s
       phase_offset: "0s"
       clock_group: alert-test
       generator:
         type: sequence
         values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
         repeat: true
       labels:
         instance: server-01
         job: node
       encoder:
         type: prometheus_text
       sink:
         type: stdout

     - signal_type: metrics
       name: memory_usage_percent
       rate: 1
       duration: 120s
       phase_offset: "3s"
       clock_group: alert-test
       generator:
         type: sequence
         values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
         repeat: true
       labels:
         instance: server-01
         job: node
       encoder:
         type: prometheus_text
       sink:
         type: stdout
   ```

   This models: CPU spikes to 95% at t=0, memory follows 3 seconds later to 88%. An alert
   rule `cpu > 90 AND memory > 85` would fire at t=3s when both conditions overlap.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/config/mod.rs` | modified |
| `sonda-core/src/schedule/multi_runner.rs` | modified |
| `sonda-core/src/schedule/launch.rs` | modified |
| `sonda-core/CLAUDE.md` | modified |
| `README.md` | modified |
| `examples/multi-metric-correlation.yaml` | new |

### Test criteria
- A scenario with `phase_offset: 5s` does not emit events for the first 5 seconds.
- A scenario with `phase_offset: 0s` emits events immediately.
- Two scenarios in the same `clock_group` start from a common reference time.
- `phase_offset` deserializes correctly from YAML duration strings.
- `clock_group` is optional — existing multi-scenario configs without it continue to work.
- The example YAML file loads and runs both scenarios concurrently.
- CPU scenario starts emitting before memory scenario (3-second offset).
- All existing tests continue to pass (backward compatibility).

### Review criteria
- `phase_offset` is a sleep in the spawned thread, not a delay before spawning.
- The sleep does not block the caller of `launch_scenario` or `run_multi`.
- `clock_group` implementation is minimal for MVP (shared start reference only, not
  cross-scenario signaling).
- No changes to the `ValueGenerator` trait — tick values remain per-scenario.
- Backward compatibility: `phase_offset: None` and `clock_group: None` preserve current behavior.
- Duration parsing handles all standard formats (`"5s"`, `"1m30s"`, `"500ms"`).
- The design is extensible for future cross-scenario coordination without breaking changes.

### UAT criteria
- Run the multi-metric correlation example and observe that CPU emits before memory.
- Pipe both to VictoriaMetrics and query to verify the temporal offset.
- Run an existing multi-scenario YAML (without phase_offset) and confirm it still works.
- Time the actual offset between first events of each scenario — should be approximately
  3 seconds (within 500ms tolerance for thread scheduling).

---

## Dependency Graph

```
Slice 7.0 (Prometheus remote write protobuf encoder)
  |
Slice 7.1 (CSV/file replay generator)
  |
Slice 7.2 (pre-built Grafana dashboards + recording rule example)
  |
Slice 7.3 (multi-metric correlation with phase_offset + clock_group)
```

Slice 7.0 unlocks vmagent relay and the broader Prometheus ecosystem. Slice 7.1 adds production
data replay. Slice 7.2 adds visualization tooling. Slice 7.3 adds the final piece for compound
alert testing. Each slice is independently valuable but together they form a complete alert
triage toolkit.

---

## Post-Phase 7

With Phase 7 complete, Sonda provides a full-fidelity alert testing pipeline: generate
production-realistic metrics via sequence or CSV replay, push them through Prometheus remote
write to any TSDB backend, visualize in Grafana, and validate compound alert rules with
correlated multi-metric scenarios. Future alert triage improvements (not designed here):

- **Cross-scenario signaling** — a scenario can trigger a burst or value change in another
  scenario based on shared state (e.g., "when CPU > 90, trigger memory pressure").
- **Alert state verification** — query Prometheus/Alertmanager/vmalert API to automatically
  verify alert state matches expectations.
- **Scenario templates** — parameterized YAML templates with variable substitution for
  reusable alert test patterns.
- **OpenTelemetry Metrics (OTLP)** — OTLP protobuf encoder for pushing to OpenTelemetry
  Collectors and OTLP-native backends.
- **Histogram and summary generators** — generate distribution metrics (histograms with
  bucket boundaries, summaries with quantiles) for testing percentile-based alerts.
- **Trace generation** — correlated trace spans for testing distributed tracing pipelines,
  completing the "metrics, logs, traces" vision from the architecture doc.
