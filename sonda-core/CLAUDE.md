# sonda-core — The Engine

This is the library crate. It owns **all** domain logic. If it generates signals, schedules events,
encodes data, or delivers output — it lives here.

## Module Layout

```
src/
├── lib.rs              ← public API surface, re-exports, SondaError + sub-enums
│                          (ConfigError, GeneratorError, EncoderError, RuntimeError)
├── util.rs             ← pub(crate) shared utility functions (splitmix64 deterministic hash)
├── scenarios/
│   └── mod.rs          ← pre-built scenario catalog: BuiltinScenario struct, static CATALOG array,
│                          list(), get(), get_yaml(), list_by_category(), available_names().
│                          YAML files live in sonda-core/scenarios/*.yaml (embedded via include_str!).
├── model/
│   ├── mod.rs          ← module declarations
│   ├── metric.rs       ← ValidatedMetricName (newtype over Arc<str>, validates once at construction),
│   │                      MetricEvent (ValidatedMetricName name, Arc<Labels>), Labels, from_parts().
│   │                      Labels::iter() returns (&str, &str). Labels::new() is #[cfg(test)] pub(crate).
│   └── log.rs          ← LogEvent (with Labels support for scenario-level static labels).
│                          Severity has explicit Ord/PartialOrd (rank-based, not derived from variant order).
├── generator/
│   ├── mod.rs          ← ValueGenerator trait + factory, CsvColumnSpec (multi-column csv_replay),
│   │                      GeneratorConfig enum (core types + 6 operational aliases:
│   │                      flap, saturation, leak, degradation, steady, spike_event).
│   │                      Aliases are pure syntactic sugar — desugared before create_generator().
│   ├── constant.rs
│   ├── uniform.rs
│   ├── sine.rs
│   ├── sawtooth.rs
│   ├── sequence.rs     ← explicit value sequence (incident pattern modeling)
│   ├── step.rs         ← monotonic step counter with optional wrap-around (rate/increase testing)
│   ├── spike.rs        ← baseline with periodic spikes (anomaly/alert testing)
│   ├── jitter.rs       ← JitterWrapper: adds deterministic uniform noise to any ValueGenerator
│   ├── csv_header.rs   ← CSV header parsing for Grafana-style label-aware column headers
│   ├── csv_replay.rs   ← CSV file-based replay for metric values
│   ├── histogram.rs    ← HistogramGenerator (cumulative bucket counts, Distribution, to_distribution)
│   ├── summary.rs      ← SummaryGenerator (quantile values via sorted observations)
│   ├── log_template.rs ← template-based log line generator
│   └── log_replay.rs   ← file-replay log line generator
├── schedule/
│   ├── mod.rs          ← GapWindow, BurstWindow, CardinalitySpikeWindow, is_in_spike,
│   │                      DynamicLabel (always-on rotating label, label_value_for_tick()),
│   │                      ParsedSchedule (parses BaseScheduleConfig into resolved Duration values)
│   ├── core_loop.rs    ← pub(crate) shared schedule loop (run_schedule_loop, TickFn, TickContext,
│   │                      TickResult). Owns all rate control, gap/burst/spike window handling,
│   │                      stats tracking, and shutdown. Signal runners provide a TickFn closure.
│   ├── stats.rs        ← ScenarioStats (live telemetry + recent_metrics buffer for scrape endpoints)
│   ├── handle.rs       ← ScenarioHandle (lifecycle: stop, join, elapsed, stats_snapshot;
│   │                      recovers from poisoned stats lock instead of panicking)
│   ├── launch.rs       ← validate_entry + prepare_entries + launch_scenario (unified launch API,
│   │                      PreparedEntry, shared expand→validate→phase_offset pipeline)
│   ├── runner.rs           ← metric event loop: builds generator/encoder/labels, delegates to core_loop
│   ├── log_runner.rs       ← log event loop: builds log generator/encoder/labels, delegates to core_loop
│   ├── histogram_runner.rs ← histogram event loop: pre-built Arc<Labels> per bucket, delegates to core_loop
│   ├── summary_runner.rs   ← summary event loop: pre-built Arc<Labels> per quantile, delegates to core_loop
│   └── multi_runner.rs     ← concurrent multi-scenario runner (run_multi, respects phase_offset per entry)
├── encoder/
│   ├── mod.rs          ← Encoder trait + factory
│   ├── prometheus.rs   ← Prometheus text exposition format
│   ├── influx.rs       ← Influx Line Protocol (post-MVP)
│   ├── json.rs         ← JSON Lines (post-MVP)
│   ├── otlp.rs         ← OTLP protobuf: hand-written prost structs for metrics + logs,
│   │                      OtlpEncoder (Metric/LogRecord), parser helpers (feature = "otlp")
│   ├── remote_write.rs ← Prometheus remote write protobuf (feature = "remote-write")
│   └── syslog.rs       ← RFC 5424 syslog format (log-only)
├── sink/
│   ├── mod.rs          ← Sink trait + factory
│   ├── stdout.rs       ← BufWriter<Stdout>
│   ├── file.rs         ← BufWriter<File>
│   ├── tcp.rs          ← TCP socket (BufWriter<TcpStream>)
│   ├── udp.rs          ← UDP socket (UdpSocket)
│   ├── http.rs         ← HTTP push sink (ureq, feature = "http")
│   ├── loki.rs         ← Loki log push sink (HTTP, ureq, feature = "http")
│   ├── remote_write.rs ← Prometheus remote write sink (batches TimeSeries, snappy, feature = "remote-write")
│   ├── channel.rs      ← in-memory channel sink (mpsc::Sender<Vec<u8>>, for testing)
│   ├── memory.rs       ← in-memory buffer sink (Vec<Vec<u8>>, for testing and embedding)
│   ├── kafka.rs        ← Kafka producer (rskafka, feature = "kafka")
│   └── otlp_grpc.rs    ← OTLP/gRPC sink: batches Metric/LogRecord, sends via tonic gRPC
│                          unary call to OTEL Collector (feature = "otlp")
└── config/
    ├── mod.rs          ← BaseScheduleConfig (shared schedule/delivery fields: name, rate, duration,
    │                      gaps, bursts, cardinality_spikes, dynamic_labels, labels, sink,
    │                      phase_offset, clock_group, jitter, jitter_seed),
    │                      ScenarioConfig (embeds BaseScheduleConfig + generator + encoder, Deref/DerefMut),
    │                      LogScenarioConfig (embeds BaseScheduleConfig + generator + encoder, Deref/DerefMut),
    │                      HistogramScenarioConfig, SummaryScenarioConfig, DistributionConfig,
    │                      ScenarioEntry (Metrics|Logs|Histogram|Summary, with base() accessor),
    │                      MultiScenarioConfig, CardinalitySpikeConfig, SpikeStrategy,
    │                      DynamicLabelConfig, DynamicLabelStrategy (Counter | ValuesList),
    │                      expand_scenario (csv_replay multi-column fan-out),
    │                      expand_entry (entry-level wrapper for expand_scenario)
    ├── aliases.rs      ← operational vocabulary aliases: desugar_entry, desugar_scenario_config.
    │                      Transforms high-level aliases (flap, steady, leak, saturation,
    │                      degradation, spike_event) into underlying GeneratorConfig variants.
    │                      Jitter-implying aliases (steady, degradation) set jitter on
    │                      BaseScheduleConfig. Integrated into prepare_entries pipeline.
    └── validate.rs     ← config validation logic, parse_duration (accepts fractional seconds via f64),
                           validate_cardinality_spike_config, validate_dynamic_label_config,
                           validate_histogram_config, validate_summary_config,
                           validate_distribution_config (min < max for Uniform)
```

## Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `config` | yes | Enables `serde::Deserialize` impls on all config types and pulls in `serde_yaml_ng` for YAML parsing. Disable for library consumers who construct configs in code and do not need YAML/JSON deserialization. |
| `http` | no | Enables `ureq` and HTTP-based sinks (`HttpPush`, `Loki`). |
| `kafka` | no | Enables `rskafka` + `tokio` + `rustls` + `rustls-pemfile` + `webpki-roots` for the Kafka sink with TLS and SASL support. |
| `remote-write` | no | Enables `prost` + `snap` + `ureq` for the Prometheus remote write encoder and sink. |
| `otlp` | no | Enables `tonic` + `prost` + `tokio` + `bytes` + `http` for the OTLP encoder and gRPC sink. |

When the `config` feature is disabled:
- All config types (`ScenarioConfig`, `EncoderConfig`, `SinkConfig`, `GeneratorConfig`, etc.) remain
  public and constructible in code.
- `Deserialize` impls and `#[serde(...)]` attributes are conditionally compiled out.
- `serde_yaml_ng` is not linked. `serde_json` remains available (used by the JSON encoder).
- Tests that parse YAML are gated behind `#[cfg(feature = "config")]`.

## How to Add a New Generator

1. Create `src/generator/your_name.rs` with a struct that implements `ValueGenerator`.
2. The struct must be `Send + Sync`. Store configuration in the struct fields.
3. Implement `fn value(&self, tick: u64) -> f64`. This is a pure function — no side effects.
4. Register it in `src/generator/mod.rs`:
   - Add a variant to the `GeneratorConfig` enum (serde-tagged).
   - Add a match arm in `create_generator()` that returns `Box::new(YourGenerator::new(...))`.
