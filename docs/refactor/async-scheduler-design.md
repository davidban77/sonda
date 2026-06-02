# Async-Scheduler Architectural Design

**Status**: DRAFT — pending Opus review pass with Phase 0 benchmark numbers in hand (numbers captured 2026-06-02; see "Benchmark baseline" section below).
**Date**: 2026-06-02
**Scope**: Sonda 1.13.x → 1.14.0 bounded async-scheduler migration.

This doc is internal (lives in `docs/refactor/`, not on the user-facing MkDocs site). It is the architectural contract for Phases 1-5 of the async-scheduler initiative tracked in `~/.claude/projects/-Users-netpanda-projects-sonda/planning/2026-06-02-async-scheduler-initiative.md`.

---

## Problem

Today: `sonda-core::schedule::multi_runner` spawns one OS thread per scenario (`std::thread::spawn`). Each thread carries ~8 MB stack on Linux. At ≥~50 concurrent scenarios on a default-tuned host, memory, context-switch overhead, and process thread limits (`ulimit -u`, `vm.max_map_count`) become the bottleneck. The Staff SR adoption review (2026-06-01) identifies this as Gap #3 and as a precondition for Gaps #1 (self-instrumentation) and #2 (resource limits) to be useful.

## Target

A bounded Tokio scheduler that decouples scenario count from thread count. Worker-pool size configurable via `--workers N` (default: `num_cpus`). Maximum scenarios per process configurable via `--max-scenarios N` (default: 100). Scenario lifecycle (start, stop, stats, gating) preserves today's observable semantics — no scenario YAML change, no breaking trait change beyond what the async boundary requires.

## Non-goals

- Changing the public scenario YAML schema.
- Persistence of scenario state across restarts (separate initiative).
- Multi-tenant authorization beyond the existing API-key model.
- Changing the CLI's user-visible behavior (output, exit codes, flags) beyond opt-in additions.

---

## 11 architecture decisions

Each decision below carries: background, options considered, recommendation, rationale, and any open question for Opus to push back on.

### 1. Sink — keep sync trait, wrap in `spawn_blocking`

**Background.** `sonda-core::sink::Sink` is sync today: `write(&mut self, &[u8]) -> Result<(), SondaError>`. All concrete sinks (stdout, file, tcp, udp, http, kafka, otlp_grpc, remote_write, loki, channel, memory) are sync internally. Several already do internal blocking I/O via `ureq` (sync HTTP) or `rskafka` (which itself uses tokio under the hood).

**Options.**
- (a) Async trait via `async-trait` crate. Every sink becomes `async fn write`. Every implementer rewrites.
- (b) Keep sync. Wrap the per-tick sink write in `tokio::task::spawn_blocking` at the schedule-loop call site.
- (c) Hybrid: introduce `AsyncSink` trait, keep `Sink` for backwards compat. Two parallel traits.

**Recommendation.** **(b) Keep sync trait, wrap in `spawn_blocking` at the call site.**

**Rationale.**
- Sonda's existing sinks are all sync at the lowest layer. Forcing async at the trait boundary doubles the trait-impl complexity for zero runtime benefit.
- `spawn_blocking` is the canonical Tokio pattern for sync I/O inside async code. It uses a separate pool (`tokio::runtime::Handle::current().spawn_blocking`) with bounded but distinct worker count.
- Sink extenders (custom users adding Kinesis, NATS, etc.) write idiomatic sync code, which is far easier than async-trait gymnastics.
- The scheduler benefit (no per-scenario OS thread) is preserved — `spawn_blocking` shares the blocking pool across all scenarios; idle scenarios don't consume a slot.

**Open question for Opus.** Is there a scenario where the sink's blocking time exceeds the tick interval often enough that `spawn_blocking` saturation becomes the bottleneck? If yes, we may need to expose `blocking-threads N` as a separate flag.

### 2. Generator — keep sync

**Background.** `sonda-core::generator::ValueGenerator::value(&self, tick: u64) -> f64`. Pure CPU.

**Recommendation.** **Keep sync.** Generators do no I/O; async would be pure ceremony. They run inline in the schedule loop.

