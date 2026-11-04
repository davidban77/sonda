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

/// Snapshot of the datastore's clock, expressed as an offset from the
/// local one.
///
/// Range samples carry *server* timestamps; verdict anchors captured from
/// the *local* clock must be translated into server coordinates before any
/// subtraction, or the clock offset between the two hosts is silently
/// baked into every verdict — one second of skew flips a tight deadline,
/// and negative skew can turn a missed deadline into a false pass.
#[derive(Debug, Clone, Copy)]
pub struct ServerClock {
    offset_secs: f64,
}

impl ServerClock {
    /// A clock with no offset — for when the server's clock could not be
    /// measured and local time is the only (warned-about) option.
    pub fn identity() -> Self {
        Self { offset_secs: 0.0 }
    }

    /// Measured server-minus-local offset in seconds.
    pub fn offset_secs(&self) -> f64 {
        self.offset_secs
    }

    /// A local wall-clock instant, expressed in server coordinates.
    pub fn at(&self, t: std::time::SystemTime) -> f64 {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
            + self.offset_secs
    }

    /// The current moment, expressed in server coordinates.
    pub fn now(&self) -> f64 {
        self.at(std::time::SystemTime::now())
    }
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

/// The agent the Prometheus, Alertmanager and TSDB range-query clients send
/// through.
///
/// `timeout` bounds each request as a whole: connecting, sending, and
/// reading the response and its body. A 4xx or 5xx is an error. No proxy is
/// read from the environment, connecting alone is bounded by 30 s, and at
/// most 5 redirects are followed.
pub(crate) fn query_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(true)
        .proxy(None)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(30)))
        .max_redirects(5)
        .build()
        .into()
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
            agent: query_agent(timeout),
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
            .into_body()
            .read_to_string()
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

    /// [`Self::alert_state`] plus the label sets of the firing series.
    ///
    /// Same request, same parse; the caller keeps the labels so provenance
    /// survives a fall back to the live poll timeline.
    pub fn alert_probe(
        &self,
        expectation: &AlertExpectation,
    ) -> Result<(AlertState, Vec<BTreeMap<String, String>>), SondaError> {
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
            .into_body()
            .read_to_string()
            .map_err(|e| {
                SondaError::Verify(VerifyError::BadResponse {
                    url: self.query_url.clone(),
                    reason: format!("response could not be read: {e}"),
                })
            })?;
        parse_alert_probe(&body).map_err(|reason| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.query_url.clone(),
                reason,
            })
        })
    }

    /// Measure the server's clock with an instant `time()` query,
    /// bracketing the request with local readings so the offset is taken
    /// against the request's midpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Verify`] when the endpoint is unreachable,
    /// times out, or responds with an unusable payload — callers should
    /// fall back to [`ServerClock::identity`] with a warning rather than
    /// abort.
    pub fn server_clock(&self) -> Result<ServerClock, SondaError> {
        let local_before = ServerClock::identity().now();
        let body = self
            .agent
            .get(&self.query_url)
            .query("query", "time()")
            .call()
            .map_err(|e| {
                SondaError::Verify(VerifyError::Query {
                    url: self.query_url.clone(),
                    reason: e.to_string(),
                })
            })?
            .into_body()
            .read_to_string()
            .map_err(|e| {
                SondaError::Verify(VerifyError::BadResponse {
                    url: self.query_url.clone(),
                    reason: format!("response could not be read: {e}"),
                })
            })?;
        let local_after = ServerClock::identity().now();
        let server = parse_server_time(&body).map_err(|reason| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.query_url.clone(),
                reason,
            })
        })?;
        Ok(ServerClock {
            offset_secs: server - (local_before + local_after) / 2.0,
        })
    }

    /// Reconstruct an expectation's state history over `[start, end]`
    /// (**server** unix seconds — see [`ServerClock`]) at `step`
    /// resolution, with observation timestamps measured from `anchor`
    /// (also server unix seconds — scenario start for firing checks,
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
            .query("start", format!("{start:.3}"))
            .query("end", format!("{end:.3}"))
            .query("step", format!("{:.3}", step.as_secs_f64()))
            .call()
            .map_err(|e| {
                SondaError::Verify(VerifyError::Query {
                    url: self.range_url.clone(),
                    reason: e.to_string(),
                })
            })?
            .into_body()
            .read_to_string()
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

