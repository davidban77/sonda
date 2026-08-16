//! Alertmanager acquisition for alert-state polling.
//!
//! The sibling of [`crate::verify::prometheus`], one hop further down the
//! alerting path. The Prometheus path asks the *rule evaluator* whether a
//! rule is firing; this one asks **Alertmanager** whether the notification
//! actually arrived. A rule that evaluates correctly but never reaches
//! Alertmanager — wrong `-notifier.url`, a dropped route, a broken relabel
//! — is green on the Prometheus path and red here. That is the whole
//! reason this module exists.
//!
//! Only `GET /api/v2/alerts` is used, filtered server-side with
//! `filter=key="value"` matchers built from the expectation.
//!
//! # What this path can and cannot promise
//!
//! Alertmanager has **no range API and no `time()` endpoint**. There is no
//! stored history to reconstruct after the fact: acquisition here is
//! *live polling only*, and a state change between two polls is invisible.
//! Verdict times therefore carry poll-interval precision, refined — never
//! extended — by the alert's own `startsAt` stamp.
//!
//! "Absent" is the resolution signal, which is only trustworthy because a
//! notifier keeps re-sending a still-firing alert. Measured live against
//! vmalert v1.149.0 at a 5s evaluation interval: every evaluation re-posts
//! the alert with `endsAt` 20s in the future, so a firing alert is never
//! less than ~15s from expiry and would have to miss four consecutive
//! evaluations to vanish between polls. Keep `--interval` well inside that
//! window. The `alert-test-pass-am` UAT row is the standing check that this
//! still holds end to end.
//!
//! `startsAt` is written by Alertmanager's clock, so comparing it against a
//! local anchor would bake clock skew straight into the verdict. The offset
//! is measured from the HTTP `Date` header of Alertmanager's own responses
//! ([`AlertmanagerClient::measure_clock_offset`]), which is **one-second
//! granular** — deliberately weaker than the `time()`-derived
//! [`crate::verify::prometheus::ServerClock`], and reported as such. Do not
//! describe Alertmanager verdicts as server-anchored to sample resolution;
//! they are not.
//!
//! # State mapping
//!
//! Alertmanager has no `pending` concept — a rule in its `for:` window has
//! not been sent yet, so it is simply absent. This module therefore never
//! produces [`AlertState::Pending`]; every matched, unexpired alert is
//! [`AlertState::Firing`] and everything else is [`AlertState::Inactive`].
//! The evaluator only ever asks "is this Firing?", so the missing middle
//! state changes no verdict.
//!
//! This module only *acquires* state — deadline decisions belong to
//! [`crate::verify::evaluator`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use crate::verify::evaluator::Observation;
use crate::verify::{AlertExpectation, AlertState};
use crate::{SondaError, VerifyError};

/// One poll of Alertmanager for a single expectation.
#[derive(Debug, Clone)]
pub struct AlertSnapshot {
    /// [`AlertState::Firing`] when at least one unexpired alert matched,
    /// [`AlertState::Inactive`] otherwise. Never `Pending` — see the module
    /// docs.
    pub state: AlertState,
    /// Label sets of the matched firing alerts — input for
    /// [`crate::verify::foreign_label_values`] provenance checks, exactly
    /// as the Prometheus path feeds them from its range series.
    pub series: Vec<BTreeMap<String, String>>,
    /// Earliest `startsAt` across the matched alerts, in **Alertmanager**
    /// unix seconds. `None` when nothing matched or no stamp parsed.
    pub started_at: Option<f64>,
    /// Whether any matched alert was silenced or inhibited
    /// (`status.state == "suppressed"`). Suppressed alerts still count as
    /// firing — a silence stops the notification, not the alert — but the
    /// report says so, because a silenced alert is rarely what a test
    /// author meant to assert.
    pub suppressed: bool,
}

/// Minimal blocking client for the Alertmanager v2 alerts API.
///
/// Every request carries an overall timeout, so a stalled Alertmanager
/// returns a [`VerifyError::Query`] instead of parking the polling thread.
pub struct AlertmanagerClient {
    alerts_url: String,
    agent: ureq::Agent,
    clock_offset_secs: f64,
}

