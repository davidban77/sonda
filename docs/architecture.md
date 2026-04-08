# Sonda — Architecture Design Document

**Status:** Draft · **Version:** 0.1 · **Date:** March 2026

---

## 1. Overview

Sonda is a synthetic telemetry generator written in Rust. It produces realistic observability signals — metrics, logs, traces, and flows — for use in lab environments, pipeline validation, load testing, and incident simulation. Its purpose is not to produce perfectly regular data or pure random noise, but to model the kinds of failure patterns that actually break real observability pipelines: gaps, micro-bursts, cardinality changes, and pattern-driven value sequences.

The project is organized as a Cargo workspace. The core library (`sonda-core`) is the primary product. The CLI (`sonda`) and HTTP server (`sonda-server`) are delivery mechanisms built on top of it. This separation ensures that the engine remains reusable, embeddable, and independently publishable.

---

## 2. Problem Statement

Testing observability pipelines is difficult because real-world telemetry is hard to reproduce on demand. Engineers working on metrics ingestion, alerting logic, or log parsing need:

- **Controllable signal shapes** — sine waves, sawtooth patterns, constant values, uniform random — to validate transform and aggregation correctness.
- **Intentional gaps and bursts** — to test alert flap detection, gap-fill logic, and buffer sizing.
- **Multiple output formats** — Prometheus text, Influx Line Protocol, JSON, remote-write — to validate multi-protocol ingestion paths.
- **High throughput generation** — to stress-test ingest capacity without involving production systems.
- **Portable, zero-dependency tooling** — runnable in CI, Docker, or bare metal without configuration overhead.

Existing tools either produce static fixtures, require complex setup, or generate noise that does not resemble realistic failure modes. Sonda fills this gap with a composable, config-driven generator that can target any observability backend.

---

## 3. Design Principles

- **Library-first.** `sonda-core` is the product. The CLI and server are thin delivery layers. All signal generation, scheduling, encoding, and sink logic lives in the core library and is reusable by any consumer.

- **Trait-based extension points.** Generators, encoders, and sinks are defined as Rust traits backed by `Box<dyn Trait>`. This enables extensibility across all three axes without modifying core dispatch logic. The dynamic dispatch overhead is negligible relative to I/O cost.

- **Config drives behavior.** All runtime behavior — signal shape, rate, duration, labels, encoder, sink — is expressible via YAML scenario files. CLI flags and environment variables can override any config value. No behavior should require a code change.

- **Explicit failure modeling.** Gaps, bursts, and cardinality spikes are first-class citizens, not afterthoughts. The scheduler encodes these as named gap windows and burst windows with precise timing semantics.

- **Highly performant and lightweight.** The generator targets high event throughput with minimal allocations. The CLI binary is statically linked (musl target) for maximum portability. No runtime dependencies.

- **API mirrors CLI.** The HTTP API (`sonda-server`) exposes the same conceptual operations as the CLI. Any scenario runnable from the CLI is runnable via the API. Behavior is not duplicated — both call into `sonda-core`.

---

## 4. Workspace Layout

Sonda is structured as a Cargo workspace with three crates:

| Crate | Responsibility |
|-------|---------------|
| **sonda-core** | Library crate. The engine. Owns all domain logic: telemetry models, schedules, generators, encoders, and sinks. Has no main function. Intended to be reusable and eventually publishable to crates.io. |
| **sonda** | Binary crate. The CLI. Thin layer over sonda-core. Responsible for argument parsing, config loading (YAML + env), and invoking core. Should contain no business logic. |
| **sonda-server** | Binary crate. The HTTP control plane (post-MVP). REST API built with axum. Allows starting, stopping, and inspecting running scenarios over HTTP. Also thin — delegates entirely to sonda-core. |

A Cargo workspace is chosen over a single crate with feature flags for the following reasons:

- **Parallel compilation** across crate boundaries — changes to `sonda` or `sonda-server` do not force a full recompile of `sonda-core`.
- **Clean dependency isolation** — `sonda-server` can pull in axum and tokio without those dependencies affecting the core library or CLI.
- **Publication path** — `sonda-core` can be published to crates.io independently, enabling third-party generators or sinks.
- **Enforced interface boundaries** — the compiler prevents `sonda` from calling internal `sonda-core` functions that are not marked `pub`.

