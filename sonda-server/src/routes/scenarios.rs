//! Scenario management endpoints.
//!
//! Implements:
//! - `POST /scenarios` — start a new scenario from a YAML or JSON body.
//! - `GET /scenarios` — list all scenarios with summary information.
//! - `GET /scenarios/:id` — inspect a single scenario with full detail and stats.
//!
//! All lifecycle logic is delegated to sonda-core. This module is pure HTTP
//! plumbing: deserialize → validate → launch → store → respond.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::json;
use sonda_core::ScenarioStats;
use tracing::{info, warn};
use uuid::Uuid;

use sonda_core::config::{LogScenarioConfig, ScenarioConfig, ScenarioEntry};
use sonda_core::schedule::launch::{launch_scenario, validate_entry};

use crate::state::AppState;

// ---- Response types ---------------------------------------------------------

/// Response body for a successfully created scenario.
#[derive(Debug, Serialize)]
pub struct CreatedScenario {
    /// Unique identifier for the scenario instance.
    pub id: String,
    /// Human-readable scenario name from the config.
    pub name: String,
    /// Always `"running"` for a freshly launched scenario.
    pub status: &'static str,
}

/// Summary of a single scenario in the list response.
#[derive(Debug, Serialize)]
pub struct ScenarioSummary {
    /// Unique scenario ID.
    pub id: String,
    /// Human-readable scenario name.
    pub name: String,
    /// Current status: "running" or "stopped".
    pub status: String,
    /// Seconds elapsed since the scenario was launched.
    pub elapsed_secs: f64,
}

/// Response body for `GET /scenarios`.
#[derive(Debug, Serialize)]
pub struct ListScenariosResponse {
    /// All known scenarios.
    pub scenarios: Vec<ScenarioSummary>,
}

/// Detailed view of a single scenario, including live stats.
#[derive(Debug, Serialize)]
pub struct ScenarioDetail {
    /// Unique scenario ID.
    pub id: String,
    /// Human-readable scenario name.
    pub name: String,
    /// Current status: "running" or "stopped".
    pub status: String,
    /// Seconds elapsed since the scenario was launched.
    pub elapsed_secs: f64,
    /// Live statistics from the runner thread.
    pub stats: StatsResponse,
}

/// Stats sub-object within the scenario detail response.
///
/// This mirrors the fields from [`ScenarioStats`] that are relevant to the
/// HTTP API. We use a dedicated response struct to decouple the wire format
/// from the internal stats representation.
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    /// Total number of events emitted since the scenario started.
    pub total_events: u64,
    /// Measured events per second.
    pub current_rate: f64,
    /// Total bytes written to the sink.
    pub bytes_emitted: u64,
    /// Number of encode or sink write errors encountered.
    pub errors: u64,
}

impl From<ScenarioStats> for StatsResponse {
    fn from(s: ScenarioStats) -> Self {
        Self {
            total_events: s.total_events,
            current_rate: s.current_rate,
            bytes_emitted: s.bytes_emitted,
            errors: s.errors,
        }
    }
}

// ---- Error helpers ----------------------------------------------------------

/// Build a 400 Bad Request response with a JSON error body.
fn bad_request(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "bad_request", "detail": detail.to_string() });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// Build a 422 Unprocessable Entity response with a JSON error body.
fn unprocessable(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "unprocessable_entity", "detail": detail.to_string() });
    (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()
}

/// Build a 500 Internal Server Error response with a JSON error body.
fn internal_error(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "internal_server_error", "detail": detail.to_string() });
    (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
}

// ---- Helpers ----------------------------------------------------------------

/// Derive the status string from whether the scenario handle is still running.
fn status_string(running: bool) -> String {
    if running {
        "running".to_string()
    } else {
        "stopped".to_string()
    }
}

// ---- Body parsing -----------------------------------------------------------

/// Determine the content type from the request headers.
///
/// Returns `true` if the content type indicates YAML (`application/x-yaml`,
/// `text/yaml`, or `application/yaml`). Defaults to trying YAML first when
/// no content-type header is present.
fn is_yaml_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            ct.contains("yaml")
                || ct.contains("x-yaml")
                || ct.starts_with("text/yaml")
                || ct.starts_with("application/yaml")
                || ct.starts_with("application/x-yaml")
        })
        .unwrap_or(true) // default: assume YAML
}

/// Attempt to parse the raw body bytes as a [`ScenarioEntry`].
///
/// Tries the following strategies in order:
///
/// 1. If JSON content-type: parse as JSON → `ScenarioEntry` (tagged with
///    `signal_type`), or as plain `ScenarioConfig` (metrics).
/// 2. Otherwise (YAML or unknown): parse as YAML → `ScenarioEntry` (tagged),
///    or as plain `ScenarioConfig` (metrics), or as plain `LogScenarioConfig`
///    (logs).
///
/// Returns a descriptive error string on failure.
fn parse_body(body: &[u8], headers: &HeaderMap) -> Result<ScenarioEntry, String> {
    if is_yaml_content_type(headers) {
        parse_yaml_body(body)
    } else {
        parse_json_body(body)
    }
}

