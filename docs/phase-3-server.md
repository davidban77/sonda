# Phase 3 — sonda-server Implementation Plan

**Goal:** HTTP REST API control plane for starting, inspecting, and stopping scenarios over HTTP.

**Prerequisite:** Phase 2 complete — multi-scenario concurrency works via threads, log pipeline stable,
burst windows implemented.

**Final exit criteria:** `sonda-server` accepts scenario YAML via POST, runs scenarios concurrently,
exposes live stats, supports graceful stop, and deploys as a single static binary.

**Design principle — DRY:** The server is a thin HTTP layer over sonda-core. It shares the same
scenario lifecycle primitives (`ScenarioHandle`, `launch_scenario`, `validate_entry`) as the CLI.
No business logic is duplicated between crates. Both CLI and server are consumers of a single
core launch/stop/stats API.

---

## Slice 3.0 — Core Lifecycle Refactor (DRY Foundation)

### Motivation

Before adding the server, extract the shared "validate → create sink → spawn runner → manage
lifecycle" pattern that is currently duplicated across CLI `main.rs`, `multi_runner.rs`, and would
be duplicated again in the server. This slice creates the shared foundation that both CLI and server
will use.

### Input state
- Phase 2 passes all gates.
- `sonda-core` runner supports shutdown via `Arc<AtomicBool>`.

### Specification

**Files to create:**

- `sonda-core/src/schedule/stats.rs`:
  ```rust
  /// Live statistics for a running scenario, updated by the runner each tick.
  #[derive(Debug, Clone, Default, Serialize)]
  pub struct ScenarioStats {
      pub total_events: u64,
      pub bytes_emitted: u64,
      pub current_rate: f64,
      pub errors: u64,
      pub in_gap: bool,
      pub in_burst: bool,
  }
  ```

- `sonda-core/src/schedule/handle.rs`:
  ```rust
  /// A running scenario's lifecycle handle.
  ///
  /// Returned by `launch_scenario`. Provides shutdown, join, and stats access.
  /// Used identically by the CLI, multi_runner, and sonda-server.
  pub struct ScenarioHandle {
      pub id: String,
      pub name: String,
      pub shutdown: Arc<AtomicBool>,
      pub thread: Option<JoinHandle<Result<(), SondaError>>>,
      pub started_at: Instant,
      pub stats: Arc<RwLock<ScenarioStats>>,
  }

  impl ScenarioHandle {
      /// Signal the scenario to stop.
      pub fn stop(&self) { ... }

      /// Check whether the scenario thread is still running.
      pub fn is_running(&self) -> bool { ... }

      /// Join the thread, consuming it. Returns the thread result.
      /// Blocks until the thread exits or the optional timeout expires.
      pub fn join(&mut self, timeout: Option<Duration>) -> Result<(), SondaError> { ... }

      /// Elapsed time since the scenario started.
      pub fn elapsed(&self) -> Duration { ... }

      /// Read the latest stats snapshot.
      pub fn stats_snapshot(&self) -> ScenarioStats { ... }
  }
  ```

- `sonda-core/src/schedule/launch.rs`:
  ```rust
  /// Validate any scenario entry (metrics or logs).
  ///
  /// Dispatches to `validate_config` or `validate_log_config` based on the
  /// entry variant. Centralizes the match that CLI and server both need.
  pub fn validate_entry(entry: &ScenarioEntry) -> Result<(), SondaError> { ... }

  /// Launch a single scenario on a new OS thread.
  ///
  /// Creates the sink, spawns the appropriate runner, wires up the shutdown
  /// flag and stats, and returns a `ScenarioHandle` for lifecycle management.
  ///
  /// This is the single function that both CLI and server call to start a
  /// scenario. No scenario launch logic exists outside this function.
  pub fn launch_scenario(
      id: String,
      entry: ScenarioEntry,
      shutdown: Arc<AtomicBool>,
  ) -> Result<ScenarioHandle, SondaError> { ... }
  ```

**Files to modify:**

- `sonda-core/src/schedule/mod.rs` — add `pub mod stats`, `pub mod handle`, `pub mod launch`.

- `sonda-core/src/schedule/runner.rs` — extend `run_with_sink` to accept an optional
  `Arc<RwLock<ScenarioStats>>`. When present, update `total_events`, `bytes_emitted`,
  `current_rate`, `in_gap`, `in_burst`, and `errors` on each tick. When `None`, behavior is
  unchanged (no overhead). The stats write lock is held only for the brief counter update, not
  during encode/write.

