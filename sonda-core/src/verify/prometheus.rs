//! Prometheus-compatible client for alert-state polling.
//!
//! Queries the instant-query API (`/api/v1/query`) for the built-in
//! `ALERTS` metric, which every Prometheus-compatible evaluator (Prometheus,
//! VictoriaMetrics/vmalert) exposes with an `alertstate` label of `pending`
//! or `firing` while a rule is active.

use std::fmt::Write as _;

use crate::verify::AlertExpectation;
use crate::{ConfigError, SondaError};

/// Observed state of one alert at a single poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertState {
    /// No `ALERTS` series matched the expectation's selector.
    Inactive,
    /// Matched series exist, but none with `alertstate="firing"`.
    Pending,
    /// At least one matched series has `alertstate="firing"`.
    Firing,
}

/// Minimal blocking client for the Prometheus instant-query API.
pub struct PrometheusClient {
    query_url: String,
    agent: ureq::Agent,
}

impl PrometheusClient {
    /// Create a client for a Prometheus-compatible base URL
    /// (e.g. `http://localhost:9090`).
    pub fn new(base_url: &str) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            query_url: format!("{base}/api/v1/query"),
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    /// Query the current state of an expected alert.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Config`] when the endpoint is unreachable or
    /// responds with a non-success API status — callers decide whether to
    /// retry (transient network errors during startup are normal).
    pub fn alert_state(&self, expectation: &AlertExpectation) -> Result<AlertState, SondaError> {
        let query = alerts_query(expectation);
        let body = self
            .agent
            .get(&self.query_url)
            .query("query", &query)
            .call()
            .map_err(|e| {
                SondaError::Config(ConfigError::invalid(format!(
                    "prometheus query failed against {}: {e}",
                    self.query_url
                )))
            })?
            .into_string()
            .map_err(|e| {
                SondaError::Config(ConfigError::invalid(format!(
                    "prometheus response could not be read: {e}"
                )))
            })?;
        parse_alert_state(&body)
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

/// Parse an instant-query response into an [`AlertState`].
pub fn parse_alert_state(body: &str) -> Result<AlertState, SondaError> {
    let parsed: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        SondaError::Config(ConfigError::invalid(format!(
            "prometheus response is not valid JSON: {e}"
        )))
    })?;
    if parsed["status"] != "success" {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "prometheus query returned status {:?}",
            parsed["status"]
        ))));
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
}
