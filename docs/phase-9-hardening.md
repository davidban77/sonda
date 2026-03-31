# Phase 9 — Codebase Hardening & Rust Quality

> Findings from a full-codebase review by Staff Engineer (architecture) and Senior Rust
> Engineer (idiom/safety) perspectives. Organized by severity into phases with actionable
> slices.

## Phase 9A — Critical: Correctness & Safety

Issues that affect correctness, safety, or data integrity. Fix before any new feature work.

### Slice 9A.1 — Server resource leak: `delete_scenario` never removes handle

**What:** `DELETE /scenarios/{id}` stops the scenario thread and joins it, but never calls
`scenarios.remove(&id)`. The `ScenarioHandle` (stats buffer, Arc overhead, all String
allocations) stays in the HashMap indefinitely.

**Where:** `sonda-server/src/routes/scenarios.rs` ~line 422-466

**Impact:** Unbounded memory leak in long-running servers. `GET /scenarios` returns an
ever-growing list of stopped scenarios that can never be cleaned up.

**Fix:** After reading final stats, call `scenarios.remove(&id)`. If stopped scenarios should
remain visible, introduce a TTL or a bounded `stopped_scenarios` map.

---

### Slice 9A.2 — Server panics on lock poisoning in read handlers

**What:** `list_scenarios`, `get_scenario`, and `get_scenario_stats` use
`.expect("AppState RwLock must not be poisoned")` on `RwLock::read()`. If any write handler
panics while holding the write lock, all subsequent reads panic — crashing the server.

**Where:** `sonda-server/src/routes/scenarios.rs` ~lines 368, 398, 485

**Impact:** One panicking write path takes down all read paths permanently. `delete_scenario`
already handles this correctly with `.map_err()`.

**Fix:** Replace `.expect(...)` with `.map_err(|e| internal_error(...))` to match the pattern
in `delete_scenario`. All lock acquisitions in request handlers must return 500, not panic.

---

### Slice 9A.3 — `SondaError::Sink` conflates all `std::io::Error` origins

**What:** `#[from] std::io::Error` on the `Sink` variant means any I/O error in the crate
auto-converts to `SondaError::Sink` — even errors from `CsvReplayGenerator::from_file` or
`LogReplayGenerator::from_file`. A missing CSV file is reported as a "sink error."

**Where:** `sonda-core/src/lib.rs` ~line 39; `generator/csv_replay.rs`, `generator/log_replay.rs`

**Impact:** Callers matching on `SondaError::Sink` get false positives. Undermines programmatic
error handling for a library crate targeting crates.io.

**Fix:** Add a `SondaError::Io` or `SondaError::Generator` variant for generator I/O. Remove
the blanket `#[from]` on Sink and use explicit `map_err` at each site.

---

### Slice 9A.4 — `tick as usize` truncation on 32-bit platforms

**What:** `SequenceGenerator`, `CsvReplayGenerator`, `LogReplayGenerator`, and
`LogTemplateGenerator` all cast `tick as usize` for indexing. On 32-bit platforms, ticks above
`u32::MAX` silently truncate, producing incorrect modular arithmetic.

**Where:** `generator/sequence.rs:59`, `generator/csv_replay.rs:167`,
`generator/log_replay.rs:75`, `generator/log_template.rs:161`

**Impact:** Silent wrong results after ~50 days at 1K events/sec on 32-bit targets. Low
probability on primary targets (x86_64/aarch64) but violates correctness guarantees.

**Fix:** Perform modulo in u64 space before casting:
`let index = (tick % len as u64) as usize;`

---

## Phase 9B — Important: Performance Hot Path

Issues that affect throughput and allocation discipline in the per-event hot path.
Directly impact the "performant and concurrent" goal.

### Slice 9B.1 — Eliminate per-tick `name.clone()` and `labels.clone()`