impl AlertmanagerClient {
    /// Create a client for an Alertmanager base URL
    /// (e.g. `http://localhost:9093`), with an overall per-request timeout
    /// covering connect, write, and read. The clock offset starts at zero;
    /// see [`Self::measure_clock_offset`] and [`Self::with_clock_offset`].
    pub fn new(base_url: &str, timeout: Duration) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            alerts_url: format!("{base}/api/v2/alerts"),
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
            clock_offset_secs: 0.0,
        }
    }

    /// Adopt a measured server-minus-local clock offset in seconds.
    pub fn with_clock_offset(mut self, offset_secs: f64) -> Self {
        self.clock_offset_secs = offset_secs;
        self
    }

    /// The offset this client applies to Alertmanager-stamped timestamps.
    pub fn clock_offset_secs(&self) -> f64 {
        self.clock_offset_secs
    }

    /// Measure Alertmanager's clock from the `Date` header of a real
    /// response, bracketing the request with local readings so the offset
    /// is taken against the request's midpoint.
    ///
    /// HTTP dates have **one-second** resolution, so the result is only
    /// good to about ±1s plus half the round trip. That is enough to keep
    /// host clock skew out of a verdict and nowhere near enough to claim
    /// sample-level precision.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Verify`] when the endpoint is unreachable or
    /// sends no parseable `Date` header — callers should fall back to a
    /// zero offset with a warning rather than abort.
    pub fn measure_clock_offset(&self) -> Result<f64, SondaError> {
        let local_before = local_unix_now();
        let response = self
            .agent
            .get(&self.alerts_url)
            .query("filter", "alertname=\"sonda-clock-probe\"")
            .call()
            .map_err(|e| {
                SondaError::Verify(VerifyError::Query {
                    url: self.alerts_url.clone(),
                    reason: e.to_string(),
                })
            })?;
        let local_after = local_unix_now();
        let header = response.header("date").ok_or_else(|| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.alerts_url.clone(),
                reason: "response carries no Date header to measure the clock from".to_string(),
            })
        })?;
        let server = parse_http_date(header).ok_or_else(|| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.alerts_url.clone(),
                reason: format!("Date header {header:?} is not an HTTP date"),
            })
        })?;
        Ok(server - (local_before + local_after) / 2.0)
    }

    /// Poll the current state of an expected alert.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Verify`] when the endpoint is unreachable,
    /// times out, or responds with an unusable payload — callers polling in
    /// a loop treat these as retryable until their deadline.
    pub fn alerts(&self, expectation: &AlertExpectation) -> Result<AlertSnapshot, SondaError> {
        let mut request = self.agent.get(&self.alerts_url);
        for filter in alert_filters(expectation) {
            // ureq percent-encodes each query value; the matcher quoting
            // below is the *other* half of the escaping contract.
            request = request.query("filter", &filter);
        }
        let body = request
            .call()
            .map_err(|e| {
                SondaError::Verify(VerifyError::Query {
                    url: self.alerts_url.clone(),
                    reason: e.to_string(),
                })
            })?
            .into_string()
            .map_err(|e| {
                SondaError::Verify(VerifyError::BadResponse {
                    url: self.alerts_url.clone(),
                    reason: format!("response could not be read: {e}"),
                })
            })?;
        let now = local_unix_now() + self.clock_offset_secs;
        parse_alerts(&body, expectation, now).map_err(|reason| {
            SondaError::Verify(VerifyError::BadResponse {
                url: self.alerts_url.clone(),
                reason,
            })
        })
    }

    /// [`Self::alerts`] reduced to just the state, for polling loops that
    /// do not need the matched labels.
    ///
    /// # Errors
    ///
    /// As [`Self::alerts`].
    pub fn alert_state(&self, expectation: &AlertExpectation) -> Result<AlertState, SondaError> {
        Ok(self.alerts(expectation)?.state)
    }
}

