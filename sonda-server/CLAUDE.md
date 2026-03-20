# sonda-server — HTTP Control Plane

> **Status: Post-MVP.** This crate is scaffolded but not implemented until Phase 3.
> Do not add dependencies or code here until Phases 0–2 are complete.

This is the binary crate for the HTTP REST API. It allows scenarios to be started, inspected, and
stopped over HTTP — enabling integration into CI pipelines, test harnesses, and dashboards.

## Design Principle

The API mirrors the CLI. Every endpoint corresponds to an operation that is also doable from the
command line. If a scenario cannot be expressed in YAML, it cannot be run via the API. This keeps the
two surfaces in sync and prevents behavior drift.

## Planned Module Layout

```
src/
├── main.rs             ← entrypoint, axum router setup, tokio runtime
├── routes/
│   ├── mod.rs
│   ├── scenarios.rs    ← POST/GET/DELETE /scenarios, GET /scenarios/:id
│   └── stats.rs        ← GET /scenarios/:id/stats
├── state.rs            ← shared app state (running scenarios, handles)
└── config.rs           ← server config: port, bind address, log level
```

## Planned API Surface

| Method | Path                   | Description                                    |
|--------|------------------------|------------------------------------------------|
| POST   | /scenarios             | Start a new scenario from YAML/JSON body       |
| GET    | /scenarios             | List all running scenarios                     |
| GET    | /scenarios/:id         | Inspect a scenario: config, tick count, errors |
| DELETE | /scenarios/:id         | Stop and remove a running scenario             |
| GET    | /scenarios/:id/stats   | Live stats: rate, total events, gap/burst state|

## Concurrency Model

Each scenario runs on a dedicated thread (spawned via `std::thread::spawn`). The axum handler
communicates with scenario threads via channels. This keeps sonda-core synchronous while the server
handles HTTP I/O asynchronously via tokio.

## Dependencies

This crate depends on:
- `sonda-core` (workspace dependency)
- `axum` for HTTP routing
- `tokio` with full features for async runtime
- `serde` + `serde_json` for request/response bodies
- `anyhow` for error handling

## When to Start

Do not begin implementation until:
- sonda-core has stable generator, encoder, and sink traits
- The scenario runner is working and tested
- At least two encoders and two sinks are implemented
- Multi-scenario concurrency (Phase 2) is validated with threads + channels
