# sonda-server — HTTP Control Plane

This is the binary crate for the HTTP REST API. It allows scenarios to be started, inspected, and
stopped over HTTP — enabling integration into CI pipelines, test harnesses, and dashboards.

## Design Principle

The API mirrors the CLI. Every endpoint corresponds to an operation that is also doable from the
command line. If a scenario cannot be expressed in YAML, it cannot be run via the API. This keeps the
two surfaces in sync and prevents behavior drift.

No business logic lives in this crate. All scenario validation and launch logic is delegated to
sonda-core via `validate_entry` and `launch_scenario`. The server crate is pure HTTP plumbing.

## Module Layout

```
src/
├── main.rs             ← entrypoint: CLI arg parsing, axum router setup, tokio runtime,
│                         graceful shutdown (Ctrl+C stops all running scenarios)
├── routes/
│   ├── mod.rs          ← router() function wires all routes; re-exports submodules
│   ├── health.rs       ← GET /health → {"status": "ok"}
│   └── scenarios.rs    ← POST /scenarios (create), GET /scenarios (list),
│                         GET /scenarios/{id} (inspect with stats),
│                         GET /scenarios/{id}/stats (detailed live stats),
│                         GET /scenarios/{id}/metrics (Prometheus text scrape),
│                         DELETE /scenarios/{id} (stop, return final stats, remove from map)
│                         parse_body(), parse_yaml_body(), parse_json_body(),
│                         post_scenario(), list_scenarios(), get_scenario(),
│                         get_scenario_stats(), get_scenario_metrics(),
│                         delete_scenario()
└── state.rs            ← AppState: Arc<RwLock<HashMap<String, ScenarioHandle>>>

tests/
├── health.rs           ← server startup, GET /health, unknown routes, SIGTERM shutdown
├── integration.rs      ← full lifecycle: POST metrics + logs → GET list → stats → DELETE → verify stopped
└── scenarios.rs        ← POST /scenarios unit-level tests (valid/invalid YAML, JSON, validation errors)
```

## Implemented API Surface (as of Slice 6.3)

| Method | Path                    | Description                                             |
|--------|-------------------------|---------------------------------------------------------|
| GET    | /health                 | Health check — always returns 200 OK                    |
| POST   | /scenarios              | Start a new scenario from YAML or JSON body, returns ID |
| GET    | /scenarios              | List all scenarios with id, name, status, elapsed       |
| GET    | /scenarios/{id}         | Inspect a scenario: detail + live stats                 |
| GET    | /scenarios/{id}/stats   | Detailed live stats: rate, target_rate, events, gap/burst state, uptime |
| GET    | /scenarios/{id}/metrics | Latest metrics in Prometheus text format (scrapeable)   |
| DELETE | /scenarios/{id}         | Stop a running scenario, return final stats, remove from map |

## Concurrency Model

Each scenario runs on a dedicated thread (spawned by `sonda_core::schedule::launch::launch_scenario`).
The axum handler stores and queries `ScenarioHandle` instances from sonda-core. This keeps sonda-core
synchronous while the server handles HTTP I/O asynchronously via tokio.

## Startup

```
cargo run -p sonda-server -- --port 8080 --bind 0.0.0.0
```

Respects `RUST_LOG` env var for log level (default: `info`).

## Dependencies

| Crate              | Purpose                                                   |
|--------------------|-----------------------------------------------------------|
| `sonda-core`       | All scenario lifecycle logic (`launch_scenario`, etc.)    |
| `axum`             | HTTP routing and handler infrastructure                   |
| `tokio`            | Async runtime (full features)                             |
| `serde` + `serde_json` + `serde_yaml` | Request/response serialization       |
| `anyhow`           | Error handling in binary code                             |
| `clap`             | CLI argument parsing                                      |
| `tower-http`       | CORS and trace middleware                                 |
| `tracing` + `tracing-subscriber` | Structured logging                      |
| `uuid`             | Generating scenario IDs                                   |
