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
