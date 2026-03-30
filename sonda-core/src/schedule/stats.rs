//! Live statistics for a running scenario.

use std::collections::VecDeque;

use serde::Serialize;

use crate::model::metric::MetricEvent;

/// Maximum number of recent metric events buffered for scrape endpoints.
///
/// This bounds memory usage to at most 100 `MetricEvent` clones per scenario.
/// The buffer is a circular deque: when full, the oldest event is evicted.
pub const MAX_RECENT_METRICS: usize = 100;

/// Live statistics for a running scenario, updated by the runner each tick.
///
/// These counters are written by the scenario thread and read by callers
/// (e.g., the CLI display or the HTTP stats endpoint) through a shared
/// [`std::sync::RwLock`]. The write lock is held only for the brief counter
/// update, not during encode/write operations.
///
/// The `recent_metrics` buffer holds the most recent metric events for
/// scrape-based integration (e.g., Prometheus pulling from
/// `GET /scenarios/{id}/metrics`). It is bounded by [`MAX_RECENT_METRICS`].
#[derive(Debug, Clone, Default, Serialize)]
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
    /// Circular buffer of recent metric events for scrape endpoints.
    ///
    /// Bounded by [`MAX_RECENT_METRICS`]. When full, the oldest event is
    /// evicted on the next push. This field is not serialized because
    /// [`MetricEvent`] does not implement `Serialize` and the buffer is
    /// consumed via a dedicated drain method, not via JSON stats responses.
    #[serde(skip)]
    pub recent_metrics: VecDeque<MetricEvent>,
}

impl ScenarioStats {
    /// Push a metric event into the recent-metrics buffer.
    ///
    /// If the buffer is at capacity ([`MAX_RECENT_METRICS`]), the oldest
    /// event is evicted before the new one is inserted.
    pub fn push_metric(&mut self, event: MetricEvent) {
        if self.recent_metrics.len() >= MAX_RECENT_METRICS {
            self.recent_metrics.pop_front();
        }
        self.recent_metrics.push_back(event);
    }

    /// Drain and return all buffered metric events.
    ///
    /// After this call the buffer is empty. The returned events are ordered
    /// oldest-first.
    pub fn drain_recent_metrics(&mut self) -> Vec<MetricEvent> {
        self.recent_metrics.drain(..).collect()
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
    }

    // ---- Contract: Send + Sync ----------------------------------------------