**What:** Every tick of the metric runner clones:
- `name.clone()` — the metric name String (invariant for the entire scenario)
- `labels.clone()` — the full BTreeMap (invariant except during spike windows)

At 10K events/sec with 5 labels, that's 10K BTreeMap deep-clones/sec.

**Where:** `schedule/runner.rs` ~lines 270, 284

**Impact:** The single largest allocation source in the hot path. Contradicts the "minimize
per-event allocations" principle stated in architecture.md and CLAUDE.md.

**Fix options (pick one):**
1. `MetricEvent` borrows name via lifetime: `MetricEvent<'a>` with `name: &'a str`
2. Use `Arc<str>` for name, `Arc<Labels>` for labels (clone is just refcount bump)
3. Only clone labels when spike windows are active; pass `&Labels` to encoder otherwise

---

### Slice 9B.2 — `MetricEvent` re-validates name on every tick

**What:** `MetricEvent::with_timestamp()` validates the metric name regex on every call. In the
runner, the name is invariant — validated once at scenario start, then re-validated on every
tick.

**Where:** `model/metric.rs` ~lines 131-144; called from `schedule/runner.rs` ~line 284

**Impact:** Unnecessary regex check per event. For a `ValidatedMetricName` newtype, validation
happens once and the type system guarantees correctness thereafter.

**Fix:** Introduce `ValidatedMetricName` newtype that validates at construction. `MetricEvent`
accepts it without re-validation. This also documents the invariant in the type system.

---

### Slice 9B.3 — `format_rfc3339_millis` allocates a String per call

**What:** The RFC 3339 timestamp formatter returns `String` instead of writing into a buffer.
Called from the JSON and syslog encoders on every event.

**Where:** `encoder/mod.rs` ~line 144

**Impact:** One String allocation per event for JSON/syslog encoding, violating the
buffer-reuse discipline that other encoders follow.

**Fix:** Change signature to `fn format_rfc3339_millis(ts: SystemTime, buf: &mut Vec<u8>)`.
Write directly into the caller's buffer.

---

### Slice 9B.4 — JSON encoder builds intermediate BTreeMap per encode

**What:** `JsonLines::encode_metric` collects labels into a new `BTreeMap<&str, &str>` on
every event before serializing. The BTreeMap nodes are heap-allocated.

**Where:** `encoder/json.rs` ~lines 94-98

**Impact:** Per-event heap allocations for BTreeMap nodes. Labels are already sorted in the
source `Labels` type — no need for an intermediate collection.

**Fix:** Write the JSON labels object directly from the Labels iterator without collecting into
an intermediate map.

---

### Slice 9B.5 — Log template `resolve_template` does N `String::replace` calls

**What:** Each placeholder in a log template triggers `message.replace(&placeholder, value)`,
creating a new String allocation per placeholder. For templates with 5 placeholders, that's 5
successive String allocations per log event.

**Where:** `generator/log_template.rs` ~line 139

**Impact:** Linear allocation growth with placeholder count in the log generation hot path.

**Fix:** Single-pass approach: scan for `{`, look up field name, write resolved output directly
into a buffer.

---

## Phase 9C — Important: Library Hygiene (crates.io Readiness)

Issues that affect sonda-core's viability as a standalone, embeddable library.
Directly impact the "portable and versatile" goal.

### Slice 9C.1 — Feature-gate `ureq` and HTTP-based sinks

**What:** `ureq` is an unconditional dependency, pulling in rustls + ring + webpki for every
consumer — even those who only need generators and encoders.

**Where:** `sonda-core/Cargo.toml` ~line 21

**Impact:** Consumers embedding sonda-core as a library pay for a full HTTP+TLS stack they may
not use. Hurts compile times, binary size, and contradicts the lean-base philosophy. The Kafka
sink correctly feature-gates its deps — HTTP sinks should too.

**Fix:** Create an `http` feature (or `network-sinks`) gating `ureq`, `HttpPush`, and `Loki`
sinks. Stdout, File, Memory, Channel remain available without features.