**Open question for Opus.** None expected. If Opus pushes back, it would be on the question of whether some future generator (e.g., live-replay from a remote source) would need async — that's a separate trait addition (`AsyncValueGenerator`) if/when it appears.

### 3. Encoder — keep sync

**Background.** `sonda-core::encoder::Encoder::encode_metric(&self, &MetricEvent, &mut Vec<u8>) -> Result<(), SondaError>`. Pure CPU writing into caller buffer.

**Recommendation.** **Keep sync.** Same reasoning as Generator. Encoders write into pre-allocated buffers; the hot path is allocation-disciplined by design.

### 4. Channel migration — `std::sync::mpsc` → `tokio::sync::mpsc` / `watch`

**Background.** Gate channels today use `std::sync::mpsc::sync_channel(N)` with sync `recv_timeout`. Two distinct usages:
- **Gate edge events** (Running ↔ Paused transitions, value updates) — FIFO, multi-producer / single-consumer.
- **Gate state** (current open/closed state) — last-value-wins, broadcast.

**Recommendation.**
- For FIFO event streams: `tokio::sync::mpsc::channel`.
- For state broadcast: `tokio::sync::watch::channel`.
- Phase 1 introduces a sync-recv adapter (`Handle::block_on(rx.recv())`) so the synchronous schedule loop can consume tokio channels. Phase 2 removes the adapter when the schedule loop itself goes async.

**Rationale.** Today's mix-and-match of edge vs state is implicit in the gate-bus implementation. Making it explicit (`mpsc` vs `watch`) clarifies the API and matches tokio idioms.

**Open question for Opus.** Are there any gate paths that should be `broadcast` (multi-consumer) instead of `mpsc` (single-consumer)? Likely no — cross-POST `while:` resolution already uses a registry, not multi-consumer broadcast.

### 5. CLI runtime — own `current_thread` runtime, `block_on`

**Background.** The CLI today is synchronous. Tokio runtimes come in two flavors: `current_thread` (single-threaded, lowest startup cost) and `multi_thread` (configurable worker count).

**Options.**
- (a) CLI starts a `multi_thread` runtime in `main`. Higher startup cost (~10-30ms).
- (b) CLI starts a `current_thread` runtime. Lower startup cost; uses the calling thread for tasks.
- (c) CLI stays sync, wraps the async core in a hand-rolled blocking adapter. High maintenance cost; two parallel paths.

**Recommendation.** **(b) `current_thread` runtime in CLI `main`, `runtime.block_on(...)` for the scenario lifecycle.**

**Rationale.**
- CLI scenarios are typically 1-10 in count. `current_thread` is sufficient for that load.
- Cold-start cost is the lowest. Important for `sonda new`, `sonda show`, `sonda list` which exit quickly.
- Server uses `multi_thread` (separate concern, configured via `--workers`).
- Validation in Phase 3 — measure cold-start delta; if it exceeds 50ms, reconsider.

**Open question for Opus.** Is there a CLI use case (e.g., `sonda run` with 50+ scenarios in one file) where `multi_thread` would noticeably help? My take: rare and the user can opt in via a future `--workers` CLI flag if needed; ship `current_thread` as the default.

### 6. Per-handle shutdown — `CancellationToken` replaces `Arc<AtomicBool>`

**Background.** PR #420 introduced per-handle `Arc<AtomicBool>` shutdown flags. The master Ctrl+C signal fans out via a watchdog thread that flips each handle's flag to false. Per-handle `stop()` flips its own flag.

**Options.**
- (a) Keep `Arc<AtomicBool>`. Works but doesn't compose with tokio's cancellation primitives.
- (b) Replace with `tokio_util::sync::CancellationToken`. Cancellation tokens compose (parent → child), can be `.cancelled().await`ed, and are the canonical Tokio cancellation primitive.

**Recommendation.** **(b) `CancellationToken`.**

**Rationale.**
- The master signal becomes a parent token. Each scenario's per-handle token is a child of the master. `master.cancel()` cancels all children automatically — no watchdog thread needed.
- The schedule loop can `select!` between gate events, tick timer, and `cancel.cancelled()` natively.
- `ScenarioHandle::stop()` calls `self.cancel.cancel()`; the task observes via the loop's select arm.
- Public surface change on `ScenarioHandle` — `pub shutdown: Arc<AtomicBool>` becomes `pub cancel: CancellationToken`. `Arc<AtomicBool>` is removed in 1.14.0 — **no deprecation shim**. The `#[non_exhaustive]` contract requires `..` in destructuring matches, so embedders see a build error at field access (`handle.shutdown.store(...)`) which is the desired loud signal.

