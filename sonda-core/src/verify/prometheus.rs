//! Prometheus-compatible acquisition for alert-state polling.
//!
//! Queries the instant-query API (`/api/v1/query`) for the built-in
//! `ALERTS` metric, which every Prometheus-compatible evaluator (Prometheus,
//! VictoriaMetrics/vmalert) exposes with an `alertstate` label of `pending`
//! or `firing` while a rule is active. This module only *acquires* state —
//! deadline decisions belong to [`crate::verify::evaluator`].

use std::fmt::Write as _;
use std::time::Duration;

use crate::verify::{AlertExpectation, AlertState};
use crate::{SondaError, VerifyError};

/// Minimal blocking client for the Prometheus instant-query API.
///
/// Every request carries an overall timeout — a stalled endpoint returns a
/// [`VerifyError::Query`] instead of parking the calling thread forever,
/// which keeps polling loops bounded and interruptible.
pub struct PrometheusClient {
    query_url: String,
    agent: ureq::Agent,
}

impl PrometheusClient {
    /// Create a client for a Prometheus-compatible base URL
    /// (e.g. `http://localhost:9090`), with an overall per-request timeout
    /// covering connect, write, and read.
    pub fn new(base_url: &str, timeout: Duration) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            query_url: format!("{base}/api/v1/query"),
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