/// Parse body bytes as YAML → `ScenarioEntry`.
///
/// Tries `ScenarioEntry` (tagged with `signal_type`) first. If that fails,
/// falls back to `ScenarioConfig` (plain metrics) and then `LogScenarioConfig`
/// (plain logs). This lets callers POST a bare metrics or logs YAML without
/// having to include the `signal_type` discriminant.
fn parse_yaml_body(body: &[u8]) -> Result<ScenarioEntry, String> {
    let text =
        std::str::from_utf8(body).map_err(|e| format!("request body is not valid UTF-8: {e}"))?;

    // Strategy 1: tagged ScenarioEntry (has `signal_type: metrics|logs`).
    if let Ok(entry) = serde_yaml::from_str::<ScenarioEntry>(text) {
        return Ok(entry);
    }

    // Strategy 2: bare ScenarioConfig → wrap in Metrics variant.
    if let Ok(config) = serde_yaml::from_str::<ScenarioConfig>(text) {
        return Ok(ScenarioEntry::Metrics(config));
    }

    // Strategy 3: bare LogScenarioConfig → wrap in Logs variant.
    if let Ok(config) = serde_yaml::from_str::<LogScenarioConfig>(text) {
        return Ok(ScenarioEntry::Logs(config));
    }

    // All three attempts failed — return a generic YAML parse error.
    // Re-parse just to get a meaningful error message.
    let yaml_err = serde_yaml::from_str::<ScenarioEntry>(text)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unknown YAML parse error".to_string());

    Err(format!("invalid YAML scenario body: {yaml_err}"))
}

/// Parse body bytes as JSON → `ScenarioEntry`.
///
/// Tries `ScenarioEntry` (tagged with `signal_type`) first. If that fails,
/// falls back to plain `ScenarioConfig` (metrics only — JSON logs require the
/// `signal_type` tag because the generator field shapes differ significantly).
fn parse_json_body(body: &[u8]) -> Result<ScenarioEntry, String> {
    // Strategy 1: tagged ScenarioEntry.
    if let Ok(entry) = serde_json::from_slice::<ScenarioEntry>(body) {
        return Ok(entry);
    }

    // Strategy 2: bare ScenarioConfig → Metrics.
    if let Ok(config) = serde_json::from_slice::<ScenarioConfig>(body) {
        return Ok(ScenarioEntry::Metrics(config));
    }

    // Re-parse for error message.
    let json_err = serde_json::from_slice::<serde_json::Value>(body)
        .map(|_| "JSON parsed but did not match any scenario schema".to_string())
        .unwrap_or_else(|e| format!("invalid JSON: {e}"));

    Err(format!("invalid JSON scenario body: {json_err}"))
}

// ---- Handlers ---------------------------------------------------------------

/// `POST /scenarios` — start a new scenario from a YAML or JSON body.
///
/// Accepts both `application/x-yaml` / `text/yaml` (YAML) and
/// `application/json` (JSON) request bodies. The body must describe a valid
/// scenario (metrics or logs).
///
/// Returns `201 Created` with `{"id": "...", "name": "...", "status": "running"}`
/// on success.
///
/// # Error responses
/// - `400 Bad Request` — body cannot be parsed as YAML or JSON.
/// - `422 Unprocessable Entity` — body parsed but failed validation (e.g. rate=0).
/// - `500 Internal Server Error` — scenario thread could not be spawned.
pub async fn post_scenario(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // 1. Parse the body into a ScenarioEntry.
    let entry = match parse_body(&body, &headers) {
        Ok(e) => e,
        Err(msg) => {
            warn!(error = %msg, "POST /scenarios: invalid request body");
            return bad_request(msg);
        }
    };

    // 2. Validate the entry (rate, duration, generator parameters, etc.).
    if let Err(e) = validate_entry(&entry) {
        warn!(error = %e, "POST /scenarios: validation failed");
        return unprocessable(e);
    }

    // 3. Assign a unique ID and extract the scenario name before moving entry.
    let id = Uuid::new_v4().to_string();
    let name = match &entry {
        ScenarioEntry::Metrics(c) => c.name.clone(),
        ScenarioEntry::Logs(c) => c.name.clone(),
    };

    // 4. Launch the scenario on a new OS thread.
    let shutdown = Arc::new(AtomicBool::new(true));
    let handle = match launch_scenario(id.clone(), entry, shutdown) {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "POST /scenarios: failed to launch scenario");
            return internal_error(e);
        }
    };

    info!(id = %id, name = %name, "scenario launched");

    // 5. Store the handle in shared state.
    match state.scenarios.write() {
        Ok(mut scenarios) => {
            scenarios.insert(id.clone(), handle);
        }
        Err(e) => {
            // Poisoned lock — highly unlikely but must be handled.
            warn!(error = %e, "POST /scenarios: scenarios lock is poisoned");
            return internal_error("internal state lock is poisoned");
        }
    }

    // 6. Respond with 201 Created.
    let response_body = CreatedScenario {
        id,
        name,
        status: "running",
    };
    (StatusCode::CREATED, Json(response_body)).into_response()
}