**Sign-off (Opus, 2026-06-02)**: hard break confirmed. Drop any deprecation-glide language.

### 7. Per-scenario state — unchanged shape, just async-aware locking

**Background.** Each scenario today owns `Arc<RwLock<ScenarioStats>>` and an inner `Arc<Mutex<Box<dyn ValueGenerator>>>` for the generator. State is small (≤1 KB plus the `current_values` map which scales with series cardinality).

**Recommendation.** **Keep the Arc/RwLock shape. Lock acquisitions stay sync — they're ~nanoseconds and never block under normal operation.**

**Rationale.**
- Tokio's `tokio::sync::Mutex` is for locks held across await points. ScenarioStats locks are NEVER held across awaits (the bench will confirm). std `RwLock` is correct.
- The `parking_lot` crate's `RwLock` is faster than std; consider as a Phase-5 tuning if profiling shows lock contention. Out of scope for the architectural design.

**Open question for Opus.** Does any phase need to hold a lock across an `await`? If yes, the lock has to switch to `tokio::sync::*`. My read: no — sink writes happen via `spawn_blocking` with the encoded bytes already in a buffer; locks are released before the spawn_blocking.

### 8. Resource-limit hooks — tower middleware + handler-side check

**Background.** No body limit, no timeout, no concurrency limit, no max-scenarios cap today.

**Recommendation.** Tower middleware stack on the server router:
- `tower_http::limit::RequestBodyLimitLayer::new(body_limit_bytes)` global, configurable via `--body-limit` (default: 1 MiB).
- `tower_http::timeout::TimeoutLayer::new(timeout)` per-route, configurable via `--request-timeout` (default: 30s).
- `tower::limit::ConcurrencyLimitLayer::new(concurrency)` on `POST /scenarios` only, configurable via `--max-concurrent-posts` (default: 8).
- `--max-scenarios N` checked in the POST handler BEFORE compile + launch. Returns HTTP 503 with structured error body listing active scenario count. Default: 100. Zero = unlimited (logged warning at startup).
- `--workers N` configures the tokio runtime worker count. Default: `num_cpus`. Server only — CLI uses `current_thread`.

**Rationale.** Layered defense; each limit catches a different failure mode. Tower middleware is the idiomatic axum stack; standard SRE pattern.

**Open question for Opus.** Should `--max-scenarios` be a hard cap (reject 6th POST when 5 are running) or a backpressure signal (queue with timeout)? My take: hard cap. Queueing scenarios has weird semantics ("your scenario will start... eventually") and surprises operators.

### 9. Self-instrumentation — new `/server/metrics` endpoint

**Background.** `GET /metrics` today serves scenario telemetry (the synthetic data sonda generates). The adoption review identified that scrapers cannot get RED metrics about sonda-server itself.

**Recommendation.** New endpoint `GET /server/metrics`. Prometheus text exposition format. Stable scrape target. Series (initial set):

```
sonda_server_active_scenarios{state="running|paused|held|unresolved|pending|finished"} gauge
sonda_server_worker_threads gauge
sonda_server_max_scenarios gauge
sonda_server_requests_total{route,method,status} counter
sonda_server_request_duration_seconds_bucket{route,method,le} histogram
sonda_server_sink_errors_total{sink_type} counter
sonda_server_blocking_pool_active gauge
sonda_server_blocking_pool_queue_depth gauge
sonda_server_build_info{version,git_sha,rust_version} gauge always 1
```

**Implementation note.** Metrics aggregator behind a `prometheus` crate or hand-rolled. Hand-rolled is fine here — keeps the dep tree small and the format is simple.

**Open question for Opus.** Should `/server/metrics` require the API key (consistent with `/scenarios/*`) or be public like `/health`? My take: PUBLIC like `/health` — Prometheus scrapes from a separate machine and asking operators to plumb API keys to their scrape config is friction. The data exposed is operational metadata, not scenario content.

