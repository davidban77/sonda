# Phase 3 — sonda-server Implementation Plan

**Goal:** HTTP REST API control plane for starting, inspecting, and stopping scenarios over HTTP.

**Prerequisite:** Phase 2 complete — multi-scenario concurrency works via threads, log pipeline stable,
burst windows implemented.

**Final exit criteria:** `sonda-server` accepts scenario YAML via POST, runs scenarios concurrently,
exposes live stats, supports graceful stop, and deploys as a single static binary.

---

## Slice 3.1 — Server Skeleton & Health Check

### Input state
- Phase 2 passes all gates.
- `sonda-core` runner supports shutdown via `Arc<AtomicBool>`.

### Specification

**Files to modify:**
- `sonda-server/Cargo.toml` — activate dependencies:
  ```toml
  [dependencies]
  sonda-core = { workspace = true }
  axum = "0.7"
  tokio = { version = "1", features = ["full"] }
  serde = { workspace = true }
  serde_json = { workspace = true }
  anyhow = { workspace = true }
  tower-http = { version = "0.5", features = ["cors", "trace"] }
  tracing = "0.1"
  tracing-subscriber = "0.3"
  uuid = { version = "1", features = ["v4"] }
  ```

**Files to create:**
- `sonda-server/src/state.rs`:
  ```rust
  pub struct AppState {
      pub scenarios: Arc<RwLock<HashMap<String, ScenarioHandle>>>,
  }

  pub struct ScenarioHandle {
      pub id: String,
      pub config_name: String,
      pub started_at: Instant,
      pub shutdown: Arc<AtomicBool>,
      pub thread: Option<JoinHandle<Result<(), SondaError>>>,
      pub stats: Arc<RwLock<ScenarioStats>>,
  }
  ```

- `sonda-server/src/routes/mod.rs`:
  ```rust
  pub mod health;
  pub fn router(state: AppState) -> Router { ... }
  ```

- `sonda-server/src/routes/health.rs`:
  - `GET /health` → `200 OK` with `{"status": "ok"}`.

**Files to rewrite:**
- `sonda-server/src/main.rs`:
  - Initialize `tracing_subscriber`.
  - Parse CLI: `--port` (default 8080), `--bind` (default 0.0.0.0).
  - Build router, create `AppState`.
  - Start axum server with `tokio::net::TcpListener`.
  - Graceful shutdown via `tokio::signal::ctrl_c()`.

### Output files
| File | Status |
|------|--------|
| `sonda-server/Cargo.toml` | modified |
| `sonda-server/src/main.rs` | rewritten |
| `sonda-server/src/state.rs` | new |
| `sonda-server/src/routes/mod.rs` | new |
| `sonda-server/src/routes/health.rs` | new |

### Test criteria
- Server starts and binds to port.
- `GET /health` → 200 with `{"status": "ok"}`.
- Unknown route → 404.
- Ctrl+C → server shuts down cleanly.

### Review criteria
- Uses `axum::Router` with shared state via `Arc`.
- `tracing` for structured logging (not `println!`).
- No business logic in server crate — only HTTP plumbing.
- Graceful shutdown waits for in-flight requests.

### UAT criteria
- `cargo run -p sonda-server -- --port 9090` → server starts, logs bind address.
- `curl http://localhost:9090/health` → `{"status":"ok"}`.
- Ctrl+C → clean exit, no panics.

---

## Slice 3.2 — POST /scenarios

### Input state
- Slice 3.1 passes all gates.

### Specification

**Files to create:**
- `sonda-server/src/routes/scenarios.rs`:
  - `POST /scenarios`:
    - Accept YAML body (`Content-Type: application/x-yaml` or `text/yaml`) or JSON.
    - Deserialize to `ScenarioConfig` or `LogScenarioConfig` (detect via `signal_type` field).
    - Validate via `sonda_core::config::validate::validate_config()`.
    - Generate UUID for scenario ID.
    - Create `Arc<AtomicBool>` shutdown flag.
    - Spawn `std::thread::spawn` running `sonda_core::schedule::runner::run()`.
    - Store `ScenarioHandle` in `AppState`.
    - Response: `201 Created`:
      ```json
      { "id": "uuid", "name": "metric_name", "status": "running" }
      ```

  - Error responses:
    - Invalid YAML/JSON → `400 Bad Request` with message.
    - Validation failure → `422 Unprocessable Entity` with field-level details.
    - Internal error → `500 Internal Server Error`.

**Files to modify:**
- `sonda-server/src/routes/mod.rs` — add `pub mod scenarios`, wire routes.

### Output files
| File | Status |
|------|--------|
| `sonda-server/src/routes/scenarios.rs` | new |
| `sonda-server/src/routes/mod.rs` | modified |

