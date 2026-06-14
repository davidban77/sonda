//! Live statistics for a running scenario.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use serde::Serialize;

use crate::model::metric::{Labels, MetricEvent, ValidatedMetricName};

/// Dedup key for the close-emit series set: a distinct `(name, labels)` pair.
pub type CloseEmitKey = (ValidatedMetricName, Arc<Labels>);

/// Series identity for the [`ScenarioStats::current_values`] map.
pub type MetricKey = (ValidatedMetricName, Arc<Labels>);

/// How long since the last successful delivery before a failing scenario is
/// considered degraded, in nanoseconds (30 seconds).
pub const DEGRADED_STALENESS_NANOS: u64 = 30 * 1_000_000_000;

/// Lifecycle position of a scenario, surfaced for `while:`-gated runs.
///
/// `Pending` covers the pre-`after:` window for chained scenarios; `Running`
/// and `Paused` reflect the live `while:` gate state; `Held` is the
/// snap-to-frozen variant of `Paused` for scenarios that opted in via
/// `delay.close.snap_to`; `Unresolved` marks a cross-POST `while:` reference
/// whose upstream has not yet registered; `Finished` marks the scenario as
/// having exited (duration expired or shutdown received).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ScenarioState {
    #[default]
    Pending,
    Running,
    Paused,
    /// Frozen at the last emitted value after a gate close, opted in via `delay.close.snap_to`.
    Held,
    Unresolved,
    Finished,
}

impl ScenarioState {
    /// Operational states surfaced as separate gauge rows by the server metrics emitter.
    pub fn operational_states() -> &'static [ScenarioState] {
        &[
            ScenarioState::Pending,
            ScenarioState::Running,
            ScenarioState::Paused,
            ScenarioState::Held,
            ScenarioState::Unresolved,
        ]
    }

    /// Stable lowercase label text for Prometheus exposition.
    pub fn as_label(&self) -> &'static str {
        // Catch-all guards against future #[non_exhaustive] variants added by
        // Phase 5; in-crate the match is exhaustive today.
        #[allow(unreachable_patterns)]
        match self {
            ScenarioState::Pending => "pending",
            ScenarioState::Running => "running",
            ScenarioState::Paused => "paused",
            ScenarioState::Held => "held",
            ScenarioState::Unresolved => "unresolved",
            ScenarioState::Finished => "finished",
            _ => "unknown",
        }
    }
}

/// Live statistics for a running scenario, updated by the runner each tick.
///
/// These counters are written by the scenario thread and read by callers
/// (e.g., the CLI display or the HTTP stats endpoint) through a shared
/// [`std::sync::RwLock`]. The write lock is held only for the brief counter
/// update, not during encode/write operations.
///
/// The `current_values` map holds the current value of each distinct
/// `(name, labels)` series the scenario has emitted. Scrape endpoints render
/// one sample per series from this map.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ScenarioStats {
    /// Total number of events emitted since the scenario started.
    pub total_events: u64,
    /// Total bytes written to the sink since the scenario started.
    pub bytes_emitted: u64,
    /// Measured events per second, updated approximately once per second.
    pub current_rate: f64,
    /// Number of encode or sink write errors encountered.
    pub errors: u64,
    /// Whether the scenario is currently in a gap window (no events emitted).
    pub in_gap: bool,
    /// Whether the scenario is currently in a burst window (elevated rate).
    pub in_burst: bool,
    /// Whether the scenario is currently in a cardinality spike window.
    pub in_cardinality_spike: bool,
    /// Sink failures observed since the most recent successful write.
    pub consecutive_failures: u64,
    /// Lifetime count of sink-write failures (warn policy only).
    pub total_sink_failures: u64,
    /// Most recent sink error message, or `None` if no failure has occurred.
    pub last_sink_error: Option<String>,
    /// Wall-clock time of the most recent successful write, as Unix nanoseconds.
    pub last_successful_write_at: Option<u64>,
    /// Current value of each distinct `(name, labels)` series the runner has
    /// emitted since the scenario started. Overwritten in place on every new
    /// emission for that series; bounded by series cardinality, not by sample
    /// count. A cardinality-spike scenario that emits thousands of distinct
    /// series will grow the map accordingly — that is the spec-correct
    /// behavior for a Prometheus-style exporter.
    #[serde(skip)]
    pub current_values: HashMap<MetricKey, MetricEvent>,
    /// Lifecycle state of the scenario.
    pub state: ScenarioState,
    /// Distinct `(name, labels)` series active since the last gate-close.
    ///
    /// Populated only when `track_close_series` is true; drained on every
    /// `running → paused` close-emit so one recovery marker is emitted per
    /// series. Uncapped — every distinct series must receive a marker.
    #[serde(skip)]
    pub close_emit_series: HashSet<CloseEmitKey>,
    /// Timestamp of the most recent tracked push, used to bump the close
    /// marker timestamp strictly past the last active emission.
    #[serde(skip)]
    pub last_emit_ts: Option<SystemTime>,
    /// Whether the push path should track distinct series for close-emit.
    #[serde(skip)]
    pub track_close_series: bool,
    /// Instant of the most recent [`ScenarioState`] change; resets on every transition.
    #[serde(skip)]
    pub last_state_transition_at: Option<Instant>,
    /// Lifetime count of resolver subscription attempts; persists across state transitions.
    pub cumulative_resolution_attempts: u64,
}