Workspace root `Cargo.toml`:

```toml
[workspace]
members = ["sonda-core", "sonda", "sonda-server"]
resolver = "2"
```

---

## 5. Core Architecture (sonda-core)

`sonda-core` is organized into six internal modules. Each module has a single responsibility and is exposed through a clean public API.

### 5.1 Telemetry Model

The telemetry model defines the canonical in-memory representation of a signal event. It is deliberately format-agnostic — encoding to Prometheus, Influx, or JSON is the encoder's concern, not the model's.

Key types:

- **MetricEvent** — a single timestamped metric sample with a name, `f64` value, and a set of string label pairs.
- **LogEvent** — a structured log line with a timestamp, severity, message, static labels (scenario-level key-value pairs), and arbitrary key-value fields.
- **Labels** — an ordered, deduplicated map of string key-value pairs. Pre-validated at construction time.

> **Design note:** The model layer avoids `String` allocations where possible. Label keys are interned. Values are represented as `f64` for metrics (no integer/histogram distinction at this layer — that is an encoder concern).

### 5.2 Generators

A generator produces `f64` values for a given tick index. Generators are defined as a trait:

```rust
pub trait ValueGenerator: Send + Sync {
    fn value(&self, tick: u64) -> f64;
}
```

Built-in generator implementations:

| Generator | Behavior |
|-----------|----------|
| **Constant** | Returns a fixed value every tick. Useful for baseline and health-check scenarios. |
| **UniformRandom** | Returns a uniformly distributed random value within `[min, max]`. Seeded for deterministic replay. |
| **Sine** | Returns a sine wave with configurable amplitude, period, and offset. |
| **Sawtooth** | Returns a linearly rising wave that resets at the period boundary. |
| **Step** | Monotonically increasing value with configurable step. Wraps at optional max. |
| **Spike** | Outputs baseline value with periodic spikes of configurable magnitude and duration. |
| **GaugeStyle** | Returns a value that random-walks within bounds — simulates a real gauge metric. |
| **Sequence** | Steps through an explicit list of `f64` values. Cycles when `repeat` is true; clamps to the last value when false. Useful for replaying short incident patterns inline. |
| **CsvReplay** | Replays numeric values read from a CSV file at construction time. Supports header rows, column selection, and repeat/clamp behavior. Enables recording production metric values (e.g., via Prometheus or VictoriaMetrics export) and replaying them exactly. |

All generators are constructed via a factory function and stored as `Box<dyn ValueGenerator>`. The caller (the scenario engine) is not aware of the concrete type. New generators can be added without modifying any dispatch code.

> **Trade-off:** Trait objects (`Box<dyn ValueGenerator>`) introduce one layer of dynamic dispatch per tick. For a generator producing 1,000 events/sec, this overhead is measured in nanoseconds and is negligible relative to encoding and I/O cost. The extensibility benefit justifies the choice.

**Operational aliases.** Six high-level aliases (`flap`, `saturation`, `leak`, `degradation`, `steady`, `spike_event`) provide an operational vocabulary on top of the core generators. Aliases are desugared into their underlying `GeneratorConfig` variants at config parse time — in the `config::aliases` module — before the generator factory ever sees them. The runtime and generator trait layer are completely unaware of aliases. Aliases that imply jitter (`steady`, `degradation`) also set jitter fields on the scenario's `BaseScheduleConfig` during desugaring, which is why desugaring operates at the scenario level rather than the generator level alone.

### 5.3 Schedules

A schedule controls when events are emitted: their rate, duration, and any intentional gaps or burst windows.

- **Rate** — target events per second (`f64` for sub-Hz rates).
- **Duration** — total run time. `None` means run indefinitely.
- **GapWindow** — a recurring silent period defined as `(gap_every, gap_for)`. During a gap, no events are emitted and the scheduler sleeps rather than busy-waiting.
- **BurstWindow** — a recurring high-rate period defined as `(burst_every, burst_for, burst_multiplier)`. During a burst, the effective rate is multiplied.