- `sonda-core/src/schedule/log_runner.rs` — same stats integration as `runner.rs`.

- `sonda-core/src/schedule/multi_runner.rs` — refactor `run_multi` to use `launch_scenario`
  and `ScenarioHandle` instead of raw `thread::spawn` + `JoinHandle`. The function now returns
  `Vec<ScenarioHandle>` (or collects errors as before). This eliminates the duplicated
  `match entry { Metrics => ..., Logs => ... }` dispatch.

- `sonda-core/src/lib.rs` — re-export `ScenarioHandle`, `ScenarioStats`, `launch_scenario`,
  `validate_entry`.

- `sonda/src/main.rs` — refactor all three subcommands to use `validate_entry` + `launch_scenario`:
  ```rust
  // Before (duplicated per signal type):
  let config = config::load_config(args)?;
  validate_config(&config)?;
  let mut sink = create_sink(&config.sink)?;
  run_with_sink(&config, sink.as_mut(), Some(running.as_ref()))?;

  // After (unified):
  let entry = config::load_as_entry(args)?;
  validate_entry(&entry)?;
  let handle = launch_scenario(uuid(), entry, running)?;
  handle.join(None)?;
  ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/schedule/stats.rs` | new |
| `sonda-core/src/schedule/handle.rs` | new |
| `sonda-core/src/schedule/launch.rs` | new |
| `sonda-core/src/schedule/mod.rs` | modified |
| `sonda-core/src/schedule/runner.rs` | modified |
| `sonda-core/src/schedule/log_runner.rs` | modified |
| `sonda-core/src/schedule/multi_runner.rs` | modified |
| `sonda-core/src/lib.rs` | modified |
| `sonda/src/main.rs` | modified |

### Test criteria
- `validate_entry` dispatches correctly for both Metrics and Logs entries.
- `launch_scenario` returns a handle whose thread is running.
- `handle.stop()` + `handle.join()` exits cleanly.
- `handle.stats_snapshot()` returns non-zero `total_events` after running briefly.
- `multi_runner::run_multi` still passes all existing tests (refactored to use handles internally).
- CLI `sonda metrics` and `sonda logs` still work end-to-end (behavior unchanged, code path changed).
- All existing tests continue to pass with no regressions.

### Review criteria
- **Zero business logic duplication.** The `match ScenarioEntry { Metrics => ..., Logs => ... }`
  dispatch exists in exactly one place: `launch_scenario`.
- Stats update is behind an `Option` — no overhead when stats are not requested.
- `ScenarioHandle` is `Send` (can be stored in server state across await points).
- CLI behavior is identical before and after the refactor.

### UAT criteria
- `sonda metrics --name test --rate 10 --duration 2s` → works as before.
- `sonda logs --mode template --rate 10 --duration 2s` → works as before.
- `sonda run --scenario examples/multi-scenario.yaml` → works as before.
- `cargo test --workspace` → all tests pass.

---

## Slice 3.1 — Server Skeleton & Health Check