/// Parse an instant `time()` response into the server's evaluation
/// timestamp in unix seconds.
///
/// Prometheus answers with a scalar (`result: [ts, "ts"]`); VictoriaMetrics
/// answers with a one-series vector whose `value: [ts, "..."]`. In both
/// shapes the *evaluation timestamp* field is the server's clock — the
/// stringified value is not used, because VM computes `time()` at
/// `now - search.latencyOffset` (30s behind by default) while stamping the
/// evaluation timestamp with the actual now.
///
/// Returns a human-readable reason on failure; the caller wraps it with
/// the query URL into a [`VerifyError::BadResponse`].
pub fn parse_server_time(body: &str) -> Result<f64, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;
    if parsed["status"] != "success" {
        return Err(format!("query returned status {:?}", parsed["status"]));
    }
    let data = &parsed["data"];
    match &data["resultType"] {
        t if t == "scalar" => data["result"][0]
            .as_f64()
            .ok_or_else(|| "scalar result carries no timestamp".to_string()),
        t if t == "vector" => data["result"][0]["value"][0]
            .as_f64()
            .ok_or_else(|| "vector result carries no timestamp".to_string()),
        other => Err(format!(
            "expected resultType \"scalar\" or \"vector\", got {other:?}"
        )),
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
    parse_alert_probe(body).map(|(state, _)| state)
}

/// An instant query's state **and** the label sets of its firing series.
///
/// The labels are in the response either way; returning only the state
/// discarded them, and the discard had a consequence. The verdict timeline
/// normally comes from a range query, which collects firing-series labels
/// for the provenance check — but when no stored sample exists yet (an alert
/// that fires faster than one `--query-step`) or the range API errors, the
/// verdict falls back to the live poll timeline, and the provenance check
/// then had nothing to inspect. The advisory disappeared precisely when the
/// run was fastest, silently (#567 r4).
pub fn parse_alert_probe(
    body: &str,
) -> Result<(AlertState, Vec<BTreeMap<String, String>>), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;
    if parsed["status"] != "success" {
        return Err(format!("query returned status {:?}", parsed["status"]));
    }
    let empty = Vec::new();
    let results = parsed["data"]["result"].as_array().unwrap_or(&empty);
    if results.is_empty() {
        return Ok((AlertState::Inactive, Vec::new()));
    }
    let mut firing_series = Vec::new();
    for series in results {
        if series["metric"]["alertstate"] != "firing" {
            continue;
        }
        let Some(metric) = series["metric"].as_object() else {
            continue;
        };
        firing_series.push(
            metric
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect(),
        );
    }
    let state = if firing_series.is_empty() {
        AlertState::Pending
    } else {
        AlertState::Firing
    };
    Ok((state, firing_series))
}

/// A one-request HTTP server, and what a query client must return for each
/// way a request can fail; shared by the Prometheus, Alertmanager and TSDB
/// client tests.
#[cfg(test)]
pub(crate) mod test_server {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::{SondaError, VerifyError};

    /// How long the server holds a connection it does not finish answering.
    /// The tests' clients time out long before.
    const HOLD: Duration = Duration::from_secs(10);

    /// A client timeout well inside [`HOLD`].
    pub(crate) const CLIENT_TIMEOUT: Duration = Duration::from_millis(300);

    /// The `Date` header of every [`Failure::Status`] response.
    pub(crate) const DATE: &str = "Tue, 03 Nov 2026 10:00:00 GMT";

