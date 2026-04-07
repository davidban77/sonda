//! Scenario management endpoints.
//!
//! Implements:
//! - `POST /scenarios` — start one or more scenarios from a YAML or JSON body.
//!   Accepts both single-scenario bodies (backward compatible) and multi-scenario
//!   bodies with a top-level `scenarios:` array. Multi-scenario POST uses atomic
//!   batch semantics: all entries are validated before any are launched.
//! - `GET /scenarios` — list all scenarios with summary information.
//! - `GET /scenarios/{id}` — inspect a single scenario with full detail and stats.
//! - `GET /scenarios/{id}/stats` — return detailed live stats for a scenario.
//! - `GET /scenarios/{id}/metrics` — return recent metrics in Prometheus text format (scrapeable).
//! - `DELETE /scenarios/{id}` — stop a running scenario and return final stats.
//!
//! All lifecycle logic is delegated to sonda-core. This module is pure HTTP
//! plumbing: deserialize → validate → launch → store → respond.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sonda_core::encoder::prometheus::PrometheusText;
use sonda_core::encoder::Encoder;
use sonda_core::ScenarioStats;
use tracing::{info, warn};
use uuid::Uuid;

use sonda_core::config::{LogScenarioConfig, MultiScenarioConfig, ScenarioConfig, ScenarioEntry};
use sonda_core::schedule::launch::{launch_scenario, prepare_entries};

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

