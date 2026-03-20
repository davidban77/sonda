//! Scheduling: rate control, duration, gap windows, burst windows.
//!
//! The scheduler controls *when* events are emitted. It does not know
//! *what* is being emitted — that is the generator and encoder's job.

pub mod runner;

use std::time::Duration;

/// Configuration for a gap window (intentional silent period).
#[derive(Debug, Clone)]
pub struct GapWindow {
    /// How often a gap occurs (e.g., every 2 minutes).
    pub every: Duration,
    /// How long the gap lasts (e.g., 20 seconds).
    pub duration: Duration,
}

/// Configuration for a burst window (high-rate period).
#[derive(Debug, Clone)]
pub struct BurstWindow {
    /// How often a burst occurs.
    pub every: Duration,
    /// How long the burst lasts.
    pub duration: Duration,
    /// Rate multiplier during the burst.
    pub multiplier: f64,
}

/// Schedule configuration for a scenario.
#[derive(Debug, Clone)]
pub struct Schedule {
    /// Target events per second.
    pub rate: f64,
    /// Total run duration. None means run indefinitely.
    pub duration: Option<Duration>,
    /// Optional recurring gap window.
    pub gap: Option<GapWindow>,
    /// Optional recurring burst window (post-MVP).
    pub burst: Option<BurstWindow>,
}

/// Returns `true` if the scheduler should be in a gap at the given elapsed time.
///
/// Gap windows are periodic. The gap occupies the tail of each cycle:
/// from `(every - duration)` to `every`. For example, with `every=10s` and
/// `duration=2s`, the gap is active during seconds 8–10 of each cycle.
///
/// # Arguments
///
/// * `elapsed` — time since the scenario started.
/// * `gap` — the gap window configuration.
pub fn is_in_gap(elapsed: Duration, gap: &GapWindow) -> bool {
    let every_secs = gap.every.as_secs_f64();
    let duration_secs = gap.duration.as_secs_f64();
    // Position within the current cycle [0, every).
    let cycle_pos = elapsed.as_secs_f64() % every_secs;
    // Gap occupies the end of each cycle: [every - duration, every).
    cycle_pos >= every_secs - duration_secs
}

/// Returns how long until the current gap ends.
///
/// This function assumes the caller has already verified that `elapsed` is
/// within a gap (i.e., [`is_in_gap`] returned `true`). The returned `Duration`
/// is the amount of time to sleep before the next event cycle begins.
///
/// # Arguments
///
/// * `elapsed` — time since the scenario started.
/// * `gap` — the gap window configuration.
pub fn time_until_gap_end(elapsed: Duration, gap: &GapWindow) -> Duration {
    let every_secs = gap.every.as_secs_f64();
    let cycle_pos = elapsed.as_secs_f64() % every_secs;
    let remaining_secs = every_secs - cycle_pos;
    // Guard against floating point producing a tiny negative or zero value.
    if remaining_secs <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(remaining_secs)
    }
}