### Input state
- Slice 3.0 passes all gates.
- `ScenarioHandle`, `launch_scenario`, `validate_entry`, and `ScenarioStats` exist in sonda-core.

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
  serde_yaml = { workspace = true }
  anyhow = { workspace = true }
  tower-http = { version = "0.5", features = ["cors", "trace"] }
  tracing = "0.1"
  tracing-subscriber = "0.3"
  uuid = { version = "1", features = ["v4"] }
  ```

**Files to create:**
- `sonda-server/src/state.rs`:
  ```rust
  /// Shared application state for the HTTP server.
  ///
  /// Holds a map of running `ScenarioHandle`s from sonda-core.
  /// No scenario lifecycle logic lives here — just the container.
  pub struct AppState {
      pub scenarios: Arc<RwLock<HashMap<String, ScenarioHandle>>>,
  }
  ```
  Note: `ScenarioHandle` is imported from `sonda_core::schedule::handle`, NOT redefined.

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
  - Graceful shutdown via `tokio::signal::ctrl_c()` — on shutdown, iterate all handles and
    call `handle.stop()`.

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
- `ScenarioHandle` is imported from sonda-core, not redefined.
- Graceful shutdown stops all running scenarios via `handle.stop()`.

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
    - Deserialize body to `ScenarioEntry` (which handles `signal_type` detection automatically
      via the existing serde tag). For convenience, also attempt deserialization as plain
      `ScenarioConfig` (metrics) or `LogScenarioConfig` (logs) when no `signal_type` field is
      present, wrapping the result in the appropriate `ScenarioEntry` variant.
    - Call `sonda_core::schedule::launch::validate_entry(&entry)`.
    - Generate UUID for scenario ID.
    - Create `Arc<AtomicBool>` shutdown flag (initialized to `true`).
    - Call `sonda_core::schedule::launch::launch_scenario(id, entry, shutdown)`.
    - Store returned `ScenarioHandle` in `AppState`.
    - Response: `201 Created`:
      ```json
      { "id": "uuid", "name": "metric_name", "status": "running" }
      ```

  - Error responses:
    - Invalid YAML/JSON → `400 Bad Request` with message.
    - Validation failure → `422 Unprocessable Entity` with detail.
    - Internal error → `500 Internal Server Error`.

  **DRY note:** The handler body is ~20 lines of HTTP plumbing. All validation and launch
  logic is a single function call to sonda-core. No `match Metrics/Logs` dispatch in server code.

**Files to modify:**
- `sonda-server/src/routes/mod.rs` — add `pub mod scenarios`, wire routes.

### Output files
| File | Status |
|------|--------|
| `sonda-server/src/routes/scenarios.rs` | new |
| `sonda-server/src/routes/mod.rs` | modified |

### Test criteria
- POST valid metrics YAML → 201, scenario ID returned, handle in AppState.
- POST valid logs YAML → 201, scenario ID returned.
- POST YAML with `signal_type: metrics` → 201 (ScenarioEntry format).
- POST invalid YAML → 400 with error message.
- POST valid YAML with rate=0 → 422 with validation detail.
- POST → scenario thread is running (verify via `handle.is_running()`).

### Review criteria
- Uses `launch_scenario` from sonda-core — no direct `thread::spawn` in server code.
- Uses `validate_entry` from sonda-core — no manual validation dispatch.
- Config deserialization handles both YAML and JSON content types.
- Error responses include enough detail to debug.

### UAT criteria
- `curl -X POST -H "Content-Type: text/yaml" --data-binary @examples/basic-metrics.yaml http://localhost:8080/scenarios` → 201 with scenario ID.
- `curl -X POST -H "Content-Type: text/yaml" --data-binary @examples/log-template.yaml http://localhost:8080/scenarios` → 201 (logs scenario).
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
    Status is derived from `handle.is_running()` — no separate status field to maintain.

  - `GET /scenarios/{id}`:
    ```json
    {
      "id": "uuid",
      "name": "interface_oper_state",
      "status": "running",
      "elapsed_secs": 45.2,
      "stats": { "total_events": 45000, "current_rate": 998.5, "bytes_emitted": 2340000, "errors": 0 }
    }
    ```
    `elapsed_secs` is computed from `handle.elapsed()`.
    `stats` is read from `handle.stats_snapshot()`.
    404 for unknown ID.

  **DRY note:** Stats are already wired into the runners since Slice 3.0. This slice adds
  only the HTTP read endpoints — no sonda-core changes needed.

### Output files
| File | Status |
|------|--------|
| `sonda-server/src/routes/scenarios.rs` | modified |

### Test criteria
- Start 2 scenarios → GET /scenarios → both listed.
- GET /scenarios/{id} → correct name, status, elapsed time.
- GET /scenarios/{id} → stats.total_events > 0 after running for 2 seconds.
- GET /scenarios/nonexistent → 404.
- Stats update frequency: within 1 second of real time.

### Review criteria
- Uses `handle.stats_snapshot()` — no direct `RwLock` access in server code.
- Uses `handle.is_running()` — no thread state inspection in server code.
- Uses `handle.elapsed()` — no `Instant` arithmetic in server code.
- JSON serialization uses `serde_json`.

### UAT criteria
- Start scenario via POST → wait 3 seconds → GET /scenarios/{id} → stats.total_events > 0.
- GET /scenarios → list includes scenario with correct name.

---

## Slice 3.4 — DELETE /scenarios/{id}

### Input state
- Slice 3.3 passes all gates.

### Specification