/// Local wall clock in unix seconds.
fn local_unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Build the `filter=` matchers for an expectation: `alertname` plus every
/// declared label, each as a quoted equality matcher.
///
/// Alertmanager ANDs repeated `filter` params, so one matcher per param is
/// both the documented shape and the one that cannot be broken by a comma
/// inside a label value.
pub fn alert_filters(expectation: &AlertExpectation) -> Vec<String> {
    let mut filters = vec![matcher("alertname", &expectation.alert)];
    if let Some(labels) = &expectation.labels {
        for (key, value) in labels {
            filters.push(matcher(key, value));
        }
    }
    filters
}

/// One `key="value"` equality matcher with the value quoted.
fn matcher(key: &str, value: &str) -> String {
    let mut out = String::with_capacity(key.len() + value.len() + 3);
    let _ = write!(out, "{key}=\"{}\"", escape_matcher_value(value));
    out
}

/// Escape a matcher value for Alertmanager's quoted-string syntax.
///
/// Alertmanager parses matcher values with Go-style escapes inside double
/// quotes, the same alphabet PromQL uses. An unescaped quote or backslash
/// silently changes which alerts the filter selects, which is the exact
/// class of bug percent-encoding alone does **not** prevent: the transport
/// layer would faithfully deliver a broken matcher.
fn escape_matcher_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Parse a `GET /api/v2/alerts` response into an [`AlertSnapshot`].
///
/// `now_unix` is the current moment **on Alertmanager's clock** (local time
/// plus the measured offset); it decides which alerts have expired.
///
/// Three things are checked per alert, and all three must hold for it to
/// count as firing:
///
/// 1. Its labels satisfy the expectation — re-checked here even though the
///    server already filtered, so a mis-escaped matcher can only ever lose
///    a match, never invent one. Escaping correctness is proven separately
///    by asserting the request line the client sends.
/// 2. `status.state` is not a state Alertmanager treats as gone. `active`,
///    `suppressed` and `unprocessed` all mean the notification exists;
///    `suppressed` is reported but still counts (a silence stops the
///    notification, not the alert).
/// 3. `endsAt` has not passed. Measured against Alertmanager 0.28.1, a
///    resolved alert is *removed* from `/api/v2/alerts` rather than lingering
///    with a past `endsAt`, so absence is the resolution signal in practice
///    and this check is the second line of defence: it covers a producer
///    that posts an already-expired alert, and it keeps the verdict correct
///    if that retention behaviour ever changes.
///
/// Returns a human-readable reason on failure; the caller wraps it with the
/// query URL into a [`VerifyError::BadResponse`].
pub fn parse_alerts(
    body: &str,
    expectation: &AlertExpectation,
    now_unix: f64,
) -> Result<AlertSnapshot, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;
    let alerts = parsed
        .as_array()
        .ok_or_else(|| "expected a JSON array of alerts".to_string())?;

    let mut series: Vec<BTreeMap<String, String>> = Vec::new();
    let mut started_at: Option<f64> = None;
    let mut suppressed = false;

    for alert in alerts {
        let labels: BTreeMap<String, String> = alert["labels"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        if !labels_match(&labels, expectation) {
            continue;
        }
        let state = alert["status"]["state"].as_str().unwrap_or("active");
        if !matches!(state, "active" | "suppressed" | "unprocessed") {
            continue;
        }
        // A zero/absent endsAt means "no end declared" — still firing.
        if let Some(ends_at) = parse_rfc3339(alert["endsAt"].as_str().unwrap_or_default()) {
            if ends_at <= now_unix {
                continue;
            }
        }
        if state == "suppressed" {
            suppressed = true;
        }
        if let Some(starts) = parse_rfc3339(alert["startsAt"].as_str().unwrap_or_default()) {
            started_at = Some(started_at.map_or(starts, |current: f64| current.min(starts)));
        }
        if !series.contains(&labels) {
            series.push(labels);
        }
    }

    Ok(AlertSnapshot {
        state: if series.is_empty() {
            AlertState::Inactive
        } else {
            AlertState::Firing
        },
        series,
        started_at,
        suppressed,
    })
}