The scheduler uses `std::time::Instant` for elapsed tracking and `std::io::BufWriter` for output batching. A shared schedule loop (`core_loop.rs`) handles all rate control, gap/burst/spike window logic, and stats tracking for both metrics and logs. Signal-specific event construction is delegated to a per-signal callback, eliminating duplication between the metric and log runners. The loop is synchronous — tight sleep intervals are sufficient for the target rate range and avoid tokio overhead in the CLI path.

> **Concurrency (post-MVP):** Multiple concurrent scenarios will be supported in a future expansion. The plan is `std::thread` per scenario with `mpsc` channels feeding a shared sink, before introducing tokio. This keeps the core synchronous and avoids making tokio a core dependency.

### 5.4 Encoders

An encoder serializes a `MetricEvent` or `LogEvent` into bytes suitable for a specific wire format. Encoders are defined as a trait:

```rust
pub trait Encoder: Send + Sync {
    fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<()>;
    fn encode_log(&self, event: &LogEvent, buf: &mut Vec<u8>) -> Result<()>;
}
```

Built-in encoder implementations:

| Encoder | Target |
|---------|--------|
| **PrometheusText** | Prometheus exposition format (`text/plain 0.0.4`). Encodes metric name, labels, value, and optional timestamp. |
| **InfluxLineProtocol** | InfluxDB line protocol. Tags become measurement tags; value field is mapped to a configurable field key. |
| **JsonLines** | One JSON object per line. Compatible with Elasticsearch, Loki, and generic HTTP ingest. |
| **RemoteWrite** | Prometheus remote-write protobuf (feature-gated). Encodes each metric as a length-prefixed `TimeSeries` message. Must be paired with the `RemoteWrite` sink, which batches, wraps in a `WriteRequest`, snappy-compresses, and POSTs with the correct protocol headers. Targets VictoriaMetrics, vmagent, Prometheus, Thanos, Cortex, Mimir, and Grafana Cloud. |
| **Otlp** | OTLP protobuf (feature-gated). Encodes metric events as length-prefixed `Metric` messages and log events as length-prefixed `LogRecord` messages. Must be paired with the `OtlpGrpc` sink, which batches and sends via gRPC to an OpenTelemetry Collector. Requires the `otlp` Cargo feature. |

Encoders pre-build any invariant content (label serialization prefixes, metric name validation) at construction time to avoid per-event work. The `encode` methods write into a caller-provided `Vec<u8>` buffer to minimize allocations.

### 5.5 Sinks

A sink consumes encoded byte buffers and delivers them to a destination. Sinks are defined as a trait:

```rust
pub trait Sink: Send + Sync {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}
```

Sink implementations follow a natural progression of complexity:

| Sink | Description |
|------|-------------|
| **Stdout** | Buffered stdout via `BufWriter`. Default sink. Zero configuration. |
| **File** | Buffered file writer. Configurable path. Supports rotation (planned). |
| **Tcp / Udp** | Raw socket delivery. Targets syslog receivers, statsd, and similar line-protocol endpoints. |
| **HttpPush** | HTTP POST to a configurable endpoint (feature-gated behind `http`). Supports custom headers for protocol-specific requirements. Uses `ureq`. |
| **RemoteWrite** | Prometheus remote write sink (feature-gated). Receives length-prefixed `TimeSeries` from the `RemoteWrite` encoder, batches them into a single `WriteRequest`, prost-encodes, snappy-compresses, and HTTP POSTs with the correct protocol headers. Requires the `remote-write` Cargo feature. |
| **Kafka** | Kafka producer via `rskafka` (pure Rust, no C deps). Topic configurable per scenario. Supports TLS and SASL authentication (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512) for managed brokers. Requires the `kafka` Cargo feature. |
| **Loki** | HTTP POST to Loki's push API (`/loki/api/v1/push`) (feature-gated behind `http`). Batches log events into Loki's JSON envelope format. Labels configurable per scenario. Uses `ureq`. |
| **OtlpGrpc** | OTLP/gRPC sink (feature-gated). Receives length-prefixed `Metric` or `LogRecord` messages from the `Otlp` encoder, batches them into `ExportMetricsServiceRequest` or `ExportLogsServiceRequest`, and sends via gRPC unary call to an OpenTelemetry Collector (default port 4317). Requires the `otlp` Cargo feature. |
| **Channel** | In-memory channel sink (`mpsc::Sender<Vec<u8>>`). For testing and inter-thread communication. |
| **Memory** | In-memory buffer sink (`Vec<Vec<u8>>`). For testing and embedding. |