**Files to modify:**
- `sonda-server/src/routes/scenarios.rs` — add:
  - `DELETE /scenarios/{id}`:
    - Call `handle.stop()` to signal shutdown.
    - Call `handle.join(Some(Duration::from_secs(5)))` to wait with timeout.
    - Read final stats via `handle.stats_snapshot()`.
    - Response: `200 OK`:
      ```json
      { "id": "uuid", "status": "stopped", "total_events": 45000 }
      ```
    - If join times out → status `"force_stopped"`, log warning via `tracing::warn!`.
    - DELETE on already-stopped → `200 OK` (idempotent).
    - DELETE on unknown ID → `404 Not Found`.

  **DRY note:** Stop + join logic is in `ScenarioHandle` methods. The handler is ~15 lines.

### Output files
| File | Status |
|------|--------|
| `sonda-server/src/routes/scenarios.rs` | modified |

### Test criteria
- Start scenario → DELETE → thread exits, status "stopped".
- DELETE returns final stats (total_events).
- DELETE already-stopped → 200 OK.
- DELETE unknown → 404.
- Sink is flushed before thread exits (runner already does this).

### Review criteria
- Uses `handle.stop()` + `handle.join(timeout)` — no direct `AtomicBool` or `JoinHandle` in server.
- Idempotent DELETE.
- `tracing::warn!` for timeout cases (not `eprintln!`).

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
  - `GET /scenarios/{id}/stats`:
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
    - `state` from `handle.is_running()`.
    - `uptime_secs` from `handle.elapsed()`.
    - All other fields from `handle.stats_snapshot()`.
    - `target_rate` is the configured rate, stored on the handle or read from the original config.
    - 404 for unknown ID.

  **DRY note:** This endpoint reads from the same `ScenarioStats` that the runners update.
  No computation or aggregation in server code — just serialization.

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
- `uptime_secs` calculated from `handle.elapsed()`, not stored.

### UAT criteria
- Start scenario → poll stats every second for 5 seconds → verify total_events increasing.
- Start scenario with gaps → verify `in_gap` toggles at correct times.
- Pipe `curl` to `jq` → verify JSON structure.

---

## Slice 3.6 — Integration Tests & CI

### Input state
- Slice 3.5 passes all gates.

### Specification

**Files to create:**
- `sonda-server/tests/integration.rs`:
  - Start server in background (bind to random port).
  - POST metrics scenario → 201.
  - POST logs scenario → 201.
  - GET /scenarios → both listed.
  - Wait 3 seconds → GET /scenarios/{id}/stats → total_events > 0.
  - DELETE → 200, status "stopped".
  - GET /scenarios → shows stopped.
  - Verify both metrics and logs scenarios work through the same API.

**Files to modify:**
- `.github/workflows/ci.yml` — add sonda-server build and integration test steps.

### Output files
| File | Status |
|------|--------|
| `sonda-server/tests/integration.rs` | new |
| `.github/workflows/ci.yml` | modified |

### Test criteria
- Integration test: full lifecycle (POST → GET → stats → DELETE) passes for both metrics and logs.
- CI builds and tests both `sonda` and `sonda-server`.

### Review criteria
- Integration test uses random port (no conflicts in CI).
- Integration test has reasonable timeouts (not flaky).
- Integration test covers both metrics and logs scenario types through the API.
- CI builds and tests both `sonda` and `sonda-server`.

### UAT criteria
- `cargo test -p sonda-server --test integration` passes locally.
- CI pipeline passes with the new integration test step.

---

## Slice 3.7 — Dockerfile & Docker Compose

### Input state
- Slice 3.6 passes all gates.

### Specification

**Files to create:**
- `Dockerfile`:
  - Multi-stage build: Rust builder stage + scratch runtime stage.
  - Static binary via `x86_64-unknown-linux-musl` target.
  - Both `sonda` and `sonda-server` binaries copied into the final image.
  - `ENTRYPOINT ["/sonda-server"]` by default, but users can override to run `/sonda` CLI.
  - Expose port 8080.

- `docker-compose.yml` — realistic observability stack demo:
  ```yaml
  services:
    sonda-server:
      build: .
      ports: ["8080:8080"]
      volumes:
        - ./examples:/scenarios  # mount scenario YAMLs

    prometheus:
      image: prom/prometheus:latest
      ports: ["9090:9090"]
      volumes:
        - ./docker/prometheus.yml:/etc/prometheus/prometheus.yml

    alertmanager:
      image: prom/alertmanager:latest
      ports: ["9093:9093"]

    grafana:
      image: grafana/grafana:latest
      ports: ["3000:3000"]
      environment:
        - GF_SECURITY_ADMIN_PASSWORD=admin
  ```