### Test criteria
- POST valid YAML → 201, scenario ID returned.
- POST invalid YAML → 400 with error message.
- POST valid YAML with rate=0 → 422 with validation detail.
- POST → scenario thread is running (verify via AppState).

### Review criteria
- Thread is `std::thread::spawn`, not tokio task (core is sync).
- Shutdown flag stored for later DELETE.
- Config deserialization handles both YAML and JSON content types.
- Error responses include enough detail to debug.

### UAT criteria
- `curl -X POST -H "Content-Type: text/yaml" --data-binary @examples/basic-metrics.yaml http://localhost:8080/scenarios` → 201 with scenario ID.
- Scenario actually runs (output appears at configured sink).
- POST garbage → 400 with "invalid YAML" message.

---

## Slice 3.3 — GET /scenarios (List & Inspect)

### Input state
- Slice 3.2 passes all gates.

### Specification

**Files to modify:**
- `sonda-server/src/routes/scenarios.rs` — add:
  - `GET /scenarios`:
    ```json
    {
      "scenarios": [
        { "id": "uuid", "name": "interface_oper_state", "status": "running", "elapsed_secs": 45.2 }
      ]
    }
    ```

  - `GET /scenarios/:id`:
    ```json
    {
      "id": "uuid",
      "name": "interface_oper_state",
      "status": "running",
      "elapsed_secs": 45.2,
      "stats": { "total_events": 45000, "current_rate": 998.5, "bytes_emitted": 2340000, "errors": 0 }
    }
    ```
    - 404 for unknown ID.

**Files to create (in sonda-core):**
- `sonda-core/src/schedule/stats.rs`:
  ```rust
  #[derive(Debug, Clone, Default)]
  pub struct ScenarioStats {
      pub total_events: u64,
      pub bytes_emitted: u64,
      pub current_rate: f64,
      pub errors: u64,
      pub in_gap: bool,
      pub in_burst: bool,
  }
  ```

**Files to modify:**
- `sonda-core/src/schedule/runner.rs` — accept optional `Arc<RwLock<ScenarioStats>>`, update stats each tick.
- `sonda-core/src/schedule/mod.rs` — add `pub mod stats`.

### Output files
| File | Status |
|------|--------|
| `sonda-server/src/routes/scenarios.rs` | modified |
| `sonda-core/src/schedule/stats.rs` | new |
| `sonda-core/src/schedule/runner.rs` | modified |
| `sonda-core/src/schedule/mod.rs` | modified |

### Test criteria
- Start 2 scenarios → GET /scenarios → both listed.
- GET /scenarios/:id → correct name, status, elapsed time.
- GET /scenarios/:id → stats.total_events > 0 after running for 2 seconds.
- GET /scenarios/nonexistent → 404.
- Stats update frequency: within 1 second of real time.

### Review criteria
- Stats use `Arc<RwLock<ScenarioStats>>` — write lock held only briefly per tick.
- `current_rate` is calculated (events in last second), not just `config.rate`.
- Runner's stats update does not add significant overhead.
- JSON serialization uses `serde_json`.

### UAT criteria
- Start scenario via POST → wait 3 seconds → GET /scenarios/:id/stats → total_events > 0.
- GET /scenarios → list includes scenario with correct name.

---

## Slice 3.4 — DELETE /scenarios/:id

### Input state
- Slice 3.3 passes all gates.

### Specification

**Files to modify:**
- `sonda-server/src/routes/scenarios.rs` — add:
  - `DELETE /scenarios/:id`:
    - Set shutdown `AtomicBool` to `true`.
    - Join thread with 5-second timeout.
    - Update status to `"stopped"`.
    - Response: `200 OK`:
      ```json
      { "id": "uuid", "status": "stopped", "total_events": 45000 }
      ```
    - If thread doesn't join in 5s → status `"force_stopped"`, log warning.
    - DELETE on already-stopped → `200 OK` (idempotent).
    - DELETE on unknown ID → `404 Not Found`.

### Output files
| File | Status |
|------|--------|
| `sonda-server/src/routes/scenarios.rs` | modified |

### Test criteria
- Start scenario → DELETE → thread exits, status "stopped".
- DELETE returns final stats (total_events).
- DELETE already-stopped → 200 OK.
- DELETE unknown → 404.
- Sink is flushed before thread exits.

### Review criteria
- Thread join has timeout (not infinite block).
- `AtomicBool` ordering is correct (`Ordering::Relaxed` is fine for shutdown flag).
- Idempotent DELETE.

### UAT criteria
- `curl -X POST ... localhost:8080/scenarios` → get ID → `curl -X DELETE localhost:8080/scenarios/$ID` → 200 with final stats.
- Verify scenario actually stops (no more output to sink).

---

## Slice 3.5 — Stats Endpoint

### Input state
- Slice 3.4 passes all gates.

### Specification