    /// ScenarioStats must be Send + Sync so it can be shared across threads
    /// via Arc<RwLock<ScenarioStats>>.
    #[test]
    fn scenario_stats_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScenarioStats>();
    }

    // ---- recent_metrics buffer: default is empty ----------------------------

    /// Default-constructed stats must have an empty recent_metrics buffer.
    #[test]
    fn default_stats_has_empty_recent_metrics_buffer() {
        let s = ScenarioStats::default();
        assert!(
            s.recent_metrics.is_empty(),
            "recent_metrics buffer must be empty on default construction"
        );
    }

    // ---- Helper: build a MetricEvent for testing ----------------------------

    /// Build a MetricEvent with the given name and value for testing.
    fn make_metric_event(name: &str, value: f64) -> crate::model::metric::MetricEvent {
        crate::model::metric::MetricEvent::new(
            name.to_string(),
            value,
            crate::model::metric::Labels::default(),
        )
        .expect("test metric name must be valid")
    }

    // ---- push_metric: adds events to the deque ------------------------------

    /// push_metric adds a single event to the buffer.
    #[test]
    fn push_metric_adds_event_to_buffer() {
        let mut s = ScenarioStats::default();
        let event = make_metric_event("up", 1.0);
        s.push_metric(event);
        assert_eq!(
            s.recent_metrics.len(),
            1,
            "buffer must contain exactly 1 event after one push"
        );
    }

    /// push_metric preserves insertion order (oldest first).
    #[test]
    fn push_metric_preserves_insertion_order() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 10.0));
        s.push_metric(make_metric_event("up", 20.0));
        s.push_metric(make_metric_event("up", 30.0));

        assert_eq!(s.recent_metrics.len(), 3);
        assert_eq!(
            s.recent_metrics[0].value, 10.0,
            "first event must be the oldest (value=10.0)"
        );
        assert_eq!(
            s.recent_metrics[1].value, 20.0,
            "second event must be value=20.0"
        );
        assert_eq!(
            s.recent_metrics[2].value, 30.0,
            "third event must be the newest (value=30.0)"
        );
    }

    /// push_metric can fill the buffer up to MAX_RECENT_METRICS.
    #[test]
    fn push_metric_fills_buffer_to_max_capacity() {
        let mut s = ScenarioStats::default();
        for i in 0..MAX_RECENT_METRICS {
            s.push_metric(make_metric_event("up", i as f64));
        }
        assert_eq!(
            s.recent_metrics.len(),
            MAX_RECENT_METRICS,
            "buffer must hold exactly MAX_RECENT_METRICS events"
        );
    }

    // ---- push_metric: eviction when full ------------------------------------

    /// When the buffer is full, push_metric evicts the oldest event.
    #[test]
    fn push_metric_evicts_oldest_when_full() {
        let mut s = ScenarioStats::default();
        // Fill to capacity with values 0..MAX_RECENT_METRICS.
        for i in 0..MAX_RECENT_METRICS {
            s.push_metric(make_metric_event("up", i as f64));
        }
        assert_eq!(s.recent_metrics.len(), MAX_RECENT_METRICS);

        // The oldest event has value 0.0.
        assert_eq!(
            s.recent_metrics.front().unwrap().value,
            0.0,
            "oldest event before eviction must be value=0.0"
        );

        // Push one more event.
        s.push_metric(make_metric_event("up", 999.0));

        // Buffer size must not exceed MAX_RECENT_METRICS.
        assert_eq!(
            s.recent_metrics.len(),
            MAX_RECENT_METRICS,
            "buffer must not grow beyond MAX_RECENT_METRICS after eviction"
        );

        // The oldest event (value=0.0) was evicted; now value=1.0 is oldest.
        assert_eq!(
            s.recent_metrics.front().unwrap().value,
            1.0,
            "oldest event after eviction must be value=1.0"
        );

        // The newest event is value=999.0.
        assert_eq!(
            s.recent_metrics.back().unwrap().value,
            999.0,
            "newest event after eviction must be value=999.0"
        );
    }

    /// Multiple evictions work correctly: push MAX + 5 events, oldest 5 are gone.
    #[test]
    fn push_metric_multiple_evictions_discard_oldest() {
        let mut s = ScenarioStats::default();
        let total = MAX_RECENT_METRICS + 5;
        for i in 0..total {
            s.push_metric(make_metric_event("up", i as f64));
        }

        assert_eq!(s.recent_metrics.len(), MAX_RECENT_METRICS);

        // Oldest should be value 5.0 (0..4 evicted).
        assert_eq!(
            s.recent_metrics.front().unwrap().value,
            5.0,
            "after MAX+5 pushes, oldest event must be value=5.0"
        );

        // Newest should be value (total-1) = MAX_RECENT_METRICS + 4.
        assert_eq!(
            s.recent_metrics.back().unwrap().value,
            (total - 1) as f64,
            "newest event must be the last pushed value"
        );
    }

    // ---- drain_recent_metrics: returns all and empties ----------------------

    /// drain_recent_metrics returns all buffered events and empties the deque.
    #[test]
    fn drain_recent_metrics_returns_all_events_and_empties_buffer() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 1.0));
        s.push_metric(make_metric_event("up", 2.0));
        s.push_metric(make_metric_event("up", 3.0));

        let drained = s.drain_recent_metrics();
        assert_eq!(drained.len(), 3, "drain must return all 3 buffered events");
        assert!(
            s.recent_metrics.is_empty(),
            "buffer must be empty after drain"
        );
    }

    /// drain_recent_metrics returns events ordered oldest-first.
    #[test]
    fn drain_recent_metrics_returns_oldest_first_order() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 100.0));
        s.push_metric(make_metric_event("up", 200.0));
        s.push_metric(make_metric_event("up", 300.0));

        let drained = s.drain_recent_metrics();
        assert_eq!(drained[0].value, 100.0, "first drained must be oldest");
        assert_eq!(drained[1].value, 200.0, "second drained must be middle");
        assert_eq!(drained[2].value, 300.0, "third drained must be newest");
    }

    /// drain_recent_metrics on an empty buffer returns an empty Vec.
    #[test]
    fn drain_recent_metrics_on_empty_buffer_returns_empty_vec() {
        let mut s = ScenarioStats::default();
        let drained = s.drain_recent_metrics();
        assert!(
            drained.is_empty(),
            "draining an empty buffer must return an empty Vec"
        );
    }

    /// After draining, pushing new events starts fresh.
    #[test]
    fn drain_then_push_starts_fresh_buffer() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 1.0));
        s.push_metric(make_metric_event("up", 2.0));

        let first_drain = s.drain_recent_metrics();
        assert_eq!(first_drain.len(), 2);
        assert!(s.recent_metrics.is_empty());

        // Push new events after drain.
        s.push_metric(make_metric_event("up", 10.0));
        assert_eq!(s.recent_metrics.len(), 1);

        let second_drain = s.drain_recent_metrics();
        assert_eq!(second_drain.len(), 1);
        assert_eq!(
            second_drain[0].value, 10.0,
            "new event after drain must be retrievable"
        );
    }

    /// Calling drain twice without intermediate pushes returns empty on second call.
    #[test]
    fn drain_twice_returns_empty_on_second_call() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 42.0));

        let first = s.drain_recent_metrics();
        assert_eq!(first.len(), 1);

        let second = s.drain_recent_metrics();
        assert!(
            second.is_empty(),
            "second drain must return empty Vec after first drain consumed all events"
        );
    }

    // ---- recent_metrics is not serialized (serde skip) ----------------------

    /// The recent_metrics field is skipped during JSON serialization.
    #[test]
    fn recent_metrics_buffer_is_not_serialized_to_json() {
        let mut s = ScenarioStats::default();
        s.push_metric(make_metric_event("up", 1.0));
        s.push_metric(make_metric_event("up", 2.0));

        let json = serde_json::to_string(&s).expect("must serialize");
        assert!(
            !json.contains("recent_metrics"),
            "recent_metrics must not appear in JSON output (serde skip): {json}"
        );
    }
}
