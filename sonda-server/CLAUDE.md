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
│   └── scenarios.rs    ← GET /scenarios (list), GET /scenarios/:id (inspect)
└── state.rs            ← AppState: Arc<RwLock<HashMap<String, ScenarioHandle>>>
```

## Implemented API Surface (as of Slice 3.3)

| Method | Path             | Description                                    |
|--------|------------------|------------------------------------------------|
| GET    | /health          | Health check — always returns 200 OK           |
| GET    | /scenarios       | List all scenarios with id, name, status, elapsed |
| GET    | /scenarios/:id   | Inspect a scenario: detail + live stats        |

## Planned API Surface (Slices 3.2–3.5)

| Method | Path                   | Description                                    |
|--------|------------------------|------------------------------------------------|
| POST   | /scenarios             | Start a new scenario from YAML/JSON body       |
| GET    | /scenarios             | List all running scenarios                     |
| GET    | /scenarios/:id         | Inspect a scenario: config, tick count, errors |
| DELETE | /scenarios/:id         | Stop and remove a running scenario             |
| GET    | /scenarios/:id/stats   | Live stats: rate, total events, gap/burst state|

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