**Files to modify:**
- `sonda-server/src/routes/scenarios.rs` — add:
  - `GET /scenarios/:id/stats`:
    ```json
    {
      "total_events": 45000,
      "current_rate": 998.5,
      "target_rate": 1000,
      "bytes_emitted": 2340000,
      "errors": 0,
      "uptime_secs": 45.2,
      "state": "running",
      "in_gap": false,
      "in_burst": false
    }
    ```
    - Lightweight: reads latest stats snapshot, no computation.
    - 404 for unknown ID.

### Output files
| File | Status |
|------|--------|
| `sonda-server/src/routes/scenarios.rs` | modified |

### Test criteria
- Stats endpoint returns all expected fields.
- Fields update as scenario progresses.
- `in_gap` is true during gap window.
- After scenario stopped: returns final stats with `state: "stopped"`.
- Unknown ID → 404.

### Review criteria
- Read-only endpoint, no write lock.
- Response includes both `current_rate` (measured) and `target_rate` (configured).
- `uptime_secs` calculated from `started_at`, not stored.

### UAT criteria
- Start scenario → poll stats every second for 5 seconds → verify total_events increasing.
- Start scenario with gaps → verify `in_gap` toggles at correct times.
- Pipe `curl` to `jq` → verify JSON structure.

---

## Slice 3.6 — Static Binary, Docker & Integration Tests

### Input state
- Slice 3.5 passes all gates.

### Specification

**Files to create:**
- `sonda-server/tests/integration.rs`:
  - Start server in background (bind to random port).
  - POST scenario → 201.
  - GET /scenarios → scenario listed.
  - Wait 3 seconds → GET /scenarios/:id/stats → total_events > 0.
  - DELETE → 200, status "stopped".
  - GET /scenarios → shows stopped.

- `Dockerfile`:
  ```dockerfile
  FROM scratch
  COPY target/x86_64-unknown-linux-musl/release/sonda /sonda
  COPY target/x86_64-unknown-linux-musl/release/sonda-server /sonda-server
  ENTRYPOINT ["/sonda-server"]
  ```

- `docker-compose.yml` (demo: sonda-server + VictoriaMetrics):
  ```yaml
  services:
    sonda-server:
      build: .
      ports: ["8080:8080"]
    victoriametrics:
      image: victoriametrics/victoria-metrics
      ports: ["8428:8428"]
  ```

**Files to modify:**
- `README.md` — add server section: API reference, deployment guide, Docker instructions.
- `.github/workflows/ci.yml` — add sonda-server build and integration test steps.

### Output files
| File | Status |
|------|--------|
| `sonda-server/tests/integration.rs` | new |
| `Dockerfile` | new |
| `docker-compose.yml` | new |
| `README.md` | modified |
| `.github/workflows/ci.yml` | modified |

### Test criteria
- Integration test: full lifecycle (POST → GET → stats → DELETE) passes.
- Static binary: `cargo build --release --target x86_64-unknown-linux-musl -p sonda-server` succeeds.
- Binary is statically linked: `file` command confirms.
- Docker build succeeds: `docker build .` completes.

### Review criteria
- Integration test uses random port (no conflicts in CI).
- Integration test has reasonable timeouts (not flaky).
- Dockerfile uses `scratch` base (minimal image).
- README API reference covers all endpoints with examples.
- CI builds and tests both `sonda` and `sonda-server`.

### UAT criteria
- **Full API lifecycle** via curl:
  1. Start server.
  2. POST scenario → get ID.
  3. GET /scenarios → listed.
  4. GET /scenarios/:id/stats → events increasing.
  5. DELETE → stopped.
  6. Verify output at sink.
- **Docker**: `docker compose up` → sonda-server starts, accepts scenarios.
- **Binary size**: sonda-server musl binary < 15MB.
- **Memory**: server with 5 concurrent scenarios at 1000/sec each → RSS < 50MB.

---

## Dependency Graph

```
Slice 3.1 (skeleton + health)
  ↓
Slice 3.2 (POST /scenarios)
  ↓
Slice 3.3 (GET list/inspect + stats model)
  ↓
Slice 3.4 (DELETE)
  ↓
Slice 3.5 (stats endpoint)
  ↓
Slice 3.6 (static binary, Docker, integration tests)
```

Strictly sequential — each endpoint builds on state management from the prior slice.

---

## Post-Phase 3

With all four phases complete, Sonda has a CLI, multi-signal support, concurrent execution, and a
REST API. Future work (not designed here):

- Kafka sink (evaluate `rdkafka` vs pure-Rust client for musl compatibility)
- Prometheus remote-write encoder (protobuf via `prost`)
- Dynamic label cardinality (rotating hostnames, pod churn simulation)
- OpenTelemetry encoder (OTLP)
- Clustering (deferred until single-instance limits are understood)