5. Add unit tests in the same file: test determinism, edge cases, boundary values.
6. Update the YAML config schema doc if it exists.

## How to Add a New Encoder

Same pattern as generators:
1. Create `src/encoder/your_format.rs` implementing the `Encoder` trait.
2. The `encode_metric` method writes into a caller-provided `&mut Vec<u8>`. Never allocate a new
   buffer — reuse what is given.
3. Pre-build any invariant content (label prefix, metric name validation) in `new()`. The encode
   method should do as little work as possible per event.
4. Register in `src/encoder/mod.rs` factory.
5. Test with known inputs → expected byte output. Use `assert_eq!(String::from_utf8(buf).unwrap(), ...)`.

### Encoder Precision

The `PrometheusText`, `InfluxLineProtocol`, and `JsonLines` encoders support an optional
`precision: Option<u8>` field that limits decimal places in formatted metric values. When `None`,
full `f64` precision is preserved (default behavior). When set, values are formatted with the
specified number of decimal places. Use `write_value()` from `encoder/mod.rs` for text encoders;
JSON encoders pre-round the value before passing it to serde. Precision is validated in
`config/validate.rs` (must be 0..=17).

## How to Add a New Sink

1. Create `src/sink/your_sink.rs` implementing the `Sink` trait.
2. `write(&mut self, data: &[u8])` delivers one encoded event. `flush(&mut self)` forces delivery.
3. Sinks own their I/O resources (file handles, sockets, etc.).
4. Register in `src/sink/mod.rs` factory. The `create_sink()` function accepts an optional
   `labels: Option<&HashMap<String, String>>` parameter. Most sinks ignore this (pass `None`).
   The Loki sink uses it for stream labels, sourced from the scenario-level `labels` config.
5. Test with a mock or in-memory buffer sink.

## Performance Guidelines

- **No per-event allocations.** The hot path is: generate value → build MetricEvent → encode into
  buffer → write to sink. Each step should write into pre-allocated or caller-provided memory.
- **Arc-wrapped MetricEvent fields.** `MetricEvent::name` is a `ValidatedMetricName` (newtype
  over `Arc<str>` that validates the metric name regex once at construction) and `MetricEvent::labels`
  is `Arc<Labels>`. Cloning a MetricEvent is O(1) — just reference-count bumps. The metric runner
  constructs a `ValidatedMetricName` once before the loop and uses `MetricEvent::from_parts` (no
  per-tick validation) with `name.clone()` (no per-tick heap allocation). Only when a cardinality
  spike is active does the runner deep-clone the inner Labels to insert the spike key.
