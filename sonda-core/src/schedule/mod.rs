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
///
/// This struct is defined here for future use by the runner and any caller that
/// needs to inspect or serialize the resolved schedule. It is not yet consumed
/// by the runner (which reads directly from `ScenarioConfig`); the runner will
/// be refactored to accept a `Schedule` in a later slice.
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a GapWindow for testing.
    fn gap(every_secs: u64, duration_secs: u64) -> GapWindow {
        GapWindow {
            every: Duration::from_secs(every_secs),
            duration: Duration::from_secs(duration_secs),
        }
    }

    // ---- is_in_gap: spec-mandated cases (gap_every=10s, gap_for=2s) ----------

    /// At elapsed=0s we are at the start of a cycle — not in a gap.
    #[test]
    fn is_in_gap_at_zero_is_false() {
        let g = gap(10, 2);
        assert!(!is_in_gap(Duration::from_secs(0), &g));
    }

    /// The gap starts at (every - duration) = 8s. At 8.5s we are inside the gap.
    #[test]
    fn is_in_gap_at_8_5s_is_true() {
        let g = gap(10, 2);
        assert!(is_in_gap(Duration::from_millis(8500), &g));
    }

    /// Exactly at the start of the gap boundary (8.0s) should be in-gap.
    #[test]
    fn is_in_gap_at_exact_gap_start_is_true() {
        let g = gap(10, 2);
        assert!(is_in_gap(Duration::from_secs(8), &g));
    }

    /// At 10s (cycle_pos == 0.0) we are at the start of a new cycle — not in gap.
    #[test]
    fn is_in_gap_at_10s_new_cycle_is_false() {
        let g = gap(10, 2);
        assert!(!is_in_gap(Duration::from_secs(10), &g));
    }

    /// At 18.5s we are in the second cycle, 8.5s into it — inside the gap.
    #[test]
    fn is_in_gap_at_18_5s_second_cycle_is_true() {
        let g = gap(10, 2);
        assert!(is_in_gap(Duration::from_millis(18500), &g));
    }

    /// At 20s we are at the start of the third cycle — not in gap.
    #[test]
    fn is_in_gap_at_20s_third_cycle_start_is_false() {
        let g = gap(10, 2);
        assert!(!is_in_gap(Duration::from_secs(20), &g));
    }

    /// At 5s in a 10s/2s gap we are mid-cycle before the gap.
    #[test]
    fn is_in_gap_at_5s_is_false() {
        let g = gap(10, 2);
        assert!(!is_in_gap(Duration::from_secs(5), &g));
    }

    /// A very short gap_for of 1ms at cycle_pos just before the end.
    #[test]
    fn is_in_gap_sub_millisecond_gap_duration() {
        let g = GapWindow {
            every: Duration::from_secs(10),
            duration: Duration::from_millis(1),
        };
        // At 9.9995s (9999.5ms into a 10000ms cycle) — should be in gap.
        assert!(is_in_gap(Duration::from_millis(9999), &g));
        // At 5s — should not be in gap.
        assert!(!is_in_gap(Duration::from_secs(5), &g));
    }

    /// Validates that is_in_gap works correctly with minute-scale durations
    /// (e.g., gap_every=2m, gap_for=20s from the architecture example).
    #[test]
    fn is_in_gap_minute_scale_cycle() {
        // every=120s, duration=20s → gap from 100s to 120s in each cycle.
        let g = GapWindow {
            every: Duration::from_secs(120),
            duration: Duration::from_secs(20),
        };
        assert!(!is_in_gap(Duration::from_secs(0), &g));
        assert!(!is_in_gap(Duration::from_secs(50), &g));
        assert!(!is_in_gap(Duration::from_secs(99), &g));
        assert!(is_in_gap(Duration::from_secs(100), &g));
        assert!(is_in_gap(Duration::from_secs(110), &g));
        assert!(is_in_gap(Duration::from_secs(119), &g));
        // At exactly 120s we are at the start of cycle 2 — not in gap.
        assert!(!is_in_gap(Duration::from_secs(120), &g));
    }

    // ---- time_until_gap_end: spec-mandated cases -----------------------------

    /// During a gap at elapsed=9s with every=10s: 1s remains until cycle end.
    #[test]
    fn time_until_gap_end_at_9s_returns_1s() {
        let g = gap(10, 2);
        let remaining = time_until_gap_end(Duration::from_secs(9), &g);
        // Allow for floating-point imprecision: within 1ms of 1s.
        let diff = (remaining.as_secs_f64() - 1.0).abs();
        assert!(
            diff < 0.001,
            "expected ~1s remaining, got {remaining:?} (diff={diff})"
        );
    }

    /// At gap start (8.0s) with every=10s, gap_for=2s: 2s remain.
    #[test]
    fn time_until_gap_end_at_gap_start_returns_gap_duration() {
        let g = gap(10, 2);
        let remaining = time_until_gap_end(Duration::from_secs(8), &g);
        let diff = (remaining.as_secs_f64() - 2.0).abs();
        assert!(
            diff < 0.001,
            "expected ~2s remaining, got {remaining:?} (diff={diff})"
        );
    }

    /// Very close to cycle boundary: remaining time is close to zero but not negative.
    #[test]
    fn time_until_gap_end_near_cycle_boundary_is_non_negative() {
        let g = gap(10, 2);
        // 9.999s into the cycle — only 1ms to go.
        let remaining = time_until_gap_end(Duration::from_millis(9999), &g);
        assert!(
            remaining >= Duration::ZERO,
            "remaining must never be negative"
        );
        assert!(
            remaining.as_millis() <= 2,
            "expected ~1ms, got {remaining:?}"
        );
    }

    /// In the second cycle at 18s (= 8s into cycle 2), remaining should be ~2s.
    #[test]
    fn time_until_gap_end_second_cycle_at_18s() {
        let g = gap(10, 2);
        let remaining = time_until_gap_end(Duration::from_secs(18), &g);
        let diff = (remaining.as_secs_f64() - 2.0).abs();
        assert!(
            diff < 0.001,
            "expected ~2s remaining in second cycle, got {remaining:?}"
        );
    }

    // ---- Rate math -----------------------------------------------------------

    /// Rate=1000 → inter-event interval = 1ms.
    #[test]
    fn rate_1000_yields_1ms_interval() {
        let interval = Duration::from_secs_f64(1.0 / 1000.0);
        assert_eq!(interval.as_millis(), 1);
    }

    /// Rate=1 → inter-event interval = 1s.
    #[test]
    fn rate_1_yields_1s_interval() {
        let interval = Duration::from_secs_f64(1.0 / 1.0);
        assert_eq!(interval.as_secs(), 1);
    }

    /// Rate=0.5 → inter-event interval = 2s.
    #[test]
    fn rate_0_5_yields_2s_interval() {
        let interval = Duration::from_secs_f64(1.0 / 0.5);
        assert_eq!(interval.as_secs(), 2);
    }

    // ---- GapWindow: Clone and Debug contracts --------------------------------

    #[test]
    fn gap_window_is_cloneable() {
        let g = gap(10, 2);
        let cloned = g.clone();
        assert_eq!(cloned.every, Duration::from_secs(10));
        assert_eq!(cloned.duration, Duration::from_secs(2));
    }

    #[test]
    fn gap_window_is_debuggable() {
        let g = gap(10, 2);
        let s = format!("{g:?}");
        assert!(s.contains("GapWindow"), "Debug output must name the struct");
    }

    // ---- Schedule: Clone and Debug contracts ---------------------------------

    #[test]
    fn schedule_is_cloneable_and_debuggable() {
        let sched = Schedule {
            rate: 100.0,
            duration: Some(Duration::from_secs(30)),
            gap: Some(gap(10, 2)),
            burst: None,
        };
        let cloned = sched.clone();
        assert_eq!(cloned.rate, 100.0);
        let s = format!("{sched:?}");
        assert!(s.contains("Schedule"));
    }
}