---

### Slice 9C.2 — Feature-gate `serde_yaml` and `serde_json` in sonda-core

**What:** Both serialization crates are unconditional dependencies of sonda-core. Config
deserialization is a CLI/server concern, not a library concern.

**Where:** `sonda-core/Cargo.toml` ~lines 20-21

**Impact:** Library consumers who use sonda-core programmatically (building configs in code)
don't need YAML/JSON parsing.

**Fix:** Gate the config deserialization module behind a `config` feature (enabled by default
in the workspace for CLI/server). The config types keep their `Deserialize` derives only when
the feature is active.

---

### Slice 9C.3 — Migrate from `serde_yaml` to `serde_yml`

**What:** `serde_yaml 0.9` was archived by its maintainer in 2023. The maintained fork is
`serde_yml`.

**Where:** Workspace `Cargo.toml` ~line 22

**Impact:** Supply chain risk for a crate targeting crates.io. No upstream bug fixes.

**Fix:** Mechanical replacement — API is compatible. Update the dependency and verify all tests
pass.

---

### Slice 9C.4 — Structured error types for `SondaError`

**What:** All `SondaError` variants except `Sink` carry a `String`. Callers cannot
programmatically distinguish "invalid rate" from "invalid duration" without string inspection.

**Where:** `sonda-core/src/lib.rs` ~lines 29-42

**Impact:** Library consumers cannot handle specific error cases. For a published crate, error
ergonomics are part of the API contract.

**Fix:** Introduce typed sub-enums: `ConfigError`, `GeneratorError`, `EncoderError`. The main
`SondaError` delegates via `#[from]`.

---

## Phase 9D — Important: DRY & Maintainability

Structural duplication that creates maintenance burden and divergence risk.

### Slice 9D.1 — Unify metric and log runner loops

**What:** `runner.rs` (metrics) and `log_runner.rs` (logs) are near-identical: gap/burst/spike
window parsing, rate control loop, stats tracking, shutdown handling are duplicated verbatim.
Subtle asymmetries already exist (e.g., `Labels::from_pairs(&[])?` vs `Labels::default()`).

**Where:** `schedule/runner.rs` vs `schedule/log_runner.rs`

**Impact:** Any bug fix or feature (new window type, rate control improvement) must be applied
identically to both files. As signal types grow (traces, flows), duplication grows linearly.

**Fix:** Extract the common schedule loop into a generic runner parameterized on event type, or
use a callback/strategy pattern for the generate-encode step.

---

### Slice 9D.2 — Unify `ScenarioConfig` and `LogScenarioConfig`

**What:** Both config types share 10 of 12 fields identically. Only the generator type differs.

**Where:** `config/mod.rs` ~lines 148-193 vs 294-337

**Impact:** Same maintenance burden as 9D.1 — any config field addition touches two structs.

**Fix:** Extract `BaseScheduleConfig` with shared fields. Both signal-specific configs embed it
and add only their generator field.

---

### Slice 9D.3 — Deduplicate SplitMix64 implementation

**What:** The SplitMix64 mixing function is copy-pasted in three files.

**Where:** `generator/uniform.rs:37-43`, `generator/log_template.rs:67-73`,
`schedule/mod.rs:170-175`

**Impact:** Bug fix or optimization must be applied three times.

**Fix:** Extract to a shared `util::splitmix64(seed: u64) -> u64` function.

---

## Phase 9E — Minor: Polish & Consistency

Lower-priority items. Address opportunistically or bundle with nearby changes.

### Slice 9E.1 — `parse_duration` should accept fractional seconds

**Where:** `config/validate.rs` ~line 56

`"1.5s"` is rejected because the numeric part is parsed as `u64`. Parse as `f64` and convert
to milliseconds. Backward-compatible.

---

### Slice 9E.2 — Server graceful shutdown should join scenario threads

**Where:** `sonda-server/src/main.rs` ~lines 65-78