    /// How one request fails.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Failure {
        /// Answered with this status, a [`DATE`] header and a JSON error body.
        Status(u16),
        /// Nothing listens on the port.
        Refused,
        /// Accepted and read, never answered.
        Silence,
        /// A 200 whose body ends after 9 of the 100 bytes it promised.
        CutBody,
        /// A 200 that sends 9 of the 100 body bytes it promised, then waits.
        StalledBody,
    }

    /// A base URL for `failure`, and, when a server is behind it, the head
    /// (request line and headers) of the one request it reads.
    pub(crate) fn serve(failure: Failure) -> (String, Option<mpsc::Receiver<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        if let Failure::Refused = failure {
            drop(listener);
            return (base, None);
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let _ = tx.send(head);
            let cut =
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"status\"";
            match failure {
                Failure::Status(code) => {
                    let body = r#"{"status":"error","error":"boom"}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {code} X\r\nDate: {DATE}\r\n\
                         Content-Type: application/json\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
                Failure::CutBody => {
                    let _ = stream.write_all(cut.as_bytes());
                }
                Failure::StalledBody => {
                    let _ = stream.write_all(cut.as_bytes());
                    let _ = stream.flush();
                    std::thread::sleep(HOLD);
                }
                Failure::Silence => std::thread::sleep(HOLD),
                Failure::Refused => {}
            }
        });
        (base, Some(rx))
    }

    /// Assert that `result` is the error a query client returns for
    /// `failure`, naming `url`: a status, a refused connection or silence is
    /// `Query`, a body that cannot be read `BadResponse`. A server behind the
    /// URL must have read the request first, and an unanswered request must
    /// have ended in under half of [`HOLD`], so before the server let go.
    /// `what` names the call in a failure.
    pub(crate) fn assert_fails_as_before<T: std::fmt::Debug>(
        what: &str,
        failure: Failure,
        url: &str,
        head: Option<mpsc::Receiver<String>>,
        elapsed: Duration,
        result: Result<T, SondaError>,
    ) {
        if let Some(head) = head {
            let head = head
                .recv_timeout(HOLD)
                .unwrap_or_else(|e| panic!("{what}: the request never reached the server: {e}"));
            assert!(head.starts_with("GET "), "{what}: {head}");
        }
        if matches!(failure, Failure::Silence | Failure::StalledBody) {
            assert!(elapsed < HOLD / 2, "{what}: {failure:?} took {elapsed:?}");
        }
        match (failure, result) {
            (
                Failure::Status(code),
                Err(SondaError::Verify(VerifyError::Query { url: named, reason })),
            ) => {
                assert_eq!(named, url, "{what}");
                assert!(reason.ends_with(&code.to_string()), "{what}: {reason}");
            }
            (
                Failure::Refused | Failure::Silence,
                Err(SondaError::Verify(VerifyError::Query { url: named, reason })),
            ) => {
                assert_eq!(named, url, "{what}");
                assert!(!reason.is_empty(), "{what}");
            }
            (
                Failure::CutBody | Failure::StalledBody,
                Err(SondaError::Verify(VerifyError::BadResponse { url: named, reason })),
            ) => {
                assert_eq!(named, url, "{what}");
                let prefix = "response could not be read: ";
                assert!(
                    reason.starts_with(prefix) && reason.len() > prefix.len(),
                    "{what}: {reason}"
                );
            }
            (failure, other) => panic!("{what}: {failure:?}: got {other:?}"),
        }
    }

    /// The values of the query parameter `name` in `head`'s request line,
    /// decoded (`+` is a space, `%XX` a byte).
    pub(crate) fn query_values(head: &str, name: &str) -> Vec<String> {
        let target = head.split(' ').nth(1).expect("a request target");
        let query = target.split_once('?').map_or("", |(_, q)| q);
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| {
                let raw = value.as_bytes();
                let mut bytes = Vec::with_capacity(raw.len());
                let mut i = 0;
                while i < raw.len() {
                    match raw[i] {
                        b'+' => bytes.push(b' '),
                        b'%' => {
                            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).expect("hex");
                            bytes.push(u8::from_str_radix(hex, 16).expect("a %XX escape"));
                            i += 2;
                        }
                        b => bytes.push(b),
                    }
                    i += 1;
                }
                String::from_utf8(bytes).expect("UTF-8")
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_server::{self, Failure, CLIENT_TIMEOUT};
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
    fn server_time_parses_prometheus_scalar() {
        let body = r#"{"status":"success","data":{"resultType":"scalar","result":[1754700000.123,"1754700000.123"]}}"#;
        let ts = parse_server_time(body).expect("parse");
        assert!((ts - 1_754_700_000.123).abs() < 1e-6);
    }

    #[test]
    fn server_time_parses_victoriametrics_vector_from_eval_timestamp() {
        // VM answers time() as a one-series vector, and computes the string
        // value at now - search.latencyOffset (30s behind by default). The
        // evaluation timestamp is the server's actual clock — observed live
        // against v1.149.0.
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1786311458,"1786311428.689"]}]}}"#;
        let ts = parse_server_time(body).expect("parse");
        assert!((ts - 1_786_311_458.0).abs() < 1e-6);
    }

    #[test]
    fn server_time_rejects_empty_vector_and_matrix() {
        let empty = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        assert!(parse_server_time(empty).is_err());
        let matrix = r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
        assert!(parse_server_time(matrix).is_err());
    }

    #[test]
    fn server_clock_translates_local_instants() {
        let clock = ServerClock { offset_secs: -6.0 };
        let t = std::time::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!((clock.at(t) - 994.0).abs() < 1e-9);
        assert_eq!(ServerClock::identity().offset_secs(), 0.0);
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

    /// Every request the client makes fails the same way for each failure:
    /// `Query` for a status, a refused connection or silence, `BadResponse`
    /// for a body that cannot be read, naming the endpoint it queried.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::bad_request(Failure::Status(400))]
    #[case::not_found(Failure::Status(404))]
    #[case::too_many_requests(Failure::Status(429))]
    #[case::server_error(Failure::Status(500))]
    #[case::unavailable(Failure::Status(503))]
    #[case::refused_connection(Failure::Refused)]
    #[case::no_answer_times_out(Failure::Silence)]
    #[case::cut_body(Failure::CutBody)]
    #[case::stalled_body_times_out(Failure::StalledBody)]
    fn each_request_fails_as_before(#[case] failure: Failure) {
        type Call = fn(&PrometheusClient) -> Result<String, SondaError>;
        let calls: [(&str, bool, Call); 4] = [
            ("alert_state", false, |c| c.alert_state(&expectation(None)).map(|s| format!("{s:?}"))),
            ("alert_probe", false, |c| c.alert_probe(&expectation(None)).map(|s| format!("{s:?}"))),
            ("server_clock", false, |c| c.server_clock().map(|s| format!("{s:?}"))),
            ("range_timeline", true, |c| {
                c.range_timeline(&expectation(None), 0.0, 10.0, Duration::from_secs(1), 0.0)
                    .map(|t| format!("{:?}", t.observations))
            }),
        ];
        for (name, range, call) in calls {
            let (base, head) = test_server::serve(failure);
            let client = PrometheusClient::new(&base, CLIENT_TIMEOUT);
            let url = if range { client.range_url.clone() } else { client.query_url.clone() };
            let started = std::time::Instant::now();
            let result = call(&client);
            test_server::assert_fails_as_before(name, failure, &url, head, started.elapsed(), result);
        }
    }

    /// The PromQL reaches the server as written, through characters a query
    /// string reserves (`+`, `&`, `=`, `#`, `%`), a space and non-ASCII.
    #[test]
    fn a_query_reaches_the_server_as_written() {
        let labels = BTreeMap::from([(
            "path".to_string(),
            "/a b+c&d=e#f%20 ü \"q\" {x}".to_string(),
        )]);
        let expectation = expectation(Some(labels));
        let (base, head) = test_server::serve(Failure::Status(500));
        let client = PrometheusClient::new(&base, CLIENT_TIMEOUT);
        let _ = client.alert_state(&expectation);
        let head = head
            .expect("a server")
            .recv_timeout(Duration::from_secs(5))
            .expect("the request reached the server");
        assert_eq!(
            test_server::query_values(&head, "query"),
            vec![alerts_query(&expectation)]
        );
    }

    #[test]
    fn the_query_agent_bounds_the_whole_request_and_errs_on_a_status() {
        let agent = query_agent(Duration::from_millis(1234));
        let config = agent.config();
        assert!(config.http_status_as_error());
        assert!(config.proxy().is_none());
        assert_eq!(config.timeouts().global, Some(Duration::from_millis(1234)));
        assert_eq!(config.timeouts().connect, Some(Duration::from_secs(30)));
        assert_eq!(config.max_redirects(), 5);
    }
}