### 5.6 Packs

A metric pack is a reusable bundle of metric names and label schemas that expands into a multi-metric scenario. Packs are a **config-level composition concept** — the `packs/` module resolves a pack reference into N `ScenarioEntry` items before `prepare_entries()` runs. The runtime (scheduler, core loop, runners) has no knowledge of packs; it only sees the expanded entries.

Key types:

- **MetricPackDef** — a pack definition: name, description, category, shared labels, and a list of `MetricSpec` entries.
- **MetricSpec** — a single metric within a pack: name, optional per-metric labels, optional default generator.
- **PackScenarioConfig** — user-facing YAML config that references a pack by name and provides rate, duration, sink, encoder, labels, and per-metric overrides.
- **expand_pack()** — the expansion function. Takes a `MetricPackDef` and a `PackScenarioConfig`, returns `Vec<ScenarioEntry>`. Label merge order: shared → per-metric → user → override. Generator selection: override → spec → constant(0.0).

Built-in packs are embedded via `include_str!` and stored in a static catalog array (zero heap allocations for catalog access). Packs like `node_exporter_cpu` contain multiple specs with the same metric name but different label sets (e.g., one `node_cpu_seconds_total` per CPU mode). Overrides key on metric name, so a single override entry applies to all specs sharing that name.

> **Design note:** Packs deliberately do not carry rate, duration, sink, or encoder — those are delivery concerns supplied by the user at run time. This separation means the same pack definition works unchanged across stdout testing, remote-write to production, and Kafka ingest.

### 5.7 Cargo Features

`sonda-core` uses Cargo features to keep the default dependency footprint minimal. Consumers who only need generators and encoders can omit heavy transitive dependencies like HTTP/TLS stacks and async runtimes.

| Feature | Default | Dependencies Gated | Description |
|---------|---------|-------------------|-------------|
| `config` | yes | `serde_yaml_ng` | Enables `serde::Deserialize` on all config types. Disable for library consumers who construct configs in code. |
| `http` | no | `ureq` (+ rustls, ring, webpki) | Enables `HttpPush` and `Loki` sinks. |
| `kafka` | no | `rskafka`, `tokio`, `rustls`, `rustls-pemfile`, `webpki-roots` | Enables the Kafka sink with TLS and SASL support. |
| `remote-write` | no | `prost`, `snap`, `ureq` | Enables Prometheus remote write encoder and sink. |
| `otlp` | no | `tonic`, `prost`, `tokio` | Enables OTLP protobuf encoder and gRPC sink. |

The CLI (`sonda`) and HTTP server (`sonda-server`) enable all features they need in their own `Cargo.toml`. End users of the pre-built binary or Docker image get every feature enabled.

---

## 6. Configuration

Sonda uses a layered configuration model. From lowest to highest precedence:

1. **YAML scenario file** — defines the full scenario: signal shape, rate, duration, labels, encoder, sink.
2. **Environment variables** — can override any scalar config value. Prefixed with `SONDA_`.
3. **CLI flags** — override any value for one-off runs. Take highest precedence.

Example YAML scenario:

```yaml
name: interface_oper_state
rate: 1000          # events/sec
duration: 30s

generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0

gaps:
  every: 2m
  for: 20s

labels:
  hostname: t0-a1
  zone: eu1

encoder:
  type: prometheus_text
sink:
  type: stdout
```

YAML is chosen as the primary scenario format because it is familiar to the observability and infrastructure engineering community (Prometheus, Ansible, Kubernetes all use YAML for configuration). App-level settings such as API port and log level are passed as CLI flags or environment variables — there is no separate app config file.

---

## 7. CLI Design (sonda)

The CLI crate is intentionally thin. Its only responsibilities are:

- Parse arguments using `clap` (derive API).
- Load and validate the YAML scenario file.
- Merge CLI overrides onto the loaded config.
- Instantiate the appropriate generator, encoder, and sink from `sonda-core`.
- Hand control to the `sonda-core` scenario runner.