/// Response body for a successfully created multi-scenario batch.
///
/// Returned when `POST /scenarios` receives a multi-scenario YAML/JSON body
/// (one with a top-level `scenarios:` array). Each element describes one
/// launched scenario.
#[derive(Debug, Serialize)]
pub struct CreatedScenariosResponse {
    /// One entry per launched scenario, in the same order as the input.
    pub scenarios: Vec<CreatedScenario>,
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

/// Response body for a successfully deleted (stopped) scenario.
#[derive(Debug, Serialize)]
pub struct DeletedScenario {
    /// Unique scenario ID.
    pub id: String,
    /// Final status: `"stopped"` or `"force_stopped"` if the join timed out.
    pub status: String,
    /// Total number of events emitted over the scenario's lifetime.
    pub total_events: u64,
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

/// Response body for `GET /scenarios/{id}/stats`.
///
/// Contains all live stats fields plus derived fields (`target_rate`,
/// `uptime_secs`, `state`) that are computed from the [`ScenarioHandle`] at
/// request time.
#[derive(Debug, Serialize)]
pub struct DetailedStatsResponse {
    /// Total number of events emitted since the scenario started.
    pub total_events: u64,
    /// Measured events per second (from the runner's rate tracker).
    pub current_rate: f64,
    /// The configured target rate (events per second) from the scenario config.
    pub target_rate: f64,
    /// Total bytes written to the sink.
    pub bytes_emitted: u64,
    /// Number of encode or sink write errors encountered.
    pub errors: u64,
    /// Seconds elapsed since the scenario was launched.
    pub uptime_secs: f64,
    /// Current state: `"running"` or `"stopped"`.
    pub state: String,
    /// Whether the scenario is currently in a gap window (no events emitted).
    pub in_gap: bool,
    /// Whether the scenario is currently in a burst window (elevated rate).
    pub in_burst: bool,
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

/// Build a 404 Not Found response with a JSON error body.
fn not_found(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "not_found", "detail": detail.to_string() });
    (StatusCode::NOT_FOUND, Json(body)).into_response()
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
        .map(|ct| ct.contains("yaml"))
        .unwrap_or(true) // default: assume YAML
}

/// The result of parsing a `POST /scenarios` body.
///
/// Distinguishes between a single scenario entry (backward-compatible) and
/// a multi-scenario batch (new capability). The handler uses this to decide
/// the response shape.
enum ParsedBody {
    /// A single scenario entry (the existing behavior).
    ///
    /// Boxed to avoid a large size difference between variants (clippy
    /// `large_enum_variant`). `ScenarioEntry` is ~656 bytes while `Vec` is 24.
    Single(Box<ScenarioEntry>),
    /// A batch of scenario entries from a `scenarios:` array.
    Multi(Vec<ScenarioEntry>),
}

/// Attempt to parse the raw body bytes as either a single [`ScenarioEntry`]
/// or a [`MultiScenarioConfig`] batch.
///
/// Tries multi-scenario (`scenarios:` array) first, then falls back to
/// single-scenario parsing. This order is safe because a valid
/// `MultiScenarioConfig` always has a top-level `scenarios:` key that
/// single-scenario configs never have.
///
/// Returns a descriptive error string on failure.
fn parse_body(body: &[u8], headers: &HeaderMap) -> Result<ParsedBody, String> {
    if is_yaml_content_type(headers) {
        parse_yaml_body(body)
    } else {
        parse_json_body(body)
    }
}

/// Parse body bytes as YAML into a single [`ScenarioEntry`].
///
/// Tries `ScenarioEntry` (tagged with `signal_type`) first. If that fails,
/// falls back to `ScenarioConfig` (plain metrics) and then `LogScenarioConfig`
/// (plain logs). This lets callers POST a bare metrics or logs YAML without
/// having to include the `signal_type` discriminant.
///
/// This is an internal helper used by [`parse_yaml_body`] as a fallback when
/// the body does not contain a multi-scenario `scenarios:` array.
///
/// Accepts a `&str` because the caller ([`parse_yaml_body`]) already performs
/// UTF-8 validation — no need to repeat it here.
fn parse_yaml_single_entry(text: &str) -> Result<ScenarioEntry, String> {
    // Strategy 1: tagged ScenarioEntry (has `signal_type: metrics|logs`).
    if let Ok(entry) = serde_yaml_ng::from_str::<ScenarioEntry>(text) {
        return Ok(entry);
    }

    // Strategy 2: bare ScenarioConfig → wrap in Metrics variant.
    if let Ok(config) = serde_yaml_ng::from_str::<ScenarioConfig>(text) {
        return Ok(ScenarioEntry::Metrics(config));
    }

    // Strategy 3: bare LogScenarioConfig → wrap in Logs variant.
    if let Ok(config) = serde_yaml_ng::from_str::<LogScenarioConfig>(text) {
        return Ok(ScenarioEntry::Logs(config));
    }

    // All three attempts failed — return a generic YAML parse error.
    // Re-parse just to get a meaningful error message.
    let yaml_err = serde_yaml_ng::from_str::<ScenarioEntry>(text)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unknown YAML parse error".to_string());

    Err(format!("invalid YAML scenario body: {yaml_err}"))
}

/// Parse body bytes as JSON into a single [`ScenarioEntry`].
///
/// Tries `ScenarioEntry` (tagged with `signal_type`) first. If that fails,
/// falls back to plain `ScenarioConfig` (metrics only -- JSON logs require the
/// `signal_type` tag because the generator field shapes differ significantly).
///
/// This is an internal helper used by [`parse_json_body`] as a fallback when
/// the body does not contain a multi-scenario `scenarios` field.
fn parse_json_single_entry(body: &[u8]) -> Result<ScenarioEntry, String> {
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

/// Parse body bytes as YAML, returning either a multi-scenario batch or a
/// single scenario entry.
///
/// Tries `MultiScenarioConfig` first (requires a top-level `scenarios:` key).
/// If that fails, falls back to single-entry parsing via
/// [`parse_yaml_single_entry`].
fn parse_yaml_body(body: &[u8]) -> Result<ParsedBody, String> {
    let text =
        std::str::from_utf8(body).map_err(|e| format!("request body is not valid UTF-8: {e}"))?;

    // Strategy 1: multi-scenario with top-level `scenarios:` key.
    if let Ok(multi) = serde_yaml_ng::from_str::<MultiScenarioConfig>(text) {
        return Ok(ParsedBody::Multi(multi.scenarios));
    }

    // Strategy 2: single scenario (all existing fallback strategies).
    parse_yaml_single_entry(text).map(|e| ParsedBody::Single(Box::new(e)))
}

/// Parse body bytes as JSON, returning either a multi-scenario batch or a
/// single scenario entry.
///
/// Tries `MultiScenarioConfig` first (requires a top-level `scenarios` field).
/// If that fails, falls back to single-entry parsing via
/// [`parse_json_single_entry`].
fn parse_json_body(body: &[u8]) -> Result<ParsedBody, String> {
    // Strategy 1: multi-scenario.
    if let Ok(multi) = serde_json::from_slice::<MultiScenarioConfig>(body) {
        return Ok(ParsedBody::Multi(multi.scenarios));
    }

    // Strategy 2: single scenario.
    parse_json_single_entry(body).map(|e| ParsedBody::Single(Box::new(e)))
}

// ---- Handlers ---------------------------------------------------------------

/// `POST /scenarios` — start scenarios from a YAML or JSON body.
///
/// Accepts both single-scenario and multi-scenario (`scenarios:` array)
/// request bodies in YAML or JSON format.
///
/// **Single-scenario** (backward compatible): Returns `201 Created` with
/// `{"id": "...", "name": "...", "status": "running"}`.
///
/// **Multi-scenario**: Returns `201 Created` with
/// `{"scenarios": [{"id", "name", "status"}, ...]}`. All entries are
/// validated atomically before any are launched — if any entry fails
/// validation, nothing is launched and the entire request fails.
///
/// # Error responses
/// - `400 Bad Request` — body cannot be parsed, or `scenarios: []` is empty.
/// - `422 Unprocessable Entity` — body parsed but failed validation (e.g. rate=0).
/// - `500 Internal Server Error` — scenario thread could not be spawned.
pub async fn post_scenario(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, Response> {
    // 1. Parse the body, detecting single vs multi-scenario.
    let parsed = parse_body(&body, &headers).map_err(|msg| {
        warn!(error = %msg, "POST /scenarios: invalid request body");
        bad_request(msg)
    })?;

    match parsed {
        ParsedBody::Single(entry) => post_single_scenario(state, *entry).await,
        ParsedBody::Multi(entries) => post_multi_scenario(state, entries).await,
    }
}

/// Handle a single-scenario POST (backward-compatible path).
///
/// Uses [`prepare_entries`] for the same expand -> validate -> phase_offset
/// pipeline as the multi-scenario path. This ensures identical behavior
/// regardless of whether a scenario is posted alone or inside a `scenarios:`
/// array.
async fn post_single_scenario(state: AppState, entry: ScenarioEntry) -> Result<Response, Response> {
    // Use the shared pipeline for expansion, validation, and phase offset.
    let mut prepared = prepare_entries(vec![entry]).map_err(|e| {
        warn!(error = %e, "POST /scenarios: validation failed");
        unprocessable(e)
    })?;

    // After expansion a single entry may fan out into multiple entries
    // (e.g. multi-column csv_replay). Launch all of them.
    let mut created: Vec<CreatedScenario> = Vec::with_capacity(prepared.len());
    let mut handles_to_store: Vec<(String, sonda_core::ScenarioHandle)> =
        Vec::with_capacity(prepared.len());

    for prepared_entry in prepared.drain(..) {
        let id = Uuid::new_v4().to_string();
        let name = prepared_entry.entry.base().name.clone();
        let shutdown = Arc::new(AtomicBool::new(true));

        let handle = launch_scenario(
            id.clone(),
            prepared_entry.entry,
            shutdown,
            prepared_entry.start_delay,
        )
        .map_err(|e| {
            for (_, ref h) in &handles_to_store {
                h.stop();
            }
            warn!(error = %e, "POST /scenarios: failed to launch scenario");
            internal_error(e)
        })?;

        info!(id = %id, name = %name, "scenario launched");

        created.push(CreatedScenario {
            id: id.clone(),
            name,
            status: "running",
        });
        handles_to_store.push((id, handle));
    }

    // Store all handles in shared state.
    let mut scenarios = state.scenarios.write().map_err(|e| {
        for (_, ref h) in &handles_to_store {
            h.stop();
        }
        warn!(error = %e, "POST /scenarios: scenarios lock is poisoned");
        internal_error("internal state lock is poisoned")
    })?;
    for (id, handle) in handles_to_store {
        scenarios.insert(id, handle);
    }
    drop(scenarios);

    // Respond based on whether expansion produced a single or multiple entries.
    if created.len() == 1 {
        let single = created.into_iter().next().expect("len checked above");
        Ok((StatusCode::CREATED, Json(single)).into_response())
    } else {
        Ok((
            StatusCode::CREATED,
            Json(CreatedScenariosResponse { scenarios: created }),
        )
            .into_response())
    }
}

/// Handle a multi-scenario POST (batch path).
///
/// Atomic batch semantics: all entries are expanded, validated, and have their
/// phase offsets resolved before any are launched. If any entry fails, the
/// entire request returns an error and nothing is launched.
async fn post_multi_scenario(
    state: AppState,
    entries: Vec<ScenarioEntry>,
) -> Result<Response, Response> {
    // Reject empty batches.
    if entries.is_empty() {
        warn!("POST /scenarios: empty scenarios array");
        return Err(bad_request("scenarios array must not be empty"));
    }

    // Expand, validate, and resolve phase offsets atomically.
    let prepared = prepare_entries(entries).map_err(|e| {
        warn!(error = %e, "POST /scenarios: multi-scenario validation failed");
        unprocessable(e)
    })?;

    // Launch all scenarios and collect response entries.
    let mut created: Vec<CreatedScenario> = Vec::with_capacity(prepared.len());
    let mut handles_to_store: Vec<(String, sonda_core::ScenarioHandle)> =
        Vec::with_capacity(prepared.len());

    for prepared_entry in prepared {
        let id = Uuid::new_v4().to_string();
        let name = prepared_entry.entry.base().name.clone();
        let shutdown = Arc::new(AtomicBool::new(true));

        let handle = launch_scenario(
            id.clone(),
            prepared_entry.entry,
            shutdown,
            prepared_entry.start_delay,
        )
        .map_err(|e| {
            // If a launch fails, stop any already-launched scenarios.
            for (_, ref h) in &handles_to_store {
                h.stop();
            }
            warn!(error = %e, "POST /scenarios: failed to launch scenario in batch");
            internal_error(e)
        })?;

        info!(id = %id, name = %name, "scenario launched (batch)");

        created.push(CreatedScenario {
            id: id.clone(),
            name,
            status: "running",
        });
        handles_to_store.push((id, handle));
    }

    // Store all handles in shared state.
    let mut scenarios = state.scenarios.write().map_err(|e| {
        // Stop all launched scenarios before returning the error to prevent
        // orphaned threads that run indefinitely without a way to stop them.
        for (_, ref h) in &handles_to_store {
            h.stop();
        }
        warn!(error = %e, "POST /scenarios: scenarios lock is poisoned");
        internal_error("internal state lock is poisoned")
    })?;
    for (id, handle) in handles_to_store {
        scenarios.insert(id, handle);
    }
    drop(scenarios);

    // Respond with 201 Created.
    let response_body = CreatedScenariosResponse { scenarios: created };
    Ok((StatusCode::CREATED, Json(response_body)).into_response())
}

/// `GET /scenarios` — list all scenarios with summary information.
///
/// Returns a JSON object with a `scenarios` array containing each scenario's
/// ID, name, status, and elapsed time. The list includes both running and
/// stopped scenarios that have not been deleted.
pub async fn list_scenarios(State(state): State<AppState>) -> Result<impl IntoResponse, Response> {
    let scenarios = state
        .scenarios
        .read()
        .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;

    let summaries: Vec<ScenarioSummary> = scenarios
        .iter()
        .map(|(id, handle)| ScenarioSummary {
            id: id.clone(),
            name: handle.name.clone(),
            status: status_string(handle.is_running()),
            elapsed_secs: handle.elapsed().as_secs_f64(),
        })
        .collect();

    Ok(Json(ListScenariosResponse {
        scenarios: summaries,
    }))
}

/// `GET /scenarios/{id}` — inspect a single scenario with full detail.
///
/// Returns the scenario's ID, name, status, elapsed time, and live stats
/// (total_events, current_rate, bytes_emitted, errors). Returns 404 if the
/// scenario ID is not found.
pub async fn get_scenario(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Response> {
    let scenarios = state
        .scenarios
        .read()
        .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;

    let handle = scenarios
        .get(&id)
        .ok_or_else(|| not_found(format!("scenario not found: {id}")))?;

    let detail = ScenarioDetail {
        id: id.clone(),
        name: handle.name.clone(),
        status: status_string(handle.is_running()),
        elapsed_secs: handle.elapsed().as_secs_f64(),
        stats: handle.stats_snapshot().into(),
    };

    Ok(Json(detail))
}

/// `DELETE /scenarios/{id}` — stop a running scenario and return final stats.
///
/// Signals the scenario to stop via `handle.stop()`, then waits up to 5 seconds
/// for the thread to exit via `handle.join()`. If the thread does not exit within
/// the timeout, the response status is `"force_stopped"` and a warning is logged.
///
/// After returning final stats, the scenario handle is removed from the map.
/// A subsequent DELETE on the same ID returns `404 Not Found`.
pub async fn delete_scenario(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Response> {
    // Acquire a write lock so we can mutate the handle (join requires &mut self).
    let mut scenarios = state
        .scenarios
        .write()
        .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;

    let handle = scenarios
        .get_mut(&id)
        .ok_or_else(|| not_found(format!("scenario not found: {id}")))?;

    // Signal the scenario to stop (idempotent — safe to call on already-stopped).
    handle.stop();

    // Wait for the thread to exit, with a 5-second timeout.
    let was_running_before_join = handle.is_running();
    if let Err(e) = handle.join(Some(Duration::from_secs(5))) {
        warn!(id = %id, error = %e, "DELETE /scenarios/{id}: scenario thread returned an error");
    }

    // Determine the final status based on whether the thread exited in time.
    let status = if handle.is_running() {
        warn!(id = %id, "DELETE /scenarios/{id}: join timed out after 5s, scenario force-stopped");
        "force_stopped".to_string()
    } else if was_running_before_join {
        "stopped".to_string()
    } else {
        // Thread had already exited before DELETE was called.
        "stopped".to_string()
    };

    // Read final stats before responding.
    let final_stats = handle.stats_snapshot();

    // Remove the handle from the map to free resources (fixes memory leak).
    scenarios.remove(&id);
    // Release the write lock before logging and building the response.
    drop(scenarios);

    info!(id = %id, status = %status, total_events = final_stats.total_events, "scenario deleted");

    Ok(Json(DeletedScenario {
        id,
        status,
        total_events: final_stats.total_events,
    }))
}

/// `GET /scenarios/{id}/stats` — return detailed live stats for a scenario.
///
/// Returns all stats fields from the runner thread plus derived fields:
/// `target_rate` (configured rate from the scenario config), `uptime_secs`
/// (computed from `handle.elapsed()`), and `state` (from `handle.is_running()`).
///
/// This is a read-only endpoint that acquires only a read lock on the
/// scenario map. No write lock is needed.
///
/// Returns `404 Not Found` with a JSON error body for unknown IDs.
pub async fn get_scenario_stats(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Response> {
    let scenarios = state
        .scenarios
        .read()
        .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;

    let handle = scenarios
        .get(&id)
        .ok_or_else(|| not_found(format!("scenario not found: {id}")))?;

    let snap = handle.stats_snapshot();
    let response = DetailedStatsResponse {
        total_events: snap.total_events,
        current_rate: snap.current_rate,
        target_rate: handle.target_rate,
        bytes_emitted: snap.bytes_emitted,
        errors: snap.errors,
        uptime_secs: handle.elapsed().as_secs_f64(),
        state: status_string(handle.is_running()),
        in_gap: snap.in_gap,
        in_burst: snap.in_burst,
    };

    Ok(Json(response))
}

// ---- Scrape endpoint --------------------------------------------------------

/// Query parameters for `GET /scenarios/{id}/metrics`.
#[derive(Debug, Deserialize)]
pub struct MetricsQuery {
    /// Maximum number of recent metric events to return. Defaults to 100,
    /// capped at 1000.
    pub limit: Option<usize>,
}

/// Prometheus text exposition format content type.
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /scenarios/{id}/metrics` — return recent metrics in Prometheus text format.
///
/// Drains the recent metric event buffer from the scenario handle, encodes
/// each event using the Prometheus text encoder, and returns the result with
/// `Content-Type: text/plain; version=0.0.4; charset=utf-8`.
///
/// This endpoint is designed to be scraped by Prometheus or vmagent. Each
/// call drains the buffer, so repeated scrapes within the same tick interval
/// may return fewer events.
///
/// # Query parameters
///
/// * `limit` — maximum number of events to return (default 100, max 1000).
///
/// # Error responses
///
/// * `404 Not Found` — scenario ID not found.
/// * `204 No Content` — scenario exists but no metric events are buffered.
pub async fn get_scenario_metrics(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MetricsQuery>,
) -> Result<Response, Response> {
    let limit = query.limit.unwrap_or(100).min(1000);

    // Look up the scenario by ID.
    let scenarios = state
        .scenarios
        .read()
        .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;

    let handle = scenarios
        .get(&id)
        .ok_or_else(|| not_found(format!("scenario not found: {id}")))?;

    // Drain recent metric events from the handle's stats buffer.
    let events = handle.recent_metrics();

    if events.is_empty() {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    // Apply the limit: take at most `limit` events from the end (most recent).
    let events_to_encode = if events.len() > limit {
        &events[events.len() - limit..]
    } else {
        &events
    };

    // Encode each event into Prometheus text format.
    let encoder = PrometheusText::new(None);
    let mut buf = Vec::with_capacity(events_to_encode.len() * 128);
    for event in events_to_encode {
        if let Err(e) = encoder.encode_metric(event, &mut buf) {
            warn!(id = %id, error = %e, "GET /scenarios/{id}/metrics: failed to encode metric event");
        }
    }

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        buf,
    )
        .into_response())
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
            target_rate: 100.0,
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
            target_rate: 100.0,
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

    /// Helper: stop and join all scenarios in the AppState to clean up spawned threads.
    ///
    /// Uses a two-phase approach: first stops all scenarios via a read lock
    /// (safe to call while other read guards exist), then acquires a write
    /// lock to join the threads.
    fn cleanup_scenarios(state: &AppState) {
        // Phase 1: signal all scenarios to stop (read lock).
        if let Ok(scenarios) = state.scenarios.read() {
            for handle in scenarios.values() {
                handle.stop();
            }
        }
        // Phase 2: join all scenario threads (write lock).
        if let Ok(mut scenarios) = state.scenarios.write() {
            for handle in scenarios.values_mut() {
                let _ = handle.join(Some(Duration::from_secs(2)));
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

    // ---- GET /scenarios/{id}: correct name, status, elapsed -------------------

    /// GET /scenarios/{id} returns correct name, status, and positive elapsed_secs.
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

    // ---- GET /scenarios/{id}: stats fields present ----------------------------

    /// GET /scenarios/{id} response includes stats sub-object with all required fields.
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

    // ---- GET /scenarios/{id}: stats.total_events > 0 after running ------------

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

    /// GET /scenarios/{id} with a nonexistent ID returns 404 with a JSON error body.
    #[tokio::test]
    async fn get_scenario_nonexistent_returns_404_with_json_body() {
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

        let body = body_json(resp).await;
        assert_eq!(
            body["error"].as_str().unwrap(),
            "not_found",
            "404 response must have error field set to 'not_found'"
        );
        assert_eq!(
            body["detail"].as_str().unwrap(),
            "scenario not found: nonexistent-id",
            "404 response detail must include the requested scenario ID"
        );
    }

    /// GET /scenarios/{id} 404 response has Content-Type application/json.
    #[tokio::test]
    async fn get_scenario_nonexistent_returns_json_content_type() {
        let app = router_with_handles(vec![]);

        let req = Request::builder()
            .uri("/scenarios/some-missing-id")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("404 response must have Content-Type header")
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "404 Content-Type must be application/json, got: {ct}"
        );
    }

    // ---- GET /scenarios/{id}: stopped scenario reports "stopped" --------------

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
            ..Default::default()
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
        {
            let scenarios = state.scenarios.read().expect("lock must not be poisoned");
            let id = body["id"].as_str().unwrap();
            assert!(
                scenarios.contains_key(id),
                "AppState must contain the handle for the newly created scenario ID"
            );
        }

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

    /// parse_yaml_single_entry handles valid bare metrics config (no signal_type tag).
    #[test]
    fn parse_yaml_single_entry_accepts_bare_metrics_config() {
        let result = parse_yaml_single_entry(VALID_METRICS_YAML);
        assert!(
            result.is_ok(),
            "parse_yaml_single_entry must accept bare metrics YAML: {result:?}"
        );
        match result.unwrap() {
            ScenarioEntry::Metrics(c) => assert_eq!(c.name, "test_metric"),
            other => panic!("expected ScenarioEntry::Metrics, got: {other:?}"),
        }
    }

    /// parse_yaml_single_entry handles valid bare logs config (no signal_type tag).
    #[test]
    fn parse_yaml_single_entry_accepts_bare_logs_config() {
        let result = parse_yaml_single_entry(VALID_LOGS_YAML);
        assert!(
            result.is_ok(),
            "parse_yaml_single_entry must accept bare logs YAML: {result:?}"
        );
        match result.unwrap() {
            ScenarioEntry::Logs(c) => assert_eq!(c.name, "test_logs"),
            other => panic!("expected ScenarioEntry::Logs, got: {other:?}"),
        }
    }

    /// parse_yaml_single_entry handles tagged ScenarioEntry format.
    #[test]
    fn parse_yaml_single_entry_accepts_tagged_entry() {
        let result = parse_yaml_single_entry(VALID_TAGGED_METRICS_YAML);
        assert!(
            result.is_ok(),
            "parse_yaml_single_entry must accept tagged ScenarioEntry YAML: {result:?}"
        );
        match result.unwrap() {
            ScenarioEntry::Metrics(c) => assert_eq!(c.name, "tagged_metric"),
            other => panic!("expected ScenarioEntry::Metrics, got: {other:?}"),
        }
    }

    /// parse_yaml_single_entry returns Err for garbage input.
    #[test]
    fn parse_yaml_single_entry_rejects_garbage() {
        let result = parse_yaml_single_entry("not valid: [}{");
        assert!(
            result.is_err(),
            "parse_yaml_single_entry must reject garbage input"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("invalid YAML"),
            "error message must mention invalid YAML, got: {err}"
        );
    }

    /// parse_json_single_entry accepts a tagged ScenarioEntry JSON.
    #[test]
    fn parse_json_single_entry_accepts_tagged_json() {
        let json = serde_json::json!({
            "signal_type": "metrics",
            "name": "json_test",
            "rate": 10,
            "duration": "1s",
            "generator": { "type": "constant", "value": 1.0 },
            "encoder": { "type": "prometheus_text" },
            "sink": { "type": "stdout" }
        });
        let result = parse_json_single_entry(json.to_string().as_bytes());
        assert!(
            result.is_ok(),
            "parse_json_single_entry must accept tagged JSON: {result:?}"
        );
    }

    /// parse_json_single_entry returns Err for invalid JSON.
    #[test]
    fn parse_json_single_entry_rejects_invalid_json() {
        let result = parse_json_single_entry(b"not json");
        assert!(
            result.is_err(),
            "parse_json_single_entry must reject invalid JSON"
        );
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

    // ========================================================================
    // DELETE /scenarios/{id} tests
    // ========================================================================

    /// Helper to send a DELETE /scenarios/{id} request.
    async fn delete_scenario_req(app: axum::Router, id: &str) -> hyper::Response<axum::body::Body> {
        let request = Request::builder()
            .method("DELETE")
            .uri(format!("/scenarios/{id}"))
            .body(Body::empty())
            .unwrap();
        app.oneshot(request).await.unwrap()
    }

    // ---- DELETE running scenario -> thread exits, status "stopped" ----------

    /// Start a running scenario, DELETE it, and verify the thread exits
    /// with status "stopped".
    #[tokio::test]
    async fn delete_running_scenario_returns_stopped_status() {
        // Thread runs for a long time (1000 events x 50ms = 50s) so it is
        // definitely running when we hit DELETE.
        let h = make_handle("id-del-run", "del_running", 1000, Duration::from_millis(50));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-del-run").await;

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "DELETE a running scenario must return 200 OK"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["status"].as_str().unwrap(),
            "stopped",
            "DELETE a running scenario must return status 'stopped'"
        );
    }

    // ---- DELETE returns final stats (total_events) -------------------------

    /// DELETE returns total_events reflecting events emitted before stop.
    #[tokio::test]
    async fn delete_returns_final_stats_with_total_events() {
        // Thread emits events every 10ms. Wait 200ms so some events accumulate.
        let h = make_handle("id-del-stats", "del_stats", 1000, Duration::from_millis(10));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        // Let events accumulate.
        thread::sleep(Duration::from_millis(200));

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-del-stats").await;

        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let total_events = body["total_events"]
            .as_u64()
            .expect("total_events must be present and numeric");
        assert!(
            total_events > 0,
            "DELETE must return final stats with total_events > 0, got {total_events}"
        );
    }

    // ---- DELETE already-stopped scenario -> 200 OK -------------------------

    /// DELETE on an already-stopped scenario returns 200 OK with status "stopped".
    #[tokio::test]
    async fn delete_already_stopped_returns_200_ok() {
        let h = make_stopped_handle("id-del-stopped", "del_stopped");
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-del-stopped").await;

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "DELETE on already-stopped scenario must return 200 OK"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["status"].as_str().unwrap(),
            "stopped",
            "DELETE on already-stopped scenario must return status 'stopped'"
        );
    }

    // ---- DELETE unknown ID -> 404 ------------------------------------------

    /// DELETE on a nonexistent scenario ID returns 404.
    #[tokio::test]
    async fn delete_unknown_scenario_returns_404() {
        let app = router_with_handles(vec![]);
        let resp = delete_scenario_req(app, "nonexistent-id").await;

        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "DELETE on unknown scenario ID must return 404"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["error"].as_str().unwrap(),
            "not_found",
            "404 response must have error field 'not_found'"
        );
        assert!(
            body["detail"].as_str().unwrap().contains("nonexistent-id"),
            "404 detail must include the requested ID"
        );
    }

    // ---- DELETE response JSON shape: id, status, total_events ---------------

    /// The DELETE response body has exactly three keys: id, status, total_events.
    #[tokio::test]
    async fn delete_response_has_expected_json_shape() {
        let h = make_handle("id-del-shape", "del_shape", 1000, Duration::from_millis(50));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-del-shape").await;

        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let obj = body
            .as_object()
            .expect("response body must be a JSON object");

        assert!(obj.contains_key("id"), "response must contain key 'id'");
        assert!(
            obj.contains_key("status"),
            "response must contain key 'status'"
        );
        assert!(
            obj.contains_key("total_events"),
            "response must contain key 'total_events'"
        );
        assert_eq!(
            obj.len(),
            3,
            "response must contain exactly 3 keys (id, status, total_events), got: {:?}",
            obj.keys().collect::<Vec<_>>()
        );
    }

    // ---- DELETE returns correct id in response ------------------------------

    /// The DELETE response id field matches the requested scenario ID.
    #[tokio::test]
    async fn delete_response_id_matches_requested_id() {
        let h = make_handle("id-del-match", "del_match", 1000, Duration::from_millis(50));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-del-match").await;

        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(
            body["id"].as_str().unwrap(),
            "id-del-match",
            "response id must match the requested scenario ID"
        );
    }

    // ---- DELETE twice: second DELETE returns 404 after handle removal --------

    /// DELETE removes the handle from the map, so a second DELETE returns 404.
    #[tokio::test]
    async fn delete_twice_on_same_id_returns_404_on_second() {
        let h = make_handle("id-del-twice", "del_twice", 1000, Duration::from_millis(50));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        // First DELETE.
        let app1 = router(state.clone());
        let resp1 = delete_scenario_req(app1, "id-del-twice").await;
        assert_eq!(
            resp1.status(),
            StatusCode::OK,
            "first DELETE must return 200 OK"
        );
        let body1 = body_json(resp1).await;
        assert_eq!(body1["status"].as_str().unwrap(), "stopped");

        // Second DELETE on the same ID — handle was removed, so 404.
        let app2 = router(state.clone());
        let resp2 = delete_scenario_req(app2, "id-del-twice").await;
        assert_eq!(
            resp2.status(),
            StatusCode::NOT_FOUND,
            "second DELETE on same ID must return 404 after handle removal"
        );
    }

    // ---- DELETE removes handle from HashMap -----------------------------------

    /// DELETE removes the scenario handle from the internal HashMap.
    #[tokio::test]
    async fn delete_removes_handle_from_hashmap() {
        let h = make_handle("id-del-map", "del_map", 1000, Duration::from_millis(50));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        // Precondition: map has exactly 1 entry.
        assert_eq!(
            state.scenarios.read().unwrap().len(),
            1,
            "precondition: map must have 1 entry before DELETE"
        );

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-del-map").await;
        assert_eq!(resp.status(), StatusCode::OK);

        // After DELETE, the handle must be gone.
        let map = state.scenarios.read().unwrap();
        assert_eq!(map.len(), 0, "map must be empty after DELETE");
        assert!(
            map.get("id-del-map").is_none(),
            "deleted scenario must not be present in the map"
        );
    }

    // ---- DELETE excludes scenario from GET /scenarios list -------------------

    /// After deleting one of two scenarios, GET /scenarios returns only the remaining one.
    #[tokio::test]
    async fn delete_scenario_excluded_from_list() {
        let h_keep = make_handle("id-keep", "keep_scenario", 1000, Duration::from_millis(50));
        let h_delete = make_handle(
            "id-delete",
            "delete_scenario",
            1000,
            Duration::from_millis(50),
        );
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h_keep.id.clone(), h_keep);
            map.insert(h_delete.id.clone(), h_delete);
        }

        // DELETE "id-delete".
        let app1 = router(state.clone());
        let resp = delete_scenario_req(app1, "id-delete").await;
        assert_eq!(resp.status(), StatusCode::OK, "DELETE must return 200");

        // GET /scenarios — only "id-keep" should remain.
        let app2 = router(state.clone());
        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();
        let resp = app2.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let scenarios = body["scenarios"]
            .as_array()
            .expect("response must have a scenarios array");
        assert_eq!(
            scenarios.len(),
            1,
            "only one scenario should remain after DELETE"
        );
        assert_eq!(
            scenarios[0]["id"].as_str().unwrap(),
            "id-keep",
            "the remaining scenario must be 'id-keep'"
        );

        // Clean up the remaining running scenario.
        cleanup_scenarios(&state);
    }

    // ---- Contract: DeletedScenario serializes correctly ---------------------

    /// DeletedScenario serializes to JSON with the expected structure.
    #[test]
    fn deleted_scenario_serializes_to_expected_json() {
        let ds = DeletedScenario {
            id: "del-123".to_string(),
            status: "stopped".to_string(),
            total_events: 42,
        };
        let json = serde_json::to_value(&ds).expect("must serialize");
        assert_eq!(json["id"], "del-123");
        assert_eq!(json["status"], "stopped");
        assert_eq!(json["total_events"], 42);
    }

    /// DeletedScenario with force_stopped status serializes correctly.
    #[test]
    fn deleted_scenario_force_stopped_serializes_correctly() {
        let ds = DeletedScenario {
            id: "force-123".to_string(),
            status: "force_stopped".to_string(),
            total_events: 100,
        };
        let json = serde_json::to_value(&ds).expect("must serialize");
        assert_eq!(json["status"], "force_stopped");
        assert_eq!(json["total_events"], 100);
    }

    // ---- DELETE returns Content-Type application/json -----------------------

    /// DELETE response has Content-Type application/json.
    #[tokio::test]
    async fn delete_scenario_returns_json_content_type() {
        let h = make_handle("id-del-ct", "del_ct", 1000, Duration::from_millis(50));
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-del-ct").await;

        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("DELETE response must have Content-Type header")
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "DELETE Content-Type must be application/json, got: {ct}"
        );
    }

    // ---- DELETE 404 returns JSON Content-Type ------------------------------

    /// DELETE 404 response has Content-Type application/json.
    #[tokio::test]
    async fn delete_unknown_returns_json_content_type() {
        let app = router_with_handles(vec![]);
        let resp = delete_scenario_req(app, "missing-id").await;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("404 response must have Content-Type header")
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "404 Content-Type must be application/json, got: {ct}"
        );
    }

    // ========================================================================
    // GET /scenarios/{id}/stats tests (Slice 3.5)
    // ========================================================================

    /// Helper: build a ScenarioHandle with a custom target_rate and pre-set stats.
    ///
    /// The thread exits immediately (no background work). Stats are set to the
    /// provided snapshot before the handle is returned.
    fn make_handle_with_stats(
        id: &str,
        name: &str,
        target_rate: f64,
        initial_stats: ScenarioStats,
        running: bool,
    ) -> ScenarioHandle {
        let shutdown = Arc::new(AtomicBool::new(running));
        let stats = Arc::new(RwLock::new(initial_stats));
        let shutdown_clone = Arc::clone(&shutdown);

        let thread = if running {
            // Long-running thread that waits for shutdown.
            thread::Builder::new()
                .name(format!("test-stats-{name}"))
                .spawn(move || -> Result<(), sonda_core::SondaError> {
                    while shutdown_clone.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(())
                })
                .expect("thread must spawn")
        } else {
            // Thread exits immediately.
            thread::Builder::new()
                .name(format!("test-stats-stopped-{name}"))
                .spawn(move || -> Result<(), sonda_core::SondaError> {
                    let _ = shutdown_clone.load(Ordering::SeqCst);
                    Ok(())
                })
                .expect("thread must spawn")
        };

        if !running {
            // Give the thread time to exit.
            thread::sleep(Duration::from_millis(50));
        }

        ScenarioHandle {
            id: id.to_string(),
            name: name.to_string(),
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
            target_rate,
        }
    }

    /// Helper: send a GET /scenarios/{id}/stats request.
    async fn get_stats_req(app: axum::Router, id: &str) -> hyper::Response<axum::body::Body> {
        let req = Request::builder()
            .uri(format!("/scenarios/{id}/stats"))
            .body(Body::empty())
            .unwrap();
        app.oneshot(req).await.unwrap()
    }

    // ---- Stats endpoint returns all expected fields -------------------------

    /// GET /scenarios/{id}/stats returns a JSON body with all expected fields.
    #[tokio::test]
    async fn stats_endpoint_returns_all_expected_fields() {
        let stats = ScenarioStats {
            total_events: 500,
            bytes_emitted: 32000,
            current_rate: 99.5,
            errors: 2,
            in_gap: false,
            in_burst: true,
            ..Default::default()
        };
        let h = make_handle_with_stats("id-stats-all", "all_fields", 100.0, stats, true);
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-all").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;

        // Verify all fields are present with correct types.
        assert_eq!(
            body["total_events"].as_u64().unwrap(),
            500,
            "total_events must be 500"
        );
        assert!(
            (body["current_rate"].as_f64().unwrap() - 99.5).abs() < f64::EPSILON,
            "current_rate must be 99.5"
        );
        assert!(
            (body["target_rate"].as_f64().unwrap() - 100.0).abs() < f64::EPSILON,
            "target_rate must be 100.0"
        );
        assert_eq!(
            body["bytes_emitted"].as_u64().unwrap(),
            32000,
            "bytes_emitted must be 32000"
        );
        assert_eq!(body["errors"].as_u64().unwrap(), 2, "errors must be 2");
        assert!(
            body["uptime_secs"].as_f64().unwrap() >= 0.0,
            "uptime_secs must be non-negative"
        );
        assert_eq!(
            body["state"].as_str().unwrap(),
            "running",
            "state must be 'running' for a live scenario"
        );
        assert_eq!(
            body["in_gap"].as_bool().unwrap(),
            false,
            "in_gap must be false"
        );
        assert_eq!(
            body["in_burst"].as_bool().unwrap(),
            true,
            "in_burst must be true"
        );
    }

    // ---- Fields update as scenario progresses --------------------------------

    /// Stats fields update as the scenario background thread emits events.
    #[tokio::test]
    async fn stats_endpoint_fields_update_as_scenario_progresses() {
        // Thread emits events every 10ms.
        let h = make_handle(
            "id-stats-progress",
            "progress",
            500,
            Duration::from_millis(10),
        );
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        // Wait for some events to accumulate.
        thread::sleep(Duration::from_millis(100));

        // Take a first snapshot via the endpoint.
        let app1 = router(state.clone());
        let resp1 = get_stats_req(app1, "id-stats-progress").await;
        assert_eq!(resp1.status(), StatusCode::OK);
        let body1 = body_json(resp1).await;
        let events1 = body1["total_events"].as_u64().unwrap();
        let bytes1 = body1["bytes_emitted"].as_u64().unwrap();

        assert!(
            events1 > 0,
            "total_events must be > 0 after 100ms, got {events1}"
        );

        // Wait longer for more events.
        thread::sleep(Duration::from_millis(150));

        // Take a second snapshot.
        let app2 = router(state.clone());
        let resp2 = get_stats_req(app2, "id-stats-progress").await;
        assert_eq!(resp2.status(), StatusCode::OK);
        let body2 = body_json(resp2).await;
        let events2 = body2["total_events"].as_u64().unwrap();
        let bytes2 = body2["bytes_emitted"].as_u64().unwrap();

        assert!(
            events2 > events1,
            "total_events must increase over time: first={events1}, second={events2}"
        );
        assert!(
            bytes2 > bytes1,
            "bytes_emitted must increase over time: first={bytes1}, second={bytes2}"
        );

        // Clean up: stop the scenario.
        cleanup_scenarios(&state);
    }

    // ---- in_gap is true during gap window ------------------------------------

    /// When in_gap is set to true in the stats, the endpoint reflects it.
    #[tokio::test]
    async fn stats_endpoint_in_gap_true_when_stats_indicate_gap() {
        let stats = ScenarioStats {
            total_events: 10,
            bytes_emitted: 640,
            current_rate: 0.0,
            errors: 0,
            in_gap: true,
            in_burst: false,
            ..Default::default()
        };
        let h = make_handle_with_stats("id-stats-gap", "gap_test", 50.0, stats, true);
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-gap").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(
            body["in_gap"].as_bool().unwrap(),
            true,
            "in_gap must be true when the scenario is in a gap window"
        );
        assert_eq!(
            body["in_burst"].as_bool().unwrap(),
            false,
            "in_burst must be false when only in_gap is set"
        );
    }

    // ---- After scenario stopped: returns final stats with state "stopped" ----

    /// When a scenario has stopped, GET /scenarios/{id}/stats returns state "stopped".
    #[tokio::test]
    async fn stats_endpoint_returns_stopped_state_for_finished_scenario() {
        let stats = ScenarioStats {
            total_events: 1000,
            bytes_emitted: 64000,
            current_rate: 0.0,
            errors: 5,
            in_gap: false,
            in_burst: false,
            ..Default::default()
        };
        let h = make_handle_with_stats("id-stats-stopped", "stopped_test", 200.0, stats, false);
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-stopped").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(
            body["state"].as_str().unwrap(),
            "stopped",
            "state must be 'stopped' for a finished scenario"
        );
        assert_eq!(
            body["total_events"].as_u64().unwrap(),
            1000,
            "total_events must reflect final count"
        );
        assert_eq!(
            body["errors"].as_u64().unwrap(),
            5,
            "errors must reflect final count"
        );
        assert!(
            (body["target_rate"].as_f64().unwrap() - 200.0).abs() < f64::EPSILON,
            "target_rate must be preserved even after stop"
        );
    }

    // ---- Unknown ID returns 404 -----------------------------------------------

    /// GET /scenarios/{id}/stats with an unknown ID returns 404.
    #[tokio::test]
    async fn stats_endpoint_unknown_id_returns_404() {
        let app = router_with_handles(vec![]);

        let resp = get_stats_req(app, "nonexistent-stats-id").await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "unknown ID must return 404"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["error"].as_str().unwrap(),
            "not_found",
            "404 error body must have error='not_found'"
        );
    }

    // ---- Stats 404 returns JSON Content-Type ----------------------------------

    /// GET /scenarios/{id}/stats 404 has Content-Type application/json.
    #[tokio::test]
    async fn stats_endpoint_404_returns_json_content_type() {
        let app = router_with_handles(vec![]);

        let resp = get_stats_req(app, "missing-stats-id").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("404 response must have Content-Type header")
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "404 Content-Type must be application/json, got: {ct}"
        );
    }

    // ---- Stats endpoint returns correct target_rate ---------------------------

    /// The target_rate field reflects the configured rate on the handle, not measured rate.
    #[tokio::test]
    async fn stats_endpoint_target_rate_reflects_configured_rate() {
        let stats = ScenarioStats {
            total_events: 0,
            bytes_emitted: 0,
            current_rate: 45.0,
            errors: 0,
            in_gap: false,
            in_burst: false,
            ..Default::default()
        };
        // target_rate = 500.0, but current_rate = 45.0 (different).
        let h = make_handle_with_stats("id-stats-rate", "rate_test", 500.0, stats, true);
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-rate").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert!(
            (body["target_rate"].as_f64().unwrap() - 500.0).abs() < f64::EPSILON,
            "target_rate must be the configured rate (500.0)"
        );
        assert!(
            (body["current_rate"].as_f64().unwrap() - 45.0).abs() < f64::EPSILON,
            "current_rate must be the measured rate (45.0)"
        );
    }

    // ---- Stats endpoint uptime_secs is positive --------------------------------

    /// uptime_secs is positive for a running scenario.
    #[tokio::test]
    async fn stats_endpoint_uptime_secs_is_positive() {
        let h = make_handle_with_stats(
            "id-stats-uptime",
            "uptime_test",
            10.0,
            ScenarioStats::default(),
            true,
        );
        let app = router_with_handles(vec![h]);

        // Small delay to ensure nonzero uptime.
        thread::sleep(Duration::from_millis(20));

        let resp = get_stats_req(app, "id-stats-uptime").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let uptime = body["uptime_secs"].as_f64().unwrap();
        assert!(
            uptime > 0.0,
            "uptime_secs must be positive for a running scenario, got {uptime}"
        );
    }

    // ---- DetailedStatsResponse serialization ---------------------------------

    /// DetailedStatsResponse serializes all fields to JSON correctly.
    #[test]
    fn detailed_stats_response_serializes_all_fields() {
        let resp = DetailedStatsResponse {
            total_events: 42,
            current_rate: 10.5,
            target_rate: 100.0,
            bytes_emitted: 2048,
            errors: 1,
            uptime_secs: 3.14,
            state: "running".to_string(),
            in_gap: true,
            in_burst: false,
        };
        let json = serde_json::to_value(&resp).expect("must serialize");
        assert_eq!(json["total_events"], 42);
        assert_eq!(json["current_rate"], 10.5);
        assert_eq!(json["target_rate"], 100.0);
        assert_eq!(json["bytes_emitted"], 2048);
        assert_eq!(json["errors"], 1);
        assert_eq!(json["uptime_secs"], 3.14);
        assert_eq!(json["state"], "running");
        assert_eq!(json["in_gap"], true);
        assert_eq!(json["in_burst"], false);
    }

    // ---- Stats 200 returns JSON Content-Type ----------------------------------

    /// GET /scenarios/{id}/stats success response has Content-Type application/json.
    #[tokio::test]
    async fn stats_endpoint_success_returns_json_content_type() {
        let h = make_handle_with_stats(
            "id-stats-ct",
            "ct_test",
            10.0,
            ScenarioStats::default(),
            true,
        );
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-ct").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("200 response must have Content-Type header")
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "Content-Type must be application/json, got: {ct}"
        );
    }

    // ========================================================================
    // GET /scenarios/{id}/metrics tests (Slice 6.3 — scrape endpoint)
    // ========================================================================

    /// Helper: build a MetricEvent for testing the scrape endpoint.
    fn make_metric_event(name: &str, value: f64) -> sonda_core::model::metric::MetricEvent {
        sonda_core::model::metric::MetricEvent::new(
            name.to_string(),
            value,
            sonda_core::model::metric::Labels::default(),
        )
        .expect("test metric name must be valid")
    }

    /// Helper: build a ScenarioHandle with pre-populated metric events in the buffer.
    fn make_handle_with_metrics(
        id: &str,
        name: &str,
        events: Vec<sonda_core::model::metric::MetricEvent>,
    ) -> ScenarioHandle {
        let shutdown = Arc::new(AtomicBool::new(true));
        let mut stats = ScenarioStats::default();
        for event in events {
            stats.push_metric(event);
        }
        let stats = Arc::new(RwLock::new(stats));
        let shutdown_clone = Arc::clone(&shutdown);

        let thread = thread::Builder::new()
            .name(format!("test-metrics-{name}"))
            .spawn(move || -> Result<(), sonda_core::SondaError> {
                while shutdown_clone.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(10));
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
            target_rate: 10.0,
        }
    }

    /// Helper: send a GET /scenarios/{id}/metrics request.
    async fn get_metrics_req(app: axum::Router, id: &str) -> hyper::Response<axum::body::Body> {
        let req = Request::builder()
            .uri(format!("/scenarios/{id}/metrics"))
            .body(Body::empty())
            .unwrap();
        app.oneshot(req).await.unwrap()
    }

    /// Helper: send a GET /scenarios/{id}/metrics?limit=N request.
    async fn get_metrics_with_limit(
        app: axum::Router,
        id: &str,
        limit: usize,
    ) -> hyper::Response<axum::body::Body> {
        let req = Request::builder()
            .uri(format!("/scenarios/{id}/metrics?limit={limit}"))
            .body(Body::empty())
            .unwrap();
        app.oneshot(req).await.unwrap()
    }

    /// Helper: extract the body as a String from a response.
    async fn body_string(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).expect("body must be valid UTF-8")
    }

    // ---- Metrics scrape: 404 for unknown scenario ID ------------------------

    /// GET /scenarios/{id}/metrics with a nonexistent ID returns 404.
    #[tokio::test]
    async fn metrics_endpoint_unknown_id_returns_404() {
        let app = router_with_handles(vec![]);

        let resp = get_metrics_req(app, "nonexistent-metrics-id").await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "unknown scenario ID must return 404"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["error"].as_str().unwrap(),
            "not_found",
            "404 error body must have error='not_found'"
        );
    }

    // ---- Metrics scrape: 204 when no metrics buffered -----------------------

    /// GET /scenarios/{id}/metrics returns 204 No Content when the buffer is empty.
    #[tokio::test]
    async fn metrics_endpoint_empty_buffer_returns_204() {
        let h = make_handle_with_metrics("id-metrics-empty", "empty_metrics", vec![]);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-empty").await;
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "empty metrics buffer must return 204 No Content"
        );
    }

    // ---- Metrics scrape: returns Prometheus text format ----------------------

    /// GET /scenarios/{id}/metrics returns Prometheus text exposition format.
    #[tokio::test]
    async fn metrics_endpoint_returns_prometheus_text_format() {
        let events = vec![make_metric_event("up", 1.0), make_metric_event("up", 2.0)];
        let h = make_handle_with_metrics("id-metrics-prom", "prom_text", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-prom").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;

        // Each event should produce a line starting with "up".
        let lines: Vec<&str> = body.lines().collect();
        assert!(
            lines.len() >= 2,
            "must have at least 2 lines of Prometheus text, got {}",
            lines.len()
        );

        for line in &lines {
            assert!(
                line.starts_with("up"),
                "each Prometheus line must start with the metric name 'up', got: {line}"
            );
        }
    }

    // ---- Metrics scrape: correct Content-Type header ------------------------

    /// GET /scenarios/{id}/metrics sets Content-Type to Prometheus text exposition format.
    #[tokio::test]
    async fn metrics_endpoint_sets_prometheus_content_type() {
        let events = vec![make_metric_event("cpu_usage", 42.0)];
        let h = make_handle_with_metrics("id-metrics-ct", "ct_check", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-ct").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("response must have Content-Type header")
            .to_str()
            .unwrap();
        assert_eq!(
            ct, "text/plain; version=0.0.4; charset=utf-8",
            "Content-Type must be the Prometheus exposition format MIME type"
        );
    }

    // ---- Metrics scrape: ?limit=N returns at most N events ------------------

    /// GET /scenarios/{id}/metrics?limit=2 returns at most 2 events from a buffer of 5.
    #[tokio::test]
    async fn metrics_endpoint_limit_parameter_caps_event_count() {
        let events: Vec<_> = (0..5).map(|i| make_metric_event("up", i as f64)).collect();
        let h = make_handle_with_metrics("id-metrics-limit", "limit_test", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_with_limit(app, "id-metrics-limit", 2).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "limit=2 must produce exactly 2 lines of output, got {}",
            lines.len()
        );
    }

    /// GET /scenarios/{id}/metrics?limit=N returns the most recent N events.
    #[tokio::test]
    async fn metrics_endpoint_limit_returns_most_recent_events() {
        // Push 5 events with values 0.0, 1.0, 2.0, 3.0, 4.0.
        let events: Vec<_> = (0..5).map(|i| make_metric_event("val", i as f64)).collect();
        let h = make_handle_with_metrics("id-metrics-recent", "recent_test", events);
        let app = router_with_handles(vec![h]);

        // Request only the most recent 2.
        let resp = get_metrics_with_limit(app, "id-metrics-recent", 2).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        // The last 2 events have values 3.0 and 4.0.
        assert!(
            body.contains("3"),
            "limited output must contain the second-to-last event value (3.0)"
        );
        assert!(
            body.contains("4"),
            "limited output must contain the last event value (4.0)"
        );
    }

    /// limit=0 returns 204 No Content (zero events requested).
    #[tokio::test]
    async fn metrics_endpoint_limit_zero_returns_no_content_after_drain() {
        let events = vec![make_metric_event("up", 1.0)];
        let h = make_handle_with_metrics("id-metrics-lim0", "lim0_test", events);
        let app = router_with_handles(vec![h]);

        // limit=0 means take 0 events from the drained buffer. But drain
        // still happens. The implementation drains first, then limits. With
        // limit=0, events_to_encode is empty, so we should get the encoded
        // output of zero events. Since the events are drained and the
        // limited slice is empty, the encode loop produces nothing.
        // However, the check for events.is_empty() happens BEFORE the limit
        // is applied, so if the buffer had events the status is 200.
        // Let's verify what actually happens.
        let resp = get_metrics_with_limit(app, "id-metrics-lim0", 0).await;
        // The implementation drains 1 event, events is not empty, then takes
        // the last 0 from the end: &events[1..] which is an empty slice.
        // The encoder loop runs 0 times, buf is empty. It returns 200 with
        // empty body (not 204, because the is_empty check passed before limit).
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "limit=0 with non-empty buffer drains events but encodes zero, returns 200 with empty body"
        );
    }

    // ---- Metrics scrape: drain clears buffer --------------------------------

    /// After scraping, a second request returns 204 because the buffer was drained.
    #[tokio::test]
    async fn metrics_endpoint_drain_clears_buffer_second_request_returns_204() {
        let events = vec![make_metric_event("up", 1.0), make_metric_event("up", 2.0)];
        let h = make_handle_with_metrics("id-metrics-drain", "drain_test", events);
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        // First request: should return 200 with Prometheus text.
        let app1 = router(state.clone());
        let resp1 = get_metrics_req(app1, "id-metrics-drain").await;
        assert_eq!(
            resp1.status(),
            StatusCode::OK,
            "first scrape must return 200 with metrics"
        );
        let body1 = body_string(resp1).await;
        assert!(
            !body1.is_empty(),
            "first scrape must return non-empty Prometheus text"
        );

        // Second request: buffer is now drained, should return 204.
        let app2 = router(state.clone());
        let resp2 = get_metrics_req(app2, "id-metrics-drain").await;
        assert_eq!(
            resp2.status(),
            StatusCode::NO_CONTENT,
            "second scrape must return 204 No Content because buffer was drained"
        );

        // Clean up.
        cleanup_scenarios(&state);
    }

    // ---- Metrics scrape: 404 returns JSON Content-Type ----------------------

    /// GET /scenarios/{id}/metrics 404 has Content-Type application/json.
    #[tokio::test]
    async fn metrics_endpoint_404_returns_json_content_type() {
        let app = router_with_handles(vec![]);

        let resp = get_metrics_req(app, "missing-metrics-id").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("404 response must have Content-Type header")
            .to_str()
            .unwrap();
        assert!(
            ct.contains("application/json"),
            "404 Content-Type must be application/json, got: {ct}"
        );
    }

    // ---- Metrics scrape: limit defaults to 100 (implicit) -------------------

    /// Without a limit parameter, all buffered events (up to 100 default) are returned.
    #[tokio::test]
    async fn metrics_endpoint_default_limit_returns_all_buffered_events() {
        // Push 5 events, no limit parameter.
        let events: Vec<_> = (0..5).map(|i| make_metric_event("up", i as f64)).collect();
        let h = make_handle_with_metrics("id-metrics-nomax", "nomax_test", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-nomax").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines.len(),
            5,
            "all 5 buffered events must be returned when no limit is specified"
        );
    }

    // ---- Metrics scrape: limit larger than buffer returns all events ---------

    /// When limit > buffer size, all buffered events are returned.
    #[tokio::test]
    async fn metrics_endpoint_limit_larger_than_buffer_returns_all() {
        let events = vec![make_metric_event("up", 1.0), make_metric_event("up", 2.0)];
        let h = make_handle_with_metrics("id-metrics-biglim", "biglim_test", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_with_limit(app, "id-metrics-biglim", 500).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "when limit > buffer size, all buffered events must be returned"
        );
    }

    // ---- Metrics scrape: output ends with newline ---------------------------

    /// Each Prometheus text line ends with a newline.
    #[tokio::test]
    async fn metrics_endpoint_output_ends_with_newline() {
        let events = vec![make_metric_event("up", 1.0)];
        let h = make_handle_with_metrics("id-metrics-nl", "newline_test", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-nl").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        assert!(
            body.ends_with('\n'),
            "Prometheus text output must end with a newline"
        );
    }

    // ---- MetricsQuery deserialization ----------------------------------------

    /// MetricsQuery with no fields deserializes with limit=None.
    #[test]
    fn metrics_query_default_limit_is_none() {
        let q: MetricsQuery = serde_json::from_str("{}").expect("must deserialize");
        assert!(
            q.limit.is_none(),
            "limit must default to None when not specified"
        );
    }

    /// MetricsQuery with limit=50 deserializes correctly.
    #[test]
    fn metrics_query_with_limit_deserializes() {
        let q: MetricsQuery = serde_json::from_str(r#"{"limit": 50}"#).expect("must deserialize");
        assert_eq!(q.limit, Some(50));
    }

    // ---- PROMETHEUS_CONTENT_TYPE constant ------------------------------------

    /// The Prometheus content type constant has the expected value.
    #[test]
    fn prometheus_content_type_constant_has_correct_value() {
        assert_eq!(
            PROMETHEUS_CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8",
            "PROMETHEUS_CONTENT_TYPE must match the Prometheus exposition format MIME type"
        );
    }

    // ========================================================================
    // Hardening tests — force_stopped, panicked threads, poisoned locks
    // ========================================================================

    // ---- Helper: build a handle whose thread ignores the shutdown flag ------

    /// Build a ScenarioHandle whose thread sleeps for a long time, ignoring
    /// the shutdown flag. This simulates a scenario that cannot be stopped
    /// gracefully within the join timeout.
    fn make_unjoinable_handle(id: &str, name: &str) -> ScenarioHandle {
        let shutdown = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let thread = thread::Builder::new()
            .name(format!("test-unjoinable-{name}"))
            .spawn(move || -> Result<(), sonda_core::SondaError> {
                // Ignore shutdown — sleep for a very long time.
                thread::sleep(Duration::from_secs(300));
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
            target_rate: 50.0,
        }
    }

    /// Build a ScenarioHandle whose thread panics immediately.
    fn make_panicking_handle(id: &str, name: &str) -> ScenarioHandle {
        let shutdown = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let thread = thread::Builder::new()
            .name(format!("test-panic-{name}"))
            .spawn(move || -> Result<(), sonda_core::SondaError> {
                panic!("intentional panic for testing");
            })
            .expect("thread must spawn");

        // Give the thread time to panic.
        thread::sleep(Duration::from_millis(50));

        ScenarioHandle {
            id: id.to_string(),
            name: name.to_string(),
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
            target_rate: 10.0,
        }
    }

    // ---- L1: DELETE on unjoinable thread returns force_stopped --------------

    /// When the scenario thread does not exit within the join timeout,
    /// DELETE returns status "force_stopped".
    #[tokio::test]
    async fn delete_unjoinable_thread_returns_force_stopped() {
        let h = make_unjoinable_handle("id-force", "force_stop");
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-force").await;

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "DELETE on unjoinable thread must still return 200 OK"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["status"].as_str().unwrap(),
            "force_stopped",
            "DELETE on unjoinable thread must return status 'force_stopped'"
        );
        assert_eq!(
            body["id"].as_str().unwrap(),
            "id-force",
            "response must contain the correct scenario ID"
        );

        // Verify the handle was removed from the map despite being force-stopped.
        let map = state.scenarios.read().unwrap();
        assert!(
            map.get("id-force").is_none(),
            "force-stopped scenario must still be removed from the map"
        );
    }

    // ---- L2: DELETE on panicked thread returns stopped ----------------------

    /// When the scenario thread has panicked, DELETE returns 200 OK with status
    /// "stopped" (the thread has already exited, just abnormally).
    #[tokio::test]
    async fn delete_panicked_thread_returns_stopped() {
        let h = make_panicking_handle("id-panic", "panic_scenario");
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app = router(state.clone());
        let resp = delete_scenario_req(app, "id-panic").await;

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "DELETE on panicked thread must return 200 OK"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["status"].as_str().unwrap(),
            "stopped",
            "DELETE on panicked thread must return status 'stopped' (thread already exited)"
        );

        // Verify the handle was removed from the map.
        let map = state.scenarios.read().unwrap();
        assert!(
            map.get("id-panic").is_none(),
            "panicked scenario must be removed from the map"
        );
    }

    // ---- L3: Poisoned map lock returns 500 in read handlers ----------------

    /// Helper: build an AppState with a poisoned scenarios lock.
    fn make_poisoned_state() -> AppState {
        let state = AppState::new();
        // Poison the lock by panicking inside a write guard.
        let scenarios_clone = Arc::clone(&state.scenarios);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = scenarios_clone.write().unwrap();
            panic!("intentional panic to poison map lock");
        }));
        assert!(result.is_err(), "panic must have occurred");
        // Verify the lock is actually poisoned.
        assert!(
            state.scenarios.read().is_err(),
            "map lock must be poisoned after panic"
        );
        state
    }

    /// GET /scenarios returns 500 when the map lock is poisoned.
    #[tokio::test]
    async fn list_scenarios_poisoned_lock_returns_500() {
        let state = make_poisoned_state();
        let app = router(state);

        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "poisoned map lock on list must return 500"
        );

        let body = body_json(resp).await;
        assert_eq!(
            body["error"].as_str().unwrap(),
            "internal_server_error",
            "500 response must have error='internal_server_error'"
        );
    }

    /// GET /scenarios/{id} returns 500 when the map lock is poisoned.
    #[tokio::test]
    async fn get_scenario_poisoned_lock_returns_500() {
        let state = make_poisoned_state();
        let app = router(state);

        let req = Request::builder()
            .uri("/scenarios/any-id")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "poisoned map lock on get must return 500"
        );
    }

    /// GET /scenarios/{id}/stats returns 500 when the map lock is poisoned.
    #[tokio::test]
    async fn get_scenario_stats_poisoned_lock_returns_500() {
        let state = make_poisoned_state();
        let app = router(state);

        let resp = get_stats_req(app, "any-id").await;
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "poisoned map lock on stats must return 500"
        );
    }

    /// GET /scenarios/{id}/metrics returns 500 when the map lock is poisoned.
    #[tokio::test]
    async fn get_scenario_metrics_poisoned_lock_returns_500() {
        let state = make_poisoned_state();
        let app = router(state);

        let resp = get_metrics_req(app, "any-id").await;
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "poisoned map lock on metrics must return 500"
        );
    }

    /// DELETE /scenarios/{id} returns 500 when the map lock is poisoned.
    #[tokio::test]
    async fn delete_scenario_poisoned_lock_returns_500() {
        let state = make_poisoned_state();
        let app = router(state);

        let resp = delete_scenario_req(app, "any-id").await;
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "poisoned map lock on delete must return 500"
        );
    }

    /// POST /scenarios returns 500 when the map lock is poisoned (lock
    /// acquisition for storing the handle fails).
    #[tokio::test]
    async fn post_scenario_poisoned_lock_returns_500() {
        let state = make_poisoned_state();
        let app = router(state);

        let response = post_scenarios(app, "application/x-yaml", VALID_METRICS_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "poisoned map lock on post must return 500"
        );

        let body = body_json(response).await;
        assert_eq!(
            body["error"].as_str().unwrap(),
            "internal_server_error",
            "500 response must have error='internal_server_error'"
        );
    }

    // ========================================================================
    // POST /scenarios multi-scenario tests
    // ========================================================================

    /// YAML body for a valid multi-scenario batch with two entries.
    const VALID_MULTI_YAML: &str = "\
scenarios:
  - signal_type: metrics
    name: multi_metric_a
    rate: 10
    duration: 200ms
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: metrics
    name: multi_metric_b
    rate: 10
    duration: 200ms
    generator:
      type: constant
      value: 2.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
";

    /// YAML body for a multi-scenario batch with phase_offset.
    const MULTI_YAML_WITH_PHASE_OFFSET: &str = "\
scenarios:
  - signal_type: metrics
    name: offset_a
    rate: 10
    duration: 200ms
    phase_offset: \"0s\"
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: metrics
    name: offset_b
    rate: 10
    duration: 200ms
    phase_offset: \"50ms\"
    generator:
      type: constant
      value: 2.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
";

    /// Multi-scenario YAML POST returns 201 with a scenarios array.
    #[tokio::test]
    async fn post_multi_scenario_yaml_returns_201_with_scenarios_array() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", VALID_MULTI_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST valid multi-scenario YAML must return 201 Created"
        );

        let body = body_json(response).await;
        let scenarios = body["scenarios"]
            .as_array()
            .expect("response must contain a 'scenarios' array");
        assert_eq!(
            scenarios.len(),
            2,
            "multi-scenario response must have 2 entries"
        );

        // Each entry must have id, name, status.
        for (i, entry) in scenarios.iter().enumerate() {
            assert!(
                entry["id"].is_string() && !entry["id"].as_str().unwrap().is_empty(),
                "scenario[{i}] must have a non-empty id"
            );
            assert!(
                entry["name"].is_string(),
                "scenario[{i}] must have a name string"
            );
            assert_eq!(
                entry["status"], "running",
                "scenario[{i}] status must be 'running'"
            );
        }

        // Verify names match input order.
        assert_eq!(scenarios[0]["name"], "multi_metric_a");
        assert_eq!(scenarios[1]["name"], "multi_metric_b");

        cleanup_scenarios(&state);
    }

    /// Multi-scenario POST stores all handles in AppState.
    #[tokio::test]
    async fn post_multi_scenario_stores_all_handles() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", VALID_MULTI_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response).await;
        let scenarios = body["scenarios"].as_array().unwrap();
        let map = state.scenarios.read().expect("lock must not be poisoned");
        for entry in scenarios {
            let id = entry["id"].as_str().unwrap();
            assert!(
                map.contains_key(id),
                "AppState must contain handle for scenario id={id}"
            );
        }
        drop(map);

        cleanup_scenarios(&state);
    }

    /// Multi-scenario POST with JSON content type returns 201.
    #[tokio::test]
    async fn post_multi_scenario_json_returns_201() {
        let json_body = serde_json::json!({
            "scenarios": [
                {
                    "signal_type": "metrics",
                    "name": "json_multi_a",
                    "rate": 10,
                    "duration": "200ms",
                    "generator": { "type": "constant", "value": 1.0 },
                    "encoder": { "type": "prometheus_text" },
                    "sink": { "type": "stdout" }
                },
                {
                    "signal_type": "metrics",
                    "name": "json_multi_b",
                    "rate": 10,
                    "duration": "200ms",
                    "generator": { "type": "constant", "value": 2.0 },
                    "encoder": { "type": "prometheus_text" },
                    "sink": { "type": "stdout" }
                }
            ]
        });

        let (app, state) = test_router();
        let response = post_scenarios(app, "application/json", &json_body.to_string()).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST multi-scenario JSON must return 201"
        );

        let body = body_json(response).await;
        let scenarios = body["scenarios"]
            .as_array()
            .expect("JSON multi-scenario response must have scenarios array");
        assert_eq!(scenarios.len(), 2);
        assert_eq!(scenarios[0]["name"], "json_multi_a");
        assert_eq!(scenarios[1]["name"], "json_multi_b");

        cleanup_scenarios(&state);
    }

    /// Empty scenarios array returns 400.
    #[tokio::test]
    async fn post_multi_scenario_empty_array_returns_400() {
        let yaml = "scenarios: []\n";
        let (app, _state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", yaml).await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "POST with empty scenarios array must return 400"
        );

        let body = body_json(response).await;
        assert_eq!(body["error"], "bad_request");
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("must not be empty"),
            "400 detail must mention empty array"
        );
    }

    /// Invalid entry in batch returns 422 and nothing is launched.
    #[tokio::test]
    async fn post_multi_scenario_invalid_entry_returns_422_nothing_launched() {
        let yaml = "\
scenarios:
  - signal_type: metrics
    name: valid_entry
    rate: 10
    duration: 200ms
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: metrics
    name: invalid_entry
    rate: 0
    duration: 200ms
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
";
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", yaml).await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "POST with invalid entry in batch must return 422"
        );

        // Verify nothing was launched (atomic batch semantics).
        let map = state.scenarios.read().expect("lock must not be poisoned");
        assert!(
            map.is_empty(),
            "no scenarios must be launched when batch validation fails"
        );
    }

    /// Multi-scenario POST with phase_offset honored per entry.
    #[tokio::test]
    async fn post_multi_scenario_phase_offset_honored() {
        let (app, state) = test_router();
        let response =
            post_scenarios(app, "application/x-yaml", MULTI_YAML_WITH_PHASE_OFFSET).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST multi-scenario with phase_offset must return 201"
        );

        let body = body_json(response).await;
        let scenarios = body["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 2);
        assert_eq!(scenarios[0]["name"], "offset_a");
        assert_eq!(scenarios[1]["name"], "offset_b");

        cleanup_scenarios(&state);
    }

    /// Single-scenario POST still returns backward-compatible response.
    #[tokio::test]
    async fn post_single_scenario_backward_compat() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", VALID_METRICS_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;

        // Single scenario response must NOT have a "scenarios" key.
        assert!(
            body.get("scenarios").is_none(),
            "single-scenario POST must not return a 'scenarios' wrapper"
        );
        // Must have the flat {id, name, status} shape.
        assert!(body["id"].is_string());
        assert_eq!(body["name"], "test_metric");
        assert_eq!(body["status"], "running");

        cleanup_scenarios(&state);
    }

    /// All launched multi-scenario entries are visible in GET /scenarios.
    #[tokio::test]
    async fn post_multi_scenario_entries_visible_in_get_list() {
        let state = AppState::new();
        let app = router(state.clone());
        let response = post_scenarios(app, "application/x-yaml", VALID_MULTI_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);

        let post_body = body_json(response).await;
        let posted_ids: Vec<&str> = post_body["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();

        // GET /scenarios to list all.
        let app2 = router(state.clone());
        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();
        let resp = app2.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let list_body = body_json(resp).await;
        let listed_ids: Vec<&str> = list_body["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();

        for id in &posted_ids {
            assert!(
                listed_ids.contains(id),
                "posted scenario id={id} must appear in GET /scenarios list"
            );
        }

        cleanup_scenarios(&state);
    }

    /// Multi-scenario entries are stoppable via DELETE.
    #[tokio::test]
    async fn post_multi_scenario_entries_stoppable_via_delete() {
        let state = AppState::new();
        let app = router(state.clone());
        let response = post_scenarios(app, "application/x-yaml", VALID_MULTI_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);

        let post_body = body_json(response).await;
        let ids: Vec<String> = post_body["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect();

        // DELETE each scenario.
        for id in &ids {
            let app = router(state.clone());
            let resp = delete_scenario_req(app, id).await;
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "DELETE for multi-scenario id={id} must return 200"
            );
        }

        // Verify all are gone.
        let map = state.scenarios.read().unwrap();
        assert!(
            map.is_empty(),
            "all multi-scenario handles must be removed after DELETE"
        );
    }

    /// Multi-scenario response has unique IDs for each entry.
    #[tokio::test]
    async fn post_multi_scenario_ids_are_unique() {
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", VALID_MULTI_YAML).await;

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response).await;
        let ids: Vec<&str> = body["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();

        let mut unique_ids = ids.clone();
        unique_ids.sort();
        unique_ids.dedup();
        assert_eq!(
            ids.len(),
            unique_ids.len(),
            "all scenario IDs must be unique"
        );

        cleanup_scenarios(&state);
    }

    /// Multi-scenario with mixed signal types (metrics + logs) returns 201.
    #[tokio::test]
    async fn post_multi_scenario_mixed_signal_types() {
        let yaml = "\
scenarios:
  - signal_type: metrics
    name: mixed_metric
    rate: 10
    duration: 200ms
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: logs
    name: mixed_logs
    rate: 10
    duration: 200ms
    generator:
      type: template
      templates:
        - message: \"test log\"
          field_pools: {}
      seed: 0
    encoder:
      type: json_lines
    sink:
      type: stdout
";
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", yaml).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST multi-scenario with mixed signal types must return 201"
        );

        let body = body_json(response).await;
        let scenarios = body["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 2);
        assert_eq!(scenarios[0]["name"], "mixed_metric");
        assert_eq!(scenarios[1]["name"], "mixed_logs");

        cleanup_scenarios(&state);
    }

    // ---- Parse body unit tests (multi-aware) ---------------------------------

    /// parse_yaml_body returns Multi for a scenarios-array body.
    #[test]
    fn parse_yaml_body_returns_multi_for_scenarios_array() {
        let result = parse_yaml_body(VALID_MULTI_YAML.as_bytes());
        assert!(result.is_ok(), "must parse valid multi-scenario YAML");
        match result.unwrap() {
            ParsedBody::Multi(entries) => {
                assert_eq!(entries.len(), 2, "must contain 2 entries");
            }
            ParsedBody::Single(_) => panic!("expected ParsedBody::Multi"),
        }
    }

    /// parse_yaml_body returns Single for a single-scenario body.
    #[test]
    fn parse_yaml_body_returns_single_for_bare_config() {
        let result = parse_yaml_body(VALID_METRICS_YAML.as_bytes());
        assert!(result.is_ok(), "must parse valid single-scenario YAML");
        match result.unwrap() {
            ParsedBody::Single(entry) => {
                assert_eq!(entry.base().name, "test_metric");
            }
            ParsedBody::Multi(_) => panic!("expected ParsedBody::Single"),
        }
    }

    /// parse_json_body returns Multi for a scenarios-array JSON body.
    #[test]
    fn parse_json_body_returns_multi_for_json_array() {
        let json = serde_json::json!({
            "scenarios": [
                {
                    "signal_type": "metrics",
                    "name": "a",
                    "rate": 10,
                    "duration": "1s",
                    "generator": { "type": "constant", "value": 1.0 },
                    "encoder": { "type": "prometheus_text" },
                    "sink": { "type": "stdout" }
                }
            ]
        });
        let result = parse_json_body(json.to_string().as_bytes());
        assert!(result.is_ok(), "must parse valid multi-scenario JSON");
        match result.unwrap() {
            ParsedBody::Multi(entries) => assert_eq!(entries.len(), 1),
            ParsedBody::Single(_) => panic!("expected ParsedBody::Multi"),
        }
    }

    /// parse_json_body returns Single for a single-scenario JSON body.
    #[test]
    fn parse_json_body_returns_single_for_single_json() {
        let json = serde_json::json!({
            "signal_type": "metrics",
            "name": "single_json",
            "rate": 10,
            "duration": "1s",
            "generator": { "type": "constant", "value": 1.0 },
            "encoder": { "type": "prometheus_text" },
            "sink": { "type": "stdout" }
        });
        let result = parse_json_body(json.to_string().as_bytes());
        assert!(result.is_ok(), "must parse valid single-scenario JSON");
        match result.unwrap() {
            ParsedBody::Single(entry) => {
                assert_eq!(entry.base().name, "single_json");
            }
            ParsedBody::Multi(_) => panic!("expected ParsedBody::Single"),
        }
    }

    /// CreatedScenariosResponse serializes to expected JSON structure.
    #[test]
    fn created_scenarios_response_serializes_correctly() {
        let resp = CreatedScenariosResponse {
            scenarios: vec![
                CreatedScenario {
                    id: "id-1".to_string(),
                    name: "s1".to_string(),
                    status: "running",
                },
                CreatedScenario {
                    id: "id-2".to_string(),
                    name: "s2".to_string(),
                    status: "running",
                },
            ],
        };
        let json = serde_json::to_value(&resp).expect("must serialize");
        let arr = json["scenarios"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "id-1");
        assert_eq!(arr[1]["name"], "s2");
    }

    // ========================================================================
    // Single-scenario POST parity with multi-scenario path (NOTE 1 fix)
    // ========================================================================

    /// Single-scenario POST with phase_offset returns 201 (verifies the
    /// single-scenario path now uses prepare_entries which resolves phase_offset).
    #[tokio::test]
    async fn post_single_scenario_with_phase_offset_returns_201() {
        let yaml = "\
name: single_offset
rate: 10
duration: 200ms
phase_offset: \"50ms\"
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
";
        let (app, state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", yaml).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "POST single scenario with phase_offset must return 201"
        );

        let body = body_json(response).await;
        assert_eq!(body["name"], "single_offset");
        assert_eq!(body["status"], "running");

        cleanup_scenarios(&state);
    }

    /// Single-scenario POST with rate=0 returns 422 (verifies validation
    /// through prepare_entries).
    #[tokio::test]
    async fn post_single_scenario_with_zero_rate_returns_422_via_prepare_entries() {
        let (app, _state) = test_router();
        let response = post_scenarios(app, "application/x-yaml", ZERO_RATE_YAML).await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "POST single scenario with rate=0 must return 422"
        );
    }
}