### 10. Backwards compatibility — `#[non_exhaustive]` absorbs most diffs, one hard break

**Recommendation.**
- `ScenarioHandle::shutdown: Arc<AtomicBool>` → `ScenarioHandle::cancel: CancellationToken`. Hard break, embedders rebuild.
- `run_multi`, `run_multi_compiled`, `launch_scenario`, `launch_scenario_with_gates` become `async fn`. Hard break.
- `Sink`, `Encoder`, `ValueGenerator` traits unchanged.
- `ScenarioStats`, `GateContext`, `CompiledFile` unchanged.
- New `pub fn ScenarioHandle::cancel(&self)` for explicit cancellation; old `stop()` stays for one minor as a synonym that calls `cancel.cancel()`.
- New optional `pub fn ScenarioHandle::state(&self) -> ScenarioState` (refs adoption-review issue #437, while we're touching the handle).

**Versioning.** Land as `1.14.0`. Conventional commit `feat:` carries the minor bump via release-please. CHANGELOG records the migration with a brief code-snippet.

**Open question for Opus.** Is there a deprecation glide path that lowers embedder pain? My take: no — the public API is small (4 functions, 1 struct field), embedders rebuilding is ~10 minutes. Adding deprecation shims doubles the code we maintain for one cycle.

### 11. Rollout — hard switch, no feature flag

**Recommendation.** Land via the landing-branch pattern (5 phase PRs + landing → main). Single `1.14.0` release. No "old-scheduler" feature gate.

**Rationale.**
- Two scheduler implementations behind a feature gate doubles maintenance cost.
- Sonda's release cadence (~weekly minor) means embedders are already pinning exact minor versions per the adoption review.
- The `#[non_exhaustive]` contract means upgrade breakage is at compile time, not runtime — embedders see the problem when they bump the dependency.
- We carry one path of code, document the change clearly in CHANGELOG, and move on.

**Open question for Opus.** Is there an in-production user we know of who would be hurt by a hard switch? My take: not that I'm aware of; the user base is workshop / CI / lab. If Opus knows of one, factor it in.

---

## Phase summary linked to decisions

| Phase | Decisions touched | Production surface change |
|---|---|---|
| 0 | — | Bench harness + this design doc |
| 1 | #4 (channels) | Internal-only; adapter is throwaway |
| 2 | #1, #2, #3 (sink/encoder/generator stay sync), #4 (drop adapter) | Async schedule loop |
| 3 | #5 (CLI runtime), #6 (CancellationToken) | `launch_scenario` async; ScenarioHandle field swap |
| 4 | #8 (resource limits), #9 (self-instrumentation) | New flags, new endpoint, tower middleware |
| 5 | — | Validation + docs only |

---

## Open questions consolidated for Opus

(For Opus to address in the review pass with the Phase 0 benchmark numbers in hand.)

1. Does `spawn_blocking` saturation become the bottleneck at any realistic scenario count?
2. Should `Arc<AtomicBool>` shutdown have a deprecation glide path or hard-break?
3. Should `/server/metrics` require API key or be public?
4. Should `--max-scenarios` reject (hard cap) or queue (backpressure)?
5. Is there a CLI workload where `multi_thread` runtime would help, or is `current_thread` sufficient?
6. Are any gate channels broadcast (multi-consumer)?
7. Does the bench reveal a bottleneck other than thread count (e.g., sink synchronization, lock contention)? If yes, restructure phases.

---

## Benchmark baseline (Phase 0 captured 2026-06-02)

Full numbers and methodology at `docs/refactor/async-scheduler-baseline-numbers.md`. Host: 11-core Apple M3 Pro, 36 GB RAM, macOS aarch64. 6 N-values from 1 to 500, 30s warmup + 60s measure window, 100 Hz emission per scenario.

### Summary table

| N | RSS (MB) | Threads | CPU % | Tick drift mean (ms) | Dropped-tick % |
|---|---|---|---|---|---|
| 1 | 8.1 | 2 | 0.7 | 17.80 | 0.00 |
| 10 | 8.7 | 11 | 3.9 | 22.19 | 0.00 |
| 50 | 10.0 | 51 | 13.2 | 26.18 | 0.00 |
| 100 | 11.3 | 101 | 21.0 | 22.53 | 0.00 |
| 250 | 15.7 | 251 | 46.3 | 26.36 | 0.00 |
| 500 | 23.2 | 501 | 87.2 | 24.08 | 0.00 |

### Findings that matter for the design

1. **The scheduler is NOT the bottleneck on a beefy host** at the N values tested. Tick drift stays flat (~22-26 ms mean across all N). Dropped ticks are 0% throughout. The decision tree assuming thread-count growth would visibly degrade tick fidelity — it doesn't, on macOS.
2. **Thread count IS the architectural ceiling** — confirmed. Threads grow 1:1 with N (501 at N=500). On macOS this is invisible; on Linux with default `ulimit -u` ~4096 and 8 MB stack reservations, this hits real limits.
3. **CPU saturates linearly with N**: 87% at N=500 on 11 cores. CPU is the next bottleneck the rewrite has to NOT regress on — it has nothing to do with thread vs task scheduling, so it should stay flat AFTER.
4. **RSS is benign on macOS** (8 → 23 MB across the full range) because Darwin lazily pages thread stacks. Linux at the same N would show ~4 GB virtual address space committed for stacks. **Cross-platform validation in Phase 5 is essential** — the macOS-only bench understates the architectural urgency.

### What changes in the architectural decisions

The bench numbers do **not** change any of the 11 decisions above. They DO reframe the **pitch**:

- The motivation isn't "we're hitting tick drift today on macOS" — we're not.
- The motivation IS "the model has no headroom for Linux deployments, multi-tenant exposure, or 1000+ scenario loads. Today's flat tick-drift curve will fall off a cliff at the platform-specific thread limit."

### Open questions surfaced by the bench (for Opus)

- The bench uses `SinkConfig::File { path: "/dev/null" }` because `MemorySink` / `ChannelSink` aren't exposed via `SinkConfig`. Tick-drift methodology is correspondingly coarse (per-second event-count delta, not per-event timestamp). Worth surfacing `MemorySink` via `SinkConfig::Memory` as a Phase 2.5 add for tighter Phase 5 measurement? Or is the coarse measurement enough for the AFTER comparison?
- The bench doesn't measure **scheduler wakeup latency distribution** (was in the original brief but not captured — would have required per-event timestamps from the sink). Capture this in Phase 5 via the `MemorySink` add above, or accept the proxy.
- Should we also run the bench on a Linux box (CI runner or a remote VM) for a more dramatic baseline that motivates the rewrite to stakeholders? My take: not for the design doc — the architectural argument stands. Useful for the release notes when the rewrite ships.

---

## Sign-off log

### 2026-06-02 — Opus reviewer — Phase 0 architectural sign-off

- **Verdict**: PROCEED-TO-PHASE-1 with six mandatory edits (applied to this doc and to the initiative plan).
- **Scorecard**: 10 of 11 decisions APPROVED; Decision #6 REVISED (drop deprecation-shim language, hard break with `#[non_exhaustive]`).
- **Bench numbers reviewed**: honest about limits, architectural argument stands. The reframed pitch in the baseline-numbers doc is correct.
- **Latent bug surfaced**: cross-POST `while:` resolver registry does not signal downstreams when an upstream is cancelled — exists TODAY with `Arc<AtomicBool>`, would have carried into the rewrite. Added to Phase 3 scope with "failing test first" requirement.
- **New Phase 2.5 inserted** to surface `MemorySink` via `SinkConfig::Memory` — without it, Phase 5's ≤5% drift acceptance criterion is unfalsifiable (the current per-1s event-count delta methodology has a ±10% bucket-drop threshold, larger than the regression we want to detect).
- **Phase 2 default-split into 2a + 2b** — smaller PRs review faster and isolate regressions.
- **Landing → main squash-merge mandatory** for atomic revert.
- **`clippy::await_holding_lock` added to the workspace lint set** — mechanically enforces Decision #7's lock-across-await rule.

```
### YYYY-MM-DD — <reviewer> — <decision area>

- Verdict: <approved | revise | needs more info>
- Notes:
```

---

*Last updated: 2026-06-02. Phase 0 ongoing.*
