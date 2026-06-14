//! Scenario management endpoints.
//!
//! Implements:
//! - `POST /scenarios` — start one or more scenarios from a v2 YAML or JSON
//!   body. Every body is compiled via [`sonda_core::compile_scenario_file_compiled`]
//!   and launched through the gated multi-runner so `while:` clauses
//!   reach the runtime. v1 YAML shapes are rejected with a migration hint.
//!   A single launched handle returns the flat `{id, name, state}` shape;
//!   two or more handles return the `{scenarios: [...]}` wrapper. Launches
//!   are atomic: all entries validate before any threads spawn.
//! - `GET /scenarios` — list all scenarios with summary information.
//! - `GET /scenarios/{id}` — inspect a single scenario with full detail and stats.
//! - `GET /scenarios/{id}/stats` — return detailed live stats for a scenario.
//! - `GET /scenarios/{id}/metrics` — return one Prometheus sample per series, no timestamps.
//! - `DELETE /scenarios/{id}` — stop a running scenario and return final stats.
//!
//! All lifecycle logic is delegated to sonda-core. This module is pure HTTP
//! plumbing: deserialize → compile → launch → store → respond.

use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::json;
use sonda_core::compiler::parse::detect_version;
use sonda_core::encoder::prometheus::PrometheusText;
use sonda_core::encoder::Encoder;
use sonda_core::{GateBusResolver, ScenarioState, ScenarioStats};
use tracing::{info, warn};
use uuid::Uuid;

use sonda_core::compile_scenario_file_compiled;
use sonda_core::compiler::compile_after::CompiledFile;
use sonda_core::config::ScenarioEntry;
use sonda_core::schedule::launch::prepare_entries;
use sonda_core::schedule::multi_runner::launch_multi_compiled;

use crate::routes::sink_warnings::{
    log_warnings, loki_cardinality_warnings, sink_loopback_warnings,
};
use crate::state::AppState;

// ---- Response types ---------------------------------------------------------

/// Response body for a successfully created scenario.
#[derive(Debug, Serialize)]
pub struct CreatedScenario {
    pub id: String,
    pub name: String,
    /// Live state at POST-response time. One of `"pending"`, `"running"`,
    /// `"paused"`, `"finished"`. Snapshot only — clients should poll
    /// `/scenarios/{id}` for live state thereafter.
    pub state: String,
    /// Non-fatal warnings raised while validating the posted body.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// Response body for a successfully created multi-scenario batch.
#[derive(Debug, Serialize)]
pub struct CreatedScenariosResponse {
    pub scenarios: Vec<CreatedScenario>,
    /// Non-fatal warnings raised while validating the posted body.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// Summary of a single scenario in the list response.
#[derive(Debug, Serialize)]
pub struct ScenarioSummary {
    pub id: String,
    pub name: String,
    /// Current state: one of `"pending"`, `"running"`, `"paused"`, `"held"`, `"unresolved"`, `"finished"`.
    pub state: String,
    pub elapsed_secs: f64,
    /// Whether the scenario has sink failures and no recent successful delivery.
    pub degraded: bool,
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
    pub id: String,
    pub name: String,
    /// Current state: one of `"pending"`, `"running"`, `"paused"`, `"held"`, `"unresolved"`, `"finished"`.
    pub state: String,
    pub elapsed_secs: f64,
    /// Whether the scenario has sink failures and no recent successful delivery.
    pub degraded: bool,
    pub stats: StatsResponse,
    /// Cross-POST `while:` reference snapshot when state is `unresolved`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_ref: Option<sonda_core::PendingRef>,
}

/// Response body for a successfully deleted (stopped) scenario.
#[derive(Debug, Serialize)]
pub struct DeletedScenario {
    pub id: String,
    /// Join outcome — `"stopped"` when the runner thread exited, or
    /// `"force_stopped"` when the join timed out. Distinct from the
    /// lifecycle state surfaced on `/scenarios/{id}`.
    pub status: String,
    pub total_events: u64,
}

/// One entry in the `conflicting_scenarios` array of a 409 response body.
#[derive(Debug, Serialize)]
pub struct ConflictingScenario {
    pub id: String,
    pub name: String,
    /// One of `"pending"`, `"running"`, `"paused"`, `"held"`, `"unresolved"`. Never `"finished"`.
    pub state: String,
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
    /// Sink failures observed since the most recent successful write.
    pub consecutive_failures: u64,
    /// Lifetime sink-failure count.
    pub total_sink_failures: u64,
    /// Most recent sink error message, if any.
    pub last_sink_error: Option<String>,
    /// Wall-clock Unix-nanoseconds timestamp of the last successful write.
    pub last_successful_write_at: Option<u64>,
    /// Whether the scenario has sink failures and no recent successful delivery.
    pub degraded: bool,
}

impl From<ScenarioStats> for StatsResponse {
    fn from(s: ScenarioStats) -> Self {
        Self {
            total_events: s.total_events,
            current_rate: s.current_rate,
            bytes_emitted: s.bytes_emitted,
            errors: s.errors,
            consecutive_failures: s.consecutive_failures,
            total_sink_failures: s.total_sink_failures,
            last_sink_error: s.last_sink_error,
            last_successful_write_at: s.last_successful_write_at,
            degraded: false,
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
    /// Current state: `"pending"`, `"running"`, `"paused"`, `"held"`, `"unresolved"`, or `"finished"`.
    pub state: String,
    /// Whether the scenario is currently in a gap window (no events emitted).
    pub in_gap: bool,
    /// Whether the scenario is currently in a burst window (elevated rate).
    pub in_burst: bool,
    /// Sink failures observed since the most recent successful write.
    pub consecutive_failures: u64,
    /// Lifetime sink-failure count.
    pub total_sink_failures: u64,
    /// Most recent sink error message, if any.
    pub last_sink_error: Option<String>,
    /// Wall-clock Unix-nanoseconds timestamp of the last successful write.
    pub last_successful_write_at: Option<u64>,
    /// Whether the scenario has sink failures and no recent successful delivery.
    pub degraded: bool,
    /// Seconds elapsed since the most recent state transition.
    pub current_state_secs: f64,
    /// Lifetime count of resolver subscription attempts for this scenario.
    pub cumulative_resolution_attempts: u64,
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

const CONFLICT_HINT: &str =
    "DELETE the conflicting scenarios before posting a new cascade with the same scenario_name";

/// Build a 409 Conflict response listing the active scenarios that share
/// the posted body's `scenario_name`.
fn conflict(message: String, scenarios: Vec<ConflictingScenario>) -> Response {
    let body = json!({
        "error": message,
        "conflicting_scenarios": scenarios,
        "hint": CONFLICT_HINT,
    });
    (StatusCode::CONFLICT, Json(body)).into_response()
}

// ---- Helpers ----------------------------------------------------------------

/// Map [`ScenarioStats::state`] to its lowercase wire string.
fn state_string(stats: &ScenarioStats) -> &'static str {
    match stats.state {
        ScenarioState::Pending => "pending",
        ScenarioState::Running => "running",
        ScenarioState::Paused => "paused",
        ScenarioState::Held => "held",
        ScenarioState::Unresolved => "unresolved",
        ScenarioState::Finished => "finished",
        _ => "unknown",
    }
}

/// Scan the active scenario map for entries with matching `scenario_name`
/// in `pending` / `running` / `paused` state. Finished handles are stale
/// and skipped. Future `ScenarioState` variants (`#[non_exhaustive]`) must
/// opt in to blocking explicitly — a stalled or errored handle that the
/// operator can't DELETE shouldn't lock its name forever. `Err(())`
/// indicates a poisoned lock.
fn collect_active_conflicts(state: &AppState, name: &str) -> Result<Vec<ConflictingScenario>, ()> {
    let scenarios = state.scenarios.read().map_err(|_| ())?;
    let mut conflicts = Vec::new();
    for (id, handle) in scenarios.iter() {
        if handle.scenario_name.as_deref() != Some(name) {
            continue;
        }
        let snap = handle.stats_snapshot();
        let blocks = match snap.state {
            ScenarioState::Pending
            | ScenarioState::Running
            | ScenarioState::Paused
            | ScenarioState::Held
            | ScenarioState::Unresolved => true,
            ScenarioState::Finished => false,
            _ => false,
        };
        if blocks {
            conflicts.push(ConflictingScenario {
                id: id.clone(),
                name: handle.name.clone(),
                state: state_string(&snap).to_string(),
            });
        }
    }
    Ok(conflicts)
}

// ---- Body parsing -----------------------------------------------------------

/// Migration hint appended to every v1-rejection error message so operators
/// can locate the v2 scenario guide without searching docs.
const V1_REJECTION_HINT: &str =
    "Sonda only accepts v2 scenario bodies (`version: 2` at the top level). \
     Migrate this body to v2 — see docs/configuration/v2-scenarios.md for the \
     migration guide.";

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
#[derive(Debug)]
enum ParsedBody {
    /// A compiled v2 scenario file ready for the gated multi-runner.
    ///
    /// Boxed to avoid a large size difference between variants
    /// (clippy `large_enum_variant`).
    Compiled(Box<CompiledFile>),
}

/// Categorized failure from [`parse_body`].
///
/// `Syntactic` covers genuine YAML/JSON syntax errors and content-type
/// mismatches — surfaced as 400 Bad Request. `Semantic` covers schema
/// validation failures on otherwise well-formed input (e.g. unsupported
/// `while.op`, NaN values, conflicting fields) — surfaced as 422
/// Unprocessable Entity.
#[derive(Debug)]
enum ParseFailure {
    Syntactic(String),
    Semantic(String),
}

impl ParseFailure {
    fn message(&self) -> &str {
        match self {
            ParseFailure::Syntactic(m) | ParseFailure::Semantic(m) => m,
        }
    }
}

fn format_error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut cause: Option<&(dyn std::error::Error + 'static)> = err.source();
    while let Some(c) = cause {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cause = c.source();
    }
    out
}

fn parse_body(
    body: &[u8],
    headers: &HeaderMap,
    catalog_dir: Option<&FsPath>,
) -> Result<ParsedBody, ParseFailure> {
    let text = yaml_body_text(body, headers).map_err(ParseFailure::Syntactic)?;

    let version = detect_version(&text);
    if version != Some(2) {
        return Err(ParseFailure::Syntactic(format!(
            "body is not a v2 scenario. {V1_REJECTION_HINT}"
        )));
    }

    let resolver = sonda_core::catalog::CatalogPackResolver::new(catalog_dir);
    let compiled = compile_scenario_file_compiled(&text, &resolver).map_err(|e| {
        let detail = format!(
            "v2 scenario body failed to compile: {}",
            format_error_chain(&e)
        );
        if is_semantic_schema_error(&e, &text) {
            ParseFailure::Semantic(detail)
        } else {
            ParseFailure::Syntactic(detail)
        }
    })?;

    if compiled.entries.is_empty() {
        return Err(ParseFailure::Syntactic(
            "v2 scenario body produced zero entries".to_string(),
        ));
    }

    Ok(ParsedBody::Compiled(Box::new(compiled)))
}

/// Detect schema-level deserialize failures on otherwise well-formed YAML.
///
/// Returns true only when the raw text parses as a generic YAML `Value` but
/// the AST deserialize step rejects it (e.g. unsupported `while.op`,
/// `deny_unknown_fields` violations). All other compile failures — YAML
/// syntax errors, normalize/expand/compile_after/prepare errors — map to
/// `400 Bad Request` to preserve existing behavior.
fn is_semantic_schema_error(err: &sonda_core::CompileError, text: &str) -> bool {
    use sonda_core::compiler::normalize::NormalizeError;
    use sonda_core::compiler::parse::ParseError;
    use sonda_core::CompileError;

    match err {
        CompileError::Parse(ParseError::Yaml(_)) => {
            serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text).is_ok()
        }
        CompileError::Normalize(
            NormalizeError::WhileValueIsNan { .. }
            | NormalizeError::CloseSnapToIsNan { .. }
            | NormalizeError::CloseEmitConflict { .. }
            | NormalizeError::WhileWithoutDuration { .. }
            | NormalizeError::DelayWithoutWhile { .. },
        ) => true,
        _ => false,
    }
}

/// Convert the raw request body into YAML text for the v2 compiler.
///
/// YAML bodies are decoded as UTF-8 directly. JSON bodies are reparsed into
/// a `serde_yaml_ng::Value` and re-emitted as YAML so the single downstream
/// compile path can accept either content type.
fn yaml_body_text(body: &[u8], headers: &HeaderMap) -> Result<String, String> {
    if is_yaml_content_type(headers) {
        std::str::from_utf8(body)
            .map(|s| s.to_string())
            .map_err(|e| format!("request body is not valid UTF-8: {e}"))
    } else {
        let value: serde_yaml_ng::Value =
            serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
        serde_yaml_ng::to_string(&value)
            .map_err(|e| format!("failed to transcode JSON body to YAML: {e}"))
    }
}

// ---- Handlers ---------------------------------------------------------------

fn parse_validate_strict(raw_query: Option<&str>) -> bool {
    let Some(raw) = raw_query else {
        return false;
    };
    raw.split('&').any(|pair| pair == "validate=strict")
}

#[derive(Debug, Serialize)]
struct UnresolvedRefEntry {
    scenario_name: String,
    entry_id: String,
    referenced_by: String,
}

fn collect_unresolved_refs(state: &AppState, compiled: &CompiledFile) -> Vec<UnresolvedRefEntry> {
    let mut out = Vec::new();
    let own_name = compiled.scenario_name.as_deref();
    for entry in &compiled.entries {
        let Some(clause) = entry.while_clause.as_ref() else {
            continue;
        };
        let Some(scenario_name) = clause.scenario_name.as_deref() else {
            continue;
        };
        if Some(scenario_name) == own_name {
            continue;
        }
        if state
            .gate_bus_registry
            .lookup(scenario_name, &clause.ref_id)
            .is_none()
        {
            out.push(UnresolvedRefEntry {
                scenario_name: scenario_name.to_string(),
                entry_id: clause.ref_id.clone(),
                referenced_by: entry.id.clone().unwrap_or_default(),
            });
        }
    }
    out
}

/// `POST /scenarios` — start scenarios from a v2 YAML or JSON body.
///
/// The body is compiled via [`compile_scenario_file_compiled`] and launched
/// through the gated multi-runner so `while:` clauses reach the runtime.
/// v1 YAML shapes are rejected with a migration hint.
///
/// **One launched handle**: Returns `201 Created` with the flat
/// `{"id", "name", "state"}` body.
///
/// **Multiple launched handles** (multi-entry body or pack expansion that
/// fanned out): Returns `201 Created` with `{"scenarios": [...]}`. All
/// entries validate atomically before any threads spawn.
///
/// # Error responses
/// - `400 Bad Request` — body cannot be parsed, v1 shape is rejected, or the
///   v2 compiler reports a parse/normalize/expand/compile error.
/// - `422 Unprocessable Entity` — body compiled but failed runtime validation
///   (e.g. `rate: 0`).
/// - `500 Internal Server Error` — scenario thread could not be spawned.
pub async fn post_scenario(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    body: axum::body::Bytes,
) -> Result<Response, Response> {
    let strict = parse_validate_strict(raw_query.as_deref());
    let catalog_dir = state.catalog_dir.as_deref().map(|p| p.as_path());
    let parsed = parse_body(&body, &headers, catalog_dir).map_err(|fail| {
        warn!(error = %fail.message(), "POST /scenarios: invalid request body");
        match fail {
            ParseFailure::Syntactic(m) => bad_request(m),
            ParseFailure::Semantic(m) => unprocessable(m),
        }
    })?;

    let ParsedBody::Compiled(compiled) = parsed;

    if strict {
        let unresolved = collect_unresolved_refs(&state, &compiled);
        if !unresolved.is_empty() {
            warn!(
                count = unresolved.len(),
                "POST /scenarios?validate=strict: rejecting body with unresolved refs"
            );
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "unresolved_refs",
                    "unresolved_refs": unresolved,
                })),
            )
                .into_response());
        }
    }

    if let Some(name) = compiled.scenario_name.as_deref() {
        let mut conflicts = collect_active_conflicts(&state, name).map_err(|()| {
            warn!("POST /scenarios: scenarios lock is poisoned");
            internal_error("internal state lock is poisoned")
        })?;
        if conflicts.is_empty() && state.gate_bus_registry.scenario_name_in_use(name) {
            conflicts.push(ConflictingScenario {
                id: String::new(),
                name: name.to_string(),
                state: "running".to_string(),
            });
        }
        if !conflicts.is_empty() {
            warn!(
                scenario_name = %name,
                count = conflicts.len(),
                "POST /scenarios: rejected duplicate scenario_name"
            );
            return Err(conflict(
                format!("scenario_name '{name}' is already running"),
                conflicts,
            ));
        }
    }

    // Derive ScenarioEntry values for the loopback warning helper, which
    // operates on the runtime input shape. prepare_entries doubles as
    // pre-flight validation — surfacing rate=0, bad phase_offset, etc. as
    // 422 before any thread spawns.
    let prepared_entries = sonda_core::compiler::prepare::prepare(compiled.as_ref().clone())
        .map_err(|e| {
            warn!(error = %e, "POST /scenarios: prepare failed");
            unprocessable(e)
        })?;
    let prepared = prepare_entries(prepared_entries).map_err(|e| {
        warn!(error = %e, "POST /scenarios: validation failed");
        unprocessable(e)
    })?;
    let warning_entries: Vec<ScenarioEntry> = prepared.into_iter().map(|p| p.entry).collect();
    let mut warnings = sink_loopback_warnings(&warning_entries);
    warnings.extend(loki_cardinality_warnings(&warning_entries));
    log_warnings("POST /scenarios", &warnings);
    drop(warning_entries);

    launch_compiled(state, *compiled, warnings).await
}

