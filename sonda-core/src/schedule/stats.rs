//! Live statistics for a running scenario.

use serde::Serialize;

/// Live statistics for a running scenario, updated by the runner each tick.
///
/// These counters are written by the scenario thread and read by callers
/// (e.g., the CLI display or the HTTP stats endpoint) through a shared
/// [`std::sync::RwLock`]. The write lock is held only for the brief counter
/// update, not during encode/write operations.
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
}