/// `GET /scenarios` — list all scenarios with summary information.
///
/// Returns a JSON object with a `scenarios` array containing each scenario's
/// ID, name, status, and elapsed time. The list includes both running and
/// stopped scenarios that have not been deleted.
pub async fn list_scenarios(State(state): State<AppState>) -> impl IntoResponse {
    let scenarios = state
        .scenarios
        .read()
        .expect("AppState RwLock must not be poisoned");

    let summaries: Vec<ScenarioSummary> = scenarios
        .iter()
        .map(|(id, handle)| ScenarioSummary {
            id: id.clone(),
            name: handle.name.clone(),
            status: status_string(handle.is_running()),
            elapsed_secs: handle.elapsed().as_secs_f64(),
        })
        .collect();

    Json(ListScenariosResponse {
        scenarios: summaries,
    })
}

/// `GET /scenarios/:id` — inspect a single scenario with full detail.
///
/// Returns the scenario's ID, name, status, elapsed time, and live stats
/// (total_events, current_rate, bytes_emitted, errors). Returns 404 if the
/// scenario ID is not found.
pub async fn get_scenario(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let scenarios = state
        .scenarios
        .read()
        .expect("AppState RwLock must not be poisoned");

    let handle = scenarios.get(&id).ok_or(StatusCode::NOT_FOUND)?;

    let detail = ScenarioDetail {
        id: id.clone(),
        name: handle.name.clone(),
        status: status_string(handle.is_running()),
        elapsed_secs: handle.elapsed().as_secs_f64(),
        stats: handle.stats_snapshot().into(),
    };

    Ok(Json(detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::router;
    use crate::state::AppState;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode};
    use sonda_core::ScenarioHandle;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, RwLock};
    use std::thread;
    use std::time::{Duration, Instant};
    use tower::ServiceExt;

    // ---- Helpers ---------------------------------------------------------------

    /// Build a ScenarioHandle with a background thread that increments stats.
    ///
    /// The thread emits `event_count` events at `interval` apart, incrementing
    /// total_events and bytes_emitted on each tick.
    fn make_handle(id: &str, name: &str, event_count: u64, interval: Duration) -> ScenarioHandle {
        let shutdown = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let shutdown_clone = Arc::clone(&shutdown);
        let stats_clone = Arc::clone(&stats);

        let thread = thread::Builder::new()
            .name(format!("test-{name}"))
            .spawn(move || -> Result<(), sonda_core::SondaError> {
                for _ in 0..event_count {
                    if !shutdown_clone.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(interval);
                    if let Ok(mut st) = stats_clone.write() {
                        st.total_events += 1;
                        st.bytes_emitted += 64;
                    }
                }
                Ok(())
            })
            .expect("thread must spawn");

        ScenarioHandle {
            id: id.to_string(),
            name: name.to_string(),
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
        }
    }

    /// Build a ScenarioHandle that has already finished (thread exits immediately).
    fn make_stopped_handle(id: &str, name: &str) -> ScenarioHandle {
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let shutdown_clone = Arc::clone(&shutdown);

        let thread = thread::Builder::new()
            .name(format!("test-stopped-{name}"))
            .spawn(move || -> Result<(), sonda_core::SondaError> {
                // Check shutdown immediately and exit.
                let _ = shutdown_clone.load(Ordering::SeqCst);
                Ok(())
            })
            .expect("thread must spawn");

        // Give thread time to finish.
        thread::sleep(Duration::from_millis(50));

        ScenarioHandle {
            id: id.to_string(),
            name: name.to_string(),
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
        }
    }

    /// Build a router with the given handles pre-inserted.
    fn router_with_handles(handles: Vec<ScenarioHandle>) -> axum::Router {
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            for h in handles {
                map.insert(h.id.clone(), h);
            }
        }
        router(state)
    }

    /// Build the router with fresh empty state for test use (returns state for POST tests).
    fn test_router() -> (axum::Router, AppState) {
        let state = AppState::new();
        let app = router(state.clone());
        (app, state)
    }

    /// Helper to parse a response body as serde_json::Value.
    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).expect("body must be valid JSON")
    }

    /// Helper: stop all scenarios in the AppState to clean up spawned threads.
    fn cleanup_scenarios(state: &AppState) {
        if let Ok(scenarios) = state.scenarios.read() {
            for handle in scenarios.values() {
                handle.stop();
            }
        }
    }

    /// Helper to send a POST /scenarios request with the given content type and body.
    async fn post_scenarios(
        app: axum::Router,
        content_type: &str,
        body: &str,
    ) -> hyper::Response<axum::body::Body> {
        let request = Request::builder()
            .method("POST")
            .uri("/scenarios")
            .header("content-type", content_type)
            .body(Body::from(body.to_string()))
            .unwrap();
        app.oneshot(request).await.unwrap()
    }

    /// YAML body for a valid metrics scenario with short duration.
    const VALID_METRICS_YAML: &str = "\
name: test_metric
rate: 10
duration: 200ms
generator:
  type: constant
  value: 42.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";

    /// YAML body for a valid logs scenario with short duration.
    const VALID_LOGS_YAML: &str = "\
name: test_logs
rate: 10
duration: 200ms
generator:
  type: template
  templates:
    - message: \"test log event\"
      field_pools: {}
  seed: 0