Primary CLI surface:

```
sonda metrics [OPTIONS] --scenario <file.yaml>
sonda metrics --name <name> --rate <n> --duration <d> --encoder <enc> [OVERRIDES]

sonda logs [OPTIONS] --scenario <file.yaml>
```

The CLI does not contain signal generation logic. Any behavior that is tested or benchmarked belongs in `sonda-core`.

---

## 8. API Design (sonda-server, post-MVP)

`sonda-server` exposes an HTTP REST API built with axum. It allows scenarios to be started, inspected, and stopped over HTTP — enabling integration into CI pipelines, test harnesses, and dashboards without shell access.

The API follows the same conceptual model as the CLI. A running scenario in `sonda-server` is equivalent to a running `sonda` process. All scenario logic is delegated to `sonda-core`.

| Endpoint | Description |
|----------|-------------|
| `POST /scenarios` | Start a new scenario. Body is a YAML or JSON scenario definition. Returns a scenario ID. |
| `GET /scenarios` | List all running scenarios with status and stats. |
| `GET /scenarios/{id}` | Inspect a specific scenario: config, tick count, bytes emitted, errors. |
| `DELETE /scenarios/{id}` | Stop and remove a running scenario. |
| `GET /scenarios/{id}/stats` | Return live stats: current rate, total events, gap/burst state. |

> **API principle:** The API does not invent new behavior. Every endpoint maps to an operation that is also doable from the CLI. If a scenario cannot be expressed in YAML, it cannot be run via the API either. This constraint keeps the two surfaces in sync.

---

## 9. Concurrency Model

The concurrency model evolves in phases to keep complexity aligned with actual need.

### Phase 1 — MVP (synchronous, single scenario)

The MVP runs a single scenario on the main thread. The scheduler loop is synchronous. Output is buffered via `BufWriter`. No async runtime is involved. This is the simplest correct implementation and the right place to start.

### Phase 2 — Multiple concurrent scenarios (threads + channels)

Each scenario runs on a dedicated OS thread. A shared sink (or per-scenario sink) receives encoded buffers via an `mpsc` channel. This avoids async complexity while enabling parallelism. Backpressure is handled by bounded channel capacity.

> **Phase offset and clock groups:** Multi-scenario configs support `phase_offset` (a duration delay before a scenario begins emitting) and `clock_group` (a shared timing reference across scenarios). The phase offset is implemented as a thread sleep before the event loop starts, keeping the scheduler logic unchanged. This enables testing compound alert rules that depend on multiple correlated metrics with precise temporal relationships. See `examples/multi-metric-correlation.yaml` for an example.

### Phase 3 — Async (tokio, if needed)

If the HTTP server (`sonda-server`) or a high-throughput HTTP sink requires async I/O, tokio will be introduced in `sonda-server` as a dependency. `sonda-core` will remain async-agnostic — it exposes synchronous interfaces that can be called from async contexts via `spawn_blocking`. This keeps the core library portable and avoids tokio becoming a transitive dependency of every consumer.

> **Exception — Kafka sink:** The `kafka` Cargo feature in `sonda-core` pulls in `tokio`, `rskafka`, `rustls`, `rustls-pemfile`, and `webpki-roots` as optional dependencies. The Kafka sink spins up a private single-threaded `tokio::runtime::Runtime` inside the struct to drive async `rskafka` calls, while keeping the public `Sink` interface fully synchronous. TLS and SASL authentication (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512) are supported for managed Kafka services (Confluent Cloud, AWS MSK, Aiven). Because these dependencies are gated behind `#[cfg(feature = "kafka")]`, `sonda-core` remains async-agnostic by default. Consumers that do not need Kafka do not pay for tokio or the TLS stack. The CLI and sonda-server opt in explicitly by enabling the feature in their `Cargo.toml`.

---

## 10. Portability

Portability is a primary constraint. Sonda must run on bare metal, in Docker, and in CI without a runtime installation.

- The primary release target is `x86_64-unknown-linux-musl`, producing a fully static binary with no dynamic library dependencies.
- `aarch64-unknown-linux-musl` is a secondary target for ARM environments.
- macOS (`x86_64-apple-darwin`, `aarch64-apple-darwin`) is supported for local development.
- C dependencies (OpenSSL, libz) must be avoided or replaced with pure-Rust alternatives (`rustls`, `miniz_oxide`) to preserve static linkage.