/// Sharpen a firing timeline with Alertmanager's own `startsAt` stamp.
///
/// Live polling can only ever say "it was already firing when I asked", so
/// the observed firing time is late by up to one poll interval. The alert
/// carries the moment Alertmanager considers it to have started;
/// `starts_at_local` is that stamp already translated to the local clock
/// (Alertmanager stamp minus the measured offset), and `anchor_unix` is the
/// scenario-start anchor the timeline's durations are measured from.
///
/// The stamp is **bounded on both sides by evidence we gathered ourselves**:
///
/// - It can only move the firing observation *earlier* — a stamp later than
///   the poll that saw the alert would claim the alert had not yet started
///   when we watched it firing.
/// - It can never move earlier than the last successful *non*-firing poll —
///   a stamp behind that would contradict a state we observed directly, and
///   is evidence of clock error or of a previous, since-resolved episode of
///   the same alert.
///
/// Both bounds matter because refinement moves verdicts in exactly one
/// direction (towards `Pass`); an unbounded stamp from a skewed clock would
/// be a false-pass channel.
///
/// When there is **no** successful earlier observation — every earlier poll
/// errored, or the first poll already found the alert firing — the lower
/// bound does not exist and the timeline is returned unchanged. Falling
/// back to zero there would hand the stamp the most Pass-favourable value
/// in the range at exactly the moment nothing corroborates it, which is the
/// false-pass channel this function exists to close (#552 review W1).
///
/// Returns the timeline unchanged when there is no stamp or nothing fired.
pub fn refine_firing_timeline(
    observations: &[Observation],
    starts_at_local: Option<f64>,
    anchor_unix: f64,
) -> Vec<Observation> {
    let mut refined = observations.to_vec();
    let Some(starts_at) = starts_at_local.filter(|s| s.is_finite()) else {
        return refined;
    };
    let Some(index) = refined
        .iter()
        .position(|o| matches!(o.state, Ok(AlertState::Firing)))
    else {
        return refined;
    };
    // No successful earlier observation means no evidence to bound the
    // stamp below, and the only available fallback — zero — is the most
    // Pass-favourable value in the range. Refusing to refine is the
    // correct answer: refinement is a sharpening of evidence we have, not
    // a substitute for evidence we lack.
    let Some(floor) = refined[..index]
        .iter()
        .rev()
        .find(|o| o.state.is_ok())
        .map(|o| o.at)
    else {
        return refined;
    };
    let ceiling = refined[index].at;
    if floor >= ceiling {
        return refined;
    }
    let raw = starts_at - anchor_unix;
    let stamped = if raw <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(raw)
    };
    refined[index].at = stamped.clamp(floor, ceiling);
    refined
}

/// Whether an alert's labels satisfy the expectation's matchers.
fn labels_match(labels: &BTreeMap<String, String>, expectation: &AlertExpectation) -> bool {
    if labels.get("alertname").map(String::as_str) != Some(expectation.alert.as_str()) {
        return false;
    }
    expectation.labels.as_ref().is_none_or(|wanted| {
        wanted
            .iter()
            .all(|(key, value)| labels.get(key) == Some(value))
    })
}