- `docker/prometheus.yml` — Prometheus config that scrapes sonda-server metrics (if applicable) or receives remote-write.

- Example scenario YAMLs in `examples/` that exercise realistic patterns:
  - `examples/docker-metrics.yaml` — metrics scenario suitable for Prometheus ingestion.
  - `examples/docker-alerts.yaml` — scenario that triggers alert conditions (e.g., values crossing thresholds).

**Files to modify:**
- `README.md` — add Docker deployment section with:
  - How to build the image.
  - How to run with docker-compose.
  - How to POST scenarios to the running server.
  - How to view metrics in Grafana.

### Output files
| File | Status |
|------|--------|
| `Dockerfile` | new |
| `docker-compose.yml` | new |
| `docker/prometheus.yml` | new |
| `examples/docker-metrics.yaml` | new |
| `examples/docker-alerts.yaml` | new |
| `README.md` | modified |

### Test criteria
- `docker build .` succeeds and produces a working image.
- `docker compose up` starts all services without errors.
- `curl http://localhost:8080/health` returns 200 from the containerized server.
- POST a scenario → server runs it → metrics are observable in Prometheus/Grafana.
- sonda-server musl binary < 15MB.

### Review criteria
- Dockerfile uses scratch base (minimal image).
- Docker Compose includes a realistic observability stack (not just sonda alone).
- Volume mounts allow users to bring their own scenario YAMLs.
- README Docker section covers build, run, and usage with examples.
- No secrets or credentials in committed files.

### UAT criteria
- `docker compose up` → sonda-server starts, accepts scenarios.
- POST scenario via curl → metrics flow through the stack.
- `docker compose down` → clean shutdown.
- Image size < 20MB.

---

## Slice 3.8 — Multi-arch Builds

### Input state
- Slice 3.7 passes all gates.

### Specification

**Files to modify:**
- `Dockerfile` — update to support multi-arch builds:
  - Use `--platform` build args or multi-stage with cross-compilation.
  - Support `linux/amd64` and `linux/arm64` targets.
  - Use `xx` or `cargo-zigbuild` or separate builder stages per arch.

- `.github/workflows/ci.yml` (or new `.github/workflows/release.yml`):
  - Add multi-arch Docker build step using `docker buildx`.
  - Build and push images for `linux/amd64` and `linux/arm64`.
  - Optionally: build static binaries for both architectures as release artifacts.

- `README.md` — update Docker section to mention multi-arch support and available platforms.

### Output files
| File | Status |
|------|--------|
| `Dockerfile` | modified |
| `.github/workflows/ci.yml` or `.github/workflows/release.yml` | modified/new |
| `README.md` | modified |

### Test criteria
- `docker buildx build --platform linux/amd64,linux/arm64 .` succeeds.
- amd64 image runs on x86_64 hosts.
- arm64 image runs on ARM hosts (e.g., Mac M-series with Docker Desktop).
- Both images produce working `sonda` and `sonda-server` binaries.

### Review criteria
- Cross-compilation approach is clean (no hacky workarounds).
- CI builds both architectures without excessive build time.
- Image manifest lists both platforms.

### UAT criteria
- Pull image on an amd64 machine → `docker run` → server starts.
- Pull image on an arm64 machine (or emulated) → `docker run` → server starts.
- `docker manifest inspect` shows both platforms.

---

## Slice 3.9 — Kubernetes Readiness & Helm Chart

### Input state
- Slice 3.8 passes all gates.

### Specification

**Files to create:**
- `helm/sonda/Chart.yaml` — Helm chart metadata.
- `helm/sonda/values.yaml` — default values:
  - `image.repository`, `image.tag`
  - `server.port` (default 8080)
  - `server.bind` (default 0.0.0.0)
  - `scenarios` — list of scenario ConfigMap entries
  - `resources` — CPU/memory requests and limits
  - `replicaCount` (default 1)

- `helm/sonda/templates/deployment.yaml`:
  - Deployment with sonda-server container.
  - Liveness probe: `GET /health` on server port.
  - Readiness probe: `GET /health` on server port.
  - ConfigMap volume mount for scenario YAMLs.
  - Resource requests/limits from values.

