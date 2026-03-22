//! Scenario management endpoints.
//!
//! Implements `POST /scenarios`, which accepts a YAML or JSON scenario body,
//! launches the scenario via sonda-core, and returns the scenario ID.
//!
//! All lifecycle logic is delegated to sonda-core. This handler is pure HTTP
//! plumbing: deserialize → validate → launch → store → respond.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::json;
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

// ---- Handler ----------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::routes::router;
    use crate::state::AppState;

    // ---- Helpers ---------------------------------------------------------------

    /// Build the router with fresh empty state for test use.
    fn test_router() -> (axum::Router, AppState) {
        let state = AppState::new();
        let app = router(state.clone());
        (app, state)
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
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        app.oneshot(request).await.unwrap()
    }

    /// Helper to extract the response body as a serde_json::Value.
    async fn body_json(response: hyper::Response<axum::body::Body>) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    }

    /// Helper: stop all scenarios in the AppState to clean up spawned threads.
    fn cleanup_scenarios(state: &AppState) {
        if let Ok(scenarios) = state.scenarios.read() {
            for handle in scenarios.values() {
                handle.stop();
            }
        }
    }

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
            .body(axum::body::Body::from(VALID_METRICS_YAML.to_string()))
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
