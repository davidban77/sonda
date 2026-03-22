//! Scenario listing and inspection endpoints.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use sonda_core::ScenarioStats;

use crate::state::AppState;

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

/// Derive the status string from whether the scenario handle is still running.
fn status_string(running: bool) -> String {
    if running {
        "running".to_string()
    } else {
        "stopped".to_string()
    }
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
