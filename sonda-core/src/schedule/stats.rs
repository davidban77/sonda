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
}
