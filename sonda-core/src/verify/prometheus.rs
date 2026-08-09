//! Prometheus-compatible acquisition for alert-state polling.
//!
//! Two acquisition paths against the built-in `ALERTS` metric, which every
//! Prometheus-compatible evaluator (Prometheus, VictoriaMetrics/vmalert)
//! exposes with an `alertstate` label of `pending` or `firing` while a rule
//! is active:
//!
//! - **Instant queries** (`/api/v1/query`) drive *live* polling — deciding
//!   when a check has settled and can stop waiting.
//! - **Range queries** (`/api/v1/query_range`) produce the *verdict*
//!   timeline after the fact: the actual samples the evaluator wrote, at
//!   the rule-evaluation cadence, free of the caller's poll-interval
//!   quantization and of latency introduced by polling other expectations.
//!
//! This module only *acquires* state — deadline decisions belong to
//! [`crate::verify::evaluator`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use crate::verify::evaluator::Observation;
use crate::verify::{AlertExpectation, AlertState};
use crate::{SondaError, VerifyError};

/// Minimal blocking client for the Prometheus instant-query API.
///
/// Every request carries an overall timeout — a stalled endpoint returns a
/// [`VerifyError::Query`] instead of parking the calling thread forever,
/// which keeps polling loops bounded and interruptible.
pub struct PrometheusClient {
    query_url: String,
    range_url: String,
    agent: ureq::Agent,
}

/// The reconstructed history of one expectation over a queried window.
#[derive(Debug, Clone)]
pub struct RangeTimeline {
    /// One observation per grid step, anchored on the caller's anchor
    /// timestamp. A step with no matching sample is `Inactive`.
    pub observations: Vec<Observation>,
    /// Label sets of every distinct firing series seen in the window —
    /// input for [`crate::verify::foreign_label_values`] provenance checks.
    pub firing_series: Vec<BTreeMap<String, String>>,
}