/// Launch every entry in `compiled` through the gated multi-runner and store
/// the resulting handles in [`AppState`]. Single-vs-multi response shape is
/// decided post-launch from the count of returned handles.
async fn launch_compiled(
    state: AppState,
    compiled: CompiledFile,
    warnings: Vec<String>,
) -> Result<Response, Response> {
    let resolver: Arc<dyn sonda_core::GateBusResolver> = state.gate_bus_registry.clone();
    let mut handles = launch_multi_compiled(compiled, Some(resolver))
        .await
        .map_err(|e| {
            warn!(error = %e, "POST /scenarios: failed to launch scenarios");
            match e {
                sonda_core::SondaError::Config(_) => unprocessable(e),
                _ => internal_error(e),
            }
        })?;

    if handles.is_empty() {
        warn!("POST /scenarios: gated launch produced zero handles");
        return Err(bad_request(
            "v2 scenario body produced zero runnable entries",
        ));
    }

    let mut created: Vec<CreatedScenario> = Vec::with_capacity(handles.len());
    for handle in handles.iter_mut() {
        let new_id = Uuid::new_v4().to_string();
        let old_id = std::mem::replace(&mut handle.id, new_id.clone());
        state.gate_bus_registry.rename_handle(&old_id, &new_id);
        let name = handle.name.clone();
        let state_str = state_string(&handle.stats_snapshot()).to_string();
        info!(id = %new_id, name = %name, state = %state_str, "scenario launched");
        created.push(CreatedScenario {
            id: new_id,
            name,
            state: state_str,
            warnings: Vec::new(),
        });
    }

    let mut scenarios = state.scenarios.write().map_err(|e| {
        for handle in &handles {
            handle.stop();
        }
        warn!(error = %e, "POST /scenarios: scenarios lock is poisoned");
        internal_error("internal state lock is poisoned")
    })?;
    for (created_entry, handle) in created.iter().zip(handles) {
        scenarios.insert(created_entry.id.clone(), handle);
    }
    drop(scenarios);

    if created.len() == 1 {
        let mut single = created.into_iter().next().expect("len checked above");
        single.warnings = warnings;
        Ok((StatusCode::CREATED, Json(single)).into_response())
    } else {
        Ok((
            StatusCode::CREATED,
            Json(CreatedScenariosResponse {
                scenarios: created,
                warnings,
            }),
        )
            .into_response())
    }
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

    let now_unix_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let summaries: Vec<ScenarioSummary> = scenarios
        .iter()
        .map(|(id, handle)| {
            let snap = handle.stats_snapshot();
            ScenarioSummary {
                id: id.clone(),
                name: handle.name.clone(),
                state: state_string(&snap).to_string(),
                elapsed_secs: handle.elapsed().as_secs_f64(),
                degraded: snap.is_degraded(now_unix_nanos),
            }
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

    let now_unix_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let snap = handle.stats_snapshot();
    let degraded = snap.is_degraded(now_unix_nanos);
    let state_str = state_string(&snap).to_string();
    let pending_ref = if snap.state == ScenarioState::Unresolved {
        state.gate_bus_registry.pending_for_handle(&handle.id)
    } else {
        None
    };
    let mut stats: StatsResponse = snap.into();
    stats.degraded = degraded;
    let detail = ScenarioDetail {
        id: id.clone(),
        name: handle.name.clone(),
        state: state_str,
        elapsed_secs: handle.elapsed().as_secs_f64(),
        degraded,
        stats,
        pending_ref,
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
    let (mut handle, scenario_name_to_unregister) = {
        let mut scenarios = state
            .scenarios
            .write()
            .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;
        let handle = scenarios
            .remove(&id)
            .ok_or_else(|| not_found(format!("scenario not found: {id}")))?;
        handle
            .cleaned_up
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let scenario_name_to_unregister = handle.scenario_name.clone();
        handle.stop();
        (handle, scenario_name_to_unregister)
    };

    if let Err(e) = handle.join_async(Some(Duration::from_secs(5))).await {
        warn!(id = %id, error = %e, "DELETE /scenarios/{id}: scenario task returned an error");
    }

    let status = if handle.is_running() {
        warn!(id = %id, "DELETE /scenarios/{id}: join timed out after 5s, scenario force-stopped");
        "force_stopped".to_string()
    } else {
        "stopped".to_string()
    };

    let final_stats = handle.stats_snapshot();

    if let Some(name) = scenario_name_to_unregister {
        state.gate_bus_registry.unregister(&name);
    }

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
/// (computed from `handle.elapsed()`), and `state` (one of `pending`,
/// `running`, `paused`, `finished`).
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

    let now_unix_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let snap = handle.stats_snapshot();
    let state_str = state_string(&snap).to_string();
    let degraded = snap.is_degraded(now_unix_nanos);
    let current_state_secs = snap.current_state_secs();
    let response = DetailedStatsResponse {
        total_events: snap.total_events,
        current_rate: snap.current_rate,
        target_rate: handle.target_rate,
        bytes_emitted: snap.bytes_emitted,
        errors: snap.errors,
        uptime_secs: handle.elapsed().as_secs_f64(),
        state: state_str,
        in_gap: snap.in_gap,
        in_burst: snap.in_burst,
        consecutive_failures: snap.consecutive_failures,
        total_sink_failures: snap.total_sink_failures,
        last_sink_error: snap.last_sink_error,
        last_successful_write_at: snap.last_successful_write_at,
        degraded,
        current_state_secs,
        cumulative_resolution_attempts: snap.cumulative_resolution_attempts,
    };

    Ok(Json(response))
}

// ---- Scrape endpoint --------------------------------------------------------

/// Prometheus text exposition format content type.
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /scenarios/{id}/metrics` — emit one Prometheus sample per series.
///
/// Returns the current value of every series the scenario has emitted so far,
/// encoded in Prometheus text exposition format with no per-sample timestamp,
/// matching the shape that `node_exporter` and Prometheus self-scrape produce.
pub async fn get_scenario_metrics(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let scenarios = state
        .scenarios
        .read()
        .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;

    let handle = scenarios
        .get(&id)
        .ok_or_else(|| not_found(format!("scenario not found: {id}")))?;

    let events = handle.recent_metrics_snapshot();

    let encoder = PrometheusText::new(None).with_emit_timestamp(false);
    let mut buf = Vec::with_capacity(events.len() * 128 + 256);
    if !events.is_empty() {
        if let Some(meta) = handle.prometheus_meta.as_ref() {
            if let Err(e) = encoder.encode_metadata(
                &handle.name,
                meta.metric_type,
                meta.help.as_deref(),
                &mut buf,
            ) {
                warn!(id = %id, error = %e, "GET /scenarios/{id}/metrics: failed to encode metadata");
            }
        }
    }
    for event in &events {
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

// ---- Aggregate scrape endpoint ----------------------------------------------

fn parse_label_filters(raw: Option<&str>) -> Result<Vec<(String, String)>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut filters = Vec::new();
    for pair in raw.split('&').filter(|s| !s.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key != "label" {
            continue;
        }
        let decoded = percent_decode(value);
        let (k, v) = decoded.split_once(':').ok_or_else(|| {
            format!("label filter '{decoded}' is malformed: expected 'key:value'")
        })?;
        if k.is_empty() {
            return Err(format!("label filter '{decoded}' has an empty key"));
        }
        if v.is_empty() {
            return Err(format!("label filter '{decoded}' has an empty value"));
        }
        filters.push((k.to_string(), v.to_string()));
    }
    Ok(filters)
}

/// Parse `include_state=a,b,...` into a deduped allowlist of [`ScenarioState`].
/// Returns `Ok(None)` when the param is absent, `Err` for empty / unknown tokens.
fn parse_include_state(raw_query: Option<&str>) -> Result<Option<Vec<ScenarioState>>, String> {
    let Some(raw) = raw_query else {
        return Ok(None);
    };
    // Last occurrence wins, matching axum's `Query<T>` convention.
    let mut last_value: Option<String> = None;
    for pair in raw.split('&').filter(|s| !s.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "include_state" {
            last_value = Some(percent_decode(value));
        }
    }
    let Some(decoded) = last_value else {
        return Ok(None);
    };
    if decoded.is_empty() {
        return Err("include_state requires at least one state name".to_string());
    }
    let mut out: Vec<ScenarioState> = Vec::new();
    for token in decoded.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return Err("include_state requires at least one state name".to_string());
        }
        let state = match trimmed {
            "pending" => ScenarioState::Pending,
            "running" => ScenarioState::Running,
            "paused" => ScenarioState::Paused,
            "held" => ScenarioState::Held,
            "unresolved" => ScenarioState::Unresolved,
            "finished" => ScenarioState::Finished,
            other => {
                return Err(format!(
                    "unknown state name '{other}' in include_state — expected one of: \
                     pending, running, paused, held, unresolved, finished"
                ));
            }
        };
        if !out.contains(&state) {
            out.push(state);
        }
    }
    Ok(Some(out))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

pub async fn get_aggregate_metrics(
    State(state): State<AppState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, Response> {
    let filters = parse_label_filters(raw_query.as_deref()).map_err(bad_request)?;
    let state_filter = parse_include_state(raw_query.as_deref()).map_err(bad_request)?;

    let scenarios = state
        .scenarios
        .read()
        .map_err(|e| internal_error(format!("scenarios lock is poisoned: {e}")))?;

    let mut ids: Vec<&String> = scenarios.keys().collect();
    ids.sort();

    let encoder = PrometheusText::new(None).with_emit_timestamp(false);
    let mut buf = Vec::new();
    let mut groups: Vec<AggregateGroup> = Vec::new();
    for id in ids {
        let handle = match scenarios.get(id) {
            Some(h) => h,
            None => continue,
        };
        if !filters.iter().all(|(k, v)| {
            handle
                .labels
                .get(k.as_str())
                .map(|hv| hv == v)
                .unwrap_or(false)
        }) {
            continue;
        }
        if let Some(allow) = &state_filter {
            if !allow.contains(&handle.stats_snapshot().state) {
                continue;
            }
        }
        let events = handle.recent_metrics_snapshot();
        if events.is_empty() && handle.prometheus_meta.is_none() {
            continue;
        }
        let name = handle.name.clone();
        let meta = handle.prometheus_meta.as_ref().map(|m| m.as_ref().clone());
        match groups.iter_mut().find(|g| g.name == name) {
            Some(group) => {
                if let Some(incoming) = meta {
                    match group.meta.as_mut() {
                        Some(existing) => {
                            if existing.metric_type != incoming.metric_type
                                && existing.metric_type != sonda_core::PromMetricType::Untyped
                            {
                                let declared = [existing.metric_type, incoming.metric_type];
                                warn!(
                                    metric_name = %name,
                                    declared_types = ?declared,
                                    "GET /metrics: mixed metric_type declarations for same name; \
                                     emitting as untyped",
                                );
                                existing.metric_type = sonda_core::PromMetricType::Untyped;
                            }
                            if existing.help.is_none() && incoming.help.is_some() {
                                existing.help = incoming.help;
                            }
                        }
                        None => {
                            group.meta = Some(incoming);
                        }
                    }
                }
                group.events.extend(events);
            }
            None => {
                groups.push(AggregateGroup { name, meta, events });
            }
        }
    }

    for group in &groups {
        if let Some(meta) = group.meta.as_ref() {
            if let Err(e) = encoder.encode_metadata(
                &group.name,
                meta.metric_type,
                meta.help.as_deref(),
                &mut buf,
            ) {
                warn!(name = %group.name, error = %e, "GET /metrics: failed to encode metadata");
            }
        }
        for event in &group.events {
            if let Err(e) = encoder.encode_metric(event, &mut buf) {
                warn!(name = %group.name, error = %e, "GET /metrics: failed to encode metric event");
            }
        }
    }

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        buf,
    )
        .into_response())
}

struct AggregateGroup {
    name: String,
    meta: Option<sonda_core::PromMeta>,
    events: Vec<sonda_core::model::metric::MetricEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::router;
    use crate::state::AppState;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode};
    use sonda_core::{CancellationToken, ScenarioHandle};
    use std::sync::{Arc, RwLock};
    use std::thread;
    use std::time::{Duration, Instant};
    use tower::ServiceExt;

    // ---- Helpers ---------------------------------------------------------------

    /// Build a ScenarioHandle with a background task that increments stats.
    fn make_handle(id: &str, name: &str, event_count: u64, interval: Duration) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let cancel_clone = cancel.clone();
        let stats_clone = Arc::clone(&stats);

        let task = tokio::task::spawn(async move {
            for _ in 0..event_count {
                if cancel_clone.is_cancelled() {
                    break;
                }
                tokio::time::sleep(interval).await;
                if let Ok(mut st) = stats_clone.write() {
                    st.total_events += 1;
                    st.bytes_emitted += 64;
                }
            }
            Ok::<_, sonda_core::SondaError>(())
        });

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            100.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(std::collections::HashMap::new()),
            Some(std::sync::Arc::new(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            ))),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    /// Build a ScenarioHandle whose task exits immediately.
    fn make_stopped_handle(id: &str, name: &str) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let task = tokio::task::spawn(async { Ok::<_, sonda_core::SondaError>(()) });

        thread::sleep(Duration::from_millis(50));

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            100.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(std::collections::HashMap::new()),
            Some(std::sync::Arc::new(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            ))),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
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

    /// Valid v2 body for a metrics scenario with a short duration.
    const VALID_METRICS_YAML: &str = "\
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: test_metric
    signal_type: metrics
    name: test_metric
    generator:
      type: constant
      value: 42.0
";

    /// Valid v2 body for a logs scenario with a short duration.
    const VALID_LOGS_YAML: &str = "\
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: json_lines
  sink:
    type: stdout
scenarios:
  - id: test_logs
    signal_type: logs
    name: test_logs
    log_generator:
      type: template
      templates:
        - message: \"test log event\"
          field_pools: {}
      seed: 0
";

    /// Valid v2 body with an explicit `signal_type: metrics` entry.
    const VALID_TAGGED_METRICS_YAML: &str = "\
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: tagged_metric
    signal_type: metrics
    name: tagged_metric
    generator:
      type: constant
      value: 1.0
";

    /// v2 body with `rate: 0` — must be rejected by runtime validation.
    const ZERO_RATE_YAML: &str = "\
version: 2
kind: runnable
defaults:
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: bad_rate
    signal_type: metrics
    name: bad_rate
    rate: 0
    generator:
      type: constant
      value: 1.0
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
        assert!(entry["state"].is_string(), "state must be a string");
        assert!(
            entry["elapsed_secs"].is_f64(),
            "elapsed_secs must be a number"
        );
        assert!(entry["degraded"].is_boolean(), "degraded must be a boolean");
    }

    /// A scenario with no sink failures reports degraded=false in the list.
    #[tokio::test]
    async fn list_scenarios_degraded_false_for_healthy_scenario() {
        let h = make_handle(
            "id-healthy",
            "healthy_test",
            1000,
            Duration::from_millis(50),
        );
        let app = router_with_handles(vec![h]);

        let req = Request::builder()
            .uri("/scenarios")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = body_json(resp).await;
        let entry = &body["scenarios"][0];

        assert_eq!(
            entry["degraded"], false,
            "a scenario with no sink failures must report degraded=false"
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
        if let Ok(mut s) = h.stats.write() {
            s.state = ScenarioState::Running;
        }
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
            body["state"].as_str().unwrap(),
            "running",
            "a live scenario must have state 'running'"
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

    // ---- GET /scenarios/{id}: degraded field on detail + nested stats --------

    #[tokio::test]
    async fn get_scenario_degraded_false_for_healthy_scenario() {
        let h = make_handle(
            "id-detail-healthy",
            "detail_healthy",
            1000,
            Duration::from_millis(50),
        );
        let app = router_with_handles(vec![h]);

        let req = Request::builder()
            .uri("/scenarios/id-detail-healthy")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = body_json(resp).await;

        assert!(body["degraded"].is_boolean(), "degraded must be a boolean");
        assert_eq!(
            body["degraded"], false,
            "a scenario with no sink failures must report degraded=false"
        );
        assert_eq!(
            body["stats"]["degraded"], false,
            "nested stats.degraded must mirror the top-level field"
        );
    }

    #[tokio::test]
    async fn get_scenario_degraded_true_when_failures_and_no_delivery() {
        let mut stats = ScenarioStats::default();
        stats.total_sink_failures = 5;
        stats.consecutive_failures = 5;
        stats.last_sink_error = Some("connection refused".to_string());
        stats.last_successful_write_at = None;
        let h = make_handle_with_stats("id-detail-degraded", "detail_degraded", 100.0, stats, true);
        let app = router_with_handles(vec![h]);

        let req = Request::builder()
            .uri("/scenarios/id-detail-degraded")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body = body_json(resp).await;

        assert_eq!(
            body["degraded"], true,
            "failures with no successful delivery must report degraded=true"
        );
        assert_eq!(
            body["stats"]["degraded"], true,
            "nested stats.degraded must mirror the top-level field"
        );
    }

    // ---- GET /scenarios/{id}: stats.total_events > 0 after running ------------

    /// After running for a short time, stats.total_events > 0.
    #[tokio::test(flavor = "multi_thread")]
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

    // ---- GET /scenarios/{id}: finished scenario reports "finished" ------------

    #[tokio::test]
    async fn get_scenario_finished_reports_finished_status() {
        let h = make_stopped_handle("id-stopped", "finished_scenario");
        if let Ok(mut s) = h.stats.write() {
            s.state = ScenarioState::Finished;
        }
        let app = router_with_handles(vec![h]);

        let req = Request::builder()
            .uri("/scenarios/id-stopped")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(
            body["state"].as_str().unwrap(),
            "finished",
            "a finished scenario must have state 'finished'"
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
        // `ScenarioStats` is `#[non_exhaustive]` across the crate boundary,
        // so struct-literal construction is forbidden here. Start from
        // `Default::default()` and set the fields the test cares about.
        let mut stats = ScenarioStats::default();
        stats.total_events = 42;
        stats.bytes_emitted = 1024;
        stats.current_rate = 10.5;
        stats.errors = 3;
        stats.in_gap = true;
        stats.in_burst = false;
        let resp: StatsResponse = stats.into();
        assert_eq!(resp.total_events, 42);
        assert_eq!(resp.bytes_emitted, 1024);
        assert_eq!((resp.current_rate * 10.0).round(), 105.0);
        assert_eq!(resp.errors, 3);
    }

    // ---- state_string helper -------------------------------------------------

    #[test]
    fn state_string_maps_each_variant_to_lowercase_wire_string() {
        let mut s = ScenarioStats::default();
        s.state = ScenarioState::Pending;
        assert_eq!(state_string(&s), "pending");
        s.state = ScenarioState::Running;
        assert_eq!(state_string(&s), "running");
        s.state = ScenarioState::Paused;
        assert_eq!(state_string(&s), "paused");
        s.state = ScenarioState::Held;
        assert_eq!(state_string(&s), "held");
        s.state = ScenarioState::Unresolved;
        assert_eq!(state_string(&s), "unresolved");
        s.state = ScenarioState::Finished;
        assert_eq!(state_string(&s), "finished");
    }

    // ---- Serialization: response structs produce valid JSON ------------------

    /// ScenarioSummary serializes with all expected fields.
    #[test]
    fn scenario_summary_serializes_correctly() {
        let s = ScenarioSummary {
            id: "abc".to_string(),
            name: "test".to_string(),
            state: "running".to_string(),
            elapsed_secs: 1.5,
            degraded: false,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["id"], "abc");
        assert_eq!(json["name"], "test");
        assert_eq!(json["state"], "running");
        assert_eq!(json["elapsed_secs"], 1.5);
        assert_eq!(json["degraded"], false);
    }

    /// ScenarioDetail serializes with nested stats object.
    #[test]
    fn scenario_detail_serializes_with_nested_stats() {
        let d = ScenarioDetail {
            id: "xyz".to_string(),
            name: "detail".to_string(),
            state: "stopped".to_string(),
            elapsed_secs: 42.0,
            degraded: false,
            pending_ref: None,
            stats: StatsResponse {
                total_events: 100,
                current_rate: 5.0,
                bytes_emitted: 2048,
                errors: 1,
                consecutive_failures: 0,
                total_sink_failures: 0,
                last_sink_error: None,
                last_successful_write_at: None,
                degraded: false,
            },
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["id"], "xyz");
        assert_eq!(json["degraded"], false);
        assert_eq!(json["stats"]["total_events"], 100);
        assert_eq!(json["stats"]["errors"], 1);
        assert_eq!(json["stats"]["degraded"], false);
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
        let s = body["state"].as_str().unwrap_or("");
        assert!(
            matches!(s, "pending" | "running"),
            "state must be 'pending' or 'running' for a freshly launched scenario, got {s:?}"
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
        let s = body["state"].as_str().unwrap_or("");
        assert!(
            matches!(s, "pending" | "running"),
            "state must be 'pending' or 'running', got {s:?}"
        );

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
        let s = body["state"].as_str().unwrap_or("");
        assert!(
            matches!(s, "pending" | "running"),
            "state must be 'pending' or 'running', got {s:?}"
        );

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

    /// POST with Content-Type: application/json and a valid v2 JSON metrics body returns 201.
    #[tokio::test]
    async fn post_with_json_content_type_returns_201() {
        let json_body = serde_json::json!({
            "version": 2,
            "kind": "runnable",
            "defaults": {
                "rate": 10,
                "duration": "200ms",
                "encoder": { "type": "prometheus_text" },
                "sink": { "type": "stdout" }
            },
            "scenarios": [
                {
                    "id": "json_metric",
                    "signal_type": "metrics",
                    "name": "json_metric",
                    "generator": { "type": "constant", "value": 1.0 }
                }
            ]
        });

        let (app, state) = test_router();
        let response = post_scenarios(app, "application/json", &json_body.to_string()).await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "application/json content type must be accepted for valid v2 JSON scenario"
        );

        let body = body_json(response).await;
        assert_eq!(body["name"], "json_metric");
        let s = body["state"].as_str().unwrap_or("");
        assert!(
            matches!(s, "pending" | "running"),
            "state must be 'pending' or 'running', got {s:?}"
        );

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
            obj.contains_key("state"),
            "response must contain key 'state'"
        );
        assert_eq!(
            obj.len(),
            3,
            "response must contain exactly 3 keys (id, name, state)"
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

    /// POST v2 YAML with a negative rate returns 422.
    #[tokio::test]
    async fn post_yaml_with_negative_rate_returns_422() {
        let yaml = "\
version: 2
kind: runnable
defaults:
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: neg_rate
    signal_type: metrics
    name: neg_rate
    rate: -5
    generator:
      type: constant
      value: 1.0
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

    /// `parse_body` accepts a v2 metrics YAML and returns a single-entry CompiledFile.
    #[test]
    fn parse_body_accepts_v2_metrics_yaml() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let parsed = parse_body(VALID_METRICS_YAML.as_bytes(), &headers, None)
            .expect("v2 metrics body must parse");
        let ParsedBody::Compiled(compiled) = parsed;
        assert_eq!(compiled.entries.len(), 1);
        assert_eq!(compiled.entries[0].signal_type, "metrics");
        assert_eq!(compiled.entries[0].name, "test_metric");
    }

    /// `parse_body` accepts a v2 logs YAML and returns a single-entry CompiledFile.
    #[test]
    fn parse_body_accepts_v2_logs_yaml() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let parsed = parse_body(VALID_LOGS_YAML.as_bytes(), &headers, None)
            .expect("v2 logs body must parse");
        let ParsedBody::Compiled(compiled) = parsed;
        assert_eq!(compiled.entries.len(), 1);
        assert_eq!(compiled.entries[0].signal_type, "logs");
        assert_eq!(compiled.entries[0].name, "test_logs");
    }

    /// `parse_body` rejects a v1 flat metrics YAML (no `version: 2`).
    #[test]
    fn parse_body_rejects_v1_flat_metrics() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let v1_yaml = "\
name: legacy
rate: 10
generator:
  type: constant
  value: 1.0
";
        let err = parse_body(v1_yaml.as_bytes(), &headers, None)
            .expect_err("v1 flat YAML must be rejected");
        let msg = err.message();
        assert!(
            msg.contains("v2"),
            "rejection must mention v2 requirement, got: {msg}"
        );
        assert!(
            msg.contains("v2-scenarios.md") || msg.contains("Migrate"),
            "rejection must include migration hint, got: {msg}"
        );
    }

    /// `parse_body` rejects a v1 multi-scenario YAML without `version: 2`.
    #[test]
    fn parse_body_rejects_v1_multi_scenarios() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let v1_multi = "\
scenarios:
  - signal_type: metrics
    name: legacy
    rate: 10
    generator:
      type: constant
      value: 1.0
";
        let err = parse_body(v1_multi.as_bytes(), &headers, None)
            .expect_err("v1 multi-scenario YAML must be rejected");
        let msg = err.message();
        assert!(
            msg.contains("v2"),
            "rejection must mention v2 requirement, got: {msg}"
        );
    }

    /// `parse_body` rejects garbage input with a clear YAML error.
    #[test]
    fn parse_body_rejects_garbage_yaml() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let err = parse_body(b"not valid: [}{", &headers, None).expect_err("garbage must fail");
        assert!(!err.message().is_empty(), "error message must not be empty");
    }

    /// `parse_body` accepts a v2 JSON body and transcodes it to YAML internally.
    #[test]
    fn parse_body_accepts_v2_json() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        let json = serde_json::json!({
            "version": 2,
            "kind": "runnable",
            "defaults": {
                "rate": 10,
                "duration": "200ms",
                "encoder": { "type": "prometheus_text" },
                "sink": { "type": "stdout" }
            },
            "scenarios": [
                {
                    "id": "json_test",
                    "signal_type": "metrics",
                    "name": "json_test",
                    "generator": { "type": "constant", "value": 1.0 }
                }
            ]
        });
        let parsed = parse_body(json.to_string().as_bytes(), &headers, None)
            .expect("v2 JSON body must parse");
        let ParsedBody::Compiled(compiled) = parsed;
        assert_eq!(compiled.entries.len(), 1);
    }

    /// `parse_body` rejects invalid JSON with a descriptive error.
    #[test]
    fn parse_body_rejects_invalid_json() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        let err = parse_body(b"not json", &headers, None).expect_err("invalid JSON must fail");
        assert!(!err.message().is_empty(), "error message must not be empty");
    }

    // ---- Test: pack catalog resolution -----------------------------------------

    const PACK_YAML: &str = "\
version: 2
kind: composable
name: tiny-pack
description: A small test pack
category: network
metrics:
  - name: pack_metric_a
    generator:
      type: constant
      value: 1.0