`shutdown_signal` calls `handle.stop()` but not `handle.join()`. Scenario threads may be
flushing sinks when the process exits. Add `join(Some(Duration::from_secs(5)))` after stop.

---

### Slice 9E.3 — `Labels::iter()` should return `(&str, &str)` not `(&String, &String)`

**Where:** `model/metric.rs` ~line 84

Idiomatic Rust. Every caller converts anyway. Add `.map(|(k, v)| (k.as_str(), v.as_str()))`.

---

### Slice 9E.4 — `Labels::new()` should be `pub(crate)` to prevent unvalidated construction

**Where:** `model/metric.rs` ~lines 57-59

Public consumers should use `Labels::from_pairs()` which validates. The unvalidated constructor
should be internal-only.

---

### Slice 9E.5 — Remove `#[allow(dead_code)]` on `Schedule` struct

**Where:** `schedule/mod.rs` ~lines 43-44

Either use the struct or remove it. Dead code adds confusion.

---

### Slice 9E.6 — Add edge-case tests for `format_rfc3339_millis`

**Where:** `encoder/mod.rs` ~lines 144-179

The hand-rolled Gregorian calendar conversion (Howard Hinnant algorithm) has no tests for leap
years, century boundaries, or Dec 31 / Jan 1 transitions.

---

### Slice 9E.7 — `Severity` ordering should be explicit, not derived

**Where:** `model/log.rs` ~line 18

The derived `Ord` happens to match logical severity because of declaration order. If someone
reorders variants, ordering breaks silently. Used in `create_log_generator` sort.

---

### Slice 9E.8 — `HashMap` vs `BTreeMap` inconsistency in `log_template`

**Where:** `generator/log_template.rs` ~line 25

`TemplateEntry::field_pools` uses `HashMap` while the rest of the codebase uses `BTreeMap` for
deterministic ordering. Not a correctness issue (lookups are by name) but inconsistent.

---

## Strengths (Preserve These)

The reviews identified these patterns as exemplary — protect them during hardening:

1. **Clean workspace architecture.** The three-crate structure enforces "library is the
   product." CLI and server are genuinely thin. Do not leak business logic into them.

2. **Buffer-reuse discipline in encoders.** All encoders write into caller-provided `Vec<u8>`.
   The runner pre-allocates and reuses. This is textbook Rust buffer management.

3. **Deterministic generators via stateless hashing.** SplitMix64 hash of `seed ^ tick`
   satisfies `&self` without Mutex/RefCell. Enables deterministic testing without mocking.

4. **Comprehensive Send + Sync contract tests.** Compile-time assertions catch regressions.

5. **Deadline-based rate control.** Absolute `next_deadline` prevents timing drift. Naturally
   absorbs encode/write latency. Correct for precise rate control.

6. **No `unwrap()` in production paths.** Every `expect()` is justified with a clear message.

7. **Trait-object extensibility.** Adding generators/encoders/sinks is mechanical and
   self-contained. Zero changes to dispatch code.

8. **Feature gating for heavy deps.** Kafka (rskafka + tokio) and remote-write (prost + snap)
   are correctly gated. Extend this pattern to HTTP sinks (Phase 9C.1).

9. **Test quality.** Byte-exact regression anchors, determinism tests, edge cases, boundary
   conditions. The test suite would survive significant refactoring.

---

## Recommended Execution Order

```
Phase 9A (Critical)     — 4 slices, do first, blocks everything
Phase 9B (Performance)  — 5 slices, high impact on stated goals
Phase 9C (Library)      — 4 slices, enables crates.io publication
Phase 9D (DRY)          — 3 slices, reduces ongoing maintenance cost
Phase 9E (Polish)       — 8 slices, opportunistic or bundled
```

Total: **24 slices** across 5 phases. Phases 9A and 9B are the highest priority.
9C and 9D can run in parallel. 9E items can be picked up alongside nearby changes.
