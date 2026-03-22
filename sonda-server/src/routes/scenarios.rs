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

    /// Helper to parse a response body as serde_json::Value.
    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).expect("body must be valid JSON")
    }

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
}