- **Zero-alloc timestamp formatting.** `format_rfc3339_millis_array` writes the RFC 3339 timestamp
  into a stack-allocated `[u8; 24]` — no heap allocation. JSON and syslog encoders use this to
  borrow a `&str` without going through `String`. The `format_rfc3339_millis(ts, buf)` variant
  appends directly into the caller's `Vec<u8>` buffer.
- **Zero-allocation JSON label serialization.** The JSON encoder serializes labels and fields
  directly from their source iterators via `LabelsRef` and `StringMapRef` wrappers that implement
  `serde::Serialize`. No intermediate `BTreeMap<&str, &str>` is collected per event. The source
  `Labels` (BTreeMap) and `fields` (BTreeMap) already iterate in sorted order.
- **Single-pass log template resolution.** `LogTemplateGenerator::resolve_template` scans the
  template string once, writing literal segments and resolved `{placeholder}` values directly into
  a pre-allocated output buffer. This replaces the previous N-pass `String::replace` approach that
  allocated a new `String` per placeholder.
- **Pre-build label strings.** Labels don't change between events for a given scenario. Build the
  serialized label prefix once at construction time.
- **Use `BufWriter`.** Never write individual lines to stdout or files without buffering.
- **Benchmark before optimizing.** Use `cargo bench` with criterion if you suspect a bottleneck.
  Don't optimize speculatively.

## Error Handling

- Define errors in `src/lib.rs` using `thiserror`.
- Every public function returns `Result<T, SondaError>`.
- Never `unwrap()` in this crate. Use `?` propagation or explicit error mapping.
- **Structured error sub-enums**: `SondaError` delegates to typed sub-enums that preserve
  original error sources via `#[source]`:
  - `ConfigError` — configuration validation errors. Use `ConfigError::invalid(msg)` to construct.
  - `GeneratorError` — generator I/O errors. `FileRead { path, source: io::Error }` preserves
    the original I/O error for programmatic inspection (e.g., `ErrorKind::NotFound`).
  - `EncoderError` — encoding errors. `SerializationFailed(serde_json::Error)` and
    `TimestampBeforeEpoch(SystemTimeError)` preserve the original error. `NotSupported(String)`
    for unsupported event types. `Other(String)` for feature-gated encoder errors (protobuf, snappy).
  - `RuntimeError` — system/environment errors. `SpawnFailed(#[source] io::Error)` for thread
    spawn failures (preserves the original `io::Error` via `#[source]`), `ThreadPanicked` for
    panicked scenario threads, `ScenariosFailed(String)` for collected errors from multi-scenario
    thread joins. Separated from `ConfigError` so consumers matching on config errors are not
    confused by system failures.
- `SondaError::Sink` wraps `std::io::Error` **without** a blanket `#[from]` conversion.
  All I/O errors must be explicitly mapped to the correct variant at each call site:
  - Sink I/O errors: use `.map_err(SondaError::Sink)` or `SondaError::Sink(io_err)`.
  - Generator file I/O errors: use `SondaError::Generator(GeneratorError::FileRead { path, source })`.
  - Config validation errors: use `SondaError::Config(ConfigError::invalid(msg))`.
  - Runtime errors: use `SondaError::Runtime(RuntimeError::SpawnFailed(io_err))`.
  - Multi-scenario thread failures: use `SondaError::Runtime(RuntimeError::ScenariosFailed(msg))`.
- This prevents generator or config I/O errors from being misclassified as sink errors.

## Testing

- Every generator: test at tick=0, tick=1, tick at period boundary, tick at large values.
- Every encoder: test with known MetricEvent → assert exact byte output.
- Schedule logic: test gap window math, burst window transitions, rate calculation.
- Use `#[cfg(test)] mod tests` at the bottom of each file.
- Seed all RNG-based generators for deterministic tests.