/// Parse an RFC 3339 timestamp into unix seconds, or `None` when it is
/// absent, unparseable, or Go's zero time (which Alertmanager sends for
/// "unset").
pub fn parse_rfc3339(value: &str) -> Option<f64> {
    if value.is_empty() || value.starts_with("0001-01-01") {
        return None;
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    Some(parsed.timestamp() as f64 + f64::from(parsed.timestamp_subsec_nanos()) / 1e9)
}

/// Parse an HTTP `Date` header (RFC 7231 IMF-fixdate) into unix seconds.
///
/// Returns `None` for anything unparseable — the caller warns and falls
/// back to a zero offset rather than trusting a guess.
pub fn parse_http_date(value: &str) -> Option<f64> {
    let parsed = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?;
    Some(parsed.timestamp() as f64)
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

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// One `gettableAlert` as Alertmanager v2 renders it.
    fn alert_json(state: &str, ends_at: &str, extra: &str) -> String {
        format!(
            r#"{{"labels":{{"alertname":"HighCpuUsage","host":"sonda-test"{extra}}},
                "startsAt":"2026-08-15T12:00:00.000Z","endsAt":"{ends_at}",
                "status":{{"state":"{state}","silencedBy":[],"inhibitedBy":[]}}}}"#
        )
    }

    /// Unix seconds for 2026-08-15T12:00:00Z, the fixtures' start stamp.
    const START_UNIX: f64 = 1_786_795_200.0;

    #[test]
    fn filters_cover_alertname_and_every_label() {
        let filters = alert_filters(&expectation(Some(labels(&[
            ("severity", "critical"),
            ("host", "sonda-test"),
        ]))));
        assert_eq!(
            filters,
            vec![
                "alertname=\"HighCpuUsage\"",
                "host=\"sonda-test\"",
                "severity=\"critical\"",
            ]
        );
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    // Hostile label values: the matcher must stay one well-formed quoted
    // string. Percent-encoding happens a layer later (ureq) and cannot
    // rescue a value that broke out of its quotes here.
    #[case::plain(       "critical",      "team=\"critical\"")]
    #[case::double_quote("net\"ops",      "team=\"net\\\"ops\"")]
    #[case::backslash(   "a\\b",          "team=\"a\\\\b\"")]
    #[case::quote_escape("a\\\"b",        "team=\"a\\\\\\\"b\"")]
    #[case::spaces(      "on call",       "team=\"on call\"")]
    #[case::comma(       "a,b",           "team=\"a,b\"")]
    #[case::brace(       "a}b{c",         "team=\"a}b{c\"")]
    #[case::newline(     "a\nb",          "team=\"a\\nb\"")]
    #[case::carriage(    "a\rb",          "team=\"a\\rb\"")]
    #[case::tab(         "a\tb",          "team=\"a\\tb\"")]
    #[case::utf8(        "café-ñoño-日本", "team=\"café-ñoño-日本\"")]
    #[case::emoji(       "🔥",            "team=\"🔥\"")]
    #[case::empty(       "",              "team=\"\"")]
    fn hostile_label_values_stay_quoted(#[case] value: &str, #[case] expected: &str) {
        let filters = alert_filters(&expectation(Some(labels(&[("team", value)]))));
        assert_eq!(filters[1], expected);
    }

    #[test]
    fn active_alert_is_firing_with_labels_and_start() {
        let body = format!("[{}]", alert_json("active", "2026-08-15T12:10:00.000Z", ""));
        let snapshot = parse_alerts(&body, &expectation(None), START_UNIX + 60.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Firing);
        assert_eq!(snapshot.series.len(), 1);
        assert_eq!(
            snapshot.series[0].get("host").map(String::as_str),
            Some("sonda-test")
        );
        assert_eq!(snapshot.started_at, Some(START_UNIX));
        assert!(!snapshot.suppressed);
    }

    #[test]
    fn empty_array_is_inactive() {
        let snapshot = parse_alerts("[]", &expectation(None), START_UNIX).expect("parse");
        assert_eq!(snapshot.state, AlertState::Inactive);
        assert!(snapshot.series.is_empty());
        assert_eq!(snapshot.started_at, None);
    }

    #[test]
    fn suppressed_alert_still_counts_as_firing_and_is_flagged() {
        // A silence stops the notification, not the alert: the rule did
        // fire, so firing_within is satisfied — but the caller is told.
        let body = format!(
            "[{}]",
            alert_json("suppressed", "2026-08-15T12:10:00.000Z", "")
        );
        let snapshot = parse_alerts(&body, &expectation(None), START_UNIX + 60.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Firing);
        assert!(snapshot.suppressed);
    }

    #[test]
    fn expired_alert_is_not_firing() {
        // Alertmanager 0.28.1 drops resolved alerts from the API, so this
        // payload is the hostile case rather than the common one: a
        // producer posting an already-expired alert must not read as
        // firing just because it is present.
        let body = format!("[{}]", alert_json("active", "2026-08-15T12:05:00.000Z", ""));
        let snapshot = parse_alerts(&body, &expectation(None), START_UNIX + 600.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Inactive);
        assert!(snapshot.series.is_empty());
    }

    #[test]
    fn alert_expiring_exactly_now_is_not_firing() {
        let body = format!("[{}]", alert_json("active", "2026-08-15T12:05:00.000Z", ""));
        let snapshot = parse_alerts(&body, &expectation(None), START_UNIX + 300.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Inactive);
    }

    #[test]
    fn zero_ends_at_means_no_expiry() {
        let body = format!("[{}]", alert_json("active", "0001-01-01T00:00:00.000Z", ""));
        let snapshot = parse_alerts(&body, &expectation(None), START_UNIX + 600.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Firing);
    }

    #[test]
    fn label_recheck_rejects_an_alert_the_expectation_did_not_ask_for() {
        // Defence in depth against a mis-escaped filter: the server is not
        // trusted to have applied the matchers we meant to send, so a bad
        // matcher can lose a match but never fabricate one.
        let body = format!("[{}]", alert_json("active", "2026-08-15T12:10:00.000Z", ""));
        let wanted = expectation(Some(labels(&[("host", "other-host")])));
        let snapshot = parse_alerts(&body, &wanted, START_UNIX + 60.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Inactive);
    }

    #[test]
    fn label_recheck_rejects_a_different_alertname() {
        let body = r#"[{"labels":{"alertname":"OtherAlert","host":"sonda-test"},
            "startsAt":"2026-08-15T12:00:00.000Z","endsAt":"2026-08-15T12:10:00.000Z",
            "status":{"state":"active"}}]"#;
        let snapshot = parse_alerts(body, &expectation(None), START_UNIX + 60.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Inactive);
    }

    #[test]
    fn earliest_start_wins_across_matched_alerts() {
        let body = r#"[
            {"labels":{"alertname":"HighCpuUsage","host":"a"},
             "startsAt":"2026-08-15T12:02:00.000Z","endsAt":"2026-08-15T12:10:00.000Z",
             "status":{"state":"active"}},
            {"labels":{"alertname":"HighCpuUsage","host":"b"},
             "startsAt":"2026-08-15T12:00:30.000Z","endsAt":"2026-08-15T12:10:00.000Z",
             "status":{"state":"active"}}
        ]"#;
        let snapshot = parse_alerts(body, &expectation(None), START_UNIX + 180.0).expect("parse");
        assert_eq!(snapshot.started_at, Some(START_UNIX + 30.0));
        assert_eq!(snapshot.series.len(), 2);
    }

    #[test]
    fn expired_alert_does_not_contribute_its_start_stamp() {
        // An alert that already ended must not drag `started_at` earlier —
        // otherwise a stale notification would refine a later alert's
        // firing time backwards and could turn Late into Pass.
        let body = r#"[
            {"labels":{"alertname":"HighCpuUsage","host":"a"},
             "startsAt":"2026-08-15T11:00:00.000Z","endsAt":"2026-08-15T11:30:00.000Z",
             "status":{"state":"active"}},
            {"labels":{"alertname":"HighCpuUsage","host":"a"},
             "startsAt":"2026-08-15T12:05:00.000Z","endsAt":"2026-08-15T12:20:00.000Z",
             "status":{"state":"active"}}
        ]"#;
        let snapshot = parse_alerts(body, &expectation(None), START_UNIX + 400.0).expect("parse");
        assert_eq!(snapshot.started_at, Some(START_UNIX + 300.0));
    }

    #[test]
    fn unknown_status_state_is_ignored() {
        let body = format!(
            "[{}]",
            alert_json("resolved", "2026-08-15T12:10:00.000Z", "")
        );
        let snapshot = parse_alerts(&body, &expectation(None), START_UNIX + 60.0).expect("parse");
        assert_eq!(snapshot.state, AlertState::Inactive);
    }

    #[test]
    fn non_array_payload_is_rejected() {
        let err = parse_alerts(r#"{"status":"success"}"#, &expectation(None), 0.0)
            .expect_err("must reject");
        assert!(err.contains("array"), "{err}");
    }

    #[test]
    fn invalid_json_is_rejected() {
        assert!(parse_alerts("not json", &expectation(None), 0.0).is_err());
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::zulu(       "2026-08-15T12:00:00Z",        Some(1_786_795_200.0))]
    #[case::millis(     "2026-08-15T12:00:00.500Z",    Some(1_786_795_200.5))]
    #[case::offset(     "2026-08-15T13:00:00+01:00",   Some(1_786_795_200.0))]
    #[case::go_zero(    "0001-01-01T00:00:00Z",        None)]
    #[case::empty(      "",                            None)]
    #[case::garbage(    "not a time",                  None)]
    #[case::date_only(  "2026-08-15",                  None)]
    fn rfc3339_parsing(#[case] value: &str, #[case] expected: Option<f64>) {
        match (parse_rfc3339(value), expected) {
            (Some(got), Some(want)) => assert!((got - want).abs() < 1e-6, "{got} vs {want}"),
            (got, want) => assert_eq!(got.is_none(), want.is_none(), "{value:?}"),
        }
    }

    #[test]
    fn http_date_parses_imf_fixdate() {
        let ts = parse_http_date("Sat, 15 Aug 2026 12:00:00 GMT").expect("parse");
        assert!((ts - 1_786_795_200.0).abs() < 1e-6, "{ts}");
    }

    #[test]
    fn http_date_rejects_garbage() {
        assert!(parse_http_date("").is_none());
        assert!(parse_http_date("yesterday").is_none());
        assert!(parse_http_date("2026-08-15T12:00:00Z").is_none());
    }

    fn timeline(points: &[(u64, Option<AlertState>)]) -> Vec<Observation> {
        points
            .iter()
            .map(|(secs, state)| {
                Observation::new(
                    Duration::from_secs(*secs),
                    state.ok_or_else(|| "query failed".to_string()),
                )
            })
            .collect()
    }

    /// A three-poll firing timeline anchored at unix 1000: inactive at 0s
    /// and 10s, firing first seen at 20s.
    fn late_notice() -> Vec<Observation> {
        timeline(&[
            (0, Some(AlertState::Inactive)),
            (10, Some(AlertState::Inactive)),
            (20, Some(AlertState::Firing)),
        ])
    }

    #[test]
    fn starts_at_pulls_the_firing_observation_back_to_the_stamp() {
        let refined = refine_firing_timeline(&late_notice(), Some(1015.0), 1000.0);
        assert_eq!(refined[2].at, Duration::from_secs(15));
        // Everything else is untouched.
        assert_eq!(refined[0].at, Duration::ZERO);
        assert_eq!(refined[1].at, Duration::from_secs(10));
    }

    #[test]
    fn starts_at_never_moves_the_observation_later() {
        // A stamp after the poll would claim the alert had not started
        // when we watched it firing.
        let refined = refine_firing_timeline(&late_notice(), Some(1030.0), 1000.0);
        assert_eq!(refined[2].at, Duration::from_secs(20));
    }

    #[test]
    fn starts_at_never_contradicts_an_observed_inactive_poll() {
        // 1005 is before the 10s poll that successfully saw Inactive: a
        // skewed clock (or a previous episode's stamp) must not rewrite
        // history we watched happen.
        let refined = refine_firing_timeline(&late_notice(), Some(1005.0), 1000.0);
        assert_eq!(refined[2].at, Duration::from_secs(10));
    }

    #[test]
    fn a_failed_poll_is_not_a_floor() {
        // Only successful observations bound the stamp — a query error
        // proves nothing about the alert's state.
        let observations = timeline(&[
            (0, Some(AlertState::Inactive)),
            (10, None),
            (20, Some(AlertState::Firing)),
        ]);
        let refined = refine_firing_timeline(&observations, Some(1005.0), 1000.0);
        assert_eq!(refined[2].at, Duration::from_secs(5));
    }

    #[test]
    fn stamp_before_the_anchor_clamps_to_the_first_observation() {
        // An alert that predates the scenario: floor is the 0s poll, so
        // the refinement cannot invent negative time.
        let refined = refine_firing_timeline(&late_notice(), Some(900.0), 1000.0);
        assert_eq!(refined[2].at, Duration::from_secs(10));
    }

    #[test]
    fn no_stamp_and_no_firing_leave_the_timeline_alone() {
        assert_eq!(
            refine_firing_timeline(&late_notice(), None, 1000.0)[2].at,
            Duration::from_secs(20)
        );
        let never = timeline(&[(0, Some(AlertState::Inactive))]);
        assert_eq!(
            refine_firing_timeline(&never, Some(1000.0), 1000.0)[0].at,
            Duration::ZERO
        );
    }

    #[test]
    fn a_first_poll_that_is_already_firing_cannot_be_refined() {
        // No earlier evidence and no room below zero: the stamp has
        // nothing to sharpen, so the timeline must survive untouched.
        //
        // NOTE: this case alone is NOT discriminating — floor and ceiling
        // coincide at 0s, so it passes whether or not the no-evidence path
        // is handled (#552 review W1). The two tests below are the ones
        // with teeth; this one stays for the boundary itself.
        let observations = timeline(&[(0, Some(AlertState::Firing))]);
        let refined = refine_firing_timeline(&observations, Some(500.0), 1000.0);
        assert_eq!(refined[0].at, Duration::ZERO);
    }

    #[test]
    fn a_stamp_with_no_successful_earlier_poll_is_refused() {
        // Every earlier poll errored, so nothing we observed can bound the
        // stamp from below. A stale stamp (unix 900 against a 1000 anchor)
        // would otherwise be clamped to zero — the most Pass-favourable
        // value in the range — and flip a missed deadline into a pass.
        let observations = timeline(&[
            (0, None),
            (10, None),
            (20, None),
            (30, Some(AlertState::Firing)),
        ]);
        let refined = refine_firing_timeline(&observations, Some(900.0), 1000.0);
        assert_eq!(
            refined[3].at,
            Duration::from_secs(30),
            "a stamp nothing corroborates must not move the firing time"
        );
    }

    #[test]
    fn a_late_first_poll_that_is_already_firing_is_refused() {
        // Same hole, reached the other way: the very first observation is
        // firing, but at 30s rather than 0s, so floor and ceiling do NOT
        // coincide and a zero fallback would be a 30s jump to Pass.
        let observations = timeline(&[(30, Some(AlertState::Firing))]);
        let refined = refine_firing_timeline(&observations, Some(900.0), 1000.0);
        assert_eq!(refined[0].at, Duration::from_secs(30));
    }

    #[test]
    fn one_successful_earlier_poll_is_enough_to_refine_again() {
        // The refusal must be scoped to "no evidence", not to "some polls
        // failed" — a single successful earlier observation restores the
        // floor and the sharpening resumes.
        let observations = timeline(&[
            (0, None),
            (10, Some(AlertState::Inactive)),
            (30, Some(AlertState::Firing)),
        ]);
        let refined = refine_firing_timeline(&observations, Some(1020.0), 1000.0);
        assert_eq!(refined[2].at, Duration::from_secs(20));
    }

    #[test]
    fn a_nan_stamp_is_ignored() {
        let refined = refine_firing_timeline(&late_notice(), Some(f64::NAN), 1000.0);
        assert_eq!(refined[2].at, Duration::from_secs(20));
    }

    #[test]
    fn stalled_endpoint_times_out_with_verify_error() {
        // A listener that accepts but never answers must produce a bounded
        // Verify::Query error, not a parked polling thread — same contract
        // the Prometheus client holds.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _keep_alive = std::thread::spawn(move || {
            let _streams: Vec<_> = listener.incoming().take(1).collect();
            std::thread::sleep(Duration::from_secs(30));
        });
        let client = AlertmanagerClient::new(&format!("http://{addr}"), Duration::from_millis(300));
        let started = std::time::Instant::now();
        let result = client.alert_state(&expectation(None));
        assert!(started.elapsed() < Duration::from_secs(5));
        match result {
            Err(SondaError::Verify(VerifyError::Query { .. })) => {}
            other => panic!("expected Verify::Query error, got {other:?}"),
        }
    }
}