impl PrometheusClient {
    /// Create a client for a Prometheus-compatible base URL
    /// (e.g. `http://localhost:9090`), with an overall per-request timeout
    /// covering connect, write, and read.
    pub fn new(base_url: &str, timeout: Duration) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            query_url: format!("{base}/api/v1/query"),
            range_url: format!("{base}/api/v1/query_range"),
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
        }
    }

    /// Query the current state of an expected alert.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Verify`] when the endpoint is unreachable,
    /// times out, or responds with an unusable payload — callers polling in
    /// a loop treat these as retryable until their deadline.
    pub fn alert_state(&self, expectation: &AlertExpectation) -> Result<AlertState, SondaError> {
        let query = alerts_query(expectation);
        let body = self
            .agent
            .get(&self.query_url)
            .query("query", &query)
            .call()
            .map_err(|e| {
                SondaError::Verify(VerifyError::Query {
                    url: self.query_url.clone(),
                    reason: e.to_string(),
                })
            })?
            .into_string()
            .map_err(|e| {
                SondaError::Verify(VerifyError::BadResponse {
                    url: self.query_url.clone(),
                    reason: format!("response could not be read: {e}"),
                })
            })?;
        parse_alert_state(&body).map_err(|reason| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.query_url.clone(),
                reason,
            })
        })
    }

    /// Reconstruct an expectation's state history over `[start, end]` (unix
    /// seconds) at `step` resolution, with observation timestamps measured
    /// from `anchor` (unix seconds — scenario start for firing checks,
    /// scenario end for resolution checks).
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Verify`] when the endpoint is unreachable,
    /// times out, or responds with an unusable payload.
    pub fn range_timeline(
        &self,
        expectation: &AlertExpectation,
        start: f64,
        end: f64,
        step: Duration,
        anchor: f64,
    ) -> Result<RangeTimeline, SondaError> {
        let query = alerts_query(expectation);
        let body = self
            .agent
            .get(&self.range_url)
            .query("query", &query)
            .query("start", &format!("{start:.3}"))
            .query("end", &format!("{end:.3}"))
            .query("step", &format!("{:.3}", step.as_secs_f64()))
            .call()
            .map_err(|e| {
                SondaError::Verify(VerifyError::Query {
                    url: self.range_url.clone(),
                    reason: e.to_string(),
                })
            })?
            .into_string()
            .map_err(|e| {
                SondaError::Verify(VerifyError::BadResponse {
                    url: self.range_url.clone(),
                    reason: format!("response could not be read: {e}"),
                })
            })?;
        parse_range_timeline(&body, start, end, step.as_secs_f64(), anchor).map_err(|reason| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.range_url.clone(),
                reason,
            })
        })
    }
}

/// Parse a range-query response into a [`RangeTimeline`].
///
/// The grid runs `start..=end` at `step` (all unix seconds). Prometheus
/// aligns matrix samples to the requested grid, so states are keyed by
/// millisecond-rounded timestamps; a grid point with no matching sample is
/// `Inactive` — the rule produced no series there (with staleness markers,
/// exactly what an instant query at that moment would have said). Steps at
/// or before `anchor` clamp to zero.
pub fn parse_range_timeline(
    body: &str,
    start: f64,
    end: f64,
    step_secs: f64,
    anchor: f64,
) -> Result<RangeTimeline, String> {
    if !step_secs.is_finite() || step_secs <= 0.0 {
        return Err(format!("step must be positive, got {step_secs}"));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;
    if parsed["status"] != "success" {
        return Err(format!("query returned status {:?}", parsed["status"]));
    }
    let result_type = &parsed["data"]["resultType"];
    if result_type != "matrix" {
        return Err(format!(
            "expected resultType \"matrix\", got {result_type:?}"
        ));
    }
    let empty = Vec::new();
    let series_list = parsed["data"]["result"].as_array().unwrap_or(&empty);

    let mut firing_at: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    let mut pending_at: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    let mut firing_series: Vec<BTreeMap<String, String>> = Vec::new();
    for series in series_list {
        let firing = series["metric"]["alertstate"] == "firing";
        if firing {
            let labels: BTreeMap<String, String> = series["metric"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            if !firing_series.contains(&labels) {
                firing_series.push(labels);
            }
        }
        for value in series["values"].as_array().unwrap_or(&empty) {
            let Some(ts) = value[0].as_f64() else {
                continue;
            };
            let key = (ts * 1000.0).round() as i64;
            if firing {
                firing_at.insert(key);
            } else {
                pending_at.insert(key);
            }
        }
    }

    let mut observations = Vec::new();
    let steps = ((end - start) / step_secs).floor() as u64;
    for index in 0..=steps {
        let ts = start + index as f64 * step_secs;
        let key = (ts * 1000.0).round() as i64;
        let state = if firing_at.contains(&key) {
            AlertState::Firing
        } else if pending_at.contains(&key) {
            AlertState::Pending
        } else {
            AlertState::Inactive
        };
        let at = Duration::from_secs_f64((ts - anchor).max(0.0));
        observations.push(Observation::new(at, Ok(state)));
    }
    Ok(RangeTimeline {
        observations,
        firing_series,
    })
}

/// Build the `ALERTS{...}` selector for an expectation.
///
/// Matches on `alertname` plus any extra label matchers. `alertstate` is
/// intentionally not constrained — the response distinguishes pending from
/// firing so one query answers both.
pub fn alerts_query(expectation: &AlertExpectation) -> String {
    let mut selector = String::from("ALERTS{");
    let _ = write!(
        selector,
        "alertname=\"{}\"",
        escape_label_value(&expectation.alert)
    );
    if let Some(labels) = &expectation.labels {
        for (key, value) in labels {
            let _ = write!(selector, ",{key}=\"{}\"", escape_label_value(value));
        }
    }
    selector.push('}');
    selector
}

/// Escape a PromQL label-matcher value (backslash, quote, newline).
fn escape_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Parse an instant-query response body into an [`AlertState`].
///
/// Returns a human-readable reason on failure; the caller wraps it with the
/// query URL into a [`VerifyError::BadResponse`].
pub fn parse_alert_state(body: &str) -> Result<AlertState, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;
    if parsed["status"] != "success" {
        return Err(format!("query returned status {:?}", parsed["status"]));
    }
    let empty = Vec::new();
    let results = parsed["data"]["result"].as_array().unwrap_or(&empty);
    if results.is_empty() {
        return Ok(AlertState::Inactive);
    }
    let firing = results
        .iter()
        .any(|series| series["metric"]["alertstate"] == "firing");
    Ok(if firing {
        AlertState::Firing
    } else {
        AlertState::Pending
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn expectation(labels: Option<BTreeMap<String, String>>) -> AlertExpectation {
        AlertExpectation {
            alert: "HighCpuUsage".into(),
            labels,
            firing_within: "1m".into(),
            resolves_within: None,
        }
    }

    #[test]
    fn query_matches_alertname_only() {
        assert_eq!(
            alerts_query(&expectation(None)),
            "ALERTS{alertname=\"HighCpuUsage\"}"
        );
    }

    #[test]
    fn query_includes_extra_label_matchers() {
        let labels = BTreeMap::from([
            ("severity".to_string(), "critical".to_string()),
            ("team".to_string(), "net\"ops".to_string()),
        ]);
        assert_eq!(
            alerts_query(&expectation(Some(labels))),
            "ALERTS{alertname=\"HighCpuUsage\",severity=\"critical\",team=\"net\\\"ops\"}"
        );
    }

    #[test]
    fn empty_result_is_inactive() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        assert_eq!(parse_alert_state(body).expect("ok"), AlertState::Inactive);
    }

    #[test]
    fn pending_only_is_pending() {
        let body = r#"{"status":"success","data":{"result":[
            {"metric":{"alertname":"HighCpuUsage","alertstate":"pending"},"value":[1,"1"]}
        ]}}"#;
        assert_eq!(parse_alert_state(body).expect("ok"), AlertState::Pending);
    }

    #[test]
    fn any_firing_series_is_firing() {
        let body = r#"{"status":"success","data":{"result":[
            {"metric":{"alertname":"HighCpuUsage","alertstate":"pending"},"value":[1,"1"]},
            {"metric":{"alertname":"HighCpuUsage","alertstate":"firing"},"value":[1,"1"]}
        ]}}"#;
        assert_eq!(parse_alert_state(body).expect("ok"), AlertState::Firing);
    }

    #[test]
    fn error_status_is_rejected() {
        let body = r#"{"status":"error","errorType":"bad_data","error":"boom"}"#;
        assert!(parse_alert_state(body).is_err());
    }

    fn states(timeline: &RangeTimeline) -> Vec<AlertState> {
        timeline
            .observations
            .iter()
            .map(|o| *o.state.as_ref().expect("range states are never errors"))
            .collect()
    }

    #[test]
    fn range_grid_reconstructs_the_full_lifecycle() {
        // Grid 100..=120 step 5: inactive, pending, firing, firing, gone.
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[
            {"metric":{"alertname":"HighCpuUsage","alertstate":"pending","host":"sonda-test"},
             "values":[[105,"1"]]},
            {"metric":{"alertname":"HighCpuUsage","alertstate":"firing","host":"sonda-test"},
             "values":[[110,"1"],[115,"1"]]}
        ]}}"#;
        let timeline = parse_range_timeline(body, 100.0, 120.0, 5.0, 100.0).expect("parse");
        assert_eq!(
            states(&timeline),
            vec![
                AlertState::Inactive,
                AlertState::Pending,
                AlertState::Firing,
                AlertState::Firing,
                AlertState::Inactive,
            ]
        );
        // Observations are anchored: first at 0s, last at 20s.
        assert_eq!(timeline.observations[0].at, Duration::from_secs(0));
        assert_eq!(timeline.observations[4].at, Duration::from_secs(20));
        assert_eq!(timeline.firing_series.len(), 1);
        assert_eq!(
            timeline.firing_series[0].get("host").map(String::as_str),
            Some("sonda-test")
        );
    }

    #[test]
    fn range_transition_time_is_the_sample_time_not_a_poll_time() {
        // The whole point of range acquisition: the firing observation's
        // timestamp comes from the stored sample (anchor+5s), regardless of
        // when or how often anyone polled.
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[
            {"metric":{"alertname":"X","alertstate":"firing"},"values":[[1005,"1"],[1010,"1"]]}
        ]}}"#;
        let timeline = parse_range_timeline(body, 1000.0, 1010.0, 5.0, 1000.0).expect("parse");
        let first_firing = timeline
            .observations
            .iter()
            .find(|o| matches!(o.state, Ok(AlertState::Firing)))
            .expect("firing observed");
        assert_eq!(first_firing.at, Duration::from_secs(5));
    }

    #[test]
    fn range_empty_result_is_all_inactive_with_full_coverage() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
        let timeline = parse_range_timeline(body, 0.0, 10.0, 5.0, 0.0).expect("parse");
        assert_eq!(
            states(&timeline),
            vec![
                AlertState::Inactive,
                AlertState::Inactive,
                AlertState::Inactive
            ]
        );
        assert!(timeline.firing_series.is_empty());
    }

    #[test]
    fn range_collects_distinct_firing_series_labels() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[
            {"metric":{"alertname":"X","alertstate":"firing","host":"a"},"values":[[5,"1"]]},
            {"metric":{"alertname":"X","alertstate":"firing","host":"b"},"values":[[5,"1"]]},
            {"metric":{"alertname":"X","alertstate":"firing","host":"a"},"values":[[10,"1"]]}
        ]}}"#;
        let timeline = parse_range_timeline(body, 0.0, 10.0, 5.0, 0.0).expect("parse");
        assert_eq!(timeline.firing_series.len(), 2);
    }

    #[test]
    fn range_vector_result_is_rejected() {
        // An instant-query response fed to the range parser is a bad
        // payload, not an empty timeline.
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        let err = parse_range_timeline(body, 0.0, 10.0, 5.0, 0.0).expect_err("must reject");
        assert!(err.contains("matrix"), "{err}");
    }

    #[test]
    fn range_zero_step_is_rejected() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
        assert!(parse_range_timeline(body, 0.0, 10.0, 0.0, 0.0).is_err());
    }

    #[test]
    fn stalled_endpoint_times_out_with_verify_error() {
        // A listener that accepts but never answers must produce a bounded
        // Verify::Query error, not a parked thread (blocker 2).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _keep_alive = std::thread::spawn(move || {
            let _streams: Vec<_> = listener.incoming().take(1).collect();
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        let client = PrometheusClient::new(
            &format!("http://{addr}"),
            std::time::Duration::from_millis(300),
        );
        let started = std::time::Instant::now();
        let result = client.alert_state(&expectation(None));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        match result {
            Err(SondaError::Verify(VerifyError::Query { .. })) => {}
            other => panic!("expected Verify::Query error, got {other:?}"),
        }
    }
}
