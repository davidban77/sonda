# sonda-server — HTTP Control Plane

This is the binary crate for the HTTP REST API. It allows scenarios to be started, inspected, and
stopped over HTTP — enabling integration into CI pipelines, test harnesses, and dashboards.

## Design Principle

The API mirrors the CLI. Every endpoint corresponds to an operation that is also doable from the
command line. If a scenario cannot be expressed in YAML, it cannot be run via the API. This keeps the
two surfaces in sync and prevents behavior drift.

No business logic lives in this crate. All scenario validation and launch logic is delegated to
sonda-core via `prepare_entries` and `launch_scenario`. The server crate is pure HTTP plumbing.

## Module Layout

```
src/
├── main.rs             ← entrypoint: CLI arg parsing, axum router setup, tokio runtime,
│                         graceful shutdown (Ctrl+C stops all scenarios + joins threads with 5s timeout)
├── auth.rs             ← require_api_key middleware (Bearer token), unauthorized() helper,
│                         extract_bearer_token(), constant-time key comparison via subtle
├── routes/
│   ├── mod.rs          ← router_with_config() function: splits three sub-routers — public (/health),
│                         protected observability (/scenarios/{id}/stats, /scenarios/{id}/metrics,
│                         /metrics, /server/metrics), and protected control (/scenarios POST/GET,
│                         /scenarios/{id} GET/DELETE, /events). Auth + request-metrics middleware
│                         applied per protected sub-router; the control sub-router additionally
│                         carries the timeout + body-limit + global-concurrency tower stack.
│   ├── health.rs       ← GET /health → {"status": "ok"}
│   ├── events.rs       ← POST /events: synchronous single-event emission. Tagged-enum
│                         request body, dispatches on signal_type, builds LogEvent/MetricEvent,
│                         awaits sonda_core::emit::{emit_log, emit_metric} directly.
│                         Maps SondaError variants to HTTP status (Config→422, Sink→502, others→500).
│   ├── sink_warnings.rs ← shared loopback pre-flight helpers (extract_host,
│                          is_loopback_host, sink_loopback_warnings, collect_warnings_for_sink,
│                          log_warnings) used by both /scenarios and /events.
│   └── scenarios.rs    ← POST /scenarios (create single or multi-scenario batch from v2 YAML/JSON),
│                         GET /scenarios (list),
│                         GET /scenarios/{id} (inspect with stats),
│                         GET /scenarios/{id}/stats (detailed live stats),
│                         GET /scenarios/{id}/metrics (Prometheus text scrape),
│                         DELETE /scenarios/{id} (stop, return final stats, remove from map).
│                         parse_body() compiles v2 YAML/JSON via
│                         `sonda_core::compile_scenario_file` (empty
│                         `InMemoryPackResolver` — packs over HTTP deferred)
│                         and returns ParsedBody::Single or ParsedBody::Multi
│                         depending on how many entries the compilation produced.
│                         v1 YAML shapes are rejected up front with a migration hint
│                         (HTTP 400 + v2-scenarios.md pointer).
│                         post_scenario() dispatches to post_single_scenario()
│                         or post_multi_scenario(), list_scenarios(),
│                         get_scenario(), get_scenario_stats(),
│                         get_scenario_metrics(), delete_scenario().
└── state.rs            ← AppState: Arc<RwLock<HashMap<String, ScenarioHandle>>> + optional api_key

tests/
├── common/
│   └── mod.rs          ← shared test infrastructure: ServerGuard RAII, spawn_server(),
│                         spawn_server_with(), start_server(), start_server_with(), http_client().
│                         Spawn helpers use `--port 0` + read the stdout announce.
│                         All test files use `mod common;` for these helpers.
├── auth.rs             ← E2E tests: auth via --api-key flag, SONDA_API_KEY env var, no-key backwards compat
├── events.rs           ← POST /events E2E: logs + metrics happy paths, malformed body, unknown
│                         signal_type, missing fields, invalid sink config (422), sink-push 5xx (502),
│                         auth (401), loopback warning surfaced on success
├── health.rs           ← server startup, GET /health, unknown routes, SIGTERM shutdown
├── integration.rs      ← full lifecycle: POST metrics + logs → GET list → stats → DELETE → verify stopped
└── scenarios.rs        ← POST /scenarios unit-level tests (valid/invalid YAML, JSON, validation errors)
```

## Implemented API Surface (as of Slice 6.3)