encoder:
  type: json_lines
sink:
  type: stdout
";

    /// YAML body using explicit signal_type: metrics (ScenarioEntry format).
    const VALID_TAGGED_METRICS_YAML: &str = "\
signal_type: metrics
name: tagged_metric
rate: 10
duration: 200ms
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";

    /// YAML body with rate = 0 (validation should reject this).
    const ZERO_RATE_YAML: &str = "\
name: bad_rate
rate: 0
duration: 1s
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";

    // ========================================================================
    // GET /scenarios tests
    // ========================================================================

    // ---- GET /scenarios: empty state -----------------------------------------

    /// GET /scenarios with no scenarios returns an empty list.
    #[tokio::test]
    async fn list_scenarios_empty_returns_empty_array() {
        let app = router_with_handles(vec![]);
        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let scenarios = body["scenarios"]
            .as_array()
            .expect("scenarios must be an array");
        assert!(
            scenarios.is_empty(),
            "empty state must return empty scenarios array"
        );
    }

    // ---- GET /scenarios: two scenarios listed --------------------------------

    /// Start 2 scenarios, GET /scenarios returns both listed.
    #[tokio::test]
    async fn list_scenarios_returns_both_when_two_present() {
        let h1 = make_handle("id-aaa", "scenario_alpha", 1000, Duration::from_millis(50));
        let h2 = make_handle("id-bbb", "scenario_beta", 1000, Duration::from_millis(50));
        let app = router_with_handles(vec![h1, h2]);

        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let scenarios = body["scenarios"]
            .as_array()
            .expect("scenarios must be an array");
        assert_eq!(
            scenarios.len(),
            2,
            "must list exactly 2 scenarios, got {}",
            scenarios.len()
        );

        // Collect the IDs from the response.
        let mut ids: Vec<&str> = scenarios
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["id-aaa", "id-bbb"]);

        // Collect the names from the response.
        let mut names: Vec<&str> = scenarios
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["scenario_alpha", "scenario_beta"]);
    }

    // ---- GET /scenarios: response shape --------------------------------------

    /// Each scenario summary has id, name, status, elapsed_secs fields.
    #[tokio::test]
    async fn list_scenarios_response_shape_has_required_fields() {
        let h = make_handle("id-shape", "shape_test", 1000, Duration::from_millis(50));
        let app = router_with_handles(vec![h]);

        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = body_json(resp).await;
        let entry = &body["scenarios"][0];

        assert!(entry["id"].is_string(), "id must be a string");
        assert!(entry["name"].is_string(), "name must be a string");
        assert!(entry["status"].is_string(), "status must be a string");
        assert!(
            entry["elapsed_secs"].is_f64(),
            "elapsed_secs must be a number"
        );
    }

    // ---- GET /scenarios/:id: correct name, status, elapsed -------------------

    /// GET /scenarios/:id returns correct name, status, and positive elapsed_secs.
    #[tokio::test]
    async fn get_scenario_returns_correct_name_status_elapsed() {
        let h = make_handle(
            "id-detail",
            "detail_scenario",
            1000,
            Duration::from_millis(50),
        );
        let app = router_with_handles(vec![h]);

        // Small delay to ensure elapsed > 0.
        thread::sleep(Duration::from_millis(20));

        let req = Request::builder()
            .uri("/scenarios/id-detail")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(body["id"].as_str().unwrap(), "id-detail");
        assert_eq!(body["name"].as_str().unwrap(), "detail_scenario");
        assert_eq!(
            body["status"].as_str().unwrap(),
            "running",
            "a live scenario must have status 'running'"
        );
        let elapsed = body["elapsed_secs"].as_f64().unwrap();
        assert!(
            elapsed > 0.0,
            "elapsed_secs must be positive, got {elapsed}"
        );
    }

    // ---- GET /scenarios/:id: stats fields present ----------------------------

    /// GET /scenarios/:id response includes stats sub-object with all required fields.
    #[tokio::test]
    async fn get_scenario_response_has_stats_fields() {
        let h = make_handle(
            "id-stats-fields",
            "stats_check",
            1000,
            Duration::from_millis(50),
        );
        let app = router_with_handles(vec![h]);

        let req = Request::builder()
            .uri("/scenarios/id-stats-fields")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = body_json(resp).await;

        let stats = &body["stats"];
        assert!(stats.is_object(), "response must include a stats object");
        assert!(
            stats.get("total_events").is_some(),
            "stats must have total_events"
        );
        assert!(
            stats.get("current_rate").is_some(),
            "stats must have current_rate"
        );
        assert!(
            stats.get("bytes_emitted").is_some(),
            "stats must have bytes_emitted"
        );
        assert!(stats.get("errors").is_some(), "stats must have errors");
    }

    // ---- GET /scenarios/:id: stats.total_events > 0 after running ------------

    /// After running for a short time, stats.total_events > 0.
    #[tokio::test]
    async fn get_scenario_stats_total_events_positive_after_running() {
        // Thread emits events every 10ms. After 200ms we should have ~20 events.
        let h = make_handle("id-events", "events_check", 500, Duration::from_millis(10));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        // Wait for events to accumulate.
        thread::sleep(Duration::from_millis(200));

        let app = router(state);
        let req = Request::builder()
            .uri("/scenarios/id-events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = body_json(resp).await;

        let total_events = body["stats"]["total_events"].as_u64().unwrap();
        assert!(
            total_events > 0,
            "stats.total_events must be > 0 after running, got {total_events}"
        );
    }

    // ---- GET /scenarios/nonexistent: 404 -------------------------------------

    /// GET /scenarios/:id with a nonexistent ID returns 404.
    #[tokio::test]
    async fn get_scenario_nonexistent_returns_404() {
        let app = router_with_handles(vec![]);

        let req = Request::builder()
            .uri("/scenarios/nonexistent-id")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "nonexistent scenario ID must return 404"
        );
    }

    // ---- GET /scenarios/:id: stopped scenario reports "stopped" --------------

    /// A scenario whose thread has exited reports status "stopped".
    #[tokio::test]
    async fn get_scenario_stopped_reports_stopped_status() {
        let h = make_stopped_handle("id-stopped", "stopped_scenario");
        let app = router_with_handles(vec![h]);

        let req = Request::builder()
            .uri("/scenarios/id-stopped")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(
            body["status"].as_str().unwrap(),
            "stopped",
            "a finished scenario must have status 'stopped'"
        );
    }

    // ---- Stats update frequency: elapsed tracks real time --------------------

    /// Elapsed time reported by the endpoint must be within 1 second of real time.
    #[tokio::test]
    async fn elapsed_secs_tracks_real_time_within_one_second() {
        let h = make_handle(
            "id-elapsed",
            "elapsed_test",
            10000,
            Duration::from_millis(50),
        );
        let created_at = Instant::now();
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        // Wait a known amount of time.
        thread::sleep(Duration::from_millis(500));

        let app = router(state);
        let req = Request::builder()
            .uri("/scenarios/id-elapsed")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = body_json(resp).await;

        let reported_elapsed = body["elapsed_secs"].as_f64().unwrap();
        let actual_elapsed = created_at.elapsed().as_secs_f64();

        let diff = (reported_elapsed - actual_elapsed).abs();
        assert!(
            diff < 1.0,
            "elapsed_secs must be within 1 second of real time: reported={reported_elapsed:.3}, actual={actual_elapsed:.3}, diff={diff:.3}"
        );
    }

    // ---- Content-Type for scenario endpoints ---------------------------------

    /// GET /scenarios returns Content-Type application/json.
    #[tokio::test]
    async fn list_scenarios_sets_json_content_type() {
        let app = router_with_handles(vec![]);

        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let ct = resp
            .headers()
            .get("content-type")
            .expect("response must have Content-Type")
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "Content-Type must be application/json, got: {ct}"
        );
    }

    // ---- StatsResponse From ScenarioStats ------------------------------------

    /// StatsResponse correctly converts from ScenarioStats.
    #[test]
    fn stats_response_from_scenario_stats_converts_all_fields() {
        let stats = ScenarioStats {
            total_events: 42,
            bytes_emitted: 1024,
            current_rate: 10.5,
            errors: 3,
            in_gap: true,
            in_burst: false,
        };
        let resp: StatsResponse = stats.into();
        assert_eq!(resp.total_events, 42);
        assert_eq!(resp.bytes_emitted, 1024);
        assert_eq!((resp.current_rate * 10.0).round(), 105.0);
        assert_eq!(resp.errors, 3);
    }

    // ---- status_string helper ------------------------------------------------

    /// status_string(true) returns "running".
    #[test]
    fn status_string_true_returns_running() {
        assert_eq!(status_string(true), "running");
    }

    /// status_string(false) returns "stopped".
    #[test]
    fn status_string_false_returns_stopped() {
        assert_eq!(status_string(false), "stopped");
    }

    // ---- Serialization: response structs produce valid JSON ------------------

    /// ScenarioSummary serializes with all expected fields.
    #[test]
    fn scenario_summary_serializes_correctly() {
        let s = ScenarioSummary {
            id: "abc".to_string(),
            name: "test".to_string(),
            status: "running".to_string(),
            elapsed_secs: 1.5,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["id"], "abc");
        assert_eq!(json["name"], "test");
        assert_eq!(json["status"], "running");
        assert_eq!(json["elapsed_secs"], 1.5);
    }

    /// ScenarioDetail serializes with nested stats object.
    #[test]
    fn scenario_detail_serializes_with_nested_stats() {
        let d = ScenarioDetail {
            id: "xyz".to_string(),
            name: "detail".to_string(),
            status: "stopped".to_string(),
            elapsed_secs: 42.0,
            stats: StatsResponse {
                total_events: 100,
                current_rate: 5.0,
                bytes_emitted: 2048,
                errors: 1,
            },
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["id"], "xyz");
        assert_eq!(json["stats"]["total_events"], 100);
        assert_eq!(json["stats"]["errors"], 1);
    }

    // ========================================================================
    // POST /scenarios tests
    // ========================================================================

    // ---- Test: POST valid metrics YAML -> 201, scenario ID returned, handle in AppState

    /// POST a valid metrics YAML body returns 201 Created with id, name, and status.
    #[tokio::test]
    async fn post_valid_metrics_yaml_returns_201_with_id() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", VALID_METRICS_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST valid metrics YAML must return 201 Created"
        );

        let body = body_json(response).await;
        assert!(
            body["id"].is_string() && !body["id"].as_str().unwrap().is_empty(),
            "response must contain a non-empty 'id' string, got: {body}"
        );
        assert_eq!(
            body["name"], "test_metric",
            "response name must match the scenario name"
        );
        assert_eq!(
            body["status"], "running",
            "status must be 'running' for a freshly launched scenario"
        );

        // Verify the handle was stored in AppState.
        let scenarios = state.scenarios.read().expect("lock must not be poisoned");
        let id = body["id"].as_str().unwrap();
        assert!(
            scenarios.contains_key(id),
            "AppState must contain the handle for the newly created scenario ID"
        );

        cleanup_scenarios(&state);
    }

    // ---- Test: POST valid logs YAML -> 201, scenario ID returned

    /// POST a valid logs YAML body returns 201 Created.
    #[tokio::test]
    async fn post_valid_logs_yaml_returns_201() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "text/yaml", VALID_LOGS_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST valid logs YAML must return 201 Created"
        );

        let body = body_json(response).await;
        assert!(
            body["id"].is_string() && !body["id"].as_str().unwrap().is_empty(),
            "response must contain a non-empty 'id' for logs scenario"
        );
        assert_eq!(
            body["name"], "test_logs",
            "response name must match the logs scenario name"
        );
        assert_eq!(body["status"], "running");

        cleanup_scenarios(&state);
    }

    // ---- Test: POST YAML with signal_type: metrics -> 201 (ScenarioEntry format)

    /// POST a YAML body with explicit signal_type: metrics returns 201.
    #[tokio::test]
    async fn post_tagged_metrics_yaml_returns_201() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", VALID_TAGGED_METRICS_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST YAML with signal_type: metrics must return 201 Created"
        );

        let body = body_json(response).await;
        assert_eq!(
            body["name"], "tagged_metric",
            "name must match the tagged scenario name"
        );
        assert_eq!(body["status"], "running");

        cleanup_scenarios(&state);
    }

    // ---- Test: POST invalid YAML -> 400 with error message

    /// POST garbage text as YAML returns 400 Bad Request.
    #[tokio::test]
    async fn post_invalid_yaml_returns_400() {
        let (app, _state) = test_router();
        let response =
            post_scenarios(app, "application/x-yaml", "this is not valid yaml: [}{").await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "POST invalid YAML must return 400 Bad Request"
        );

        let body = body_json(response).await;
        assert_eq!(
            body["error"], "bad_request",
            "error field must be 'bad_request'"
        );
        assert!(
            body["detail"].is_string() && !body["detail"].as_str().unwrap().is_empty(),
            "detail field must contain a non-empty error description"
        );
    }

    /// POST an empty body returns 400 Bad Request.
    #[tokio::test]
    async fn post_empty_body_returns_400() {
        let (app, _state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", "").await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "POST empty body must return 400 Bad Request"
        );
    }

    /// POST YAML that parses but is missing required fields returns 400.
    #[tokio::test]
    async fn post_yaml_missing_required_fields_returns_400() {
        let (app, _state) = test_router();
        // Valid YAML but not a valid scenario (missing name, rate, generator).
        let response = post_scenarios(app, "text/yaml", "foo: bar\nbaz: 123\n").await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "POST YAML missing required fields must return 400"
        );
    }

    // ---- Test: POST valid YAML with rate=0 -> 422 with validation detail

    /// POST a valid YAML with rate=0 returns 422 Unprocessable Entity.
    #[tokio::test]
    async fn post_yaml_with_zero_rate_returns_422() {
        let (app, _state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", ZERO_RATE_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "POST YAML with rate=0 must return 422 Unprocessable Entity"
        );

        let body = body_json(response).await;
        assert_eq!(
            body["error"], "unprocessable_entity",
            "error field must be 'unprocessable_entity'"
        );
        assert!(
            body["detail"].is_string() && !body["detail"].as_str().unwrap().is_empty(),
            "detail must contain a description of the validation failure"
        );
    }

    // ---- Test: POST -> scenario thread is running (verify via handle.is_running())

    /// After POST, the scenario thread should be running in AppState.
    #[tokio::test]
    async fn post_scenario_thread_is_running() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "text/yaml", VALID_METRICS_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response).await;
        let id = body["id"].as_str().unwrap().to_string();

        // Check that the handle reports is_running() == true.
        let scenarios = state.scenarios.read().expect("lock must not be poisoned");
        let handle = scenarios
            .get(&id)
            .expect("handle must exist in AppState after POST");
        assert!(
            handle.is_running(),
            "scenario thread must be running after POST (is_running() must return true)"
        );

        // Clean up.
        drop(scenarios);
        cleanup_scenarios(&state);
    }

    // ---- Test: Content-type handling: application/x-yaml, text/yaml, application/json

    /// POST with Content-Type: application/x-yaml is accepted as YAML.
    #[tokio::test]
    async fn post_with_application_x_yaml_content_type_returns_201() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", VALID_METRICS_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "application/x-yaml content type must be accepted"
        );

        cleanup_scenarios(&state);
    }

    /// POST with Content-Type: text/yaml is accepted as YAML.
    #[tokio::test]
    async fn post_with_text_yaml_content_type_returns_201() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "text/yaml", VALID_METRICS_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "text/yaml content type must be accepted"
        );

        cleanup_scenarios(&state);
    }

    /// POST with Content-Type: application/json and a valid JSON metrics body returns 201.
    #[tokio::test]
    async fn post_with_json_content_type_returns_201() {
        let json_body = serde_json::json!({
            "signal_type": "metrics",
            "name": "json_metric",
            "rate": 10,
            "duration": "200ms",
            "generator": { "type": "constant", "value": 1.0 },
            "encoder": { "type": "prometheus_text" },
            "sink": { "type": "stdout" }
        });

        let (app, state) = test_router();
        let response = post_scenarios(app, "application/json", &json_body.to_string()).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "application/json content type must be accepted for valid JSON scenario"
        );

        let body = body_json(response).await;
        assert_eq!(body["name"], "json_metric");
        assert_eq!(body["status"], "running");

        cleanup_scenarios(&state);
    }

    /// POST with Content-Type: application/json and invalid JSON returns 400.
    #[tokio::test]
    async fn post_invalid_json_returns_400() {
        let (app, _state) = test_router();
        let response = post_scenarios(app, "application/json", "not json {{{").await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "invalid JSON body must return 400"
        );
    }

    /// POST with no Content-Type header defaults to YAML parsing.
    #[tokio::test]
    async fn post_with_no_content_type_defaults_to_yaml() {
        let (app, state) = test_router();
        let request = Request::builder()
            .method("POST")
            .uri("/scenarios")
            // No content-type header.
            .body(Body::from(VALID_METRICS_YAML.to_string()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST with no Content-Type header must default to YAML and succeed for valid YAML"
        );

        cleanup_scenarios(&state);
    }

    // ---- Test: Response body structure -----------------------------------------

    /// The 201 response body contains exactly three keys: id, name, status.
    #[tokio::test]
    async fn post_response_body_has_expected_keys() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "text/yaml", VALID_METRICS_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        let obj = body
            .as_object()
            .expect("response body must be a JSON object");
        assert!(obj.contains_key("id"), "response must contain key 'id'");
        assert!(obj.contains_key("name"), "response must contain key 'name'");
        assert!(
            obj.contains_key("status"),
            "response must contain key 'status'"
        );
        assert_eq!(
            obj.len(),
            3,
            "response must contain exactly 3 keys (id, name, status)"
        );

        cleanup_scenarios(&state);
    }

    /// The returned scenario ID is a valid UUID v4.
    #[tokio::test]
    async fn post_response_id_is_valid_uuid() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "text/yaml", VALID_METRICS_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        let id_str = body["id"].as_str().expect("id must be a string");
        let parsed = uuid::Uuid::parse_str(id_str);
        assert!(parsed.is_ok(), "id must be a valid UUID, got: {id_str}");

        cleanup_scenarios(&state);
    }

    // ---- Test: Negative rate -> 422 -------------------------------------------

    /// POST YAML with a negative rate returns 422.
    #[tokio::test]
    async fn post_yaml_with_negative_rate_returns_422() {
        let yaml = "\
name: neg_rate
rate: -5
duration: 1s
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";
        let (app, _state) = test_router();
        let response = post_scenarios(app, "text/yaml", yaml).await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "negative rate must return 422"
        );
    }

    // ---- Test: parse_body unit tests -------------------------------------------

    /// parse_yaml_body handles valid bare metrics config (no signal_type tag).
    #[test]
    fn parse_yaml_body_accepts_bare_metrics_config() {
        let result = parse_yaml_body(VALID_METRICS_YAML.as_bytes());
        assert!(
            result.is_ok(),
            "parse_yaml_body must accept bare metrics YAML: {result:?}"
        );
        match result.unwrap() {
            ScenarioEntry::Metrics(c) => assert_eq!(c.name, "test_metric"),
            other => panic!("expected ScenarioEntry::Metrics, got: {other:?}"),
        }
    }

    /// parse_yaml_body handles valid bare logs config (no signal_type tag).
    #[test]
    fn parse_yaml_body_accepts_bare_logs_config() {
        let result = parse_yaml_body(VALID_LOGS_YAML.as_bytes());
        assert!(
            result.is_ok(),
            "parse_yaml_body must accept bare logs YAML: {result:?}"
        );
        match result.unwrap() {
            ScenarioEntry::Logs(c) => assert_eq!(c.name, "test_logs"),
            other => panic!("expected ScenarioEntry::Logs, got: {other:?}"),
        }
    }

    /// parse_yaml_body handles tagged ScenarioEntry format.
    #[test]
    fn parse_yaml_body_accepts_tagged_entry() {
        let result = parse_yaml_body(VALID_TAGGED_METRICS_YAML.as_bytes());
        assert!(
            result.is_ok(),
            "parse_yaml_body must accept tagged ScenarioEntry YAML: {result:?}"
        );
        match result.unwrap() {
            ScenarioEntry::Metrics(c) => assert_eq!(c.name, "tagged_metric"),
            other => panic!("expected ScenarioEntry::Metrics, got: {other:?}"),
        }
    }

    /// parse_yaml_body returns Err for garbage input.
    #[test]
    fn parse_yaml_body_rejects_garbage() {
        let result = parse_yaml_body(b"not valid: [}{");
        assert!(result.is_err(), "parse_yaml_body must reject garbage input");
        let err = result.unwrap_err();
        assert!(
            err.contains("invalid YAML"),
            "error message must mention invalid YAML, got: {err}"
        );
    }

    /// parse_json_body accepts a tagged ScenarioEntry JSON.
    #[test]
    fn parse_json_body_accepts_tagged_json() {
        let json = serde_json::json!({
            "signal_type": "metrics",
            "name": "json_test",
            "rate": 10,
            "duration": "1s",
            "generator": { "type": "constant", "value": 1.0 },
            "encoder": { "type": "prometheus_text" },
            "sink": { "type": "stdout" }
        });
        let result = parse_json_body(json.to_string().as_bytes());
        assert!(
            result.is_ok(),
            "parse_json_body must accept tagged JSON: {result:?}"
        );
    }

    /// parse_json_body returns Err for invalid JSON.
    #[test]
    fn parse_json_body_rejects_invalid_json() {
        let result = parse_json_body(b"not json");
        assert!(result.is_err(), "parse_json_body must reject invalid JSON");
    }

    /// is_yaml_content_type returns true for application/x-yaml.
    #[test]
    fn is_yaml_content_type_returns_true_for_application_x_yaml() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        assert!(is_yaml_content_type(&headers));
    }

    /// is_yaml_content_type returns true for text/yaml.
    #[test]
    fn is_yaml_content_type_returns_true_for_text_yaml() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "text/yaml".parse().unwrap());
        assert!(is_yaml_content_type(&headers));
    }

    /// is_yaml_content_type returns false for application/json.
    #[test]
    fn is_yaml_content_type_returns_false_for_application_json() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        assert!(!is_yaml_content_type(&headers));
    }

    /// is_yaml_content_type defaults to true when no content-type is present.
    #[test]
    fn is_yaml_content_type_defaults_to_true_when_missing() {
        let headers = HeaderMap::new();
        assert!(
            is_yaml_content_type(&headers),
            "must default to YAML when no Content-Type header is present"
        );
    }

    // ---- Contract test: CreatedScenario serializes correctly -------------------

    /// CreatedScenario serializes to JSON with the expected structure.
    #[test]
    fn created_scenario_serializes_to_expected_json() {
        let cs = CreatedScenario {
            id: "abc-123".to_string(),
            name: "my_scenario".to_string(),
            status: "running",
        };
        let json = serde_json::to_value(&cs).expect("must serialize");
        assert_eq!(json["id"], "abc-123");
        assert_eq!(json["name"], "my_scenario");
        assert_eq!(json["status"], "running");
    }
}