> **Implication for crate selection:** Any crate added to `sonda-core` must be evaluable against the musl target. Crates with C FFI dependencies that cannot be statically linked are excluded from the core. They may appear in `sonda-server` where the portability constraint is relaxed.

---

## 11. MVP Scope

The MVP is complete when the following criteria are met:

- `sonda metrics --name <n> --rate <r> --duration <d>` generates valid Prometheus text to stdout.
- Value generators: constant, uniform random (seeded), sine, sawtooth.
- Gap windows: `--gap-every` / `--gap-for` produces intentional silent periods.
- Static labels: `--label k=v`, repeatable.
- Config: the above is expressible via a YAML scenario file.
- Tests: unit tests for schedule math, gap logic, and all value generators.
- Workspace: two crates (`sonda-core`, `sonda`) with a clean interface boundary.
- Binary: compiles to a static musl binary.

Everything else — additional encoders, sinks, `sonda-server`, log generation, concurrency, Kafka — is post-MVP.

---

## 12. Post-MVP Roadmap

| Phase | Scope |
|-------|-------|
| **Expansion A** | Additional encoders: Influx Line Protocol, JSON Lines, Prometheus remote-write (protobuf). |
| **Expansion B** | Additional sinks: file, TCP/UDP, HTTP push (remote-write to VictoriaMetrics / Prometheus). |
| **Expansion C** | Log generation: replay mode (loop file at speed factor) and template generator (structured log lines). |
| **Expansion D** | Burst windows and dynamic schedules: chained schedules, jitter, configurable burst multipliers. |
| **Expansion E** | Config-driven multi-scenario: a single YAML file defines multiple concurrent streams. |
| **Expansion F** | Concurrency: multiple scenarios on parallel threads; mpsc channel to shared sink. |
| **Expansion G** | sonda-server: REST API control plane built with axum. Start/stop/inspect scenarios over HTTP. |
| **Expansion H** | Kafka sink: produce encoded events to a Kafka topic. Topic and partition configurable. (Moved into Phase 1 as Slice 1.6.) |
| **Expansion I** | Clustering: deferred decision. If needed, a shared state store or coordination layer for sonda-server. No design committed at this time. |

---

## 13. Open Questions and Deferred Decisions

### Clustering

Whether `sonda-server` needs clustering support is an open question. A single `sonda-server` instance can run many concurrent scenarios and will likely be sufficient for most lab use cases. If multi-instance coordination is ever needed, it would require a shared state store (e.g., etcd or Redis) or a gossip layer. This decision is deferred until `sonda-server` is built and actual throughput limits are understood.

### Protobuf / gNMI Encoder

A gNMI-compatible JSON encoder is on the roadmap. Full protobuf support (for Prometheus remote-write and gNMI) will require `prost` as a dependency. Evaluated at Expansion A time.

### Trace and Flow Signal Types

The notes reference flows and traces as future signal types. These have materially different models than metrics and logs (parent-child spans, flow records with src/dst). They are not designed here and are deferred until after the metrics and log expansions are stable.

### Dynamic Label Cardinality

Cardinality spikes are implemented as time-windowed label injection. Each spike configuration defines a label key, a recurrence interval (`every`), a window duration (`for`), and a target cardinality (number of unique values). During the spike window the runner injects the label with a unique value on each tick; outside the window the label is absent.

Two strategies control value generation: `counter` produces sequential deterministic values (`prefix + tick % cardinality`), while `random` uses a SplitMix64 hash of `seed ^ index` to produce hex-string values that look random but are reproducible. Both strategies guarantee exactly `cardinality` distinct values per window.

Spike windows are evaluated per-tick in the shared schedule loop (`core_loop.rs`) alongside gap and burst windows. The same loop drives both metric and log runners — signal-specific work (event construction, encoding, sink writing) is delegated to a per-signal callback while all rate control, window handling, and stats tracking live in the shared loop. Gap windows take priority: if a gap and spike overlap, the gap suppresses all output. Multiple spike configurations can be stacked on a single scenario, each injecting a different label key.