";

    const PACK_REF_BODY: &str = "\
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - signal_type: metrics
    pack: tiny-pack
";

    fn write_catalog_pack(dir: &std::path::Path) {
        std::fs::write(dir.join("tiny-pack.yaml"), PACK_YAML).expect("write pack file");
    }

    #[test]
    fn parse_body_resolves_pack_reference_from_catalog_dir() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        write_catalog_pack(tmp.path());

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let parsed = parse_body(PACK_REF_BODY.as_bytes(), &headers, Some(tmp.path()))
            .expect("pack reference must resolve against the catalog dir");
        let ParsedBody::Compiled(compiled) = parsed;
        assert!(
            compiled.entries.iter().any(|e| e.name == "pack_metric_a"),
            "expanded pack metric must be present"
        );
    }

    #[test]
    fn parse_body_pack_reference_without_catalog_fails_cleanly() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let err = parse_body(PACK_REF_BODY.as_bytes(), &headers, None)
            .expect_err("pack reference must fail without a catalog dir");
        let msg = err.message();
        assert!(msg.contains("tiny-pack"), "error must name the pack: {msg}");
        assert!(
            msg.contains("catalog") || msg.contains("--catalog"),
            "error must explain the missing catalog: {msg}"
        );
    }

    #[tokio::test]
    async fn post_scenario_resolves_pack_reference_when_catalog_set() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        write_catalog_pack(tmp.path());

        let mut state = AppState::new();
        state.catalog_dir = Some(Arc::new(tmp.path().to_path_buf()));
        let app = router(state.clone());

        let resp = post_scenarios(app, "application/x-yaml", PACK_REF_BODY).await;
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "pack-referencing body must launch when catalog is configured"
        );
        cleanup_scenarios(&state);
    }

    #[tokio::test]
    async fn post_scenario_pack_reference_without_catalog_returns_4xx() {
        let (app, _state) = test_router();
        let resp = post_scenarios(app, "application/x-yaml", PACK_REF_BODY).await;
        assert!(
            resp.status().is_client_error(),
            "pack-referencing body must return 4xx without a catalog, got {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn post_scenario_plain_body_works_with_catalog_set() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        write_catalog_pack(tmp.path());

        let mut state = AppState::new();
        state.catalog_dir = Some(Arc::new(tmp.path().to_path_buf()));
        let app = router(state.clone());

        let resp = post_scenarios(app, "application/x-yaml", VALID_METRICS_YAML).await;
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "ordinary body must still launch with a catalog configured"
        );
        cleanup_scenarios(&state);
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
            state: "running".to_string(),
            warnings: Vec::new(),
        };
        let json = serde_json::to_value(&cs).expect("must serialize");
        assert_eq!(json["id"], "abc-123");
        assert_eq!(json["name"], "my_scenario");
        assert_eq!(json["state"], "running");
        assert!(
            json.get("warnings").is_none(),
            "empty warnings vec must be omitted from JSON"
        );
    }

    /// Populated warnings serialize as a JSON string array on the response.
    #[test]
    fn created_scenario_serializes_warnings_when_present() {
        let cs = CreatedScenario {
            id: "abc-123".to_string(),
            name: "my_scenario".to_string(),
            state: "running".to_string(),
            warnings: vec!["loopback warning".to_string()],
        };
        let json = serde_json::to_value(&cs).expect("must serialize");
        let arr = json["warnings"].as_array().expect("warnings must be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0], "loopback warning");
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
    #[tokio::test(flavor = "multi_thread")]
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
        let cancel = CancellationToken::new();
        if !running {
            cancel.cancel();
        }
        let stats = Arc::new(RwLock::new(initial_stats));
        let cancel_clone = cancel.clone();

        let task = tokio::task::spawn(async move {
            cancel_clone.cancelled().await;
            Ok::<_, sonda_core::SondaError>(())
        });

        if !running {
            thread::sleep(Duration::from_millis(50));
        }

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            target_rate,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(std::collections::HashMap::new()),
            Some(std::sync::Arc::new(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            ))),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
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
        let mut stats = ScenarioStats::default();
        stats.total_events = 500;
        stats.bytes_emitted = 32000;
        stats.current_rate = 99.5;
        stats.errors = 2;
        stats.in_gap = false;
        stats.in_burst = true;
        stats.state = ScenarioState::Running;
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
        assert!(!body["in_gap"].as_bool().unwrap(), "in_gap must be false");
        assert!(body["in_burst"].as_bool().unwrap(), "in_burst must be true");
    }

    // ---- /stats endpoint: degraded field --------------------------------------

    #[tokio::test]
    async fn stats_endpoint_degraded_false_for_healthy_scenario() {
        let h = make_handle_with_stats(
            "id-stats-healthy",
            "stats_healthy",
            10.0,
            ScenarioStats::default(),
            true,
        );
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-healthy").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert!(body["degraded"].is_boolean(), "degraded must be a boolean");
        assert_eq!(
            body["degraded"], false,
            "a scenario with no sink failures must report degraded=false"
        );
    }

    #[tokio::test]
    async fn stats_endpoint_degraded_true_when_failures_and_no_delivery() {
        let mut stats = ScenarioStats::default();
        stats.total_sink_failures = 3;
        stats.consecutive_failures = 3;
        stats.last_sink_error = Some("connection refused".to_string());
        stats.last_successful_write_at = None;
        let h = make_handle_with_stats("id-stats-degraded", "stats_degraded", 10.0, stats, true);
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-degraded").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(
            body["degraded"], true,
            "failures with no successful delivery must report degraded=true"
        );
    }

    // ---- Fields update as scenario progresses --------------------------------

    /// Stats fields update as the scenario background thread emits events.
    #[tokio::test(flavor = "multi_thread")]
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
        let mut stats = ScenarioStats::default();
        stats.total_events = 10;
        stats.bytes_emitted = 640;
        stats.current_rate = 0.0;
        stats.errors = 0;
        stats.in_gap = true;
        stats.in_burst = false;
        let h = make_handle_with_stats("id-stats-gap", "gap_test", 50.0, stats, true);
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-gap").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert!(
            body["in_gap"].as_bool().unwrap(),
            "in_gap must be true when the scenario is in a gap window"
        );
        assert!(
            !body["in_burst"].as_bool().unwrap(),
            "in_burst must be false when only in_gap is set"
        );
    }

    // ---- After scenario finished: returns final stats with state "finished" ----

    #[tokio::test]
    async fn stats_endpoint_returns_finished_state_for_finished_scenario() {
        let mut stats = ScenarioStats::default();
        stats.total_events = 1000;
        stats.bytes_emitted = 64000;
        stats.current_rate = 0.0;
        stats.errors = 5;
        stats.in_gap = false;
        stats.in_burst = false;
        stats.state = ScenarioState::Finished;
        let h = make_handle_with_stats("id-stats-finished", "finished_test", 200.0, stats, false);
        let app = router_with_handles(vec![h]);

        let resp = get_stats_req(app, "id-stats-finished").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        assert_eq!(
            body["state"].as_str().unwrap(),
            "finished",
            "state must be 'finished' for a finished scenario"
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
        let mut stats = ScenarioStats::default();
        stats.total_events = 0;
        stats.bytes_emitted = 0;
        stats.current_rate = 45.0;
        stats.errors = 0;
        stats.in_gap = false;
        stats.in_burst = false;
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
    #[allow(clippy::approx_constant)] // 3.14 is a sample stat value, not the PI constant
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
            consecutive_failures: 0,
            total_sink_failures: 0,
            last_sink_error: None,
            last_successful_write_at: None,
            degraded: false,
            current_state_secs: 0.0,
            cumulative_resolution_attempts: 0,
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
        assert_eq!(json["degraded"], false);
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

    /// Build a ScenarioHandle with pre-populated metric events in the buffer.
    fn make_handle_with_metrics(
        id: &str,
        name: &str,
        events: Vec<sonda_core::model::metric::MetricEvent>,
    ) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let mut stats = ScenarioStats::default();
        for event in events {
            stats.push_metric(event);
        }
        let stats = Arc::new(RwLock::new(stats));
        let cancel_clone = cancel.clone();

        let task = tokio::task::spawn(async move {
            cancel_clone.cancelled().await;
            Ok::<_, sonda_core::SondaError>(())
        });

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            10.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(std::collections::HashMap::new()),
            Some(std::sync::Arc::new(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            ))),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    /// Helper: send a GET /scenarios/{id}/metrics request.
    async fn get_metrics_req(app: axum::Router, id: &str) -> hyper::Response<axum::body::Body> {
        let req = Request::builder()
            .uri(format!("/scenarios/{id}/metrics"))
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

    // ---- Metrics scrape: empty buffer returns 200 with empty body -----------

    /// Empty buffer must render as `200 OK` with an empty Prometheus exposition
    /// (the contract Prometheus / vmagent / Telegraf scrapers expect). 204
    /// breaks scrapers that use `curl --fail`.
    #[tokio::test]
    async fn metrics_endpoint_empty_buffer_returns_200_empty_body() {
        let h = make_handle_with_metrics("id-metrics-empty", "empty_metrics", vec![]);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-empty").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.is_empty(),
            "empty buffer must render as empty Prometheus exposition, got: {body:?}"
        );
    }

    // ---- Metrics scrape: returns Prometheus text format ----------------------

    #[tokio::test]
    async fn metrics_endpoint_returns_prometheus_text_format() {
        let events = vec![make_metric_event("up", 1.0), make_metric_event("up", 2.0)];
        let h = make_handle_with_metrics("id-metrics-prom", "prom_text", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-prom").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|line| !line.starts_with('#')).collect();
        assert_eq!(
            sample_lines.len(),
            1,
            "same series must collapse to one sample, got {sample_lines:?}"
        );
        assert!(
            sample_lines[0].starts_with("up"),
            "sample must start with metric name 'up', got: {}",
            sample_lines[0]
        );
    }

    #[tokio::test]
    async fn per_scenario_metrics_emits_one_sample_per_series_no_timestamp() {
        let events = vec![make_metric_event("up", 42.0)];
        let h = make_handle_with_metrics("id-no-ts", "no_ts", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-no-ts").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(sample_lines.len(), 1);
        assert_eq!(
            sample_lines[0], "up 42",
            "sample must omit trailing timestamp, got: {}",
            sample_lines[0]
        );
        assert!(
            body.ends_with("up 42\n"),
            "body must end with value+newline, no timestamp: {body:?}"
        );
    }

    #[tokio::test]
    async fn per_scenario_metrics_distinct_series_emit_distinct_samples() {
        let events = vec![
            labeled_metric_event("up", 1.0, &[("host", "a")]),
            labeled_metric_event("up", 2.0, &[("host", "b")]),
        ];
        let h = make_handle_with_metrics("id-distinct", "distinct", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-distinct").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(sample_lines.len(), 2, "got: {body:?}");
    }

    #[tokio::test]
    async fn per_scenario_metrics_idempotent_across_scrapes() {
        let events = vec![make_metric_event("up", 1.0), make_metric_event("up", 2.0)];
        let h = make_handle_with_metrics("id-idem", "idem", events);
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app1 = router(state.clone());
        let body_1 = body_string(get_metrics_req(app1, "id-idem").await).await;
        let app2 = router(state.clone());
        let body_2 = body_string(get_metrics_req(app2, "id-idem").await).await;
        assert_eq!(
            body_1, body_2,
            "two scrapes must return byte-identical bodies"
        );
        assert!(!body_1.is_empty(), "first scrape must not be empty");

        cleanup_scenarios(&state);
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

    #[tokio::test]
    async fn metrics_endpoint_emits_one_sample_per_distinct_series() {
        let events: Vec<_> = (0..5)
            .map(|i| labeled_metric_event("up", i as f64, &[("host", &format!("h{i}"))]))
            .collect();
        let h = make_handle_with_metrics("id-metrics-five", "five_series", events);
        let app = router_with_handles(vec![h]);

        let resp = get_metrics_req(app, "id-metrics-five").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|line| !line.starts_with('#')).collect();
        assert_eq!(
            sample_lines.len(),
            5,
            "five distinct series must produce five samples, got {sample_lines:?}"
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

    // ========================================================================
    // GET /metrics aggregate scrape tests
    // ========================================================================

    fn make_handle_with_labels_and_metrics(
        id: &str,
        name: &str,
        labels: Vec<(&str, &str)>,
        events: Vec<sonda_core::model::metric::MetricEvent>,
    ) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let mut stats = ScenarioStats::default();
        for event in events {
            stats.push_metric(event);
        }
        let stats = Arc::new(RwLock::new(stats));
        let cancel_clone = cancel.clone();

        let task = tokio::task::spawn(async move {
            cancel_clone.cancelled().await;
            Ok::<_, sonda_core::SondaError>(())
        });

        let mut label_map = std::collections::HashMap::new();
        for (k, v) in labels {
            label_map.insert(k.to_string(), v.to_string());
        }

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            10.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(label_map),
            Some(std::sync::Arc::new(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            ))),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    async fn get_aggregate_req(
        app: axum::Router,
        query: &str,
    ) -> hyper::Response<axum::body::Body> {
        let uri = if query.is_empty() {
            "/metrics".to_string()
        } else {
            format!("/metrics?{query}")
        };
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        app.oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn aggregate_metrics_empty_state_returns_200_empty_body() {
        let app = router_with_handles(vec![]);
        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get("content-type")
            .expect("Content-Type must be present")
            .to_str()
            .unwrap();
        assert_eq!(ct, "text/plain; version=0.0.4; charset=utf-8");

        let body = body_string(resp).await;
        assert!(
            body.trim().is_empty(),
            "empty state must render as empty body, got: {body:?}"
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_single_scenario_no_filter_returns_events() {
        let events = vec![
            labeled_metric_event("up", 1.0, &[("host", "a")]),
            labeled_metric_event("up", 2.0, &[("host", "b")]),
        ];
        let h = make_handle_with_labels_and_metrics("agg-1", "single", vec![], events);
        let app = router_with_handles(vec![h]);

        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|line| !line.starts_with('#')).collect();
        assert_eq!(sample_lines.len(), 2, "must encode 2 series, got: {body:?}");
        for line in &sample_lines {
            assert!(
                line.starts_with("up"),
                "each sample line must start with metric name 'up', got: {line}"
            );
            assert!(
                !line.chars().last().unwrap().is_ascii_digit() || !line.contains(" 17"),
                "no trailing timestamp expected on aggregate scrape, got: {line}"
            );
        }
    }

    #[tokio::test]
    async fn aggregate_metrics_emits_one_sample_per_series_no_timestamp() {
        let h1 = make_handle_with_labels_and_metrics(
            "agg-no-ts-1",
            "scrape",
            vec![("device", "srl1")],
            vec![make_metric_event("up", 1.0)],
        );
        let h2 = make_handle_with_labels_and_metrics(
            "agg-no-ts-2",
            "scrape",
            vec![("device", "srl2")],
            vec![make_metric_event("up", 2.0)],
        );
        let app = router_with_handles(vec![h1, h2]);

        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(sample_lines.len(), 2);
        for line in &sample_lines {
            assert!(
                line == &"up 1" || line == &"up 2",
                "each sample must be exactly metric+value with no timestamp, got: {line}"
            );
        }
    }

    #[tokio::test]
    async fn aggregate_metrics_idempotent_across_scrapes() {
        let h = make_handle_with_labels_and_metrics(
            "agg-idem",
            "idem",
            vec![],
            vec![make_metric_event("up", 7.0)],
        );
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let app1 = router(state.clone());
        let body_1 = body_string(get_aggregate_req(app1, "").await).await;
        let app2 = router(state.clone());
        let body_2 = body_string(get_aggregate_req(app2, "").await).await;
        assert_eq!(body_1, body_2);
        assert!(!body_1.trim().is_empty());

        cleanup_scenarios(&state);
    }

    #[tokio::test]
    async fn aggregate_and_per_scenario_scrapes_are_both_idempotent() {
        let h = make_handle_with_labels_and_metrics(
            "agg-both",
            "both",
            vec![],
            vec![make_metric_event("up", 1.0)],
        );
        let state = AppState::new();
        {
            let mut map = state.scenarios.write().unwrap();
            map.insert(h.id.clone(), h);
        }

        let agg_app = router(state.clone());
        let agg_body = body_string(get_aggregate_req(agg_app, "").await).await;
        assert!(!agg_body.trim().is_empty());

        let per_app = router(state.clone());
        let per_body = body_string(get_metrics_req(per_app, "agg-both").await).await;
        assert!(!per_body.trim().is_empty());

        let per_app_2 = router(state.clone());
        let per_body_2 = body_string(get_metrics_req(per_app_2, "agg-both").await).await;
        assert_eq!(
            per_body, per_body_2,
            "per-scenario scrape must be idempotent"
        );

        cleanup_scenarios(&state);
    }

    #[tokio::test]
    async fn aggregate_metrics_filter_single_label_match_included() {
        let h1 = make_handle_with_labels_and_metrics(
            "agg-srl1",
            "srl1",
            vec![("device", "srl1")],
            vec![make_metric_event("up", 1.0)],
        );
        let h2 = make_handle_with_labels_and_metrics(
            "agg-srl2",
            "srl2",
            vec![("device", "srl2")],
            vec![make_metric_event("up", 2.0)],
        );
        let app = router_with_handles(vec![h1, h2]);

        let resp = get_aggregate_req(app, "label=device:srl1").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|line| !line.starts_with('#')).collect();
        assert_eq!(
            sample_lines.len(),
            1,
            "filter must include only one event, got: {body:?}"
        );
        assert!(
            sample_lines[0].contains(" 1"),
            "matching event must have value 1, got: {}",
            sample_lines[0]
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_filter_single_label_no_match_excluded() {
        let h1 = make_handle_with_labels_and_metrics(
            "agg-nm-1",
            "nm1",
            vec![("device", "srl1")],
            vec![make_metric_event("up", 1.0)],
        );
        let h2 = make_handle_with_labels_and_metrics(
            "agg-nm-2",
            "nm2",
            vec![("device", "srl2")],
            vec![make_metric_event("up", 2.0)],
        );
        let app = router_with_handles(vec![h1, h2]);

        let resp = get_aggregate_req(app, "label=device:srl3").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        assert!(
            body.trim().is_empty(),
            "no-match filter must return empty body, got: {body:?}"
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_filter_multi_label_and_semantics() {
        let h1 = make_handle_with_labels_and_metrics(
            "agg-ml-1",
            "ml1",
            vec![("device", "srl1"), ("if", "eth0")],
            vec![make_metric_event("up", 10.0)],
        );
        let h2 = make_handle_with_labels_and_metrics(
            "agg-ml-2",
            "ml2",
            vec![("device", "srl1"), ("if", "eth1")],
            vec![make_metric_event("up", 11.0)],
        );
        let h3 = make_handle_with_labels_and_metrics(
            "agg-ml-3",
            "ml3",
            vec![("device", "srl2"), ("if", "eth0")],
            vec![make_metric_event("up", 12.0)],
        );
        let app = router_with_handles(vec![h1, h2, h3]);

        let resp = get_aggregate_req(app, "label=device:srl1&label=if:eth0").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        let sample_lines: Vec<&str> = body.lines().filter(|line| !line.starts_with('#')).collect();
        assert_eq!(
            sample_lines.len(),
            1,
            "multi-label AND must match exactly one handle, got: {body:?}"
        );
        assert!(
            sample_lines[0].contains(" 10"),
            "matching event must have value 10, got: {}",
            sample_lines[0]
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_handle_with_no_labels_never_matches_filter() {
        let h = make_handle_with_labels_and_metrics(
            "agg-nolabels",
            "nolabels",
            vec![],
            vec![make_metric_event("up", 42.0)],
        );
        let app = router_with_handles(vec![h]);

        let resp = get_aggregate_req(app, "label=device:srl1").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_string(resp).await;
        assert!(
            body.trim().is_empty(),
            "unlabelled handle must be excluded by any filter, got: {body:?}"
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_malformed_filter_returns_400() {
        let h = make_handle_with_labels_and_metrics(
            "agg-bad",
            "bad",
            vec![("device", "srl1")],
            vec![make_metric_event("up", 1.0)],
        );

        let app = router_with_handles(vec![h]);
        let resp = get_aggregate_req(app, "label=invalid").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(body["error"].as_str().unwrap(), "bad_request");
        assert!(
            body["detail"].as_str().unwrap().contains("invalid"),
            "detail must reference the bad input, got: {body:?}"
        );

        let h2 = make_handle_with_labels_and_metrics(
            "agg-bad2",
            "bad2",
            vec![("device", "srl1")],
            vec![],
        );
        let app2 = router_with_handles(vec![h2]);
        let resp2 = get_aggregate_req(app2, "label=:value").await;
        assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
        let body2 = body_json(resp2).await;
        assert_eq!(body2["error"].as_str().unwrap(), "bad_request");
    }

    #[tokio::test]
    async fn aggregate_metrics_auth_gate_enforced() {
        let state = AppState::with_api_key(Some("the-key".to_string()));
        let app = router(state);

        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "missing bearer must return 401"
        );

        let req_ok = Request::builder()
            .uri("/metrics")
            .header("authorization", "Bearer the-key")
            .body(Body::empty())
            .unwrap();
        let resp_ok = app.oneshot(req_ok).await.unwrap();
        assert_eq!(
            resp_ok.status(),
            StatusCode::OK,
            "correct bearer must return 200"
        );
    }

    #[test]
    fn parse_label_filters_accepts_repeated_pairs() {
        let filters =
            parse_label_filters(Some("label=device:srl1&label=if:eth0")).expect("must parse");
        assert_eq!(
            filters,
            vec![
                ("device".to_string(), "srl1".to_string()),
                ("if".to_string(), "eth0".to_string()),
            ]
        );
    }

    #[test]
    fn parse_label_filters_value_may_contain_colon() {
        let filters = parse_label_filters(Some("label=addr:::1")).expect("must parse");
        assert_eq!(filters, vec![("addr".to_string(), "::1".to_string())]);
    }

    #[test]
    fn parse_label_filters_rejects_missing_colon() {
        let err = parse_label_filters(Some("label=novalue")).unwrap_err();
        assert!(err.contains("novalue"), "error must reference input: {err}");
    }

    #[test]
    fn parse_label_filters_rejects_empty_key() {
        let err = parse_label_filters(Some("label=:value")).unwrap_err();
        assert!(
            err.contains("empty key"),
            "error must mention empty key: {err}"
        );
    }

    #[test]
    fn parse_label_filters_rejects_empty_value() {
        let err = parse_label_filters(Some("label=key:")).unwrap_err();
        assert!(
            err.contains("empty value"),
            "error must mention empty value: {err}"
        );
    }

    #[test]
    fn parse_label_filters_ignores_non_label_keys() {
        let filters = parse_label_filters(Some("foo=bar&label=device:srl1")).expect("must parse");
        assert_eq!(filters, vec![("device".to_string(), "srl1".to_string())]);
    }

    #[test]
    fn parse_label_filters_empty_query_returns_empty() {
        let filters = parse_label_filters(None).expect("must parse");
        assert!(filters.is_empty());
        let filters2 = parse_label_filters(Some("")).expect("must parse");
        assert!(filters2.is_empty());
    }

    #[test]
    fn parse_include_state_absent_returns_none() {
        assert!(parse_include_state(None).unwrap().is_none());
        assert!(parse_include_state(Some("")).unwrap().is_none());
        assert!(parse_include_state(Some("label=device:srl1"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_include_state_single_value() {
        let states = parse_include_state(Some("include_state=running"))
            .unwrap()
            .expect("filter must be present");
        assert_eq!(states, vec![ScenarioState::Running]);
    }

    #[test]
    fn parse_include_state_comma_separated() {
        let states = parse_include_state(Some("include_state=running,paused"))
            .unwrap()
            .expect("filter must be present");
        assert_eq!(states, vec![ScenarioState::Running, ScenarioState::Paused]);
    }

    #[test]
    fn parse_include_state_trims_whitespace_per_token() {
        let states = parse_include_state(Some("include_state=running,%20paused"))
            .unwrap()
            .expect("filter must be present");
        assert_eq!(states, vec![ScenarioState::Running, ScenarioState::Paused]);
    }

    #[test]
    fn parse_include_state_dedups_duplicates() {
        let states = parse_include_state(Some("include_state=running,running,paused"))
            .unwrap()
            .expect("filter must be present");
        assert_eq!(states, vec![ScenarioState::Running, ScenarioState::Paused]);
    }

    #[test]
    fn parse_include_state_accepts_all_known_variants() {
        let states = parse_include_state(Some(
            "include_state=pending,running,paused,held,unresolved,finished",
        ))
        .unwrap()
        .expect("filter must be present");
        assert_eq!(
            states,
            vec![
                ScenarioState::Pending,
                ScenarioState::Running,
                ScenarioState::Paused,
                ScenarioState::Held,
                ScenarioState::Unresolved,
                ScenarioState::Finished,
            ]
        );
    }

    #[test]
    fn parse_include_state_single_held_value() {
        let states = parse_include_state(Some("include_state=held"))
            .unwrap()
            .expect("filter must be present");
        assert_eq!(states, vec![ScenarioState::Held]);
    }

    #[test]
    fn parse_include_state_empty_value_is_error() {
        let err = parse_include_state(Some("include_state=")).unwrap_err();
        assert!(
            err.contains("at least one state name"),
            "error must explain the empty value: {err}"
        );
    }

    #[test]
    fn parse_include_state_unknown_token_is_error() {
        let err = parse_include_state(Some("include_state=running,foo")).unwrap_err();
        assert!(
            err.contains("'foo'"),
            "error must quote the bad token: {err}"
        );
        assert!(
            err.contains("pending, running, paused, held, unresolved, finished"),
            "error must list valid options: {err}"
        );
    }

    #[test]
    fn parse_include_state_rejects_unknown_sentinel() {
        let err = parse_include_state(Some("include_state=unknown")).unwrap_err();
        assert!(
            err.contains("'unknown'"),
            "the catch-all `unknown` sentinel must be rejected: {err}"
        );
    }

    #[test]
    fn parse_include_state_repeated_param_takes_last() {
        let states = parse_include_state(Some("include_state=running&include_state=paused"))
            .unwrap()
            .expect("filter must be present");
        assert_eq!(states, vec![ScenarioState::Paused]);
    }

    #[test]
    fn parse_include_state_composes_with_other_query_keys() {
        let states =
            parse_include_state(Some("label=env:prod&include_state=running&validate=strict"))
                .unwrap()
                .expect("filter must be present");
        assert_eq!(states, vec![ScenarioState::Running]);
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

    /// Build a ScenarioHandle whose task sleeps for a long time, ignoring cancel.
    fn make_unjoinable_handle(id: &str, name: &str) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let task = tokio::task::spawn(async {
            tokio::time::sleep(Duration::from_secs(300)).await;
            Ok::<_, sonda_core::SondaError>(())
        });

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            50.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(std::collections::HashMap::new()),
            Some(std::sync::Arc::new(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            ))),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    /// Build a ScenarioHandle whose task panics immediately.
    fn make_panicking_handle(id: &str, name: &str) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let task = tokio::task::spawn(async {
            panic!("intentional panic for testing");
            #[allow(unreachable_code)]
            Ok::<_, sonda_core::SondaError>(())
        });

        thread::sleep(Duration::from_millis(50));

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            10.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(std::collections::HashMap::new()),
            Some(std::sync::Arc::new(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            ))),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
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

    /// v2 body for a valid multi-scenario batch with two entries.
    const VALID_MULTI_YAML: &str = "\
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: multi_metric_a
    signal_type: metrics
    name: multi_metric_a
    generator:
      type: constant
      value: 1.0
  - id: multi_metric_b
    signal_type: metrics
    name: multi_metric_b
    generator:
      type: constant
      value: 2.0
";

    /// v2 body for a multi-scenario batch exercising phase_offset.
    ///
    /// Uses a `1ms` offset on the first entry — the v2 compiler rejects
    /// `phase_offset: "0s"` because `parse_duration` disallows zero
    /// durations. A positive `1ms` keeps the test semantically
    /// "phase_offset resolved" without running afoul of that validation.
    const MULTI_YAML_WITH_PHASE_OFFSET: &str = "\
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: offset_a
    signal_type: metrics
    name: offset_a
    phase_offset: \"1ms\"
    generator:
      type: constant
      value: 1.0
  - id: offset_b
    signal_type: metrics
    name: offset_b
    phase_offset: \"50ms\"
    generator:
      type: constant
      value: 2.0
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
            let s = entry["state"].as_str().unwrap_or("");
            assert!(
                matches!(s, "pending" | "running"),
                "scenario[{i}] state must be 'pending' or 'running', got {s:?}"
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

    /// Multi-scenario POST with v2 JSON content type returns 201.
    #[tokio::test]
    async fn post_multi_scenario_json_returns_201() {
        let json_body = serde_json::json!({
            "version": 2,
            "kind": "runnable",
            "defaults": {
                "rate": 10,
                "duration": "200ms",
                "encoder": { "type": "prometheus_text" },
                "sink": { "type": "stdout" }
            },
            "scenarios": [
                {
                    "id": "json_multi_a",
                    "signal_type": "metrics",
                    "name": "json_multi_a",
                    "generator": { "type": "constant", "value": 1.0 }
                },
                {
                    "id": "json_multi_b",
                    "signal_type": "metrics",
                    "name": "json_multi_b",
                    "generator": { "type": "constant", "value": 2.0 }
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

    /// Empty v2 scenarios array returns 400 with a descriptive error.
    #[tokio::test]
    async fn post_multi_scenario_empty_array_returns_400() {
        let yaml = "version: 2\nkind: runnable\nscenarios: []\n";
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
            !body["detail"].as_str().unwrap().is_empty(),
            "400 detail must be non-empty"
        );
    }

    /// Invalid entry in a v2 batch returns 422 and nothing is launched.
    #[tokio::test]
    async fn post_multi_scenario_invalid_entry_returns_422_nothing_launched() {
        let yaml = "\
version: 2
kind: runnable
defaults:
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: valid_entry
    signal_type: metrics
    name: valid_entry
    rate: 10
    generator:
      type: constant
      value: 1.0
  - id: invalid_entry
    signal_type: metrics
    name: invalid_entry
    rate: 0
    generator:
      type: constant
      value: 1.0
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
        // Must have the flat {id, name, state} shape.
        assert!(body["id"].is_string());
        assert_eq!(body["name"], "test_metric");
        let s = body["state"].as_str().unwrap_or("");
        assert!(
            matches!(s, "pending" | "running"),
            "state must be 'pending' or 'running', got {s:?}"
        );

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
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
scenarios:
  - id: mixed_metric
    signal_type: metrics
    name: mixed_metric
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - id: mixed_logs
    signal_type: logs
    name: mixed_logs
    log_generator:
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

    /// `parse_body` returns a multi-entry CompiledFile for a v2 body that
    /// compiles into multiple entries.
    #[test]
    fn parse_body_returns_multi_entry_compiled_for_v2_scenarios_array() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/x-yaml".parse().unwrap());
        let parsed = parse_body(VALID_MULTI_YAML.as_bytes(), &headers, None)
            .expect("v2 multi YAML body must parse");
        let ParsedBody::Compiled(compiled) = parsed;
        assert_eq!(
            compiled.entries.len(),
            2,
            "multi YAML must produce 2 entries"
        );
    }

    /// CreatedScenariosResponse serializes to expected JSON structure.
    #[test]
    fn created_scenarios_response_serializes_correctly() {
        let resp = CreatedScenariosResponse {
            scenarios: vec![
                CreatedScenario {
                    id: "id-1".to_string(),
                    name: "s1".to_string(),
                    state: "running".to_string(),
                    warnings: Vec::new(),
                },
                CreatedScenario {
                    id: "id-2".to_string(),
                    name: "s2".to_string(),
                    state: "running".to_string(),
                    warnings: Vec::new(),
                },
            ],
            warnings: Vec::new(),
        };
        let json = serde_json::to_value(&resp).expect("must serialize");
        let arr = json["scenarios"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "id-1");
        assert_eq!(arr[1]["name"], "s2");
        assert!(
            json.get("warnings").is_none(),
            "empty top-level warnings vec must be omitted from JSON"
        );
    }

    /// Batch response emits a top-level `warnings` array when populated.
    #[test]
    fn created_scenarios_response_serializes_warnings_when_present() {
        let resp = CreatedScenariosResponse {
            scenarios: vec![CreatedScenario {
                id: "id-1".to_string(),
                name: "s1".to_string(),
                state: "running".to_string(),
                warnings: Vec::new(),
            }],
            warnings: vec!["loopback warning".to_string()],
        };
        let json = serde_json::to_value(&resp).expect("must serialize");
        let arr = json["warnings"].as_array().expect("warnings must be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0], "loopback warning");
    }

    // ========================================================================
    // Single-scenario POST parity with multi-scenario path (NOTE 1 fix)
    // ========================================================================

    /// Single-scenario POST with phase_offset returns 201 (verifies the
    /// single-scenario path now uses prepare_entries which resolves phase_offset).
    #[tokio::test]
    async fn post_single_scenario_with_phase_offset_returns_201() {
        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 10
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: single_offset
    signal_type: metrics
    name: single_offset
    phase_offset: \"50ms\"
    generator:
      type: constant
      value: 1.0
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
        let s = body["state"].as_str().unwrap_or("");
        assert!(
            matches!(s, "pending" | "running"),
            "state must be 'pending' or 'running', got {s:?}"
        );

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

    // ---- insta snapshots: response field shape lock-in ----------------------

    fn snapshot_settings() -> insta::Settings {
        let mut s = insta::Settings::clone_current();
        s.set_sort_maps(true);
        s.add_filter(r#"(?m)^\s+"[^"]+": null,\n"#, "");
        s.add_filter(r#",\n(\s+"[^"]+": null\n)"#, "\n");
        s
    }

    #[test]
    fn detailed_stats_response_json_snapshot_locks_field_shape() {
        let resp = DetailedStatsResponse {
            total_events: 1234,
            current_rate: 42.5,
            target_rate: 100.0,
            bytes_emitted: 567_890,
            errors: 3,
            uptime_secs: 12.5,
            state: "running".to_string(),
            in_gap: false,
            in_burst: true,
            consecutive_failures: 2,
            total_sink_failures: 7,
            last_sink_error: Some("connection refused".to_string()),
            last_successful_write_at: Some(1_700_000_000_000_000_000),
            degraded: true,
            current_state_secs: 5.25,
            cumulative_resolution_attempts: 2,
        };
        snapshot_settings().bind(|| {
            insta::assert_json_snapshot!("detailed_stats_response", resp);
        });
    }

    #[rstest::rstest]
    #[case::pending(ScenarioState::Pending, "pending")]
    #[case::running(ScenarioState::Running, "running")]
    #[case::paused(ScenarioState::Paused, "paused")]
    #[case::finished(ScenarioState::Finished, "finished")]
    fn detailed_stats_response_state_snapshot(
        #[case] state: ScenarioState,
        #[case] wire: &'static str,
    ) {
        let mut snap = ScenarioStats::default();
        snap.total_events = 100;
        snap.current_rate = if state == ScenarioState::Paused {
            0.0
        } else {
            10.0
        };
        snap.bytes_emitted = 4096;
        snap.state = state;
        let resp = DetailedStatsResponse {
            total_events: snap.total_events,
            current_rate: snap.current_rate,
            target_rate: 10.0,
            bytes_emitted: snap.bytes_emitted,
            errors: 0,
            uptime_secs: 5.0,
            state: state_string(&snap).to_string(),
            in_gap: false,
            in_burst: false,
            consecutive_failures: 0,
            total_sink_failures: 0,
            last_sink_error: None,
            last_successful_write_at: None,
            degraded: false,
            current_state_secs: 0.0,
            cumulative_resolution_attempts: 0,
        };
        assert_eq!(resp.state, wire);
        snapshot_settings().bind(|| {
            insta::assert_json_snapshot!(
                format!("detailed_stats_response_state_{wire}"),
                resp,
                {
                    ".uptime_secs" => "[uptime_secs]",
                }
            );
        });
    }

    // Sink loopback pre-flight tests (helpers + cases) live in the
    // sibling `sink_warnings` module.

    // =====================================================================
    // TYPE / HELP exposition tests (per-scenario and aggregate handlers)
    // =====================================================================

    fn handle_with_meta(
        id: &str,
        name: &str,
        meta: Option<sonda_core::PromMeta>,
        events: Vec<sonda_core::model::metric::MetricEvent>,
    ) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let mut stats = ScenarioStats::default();
        for event in events {
            stats.push_metric(event);
        }
        let stats = Arc::new(RwLock::new(stats));
        let cancel_clone = cancel.clone();
        let task = tokio::task::spawn(async move {
            cancel_clone.cancelled().await;
            Ok::<_, sonda_core::SondaError>(())
        });
        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            10.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(std::collections::HashMap::new()),
            meta.map(std::sync::Arc::new),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    fn metric_event(name: &str, value: f64) -> sonda_core::model::metric::MetricEvent {
        sonda_core::model::metric::MetricEvent::new(
            name.to_string(),
            value,
            sonda_core::model::metric::Labels::default(),
        )
        .expect("metric name must be valid")
    }

    #[tokio::test]
    async fn per_scenario_metrics_emits_type_line() {
        let meta = sonda_core::PromMeta::new(sonda_core::PromMetricType::Gauge, None);
        let h = handle_with_meta(
            "id-type",
            "memory_utilization",
            Some(meta),
            vec![metric_event("memory_utilization", 41.5)],
        );
        let app = router_with_handles(vec![h]);
        let resp = get_metrics_req(app, "id-type").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.starts_with("# TYPE memory_utilization gauge\n"),
            "body must begin with TYPE line, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn per_scenario_metrics_emits_help_line_when_set() {
        let meta = sonda_core::PromMeta::new(
            sonda_core::PromMetricType::Gauge,
            Some("memory util".to_string()),
        );
        let h = handle_with_meta(
            "id-help",
            "memory_utilization",
            Some(meta),
            vec![metric_event("memory_utilization", 41.5)],
        );
        let app = router_with_handles(vec![h]);
        let resp = get_metrics_req(app, "id-help").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.starts_with(
                "# HELP memory_utilization memory util\n# TYPE memory_utilization gauge\n"
            ),
            "body must begin with HELP and TYPE lines in order, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn per_scenario_metrics_omits_help_when_unset() {
        let meta = sonda_core::PromMeta::new(sonda_core::PromMetricType::Gauge, None);
        let h = handle_with_meta(
            "id-no-help",
            "memory_utilization",
            Some(meta),
            vec![metric_event("memory_utilization", 1.0)],
        );
        let app = router_with_handles(vec![h]);
        let resp = get_metrics_req(app, "id-no-help").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            !body.contains("# HELP"),
            "body must not contain a HELP line, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn per_scenario_metrics_log_scenario_returns_empty() {
        let h = handle_with_meta("id-log", "log_scenario", None, vec![]);
        let app = router_with_handles(vec![h]);
        let resp = get_metrics_req(app, "id-log").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.is_empty(),
            "log scenario (no prometheus_meta) must return empty body, got:\n{body}"
        );
    }

    fn handle_with_meta_and_labels(
        id: &str,
        name: &str,
        meta: Option<sonda_core::PromMeta>,
        labels: Vec<(&str, &str)>,
        events: Vec<sonda_core::model::metric::MetricEvent>,
    ) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let mut stats = ScenarioStats::default();
        for event in events {
            stats.push_metric(event);
        }
        let stats = Arc::new(RwLock::new(stats));
        let cancel_clone = cancel.clone();
        let task = tokio::task::spawn(async move {
            cancel_clone.cancelled().await;
            Ok::<_, sonda_core::SondaError>(())
        });
        let mut label_map = std::collections::HashMap::new();
        for (k, v) in labels {
            label_map.insert(k.to_string(), v.to_string());
        }
        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            10.0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            std::sync::Arc::new(label_map),
            meta.map(std::sync::Arc::new),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    #[tokio::test]
    async fn aggregate_metrics_emits_one_type_block_per_name() {
        let meta = sonda_core::PromMeta::new(sonda_core::PromMetricType::Gauge, None);
        let h1 = handle_with_meta_and_labels(
            "a-1",
            "shared_metric",
            Some(meta.clone()),
            vec![("device", "srl1")],
            vec![metric_event("shared_metric", 1.0)],
        );
        let h2 = handle_with_meta_and_labels(
            "a-2",
            "shared_metric",
            Some(meta),
            vec![("device", "srl2")],
            vec![metric_event("shared_metric", 2.0)],
        );
        let app = router_with_handles(vec![h1, h2]);
        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        let type_lines: Vec<&str> = body
            .lines()
            .filter(|l| l.starts_with("# TYPE shared_metric"))
            .collect();
        assert_eq!(
            type_lines.len(),
            1,
            "aggregate body must have exactly one TYPE line for shared_metric, got:\n{body}"
        );
        let sample_lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            sample_lines.len(),
            2,
            "aggregate body must contain both samples, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_groups_samples_by_name() {
        let meta = sonda_core::PromMeta::new(sonda_core::PromMetricType::Gauge, None);
        let h1 = handle_with_meta_and_labels(
            "g-1",
            "m1",
            Some(meta.clone()),
            vec![],
            vec![metric_event("m1", 1.0)],
        );
        let h2 = handle_with_meta_and_labels(
            "g-2",
            "m1",
            Some(meta.clone()),
            vec![],
            vec![metric_event("m1", 2.0)],
        );
        let h3 = handle_with_meta_and_labels(
            "g-3",
            "m2",
            Some(meta),
            vec![],
            vec![metric_event("m2", 3.0)],
        );
        let app = router_with_handles(vec![h1, h2, h3]);
        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        let m1_type_pos = body
            .find("# TYPE m1 gauge")
            .expect("TYPE m1 line must be present");
        let m2_type_pos = body
            .find("# TYPE m2 gauge")
            .expect("TYPE m2 line must be present");
        assert!(
            m1_type_pos < m2_type_pos,
            "m1 group must precede m2 group, got:\n{body}"
        );
        let between = &body[m1_type_pos..m2_type_pos];
        assert!(
            between.contains("m1 1") && between.contains("m1 2"),
            "samples for m1 must appear between m1 and m2 TYPE lines, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_mixed_type_collision_emits_untyped_and_warns() {
        let h1 = handle_with_meta_and_labels(
            "mix-1",
            "collision",
            Some(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Gauge,
                None,
            )),
            vec![],
            vec![metric_event("collision", 1.0)],
        );
        let h2 = handle_with_meta_and_labels(
            "mix-2",
            "collision",
            Some(sonda_core::PromMeta::new(
                sonda_core::PromMetricType::Counter,
                None,
            )),
            vec![],
            vec![metric_event("collision", 2.0)],
        );
        let app = router_with_handles(vec![h1, h2]);
        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.contains("# TYPE collision untyped"),
            "mixed-type collision must emit untyped, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_label_filter_still_works_with_type_lines() {
        let meta = sonda_core::PromMeta::new(sonda_core::PromMetricType::Gauge, None);
        let h1 = handle_with_meta_and_labels(
            "lf-1",
            "srl_metric",
            Some(meta.clone()),
            vec![("device", "srl1")],
            vec![metric_event("srl_metric", 1.0)],
        );
        let h2 = handle_with_meta_and_labels(
            "lf-2",
            "srl_metric",
            Some(meta),
            vec![("device", "srl2")],
            vec![metric_event("srl_metric", 2.0)],
        );
        let app = router_with_handles(vec![h1, h2]);
        let resp = get_aggregate_req(app, "label=device:srl1").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.contains("# TYPE srl_metric gauge"),
            "filter result must still emit TYPE line, got:\n{body}"
        );
        let sample_lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            sample_lines.len(),
            1,
            "filter must include only one sample, got:\n{body}"
        );
        assert!(
            sample_lines[0].contains(" 1"),
            "matching sample must have value 1, got: {}",
            sample_lines[0]
        );
    }

    fn labeled_metric_event(
        name: &str,
        value: f64,
        pairs: &[(&str, &str)],
    ) -> sonda_core::model::metric::MetricEvent {
        let labels =
            sonda_core::model::metric::Labels::from_pairs(pairs).expect("labels must build");
        sonda_core::model::metric::MetricEvent::new(name.to_string(), value, labels)
            .expect("metric name must be valid")
    }

    #[tokio::test]
    async fn aggregate_metrics_histogram_scenario_exposes_buckets_sum_and_count() {
        let events = vec![
            labeled_metric_event("req_latency_bucket", 12.0, &[("le", "0.1")]),
            labeled_metric_event("req_latency_bucket", 24.0, &[("le", "1")]),
            labeled_metric_event("req_latency_bucket", 50.0, &[("le", "+Inf")]),
            labeled_metric_event("req_latency_sum", 7.5, &[]),
            labeled_metric_event("req_latency_count", 50.0, &[]),
        ];

        let meta = sonda_core::PromMeta::new(sonda_core::PromMetricType::Histogram, None);
        let h = handle_with_meta("hist", "req_latency", Some(meta), events);
        let app = router_with_handles(vec![h]);
        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.contains("# TYPE req_latency histogram"),
            "expected single histogram TYPE on base name, got:\n{body}"
        );
        assert_eq!(
            body.matches("# TYPE req_latency ").count(),
            1,
            "expected exactly one TYPE line for the base name, got:\n{body}"
        );
        assert!(
            body.contains("req_latency_bucket{le=\"0.1\"}") && body.contains("le=\"+Inf\""),
            "expected bucket samples including +Inf, got:\n{body}"
        );
        assert!(
            body.contains("req_latency_sum"),
            "expected _sum series, got:\n{body}"
        );
        assert!(
            body.contains("req_latency_count"),
            "expected _count series, got:\n{body}"
        );
    }

    #[tokio::test]
    async fn aggregate_metrics_summary_scenario_exposes_quantiles_sum_and_count() {
        let events = vec![
            labeled_metric_event("rpc_duration", 0.05, &[("quantile", "0.5")]),
            labeled_metric_event("rpc_duration", 0.09, &[("quantile", "0.9")]),
            labeled_metric_event("rpc_duration", 0.18, &[("quantile", "0.99")]),
            labeled_metric_event("rpc_duration_sum", 6.0, &[]),
            labeled_metric_event("rpc_duration_count", 50.0, &[]),
        ];

        let meta = sonda_core::PromMeta::new(sonda_core::PromMetricType::Summary, None);
        let h = handle_with_meta("summ", "rpc_duration", Some(meta), events);
        let app = router_with_handles(vec![h]);
        let resp = get_aggregate_req(app, "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.contains("# TYPE rpc_duration summary"),
            "expected single summary TYPE on base name, got:\n{body}"
        );
        assert_eq!(
            body.matches("# TYPE rpc_duration ").count(),
            1,
            "expected exactly one TYPE line for the base name, got:\n{body}"
        );
        assert!(
            body.contains("quantile=\"0.5\"") && body.contains("quantile=\"0.99\""),
            "expected quantile samples, got:\n{body}"
        );
        assert!(
            body.contains("rpc_duration_sum"),
            "expected _sum series, got:\n{body}"
        );
        assert!(
            body.contains("rpc_duration_count"),
            "expected _count series, got:\n{body}"
        );
    }

    // ========================================================================
    // Brief 3 — cross-POST while: HTTP surface
    // ========================================================================

    const DOWNSTREAM_YAML: &str = "\
version: 2
kind: runnable
scenario_name: downstream_post
defaults:
  rate: 50
  duration: 5s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: dependent
    signal_type: metrics
    name: dependent
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream_metric
      op: \">\"
      value: 0
      scenario_name: upstream_post
      if_unresolved: pending
";

    const UPSTREAM_YAML: &str = "\
version: 2
kind: runnable
scenario_name: upstream_post
defaults:
  rate: 50
  duration: 5s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream_metric
    signal_type: metrics
    name: upstream_metric
    generator:
      type: constant
      value: 1.0
";

    async fn post_with_query(
        app: axum::Router,
        content_type: &str,
        body: &str,
        query: &str,
    ) -> hyper::Response<axum::body::Body> {
        let uri = if query.is_empty() {
            "/scenarios".to_string()
        } else {
            format!("/scenarios?{query}")
        };
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", content_type)
            .body(Body::from(body.to_string()))
            .unwrap();
        app.oneshot(req).await.unwrap()
    }

    async fn get_body(app: axum::Router, uri: &str) -> serde_json::Value {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        body_json(resp).await
    }

    #[tokio::test]
    async fn t14_validate_strict_with_typo_returns_422_and_unresolved_refs() {
        let (_, state) = test_router();
        let bad = DOWNSTREAM_YAML.replace("upstream_post", "totally_wrong");
        let resp = post_with_query(
            router(state.clone()),
            "application/x-yaml",
            &bad,
            "validate=strict",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "unresolved_refs");
        let unresolved = body["unresolved_refs"]
            .as_array()
            .expect("unresolved_refs must be array");
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0]["scenario_name"], "totally_wrong");
        cleanup_scenarios(&state);
    }

    #[tokio::test]
    async fn t15_validate_strict_with_mixed_refs_rejects_whole_body() {
        let (_, state) = test_router();
        let mixed = "\
version: 2
kind: runnable
scenario_name: mixed_post
defaults:
  rate: 50
  duration: 5s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: resolvable_dep
    signal_type: metrics
    name: resolvable_dep
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream_metric
      op: \">\"
      value: 0
      scenario_name: upstream_post
      if_unresolved: pending
  - id: unresolvable_dep
    signal_type: metrics
    name: unresolvable_dep
    generator:
      type: constant
      value: 1.0
    while:
      ref: missing_ref
      op: \">\"
      value: 0
      scenario_name: missing_post
      if_unresolved: pending
";
        // Post upstream so one ref resolves.
        let upstream_resp = post_with_query(
            router(state.clone()),
            "application/x-yaml",
            UPSTREAM_YAML,
            "",
        )
        .await;
        assert_eq!(upstream_resp.status(), StatusCode::CREATED);

        let resp = post_with_query(
            router(state.clone()),
            "application/x-yaml",
            mixed,
            "validate=strict",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(resp).await;
        let unresolved = body["unresolved_refs"].as_array().unwrap();
        assert_eq!(unresolved.len(), 1, "only one entry must be flagged");
        assert_eq!(unresolved[0]["scenario_name"], "missing_post");
        cleanup_scenarios(&state);
    }

    #[tokio::test]
    async fn t16_validate_strict_with_all_resolvable_returns_201() {
        let (_, state) = test_router();
        let up_resp = post_with_query(router(state.clone()), "text/yaml", UPSTREAM_YAML, "").await;
        assert_eq!(up_resp.status(), StatusCode::CREATED);

        let resp = post_with_query(
            router(state.clone()),
            "text/yaml",
            DOWNSTREAM_YAML,
            "validate=strict",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        cleanup_scenarios(&state);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t18_get_scenario_for_unresolved_returns_pending_ref() {
        let (_, state) = test_router();
        let resp = post_with_query(
            router(state.clone()),
            "application/x-yaml",
            DOWNSTREAM_YAML,
            "",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let created = body_json(resp).await;
        let id = created["id"].as_str().unwrap().to_string();

        // Give the scenario a moment to reach Unresolved.
        std::thread::sleep(Duration::from_millis(200));

        let detail = get_body(router(state.clone()), &format!("/scenarios/{id}")).await;
        assert_eq!(detail["state"], "unresolved");
        assert!(
            detail["pending_ref"].is_object(),
            "pending_ref must be populated for Unresolved, got: {detail}",
        );
        assert_eq!(detail["pending_ref"]["scenario_name"], "upstream_post");
        assert_eq!(detail["pending_ref"]["entry_id"], "upstream_metric");

        // Now post upstream and verify pending_ref clears once Running.
        let up = post_with_query(router(state.clone()), "text/yaml", UPSTREAM_YAML, "").await;
        assert_eq!(up.status(), StatusCode::CREATED);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_running = false;
        while Instant::now() < deadline {
            let detail = get_body(router(state.clone()), &format!("/scenarios/{id}")).await;
            if detail["state"] == "running" {
                saw_running = true;
                assert!(
                    detail["pending_ref"].is_null() || detail.get("pending_ref").is_none(),
                    "pending_ref must be null/absent for non-Unresolved",
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            saw_running,
            "downstream must reach Running after upstream POST"
        );
        cleanup_scenarios(&state);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t19_stats_endpoint_has_current_state_secs_and_cumulative_attempts() {
        let (_, state) = test_router();
        let resp = post_with_query(
            router(state.clone()),
            "application/x-yaml",
            DOWNSTREAM_YAML,
            "",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let created = body_json(resp).await;
        let id = created["id"].as_str().unwrap().to_string();

        std::thread::sleep(Duration::from_millis(300));
        let stats = get_body(router(state.clone()), &format!("/scenarios/{id}/stats")).await;
        assert!(stats["current_state_secs"].is_f64());
        let secs_unresolved = stats["current_state_secs"].as_f64().unwrap();
        assert!(secs_unresolved > 0.0);
        let attempts_before = stats["cumulative_resolution_attempts"].as_u64().unwrap();

        // Trigger a state transition by resolving the upstream.
        let up = post_with_query(router(state.clone()), "text/yaml", UPSTREAM_YAML, "").await;
        assert_eq!(up.status(), StatusCode::CREATED);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_running = false;
        let mut attempts_after = 0u64;
        while Instant::now() < deadline {
            let s = get_body(router(state.clone()), &format!("/scenarios/{id}/stats")).await;
            if s["state"] == "running" {
                // current_state_secs reset on transition.
                let s2 = s["current_state_secs"].as_f64().unwrap();
                assert!(
                    s2 < secs_unresolved,
                    "current_state_secs must reset on transition; was {secs_unresolved}, now {s2}",
                );
                attempts_after = s["cumulative_resolution_attempts"].as_u64().unwrap();
                saw_running = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            saw_running,
            "downstream must reach Running after upstream POST"
        );
        assert!(
            attempts_after > attempts_before,
            "cumulative_resolution_attempts must strictly increase across unresolved→running; was {attempts_before}, now {attempts_after}",
        );
        assert!(
            attempts_after >= 1,
            "the resolving sweep counts as at least one attempt, got {attempts_after}",
        );
        cleanup_scenarios(&state);
    }

    #[tokio::test]
    async fn t20_duplicate_scenario_name_returns_409_via_registry() {
        let (_, state) = test_router();
        let first = post_with_query(router(state.clone()), "text/yaml", UPSTREAM_YAML, "").await;
        assert_eq!(first.status(), StatusCode::CREATED);

        let second = post_with_query(router(state.clone()), "text/yaml", UPSTREAM_YAML, "").await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
        let body = body_json(second).await;
        assert!(body["conflicting_scenarios"].is_array());
        cleanup_scenarios(&state);
    }
}