impl ScenarioStats {
    /// Record a metric event as the current value of its `(name, labels)`
    /// series. Overwrites any prior value for that series. When
    /// `track_close_series` is set, the event's series is also recorded for
    /// close-emit.
    pub fn push_metric(&mut self, event: MetricEvent) {
        // Only gated scenarios drain the series set on close; populating it
        // unconditionally would let a non-gated scenario's set grow unbounded.
        if self.track_close_series {
            self.close_emit_series
                .insert((event.name.clone(), Arc::clone(&event.labels)));
            self.last_emit_ts = Some(event.timestamp);
        }
        let key = (event.name.clone(), Arc::clone(&event.labels));
        self.current_values.insert(key, event);
    }

    /// Enable close-emit series tracking. Called once when a close-emitter is
    /// built for the scenario.
    pub fn enable_close_series_tracking(&mut self) {
        self.track_close_series = true;
    }

    /// Move into `new_state` and reset the current-state watermark.
    pub fn transition_state(&mut self, new_state: ScenarioState) {
        self.state = new_state;
        self.last_state_transition_at = Some(Instant::now());
    }

    /// Seconds spent in the current state. Zero if no transition has been recorded.
    pub fn current_state_secs(&self) -> f64 {
        self.last_state_transition_at
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    /// Drain the close-emit series set and reset the emit-timestamp watermark.
    ///
    /// Returns the distinct `(name, labels)` series accumulated since the last
    /// close, paired with the watermark of the most recent tracked push.
    pub fn drain_close_emit_series(&mut self) -> (HashSet<CloseEmitKey>, Option<SystemTime>) {
        let series = std::mem::take(&mut self.close_emit_series);
        let ts = self.last_emit_ts.take();
        (series, ts)
    }

    /// Snapshot the current value of each series, sorted by name then label
    /// pairs so successive calls produce byte-identical output.
    pub fn current_values_snapshot(&self) -> Vec<MetricEvent> {
        let mut events: Vec<MetricEvent> = self.current_values.values().cloned().collect();
        events.sort_by(|a, b| {
            (*a.name)
                .cmp(&b.name)
                .then_with(|| a.labels.iter().cmp(b.labels.iter()))
        });
        events
    }

    /// Whether the scenario looks unhealthy. Returns `true` only when the
    /// scenario has at least one lifetime sink failure AND there is no
    /// recent successful delivery (within [`DEGRADED_STALENESS_NANOS`]). A
    /// scenario that failed once but is now delivering reads `false`.
    /// `now_unix_nanos` is the current time as Unix nanoseconds.
    pub fn is_degraded(&self, now_unix_nanos: u64) -> bool {
        if self.total_sink_failures == 0 {
            return false;
        }
        match self.last_successful_write_at {
            None => true,
            Some(last) => now_unix_nanos.saturating_sub(last) > DEGRADED_STALENESS_NANOS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Default: all counters zero, all flags false -------------------------

    /// Default-constructed stats must have zero counters and false flags.
    #[test]
    fn default_stats_has_zero_counters_and_false_flags() {
        let s = ScenarioStats::default();
        assert_eq!(s.total_events, 0, "total_events must start at zero");
        assert_eq!(s.bytes_emitted, 0, "bytes_emitted must start at zero");
        assert_eq!(s.current_rate, 0.0, "current_rate must start at zero");
        assert_eq!(s.errors, 0, "errors must start at zero");
        assert!(!s.in_gap, "in_gap must start as false");
        assert!(!s.in_burst, "in_burst must start as false");
        assert!(
            !s.in_cardinality_spike,
            "in_cardinality_spike must start as false"
        );
        assert_eq!(s.consecutive_failures, 0);
        assert_eq!(s.total_sink_failures, 0);
        assert!(s.last_sink_error.is_none());
        assert!(s.last_successful_write_at.is_none());
    }

    #[test]
    fn sink_failure_fields_serialize_to_json() {
        let s = ScenarioStats {
            consecutive_failures: 3,
            total_sink_failures: 12,
            last_sink_error: Some("connection refused".to_string()),
            last_successful_write_at: Some(1_700_000_000_000_000_000),
            ..Default::default()
        };
        let json = serde_json::to_string(&s).expect("must serialize");
        assert!(json.contains("\"consecutive_failures\":3"));
        assert!(json.contains("\"total_sink_failures\":12"));
        assert!(json.contains("\"last_sink_error\":\"connection refused\""));
        assert!(json.contains("\"last_successful_write_at\":1700000000000000000"));
    }

    #[test]
    fn last_sink_error_serializes_as_null_when_none() {
        let s = ScenarioStats::default();
        let json = serde_json::to_string(&s).expect("must serialize");
        assert!(json.contains("\"last_sink_error\":null"));
        assert!(json.contains("\"last_successful_write_at\":null"));
    }

    // ---- Clone: produces an independent copy --------------------------------

    /// Cloning stats produces an independent copy — mutations to the clone
    /// do not affect the original.
    #[test]
    fn clone_produces_independent_copy() {
        let original = ScenarioStats {
            total_events: 42,
            bytes_emitted: 1024,
            current_rate: 10.5,
            errors: 3,
            in_gap: true,
            in_burst: false,
            ..Default::default()
        };
        let mut cloned = original.clone();
        cloned.total_events = 99;
        cloned.in_gap = false;

        // Original is unchanged.
        assert_eq!(original.total_events, 42);
        assert!(original.in_gap);
        // Clone holds the new values.
        assert_eq!(cloned.total_events, 99);
        assert!(!cloned.in_gap);
    }

    // ---- Debug: can be formatted without panicking --------------------------

    #[test]
    fn debug_format_contains_struct_name() {
        let s = ScenarioStats::default();
        let debug_str = format!("{s:?}");
        assert!(
            debug_str.contains("ScenarioStats"),
            "Debug output must name the struct, got: {debug_str}"
        );
    }

    // ---- Serialize: fields appear in JSON output ----------------------------

    /// Verifying Serialize works by round-tripping through serde_json.
    #[test]
    #[allow(clippy::approx_constant)] // 3.14 is a sample rate value, not the PI constant
    fn serializes_to_json_with_all_fields_present() {
        let s = ScenarioStats {
            total_events: 7,
            bytes_emitted: 512,
            current_rate: 3.14,
            errors: 1,
            in_gap: false,
            in_burst: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&s).expect("ScenarioStats must serialize to JSON");
        assert!(
            json.contains("\"total_events\""),
            "JSON must contain total_events"
        );
        assert!(
            json.contains("\"bytes_emitted\""),
            "JSON must contain bytes_emitted"
        );
        assert!(
            json.contains("\"current_rate\""),
            "JSON must contain current_rate"
        );
        assert!(json.contains("\"errors\""), "JSON must contain errors");
        assert!(json.contains("\"in_gap\""), "JSON must contain in_gap");
        assert!(json.contains("\"in_burst\""), "JSON must contain in_burst");
        assert!(
            json.contains("\"in_cardinality_spike\""),
            "JSON must contain in_cardinality_spike"
        );
    }

    // ---- Contract: Send + Sync ----------------------------------------------

    /// ScenarioStats must be Send + Sync so it can be shared across threads
    /// via Arc<RwLock<ScenarioStats>>.
    #[test]
    fn scenario_stats_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScenarioStats>();
    }

    #[test]
    fn default_stats_has_empty_current_values_map() {
        let s = ScenarioStats::default();
        assert!(s.current_values.is_empty());
    }

    fn make_metric_event(name: &str, value: f64) -> crate::model::metric::MetricEvent {
        crate::model::metric::MetricEvent::new(
            name.to_string(),
            value,
            crate::model::metric::Labels::default(),
        )
        .expect("test metric name must be valid")
    }

    fn make_labeled_event(
        name: &str,
        value: f64,
        pairs: &[(&str, &str)],
    ) -> crate::model::metric::MetricEvent {
        let labels = crate::model::metric::Labels::from_pairs(pairs).expect("labels must build");
        crate::model::metric::MetricEvent::new(name.to_string(), value, labels)
            .expect("test metric name must be valid")
    }

    #[test]
    fn push_metric_inserts_new_series_into_current_values() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 1.0));
        assert_eq!(s.current_values.len(), 1);
    }

    #[test]
    fn push_metric_overwrites_existing_series_with_latest_value() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 1.0));
        s.push_metric(make_metric_event("up", 2.0));
        assert_eq!(s.current_values.len(), 1);
        let snap = s.current_values_snapshot();
        assert_eq!(snap[0].value, 2.0);
    }

    #[test]
    fn push_metric_distinguishes_different_label_sets() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_labeled_event("up", 1.0, &[("host", "a")]));
        s.push_metric(make_labeled_event("up", 2.0, &[("host", "b")]));
        assert_eq!(s.current_values.len(), 2);
    }

    #[test]
    fn current_values_snapshot_returns_one_event_per_series() {
        let mut s = ScenarioStats::default();
        for i in 0..100 {
            s.push_metric(make_metric_event("up", i as f64));
        }
        let snap = s.current_values_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].value, 99.0);
    }

    #[test]
    fn current_values_snapshot_is_deterministic_across_calls() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_labeled_event("up", 1.0, &[("host", "c")]));
        s.push_metric(make_labeled_event("up", 2.0, &[("host", "a")]));
        s.push_metric(make_labeled_event("up", 3.0, &[("host", "b")]));
        s.push_metric(make_labeled_event("down", 9.0, &[]));

        let first = s.current_values_snapshot();
        let second = s.current_values_snapshot();
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(&*a.name, &*b.name);
            assert!(a.labels.iter().eq(b.labels.iter()));
            assert_eq!(a.value, b.value);
        }

        let names: Vec<&str> = first.iter().map(|e| &*e.name).collect();
        assert_eq!(names, vec!["down", "up", "up", "up"]);
        let up_hosts: Vec<&str> = first
            .iter()
            .filter(|e| &*e.name == "up")
            .map(|e| {
                e.labels
                    .iter()
                    .next()
                    .map(|(_, v)| v)
                    .expect("label must exist")
            })
            .collect();
        assert_eq!(up_hosts, vec!["a", "b", "c"]);
    }

    #[test]
    fn current_values_grows_with_distinct_series_no_cap() {
        let mut s = ScenarioStats::default();
        for i in 0..200 {
            s.push_metric(make_labeled_event(
                "up",
                i as f64,
                &[("host", &format!("h{i}"))],
            ));
        }
        assert_eq!(s.current_values.len(), 200);
    }

    #[test]
    fn current_values_snapshot_on_empty_stats_returns_empty_vec() {
        let s = ScenarioStats::default();
        assert!(s.current_values_snapshot().is_empty());
    }

    #[test]
    fn current_values_is_not_serialized_to_json() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 1.0));
        let json = serde_json::to_string(&s).expect("must serialize");
        assert!(!json.contains("current_values"));
    }

    // ---- is_degraded --------------------------------------------------------

    // ---- close_emit_series: conditional population ---------------------------

    fn make_series_event(name: &str, host: &str) -> MetricEvent {
        crate::model::metric::MetricEvent::new(
            name.to_string(),
            1.0,
            crate::model::metric::Labels::from_pairs(&[("host", host)]).unwrap(),
        )
        .expect("test metric name must be valid")
    }

    #[test]
    fn push_metric_does_not_track_series_when_flag_off() {
        let mut s = ScenarioStats::default();
        for i in 0..10 {
            s.push_metric(make_series_event("up", &format!("h{i}")));
        }
        assert!(
            s.close_emit_series.is_empty(),
            "non-gated scenario must not accumulate the series set"
        );
        assert!(s.last_emit_ts.is_none(), "watermark must stay unset");
        assert_eq!(
            s.current_values.len(),
            10,
            "scrape map must hold one entry per distinct series"
        );
    }

    #[test]
    fn push_metric_tracks_series_when_flag_on() {
        let mut s = ScenarioStats::default();
        s.enable_close_series_tracking();
        s.push_metric(make_series_event("up", "a"));
        s.push_metric(make_series_event("up", "b"));
        assert_eq!(
            s.close_emit_series.len(),
            2,
            "two distinct series must be tracked"
        );
        assert!(s.last_emit_ts.is_some(), "watermark must be set");
    }

    #[test]
    fn push_metric_dedups_repeated_series_in_set() {
        let mut s = ScenarioStats::default();
        s.enable_close_series_tracking();
        for _ in 0..25 {
            s.push_metric(make_series_event("up", "a"));
        }
        assert_eq!(
            s.close_emit_series.len(),
            1,
            "the same (name, labels) series must dedup to one entry"
        );
    }

    #[test]
    fn series_set_grows_with_distinct_series_uncapped() {
        let mut s = ScenarioStats::default();
        s.enable_close_series_tracking();
        let count = 400;
        for i in 0..count {
            s.push_metric(make_series_event("up", &format!("h{i}")));
        }
        assert_eq!(s.close_emit_series.len(), count);
        assert_eq!(s.current_values.len(), count);
    }

    #[test]
    fn drain_close_emit_series_empties_set_and_resets_watermark() {
        let mut s = ScenarioStats::default();
        s.enable_close_series_tracking();
        s.push_metric(make_series_event("up", "a"));
        s.push_metric(make_series_event("up", "b"));

        let (series, ts) = s.drain_close_emit_series();
        assert_eq!(series.len(), 2);
        assert!(ts.is_some(), "drain must return the watermark");
        assert!(
            s.close_emit_series.is_empty(),
            "set must be empty after drain"
        );
        assert!(s.last_emit_ts.is_none(), "watermark must reset after drain");
    }

    #[test]
    fn drain_then_push_starts_a_fresh_window() {
        let mut s = ScenarioStats::default();
        s.enable_close_series_tracking();
        s.push_metric(make_series_event("up", "a"));
        let (first, _) = s.drain_close_emit_series();
        assert_eq!(first.len(), 1);

        s.push_metric(make_series_event("up", "b"));
        let (second, _) = s.drain_close_emit_series();
        assert_eq!(second.len(), 1, "next window starts fresh after drain");
    }

    #[test]
    fn last_emit_ts_tracks_the_most_recent_push() {
        use std::time::{Duration, UNIX_EPOCH};
        let mut s = ScenarioStats::default();
        s.enable_close_series_tracking();
        let name = ValidatedMetricName::new("up").unwrap();
        let early = UNIX_EPOCH + Duration::from_secs(1_000);
        let late = UNIX_EPOCH + Duration::from_secs(2_000);
        s.push_metric(MetricEvent::from_parts(
            name.clone(),
            1.0,
            Arc::new(Labels::default()),
            early,
        ));
        s.push_metric(MetricEvent::from_parts(
            name,
            1.0,
            Arc::new(Labels::from_pairs(&[("host", "z")]).unwrap()),
            late,
        ));
        assert_eq!(
            s.last_emit_ts,
            Some(late),
            "watermark must equal the most recent push timestamp"
        );
    }

    #[test]
    fn close_emit_fields_are_not_serialized_to_json() {
        let mut s = ScenarioStats::default();
        s.enable_close_series_tracking();
        s.push_metric(make_series_event("up", "a"));
        let json = serde_json::to_string(&s).expect("must serialize");
        assert!(!json.contains("close_emit_series"));
        assert!(!json.contains("last_emit_ts"));
        assert!(!json.contains("track_close_series"));
    }

    #[test]
    fn is_degraded_false_when_no_sink_failures() {
        let s = ScenarioStats {
            total_sink_failures: 0,
            last_successful_write_at: None,
            ..Default::default()
        };
        assert!(!s.is_degraded(0));
        assert!(!s.is_degraded(u64::MAX));
    }

    #[test]
    fn is_degraded_false_with_failures_but_recent_delivery() {
        let now = 1_000_000_000_000_000;
        let s = ScenarioStats {
            total_sink_failures: 5,
            last_successful_write_at: Some(now - 1_000_000_000),
            ..Default::default()
        };
        assert!(!s.is_degraded(now));
    }

    #[test]
    fn is_degraded_true_with_failures_and_stale_delivery() {
        let now = 1_000_000_000_000_000;
        let s = ScenarioStats {
            total_sink_failures: 5,
            last_successful_write_at: Some(now - DEGRADED_STALENESS_NANOS - 1),
            ..Default::default()
        };
        assert!(s.is_degraded(now));
    }

    #[test]
    fn is_degraded_true_with_failures_and_no_delivery_ever() {
        let s = ScenarioStats {
            total_sink_failures: 1,
            last_successful_write_at: None,
            ..Default::default()
        };
        assert!(s.is_degraded(1_000_000_000_000_000));
    }

    #[test]
    fn held_state_serializes_as_lowercase_held() {
        let s = ScenarioStats {
            state: ScenarioState::Held,
            ..Default::default()
        };
        let json = serde_json::to_string(&s).expect("must serialize");
        assert!(json.contains("\"state\":\"held\""));
    }

    #[test]
    fn held_state_is_distinct_from_paused() {
        assert_ne!(ScenarioState::Held, ScenarioState::Paused);
    }

    #[test]
    fn is_degraded_false_under_clock_skew() {
        let last = 1_000_000_000_000_000;
        let s = ScenarioStats {
            total_sink_failures: 5,
            last_successful_write_at: Some(last),
            ..Default::default()
        };
        assert!(!s.is_degraded(last - 1_000_000_000));
    }

    #[test]
    fn operational_states_returns_five_states_in_documented_order() {
        let states = ScenarioState::operational_states();
        assert_eq!(states.len(), 5);
        assert_eq!(states[0], ScenarioState::Pending);
        assert_eq!(states[1], ScenarioState::Running);
        assert_eq!(states[2], ScenarioState::Paused);
        assert_eq!(states[3], ScenarioState::Held);
        assert_eq!(states[4], ScenarioState::Unresolved);
    }

    #[test]
    fn operational_states_excludes_finished() {
        let states = ScenarioState::operational_states();
        assert!(!states.contains(&ScenarioState::Finished));
    }

    #[test]
    fn as_label_maps_every_variant_to_lowercase_text() {
        assert_eq!(ScenarioState::Pending.as_label(), "pending");
        assert_eq!(ScenarioState::Running.as_label(), "running");
        assert_eq!(ScenarioState::Paused.as_label(), "paused");
        assert_eq!(ScenarioState::Held.as_label(), "held");
        assert_eq!(ScenarioState::Unresolved.as_label(), "unresolved");
        assert_eq!(ScenarioState::Finished.as_label(), "finished");
    }

    #[test]
    fn as_label_matches_serde_lowercase_rename() {
        for state in [
            ScenarioState::Pending,
            ScenarioState::Running,
            ScenarioState::Paused,
            ScenarioState::Held,
            ScenarioState::Unresolved,
            ScenarioState::Finished,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let serialized = json.trim_matches('"').to_string();
            assert_eq!(
                serialized,
                state.as_label(),
                "as_label must match serde rename for {state:?}"
            );
        }
    }
}