- `helm/sonda/templates/service.yaml`:
  - ClusterIP service exposing the server port.

- `helm/sonda/templates/configmap.yaml`:
  - ConfigMap holding scenario YAML files from `values.scenarios`.

- `helm/sonda/templates/_helpers.tpl` — standard Helm helpers (fullname, labels, etc.).

**Files to modify:**
- `README.md` — add Kubernetes deployment section:
  - How to install the Helm chart.
  - How to configure scenarios via values.yaml.
  - How `/health` serves as liveness/readiness probe.
  - Example: `helm install sonda ./helm/sonda --set server.port=8080`.

### Output files
| File | Status |
|------|--------|
| `helm/sonda/Chart.yaml` | new |
| `helm/sonda/values.yaml` | new |
| `helm/sonda/templates/deployment.yaml` | new |
| `helm/sonda/templates/service.yaml` | new |
| `helm/sonda/templates/configmap.yaml` | new |
| `helm/sonda/templates/_helpers.tpl` | new |
| `README.md` | modified |

### Test criteria
- `helm lint ./helm/sonda` passes.
- `helm template ./helm/sonda` renders valid Kubernetes manifests.
- Rendered Deployment includes liveness and readiness probes pointing to `/health`.
- Rendered ConfigMap contains scenario YAML from values.
- Default values produce a valid, deployable chart.

### Review criteria
- Chart follows Helm best practices (labels, helpers, NOTES.txt).
- Probes use `/health` endpoint — no custom health logic.
- Scenarios are configurable via values without rebuilding the image.
- Resource defaults are reasonable for a synthetic traffic generator.
- No hardcoded image tags (uses `values.yaml`).

### UAT criteria
- `helm install sonda ./helm/sonda` in a local cluster (kind/minikube) → pod starts.
- Pod passes liveness and readiness probes.
- `kubectl port-forward` → `curl /health` → 200.
- POST scenario → metrics flow.
- `helm uninstall sonda` → clean removal.

---

## Dependency Graph

```
Slice 3.0 (core lifecycle refactor — DRY foundation)
  ↓
Slice 3.1 (server skeleton + health)
  ↓
Slice 3.2 (POST /scenarios)
  ↓
Slice 3.3 (GET list/inspect)
  ↓
Slice 3.4 (DELETE)
  ↓
Slice 3.5 (stats endpoint)
  ↓
Slice 3.6 (integration tests + CI)
  ↓
Slice 3.7 (Dockerfile + Docker Compose)
  ↓
Slice 3.8 (multi-arch builds)
  ↓
Slice 3.9 (Kubernetes readiness + Helm chart)
```

Slices 3.0–3.5 build the server API. Slice 3.6 validates it end-to-end. Slices 3.7–3.9 make it
deployable in increasingly sophisticated environments: Docker → multi-arch → Kubernetes.

---

## DRY Audit Checklist

These invariants must hold after Phase 3 is complete:

- [ ] `match ScenarioEntry { Metrics(..) => ..., Logs(..) => ... }` dispatch exists in exactly
  **one** place: `launch_scenario` in sonda-core.
- [ ] `validate_config` / `validate_log_config` dispatch exists in exactly **one** place:
  `validate_entry` in sonda-core.
- [ ] `ScenarioHandle` is defined in sonda-core and imported (not redefined) by sonda-server.
- [ ] `ScenarioStats` is defined in sonda-core and used by runners, CLI, and server alike.
- [ ] The server crate (`sonda-server`) contains zero calls to `create_sink`, `create_generator`,
  `create_encoder`, `run_with_sink`, or `run_logs_with_sink` — it only calls `launch_scenario`,
  `validate_entry`, and `ScenarioHandle` methods.
- [ ] The CLI crate (`sonda`) also uses `launch_scenario` for all subcommands.

---

## Post-Phase 3

With all slices complete, Sonda has a CLI, multi-signal support, concurrent execution, a REST API,
Docker packaging, and Kubernetes-ready deployment. Future work (not designed here):

- Prometheus remote-write encoder (protobuf via `prost`)
- Dynamic label cardinality (rotating hostnames, pod churn simulation)
- OpenTelemetry encoder (OTLP)
- Clustering (deferred until single-instance limits are understood)
- Pre-built scenario library (common failure patterns, alert test scenarios)
- Grafana dashboard provisioning in Helm chart