| Method | Path                    | Description                                             |
|--------|-------------------------|---------------------------------------------------------|
| GET    | /health                 | Health check — always returns 200 OK                    |
| POST   | /scenarios              | Start scenario(s) from a v2 YAML or JSON body. Every body is compiled through `sonda_core::compile_scenario_file`; v1 YAML shapes are rejected with a 400 + migration hint. When the compilation produces exactly one entry the response is `{id, name, status}`; otherwise it is `{scenarios: [{id, name, status}, ...]}`. |
| GET    | /scenarios              | List all scenarios with id, name, status, elapsed       |
| GET    | /scenarios/{id}         | Inspect a scenario: detail + live stats                 |
| GET    | /scenarios/{id}/stats   | Detailed live stats: rate, target_rate, events, gap/burst state, uptime |
| GET    | /scenarios/{id}/metrics | Latest metrics in Prometheus text format (scrapeable)   |
| DELETE | /scenarios/{id}         | Stop a running scenario, return final stats, remove from map |
| POST   | /events                 | Emit one log or metric event synchronously. Body is a `signal_type`-tagged JSON object (`logs` or `metrics`) carrying `labels`, the per-branch payload (`log` or `metric`), `encoder`, and `sink`. The handler builds the event, delegates encoding + delivery to `sonda_core::emit::{emit_log, emit_metric}` inside `tokio::task::spawn_blocking`, and returns `{sent, signal_type, latency_ms, warnings}` once the sink ACKs. Sink-push 5xx → 502; sink/encoder config validation failure → 422; malformed body / unknown `signal_type` / missing per-branch field → 400. |

## Error Handling

All handlers use `.map_err()` with the `?` operator for lock acquisition and other fallible
operations. No handler uses `.expect()` or `.unwrap()` on lock guards. If the `AppState` scenarios
`RwLock` is poisoned (e.g., because a write handler panicked), all handlers return `500 Internal
Server Error` with a JSON error body instead of panicking. The per-scenario stats `RwLock` in
`ScenarioHandle` uses `into_inner()` to recover data from poisoned guards without panicking.

## Concurrency Model

Scenarios run as tokio tasks on the shared multi-thread runtime that hosts the axum HTTP layer
(spawned by `sonda_core::schedule::launch::launch_scenario`). sonda-core is async-native: the
runner loop, the encoder layer, and every sink share the same runtime as the HTTP handlers. The
axum handler stores `ScenarioHandle` instances in `AppState` and queries them via the lock-free
`stats_snapshot()` path. Per-request bounding is provided by `--max-inflight-requests` (a
`tower::limit::GlobalConcurrencyLimitLayer` over an `Arc<Semaphore>`); per-scenario bounding is
provided by `--max-scenarios`. Worker count defaults to `min(available_parallelism(), 16)` —
host-CPU- and cgroup-aware, with a hard ceiling so a 64-core production host doesn't spawn 64
worker threads.

## Authentication

API key authentication is opt-in. When configured, all `/scenarios/*` endpoints require a
`Authorization: Bearer <key>` header. The `/health` endpoint is always public.

```
# Via CLI flag:
cargo run -p sonda-server -- --port 8080 --api-key my-secret-key

# Via environment variable:
SONDA_API_KEY=my-secret-key cargo run -p sonda-server -- --port 8080
```

When no API key is provided (or an empty string is given), the server runs without authentication
and all endpoints are publicly accessible (backwards compatible behaviour).

## Startup

```
cargo run -p sonda-server -- --port 8080 --bind 0.0.0.0
```

Respects `RUST_LOG` env var for log level (default: `info`).

`--port 0` lets the OS assign a port. The server then prints
`{"sonda_server":{"port":N}}` to stdout; tracing logs go to stderr.

### CLI dispatch shim

`main.rs` checks `argv[1]` before clap and `exec`s the sibling `sonda` binary
when it matches `SONDA_SUBCOMMANDS`. Sibling resolved via `env::current_exe()`.
Keep `SONDA_SUBCOMMANDS` in sync with `sonda`'s clap definitions.

## Dependencies

| Crate              | Purpose                                                   |
|--------------------|-----------------------------------------------------------|
| `sonda-core`       | All scenario lifecycle logic (`launch_scenario`, etc.)    |
| `axum`             | HTTP routing and handler infrastructure                   |
| `tokio`            | Async runtime (full features)                             |
| `serde` + `serde_json` + `serde_yaml_ng` | Request/response serialization       |
| `anyhow`           | Error handling in binary code                             |
| `clap`             | CLI argument parsing                                      |
| `tower-http`       | Timeout + request-body limit middleware for control routes |
| `tracing` + `tracing-subscriber` | Structured logging                      |
| `subtle`           | Constant-time byte comparison for API key auth            |
| `uuid`             | Generating scenario IDs                                   |
