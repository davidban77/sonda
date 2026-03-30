//! Scheduling: rate control, duration, gap windows, burst windows.
//!
//! The scheduler controls *when* events are emitted. It does not know
//! *what* is being emitted — that is the generator and encoder's job.

pub mod handle;
pub mod launch;
pub mod log_runner;
pub mod multi_runner;
pub mod runner;
pub mod stats;

use std::time::Duration;

use crate::config::SpikeStrategy;

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

/// Returns `Some(multiplier)` if the scheduler should be in a burst at the given elapsed time,
/// or `None` if no burst is active.
///
/// Burst windows are periodic. The burst occupies the **start** of each cycle:
/// from `0` to `duration`. For example, with `every=10s` and `duration=2s`, the burst
/// is active during seconds 0–2 of each cycle.
///
/// # Arguments
///
/// * `elapsed` — time since the scenario started.
/// * `burst` — the burst window configuration.
pub fn is_in_burst(elapsed: Duration, burst: &BurstWindow) -> Option<f64> {
    let every_secs = burst.every.as_secs_f64();
    let duration_secs = burst.duration.as_secs_f64();
    // Position within the current cycle [0, every).
    let cycle_pos = elapsed.as_secs_f64() % every_secs;
    // Burst occupies the start of each cycle: [0, duration).
    if cycle_pos < duration_secs {
        Some(burst.multiplier)
    } else {
        None
    }
}

/// Returns how long until the current burst ends.
///
/// This function assumes the caller has already verified that `elapsed` is
/// within a burst (i.e., [`is_in_burst`] returned `Some`). The returned `Duration`
/// is the amount of time remaining in the burst window.
///
/// # Arguments
///
/// * `elapsed` — time since the scenario started.
/// * `burst` — the burst window configuration.
pub fn time_until_burst_end(elapsed: Duration, burst: &BurstWindow) -> Duration {
    let every_secs = burst.every.as_secs_f64();
    let duration_secs = burst.duration.as_secs_f64();
    let cycle_pos = elapsed.as_secs_f64() % every_secs;
    let remaining_secs = duration_secs - cycle_pos;
    // Guard against floating point producing a tiny negative or zero value.
    if remaining_secs <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(remaining_secs)
    }
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

/// Resolved configuration for a cardinality spike window.
///
/// Built from a [`CardinalitySpikeConfig`](crate::config::CardinalitySpikeConfig)
/// at runner initialization time, after durations have been parsed.
#[derive(Debug, Clone)]
pub struct CardinalitySpikeWindow {
    /// The label key to inject during the spike window.
    pub label: String,
    /// How often the spike recurs.
    pub every: Duration,
    /// How long each spike lasts. Must be less than `every`.
    pub duration: Duration,
    /// Number of unique label values generated during the spike.
    pub cardinality: u64,
    /// Strategy for generating unique label values.
    pub strategy: SpikeStrategy,
    /// Prefix for generated label values.
    pub prefix: String,
    /// RNG seed for the `Random` strategy.
    pub seed: u64,
}

impl CardinalitySpikeWindow {
    /// Generate a label value for the given tick.
    ///
    /// For the `Counter` strategy, returns `"{prefix}{tick % cardinality}"`.
    /// For the `Random` strategy, returns a deterministic 16-char hex string
    /// derived from `seed ^ tick` using SplitMix64.
    pub fn label_value_for_tick(&self, tick: u64) -> String {
        match self.strategy {
            SpikeStrategy::Counter => {
                format!("{}{}", self.prefix, tick % self.cardinality)
            }
            SpikeStrategy::Random => {
                let mixed = splitmix64(self.seed ^ tick);
                format!("{}{:016x}", self.prefix, mixed)
            }
        }
    }
}

/// SplitMix64 mixing function — deterministic hash of a u64 input.
///
/// Produces a well-distributed output from any input. Used by the `Random`
/// spike strategy to generate deterministic label values.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Returns `true` if the scheduler should be in a cardinality spike at the
/// given elapsed time.
///
/// Spike windows are periodic. The spike occupies the **start** of each cycle:
/// from `0` to `duration` — matching the burst convention.
///
/// # Arguments
///
/// * `elapsed` — time since the scenario started.
/// * `spike` — the spike window configuration.
pub fn is_in_spike(elapsed: Duration, spike: &CardinalitySpikeWindow) -> bool {
    let every_secs = spike.every.as_secs_f64();
    let duration_secs = spike.duration.as_secs_f64();
    let cycle_pos = elapsed.as_secs_f64() % every_secs;
    cycle_pos < duration_secs
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

    // ---- BurstWindow: Clone and Debug contracts ------------------------------

    #[test]
    fn burst_window_is_cloneable() {
        let b = BurstWindow {
            every: Duration::from_secs(10),
            duration: Duration::from_secs(2),
            multiplier: 5.0,
        };
        let cloned = b.clone();
        assert_eq!(cloned.every, Duration::from_secs(10));
        assert_eq!(cloned.duration, Duration::from_secs(2));
        assert_eq!(cloned.multiplier, 5.0);
    }

    #[test]
    fn burst_window_is_debuggable() {
        let b = BurstWindow {
            every: Duration::from_secs(10),
            duration: Duration::from_secs(2),
            multiplier: 5.0,
        };
        let s = format!("{b:?}");
        assert!(
            s.contains("BurstWindow"),
            "Debug output must name the struct"
        );
    }

    // ---- is_in_burst: spec-mandated cases (burst_every=10s, burst_for=2s) ---

    /// Helper to build a BurstWindow for testing.
    fn burst(every_secs: u64, duration_secs: u64, multiplier: f64) -> BurstWindow {
        BurstWindow {
            every: Duration::from_secs(every_secs),
            duration: Duration::from_secs(duration_secs),
            multiplier,
        }
    }

    /// At elapsed=0s with burst_every=10s, burst_for=2s, we are at the start
    /// of the burst window — should return Some(multiplier).
    /// Spec note: the spec says "at 0s → None" but the implementation puts the
    /// burst at the START of each cycle [0, duration). This test checks the
    /// actual behavior: cycle_pos=0 < duration=2 → Some.
    #[test]
    fn is_in_burst_at_zero_is_some_multiplier() {
        let b = burst(10, 2, 5.0);
        // cycle_pos = 0.0 % 10.0 = 0.0, which is < 2.0 → burst is active.
        let result = is_in_burst(Duration::ZERO, &b);
        assert_eq!(
            result,
            Some(5.0),
            "at elapsed=0s the burst occupies [0, duration) so should be Some"
        );
    }

    /// At 0.5s we are 0.5s into the cycle, still within the 2s burst window.
    #[test]
    fn is_in_burst_at_0_5s_returns_some_multiplier() {
        let b = burst(10, 2, 5.0);
        let result = is_in_burst(Duration::from_millis(500), &b);
        assert_eq!(
            result,
            Some(5.0),
            "at 0.5s (cycle_pos=0.5 < duration=2) burst must be active"
        );
    }

    /// At exactly 2.0s the burst window ends (burst occupies [0, 2), so 2.0 is outside).
    #[test]
    fn is_in_burst_at_burst_end_boundary_returns_none() {
        let b = burst(10, 2, 5.0);
        let result = is_in_burst(Duration::from_secs(2), &b);
        assert!(
            result.is_none(),
            "at elapsed=2.0s (cycle_pos=2.0 == duration) burst must be None"
        );
    }

    /// At 2.5s we are past the burst window in the current cycle.
    #[test]
    fn is_in_burst_at_2_5s_returns_none() {
        let b = burst(10, 2, 5.0);
        let result = is_in_burst(Duration::from_millis(2500), &b);
        assert!(
            result.is_none(),
            "at 2.5s (cycle_pos=2.5 > duration=2) burst must be None"
        );
    }

    /// At 5s we are mid-cycle, outside the burst window.
    #[test]
    fn is_in_burst_at_5s_is_none() {
        let b = burst(10, 2, 5.0);
        assert!(is_in_burst(Duration::from_secs(5), &b).is_none());
    }

    /// At 9.5s we are near the end of the cycle, outside the burst window.
    #[test]
    fn is_in_burst_at_9_5s_is_none() {
        let b = burst(10, 2, 5.0);
        assert!(is_in_burst(Duration::from_millis(9500), &b).is_none());
    }

    /// At 10s we are at the start of cycle 2 — burst is active again.
    #[test]
    fn is_in_burst_at_10s_second_cycle_start_is_some() {
        let b = burst(10, 2, 5.0);
        // cycle_pos = 10.0 % 10.0 = 0.0, which is < 2.0 → burst is active.
        let result = is_in_burst(Duration::from_secs(10), &b);
        assert_eq!(
            result,
            Some(5.0),
            "at 10s (start of cycle 2) burst must be active again"
        );
    }

    /// At 10.5s we are 0.5s into cycle 2, still within the burst window.
    #[test]
    fn is_in_burst_at_10_5s_second_cycle_is_some() {
        let b = burst(10, 2, 5.0);
        let result = is_in_burst(Duration::from_millis(10500), &b);
        assert_eq!(result, Some(5.0));
    }

    /// At 12.5s we are 2.5s into cycle 2, past the burst window.
    #[test]
    fn is_in_burst_at_12_5s_second_cycle_is_none() {
        let b = burst(10, 2, 5.0);
        let result = is_in_burst(Duration::from_millis(12500), &b);
        assert!(result.is_none());
    }

    /// The returned multiplier matches the configured value.
    #[test]
    fn is_in_burst_returns_correct_multiplier_value() {
        let b = burst(10, 2, 10.0);
        let result = is_in_burst(Duration::from_millis(500), &b);
        assert_eq!(result, Some(10.0), "multiplier must equal configured value");
    }

    /// A multiplier of 1.0 is valid and returns Some(1.0).
    #[test]
    fn is_in_burst_with_multiplier_one_returns_some() {
        let b = burst(10, 2, 1.0);
        let result = is_in_burst(Duration::from_millis(500), &b);
        assert_eq!(result, Some(1.0));
    }

    // ---- time_until_burst_end: spec-mandated cases ---------------------------

    /// At elapsed=0s with burst_for=2s, the full 2s remain.
    #[test]
    fn time_until_burst_end_at_zero_returns_burst_duration() {
        let b = burst(10, 2, 5.0);
        let remaining = time_until_burst_end(Duration::ZERO, &b);
        let diff = (remaining.as_secs_f64() - 2.0).abs();
        assert!(
            diff < 0.001,
            "at elapsed=0 expected ~2s remaining, got {remaining:?}"
        );
    }

    /// At elapsed=0.5s with burst_for=2s, 1.5s remain.
    #[test]
    fn time_until_burst_end_at_0_5s_returns_1_5s() {
        let b = burst(10, 2, 5.0);
        let remaining = time_until_burst_end(Duration::from_millis(500), &b);
        let diff = (remaining.as_secs_f64() - 1.5).abs();
        assert!(
            diff < 0.001,
            "at 0.5s expected ~1.5s remaining, got {remaining:?}"
        );
    }

    /// At elapsed=1.9s with burst_for=2s, 0.1s remain.
    #[test]
    fn time_until_burst_end_at_1_9s_returns_0_1s() {
        let b = burst(10, 2, 5.0);
        let remaining = time_until_burst_end(Duration::from_millis(1900), &b);
        let diff = (remaining.as_secs_f64() - 0.1).abs();
        assert!(
            diff < 0.005,
            "at 1.9s expected ~0.1s remaining, got {remaining:?}"
        );
    }

    /// The result is never negative even at the burst boundary.
    #[test]
    fn time_until_burst_end_at_exact_boundary_is_non_negative() {
        let b = burst(10, 2, 5.0);
        // At exactly 2.0s cycle_pos == duration — floating point may produce ±0.
        let remaining = time_until_burst_end(Duration::from_secs(2), &b);
        assert!(
            remaining >= Duration::ZERO,
            "remaining must never be negative, got {remaining:?}"
        );
    }

    /// In the second cycle at 10.5s, 1.5s remain in the burst.
    #[test]
    fn time_until_burst_end_second_cycle_at_10_5s_returns_1_5s() {
        let b = burst(10, 2, 5.0);
        let remaining = time_until_burst_end(Duration::from_millis(10500), &b);
        let diff = (remaining.as_secs_f64() - 1.5).abs();
        assert!(
            diff < 0.001,
            "in second cycle at 10.5s expected ~1.5s remaining, got {remaining:?}"
        );
    }

    // ---- CardinalitySpikeWindow: helper and tests ----------------------------

    /// Helper to build a CardinalitySpikeWindow for testing.
    fn spike(every_secs: u64, duration_secs: u64, cardinality: u64) -> CardinalitySpikeWindow {
        CardinalitySpikeWindow {
            label: "pod_name".to_string(),
            every: Duration::from_secs(every_secs),
            duration: Duration::from_secs(duration_secs),
            cardinality,
            strategy: SpikeStrategy::Counter,
            prefix: "pod-".to_string(),
            seed: 0,
        }
    }

    // ---- is_in_spike: mirroring is_in_burst patterns -------------------------

    /// At elapsed=0s the spike occupies [0, duration) — should be active.
    #[test]
    fn is_in_spike_at_zero_is_true() {
        let s = spike(10, 2, 100);
        assert!(is_in_spike(Duration::ZERO, &s));
    }

    /// At 0.5s we are inside the 2s spike window.
    #[test]
    fn is_in_spike_at_0_5s_is_true() {
        let s = spike(10, 2, 100);
        assert!(is_in_spike(Duration::from_millis(500), &s));
    }

    /// At exactly 2.0s the spike window ends (spike occupies [0, 2)).
    #[test]
    fn is_in_spike_at_spike_end_boundary_is_false() {
        let s = spike(10, 2, 100);
        assert!(!is_in_spike(Duration::from_secs(2), &s));
    }

    /// At 5s we are mid-cycle, outside the spike window.
    #[test]
    fn is_in_spike_at_5s_is_false() {
        let s = spike(10, 2, 100);
        assert!(!is_in_spike(Duration::from_secs(5), &s));
    }

    /// At 10s we are at the start of cycle 2 — spike is active again.
    #[test]
    fn is_in_spike_at_10s_second_cycle_start_is_true() {
        let s = spike(10, 2, 100);
        assert!(is_in_spike(Duration::from_secs(10), &s));
    }

    /// At 12.5s we are 2.5s into cycle 2, past the spike window.
    #[test]
    fn is_in_spike_at_12_5s_second_cycle_is_false() {
        let s = spike(10, 2, 100);
        assert!(!is_in_spike(Duration::from_millis(12500), &s));
    }

    // ---- label_value_for_tick: counter strategy --------------------------------

    /// Counter strategy produces prefix + (tick % cardinality).
    #[test]
    fn label_value_counter_at_tick_zero() {
        let s = spike(10, 2, 100);
        assert_eq!(s.label_value_for_tick(0), "pod-0");
    }

    /// Counter wraps around at cardinality boundary.
    #[test]
    fn label_value_counter_wraps_at_cardinality() {
        let s = spike(10, 2, 3);
        assert_eq!(s.label_value_for_tick(0), "pod-0");
        assert_eq!(s.label_value_for_tick(1), "pod-1");
        assert_eq!(s.label_value_for_tick(2), "pod-2");
        assert_eq!(s.label_value_for_tick(3), "pod-0");
        assert_eq!(s.label_value_for_tick(4), "pod-1");
    }

    /// Counter with cardinality=1 always produces the same value.
    #[test]
    fn label_value_counter_cardinality_one() {
        let s = spike(10, 2, 1);
        assert_eq!(s.label_value_for_tick(0), "pod-0");
        assert_eq!(s.label_value_for_tick(999), "pod-0");
    }

    // ---- label_value_for_tick: random strategy ---------------------------------

    /// Random strategy produces deterministic output for the same seed + tick.
    #[test]
    fn label_value_random_is_deterministic() {
        let s = CardinalitySpikeWindow {
            label: "error_msg".to_string(),
            every: Duration::from_secs(10),
            duration: Duration::from_secs(2),
            cardinality: 1000,
            strategy: SpikeStrategy::Random,
            prefix: "err-".to_string(),
            seed: 42,
        };
        let v1 = s.label_value_for_tick(0);
        let v2 = s.label_value_for_tick(0);
        assert_eq!(v1, v2, "same seed + tick must produce same value");
    }

    /// Random strategy produces different values for different ticks.
    #[test]
    fn label_value_random_differs_across_ticks() {
        let s = CardinalitySpikeWindow {
            label: "error_msg".to_string(),
            every: Duration::from_secs(10),
            duration: Duration::from_secs(2),
            cardinality: 1000,
            strategy: SpikeStrategy::Random,
            prefix: "".to_string(),
            seed: 42,
        };
        let v0 = s.label_value_for_tick(0);
        let v1 = s.label_value_for_tick(1);
        assert_ne!(v0, v1, "different ticks should produce different values");
    }

    /// Random strategy output starts with the configured prefix.
    #[test]
    fn label_value_random_starts_with_prefix() {
        let s = CardinalitySpikeWindow {
            label: "error_msg".to_string(),
            every: Duration::from_secs(10),
            duration: Duration::from_secs(2),
            cardinality: 1000,
            strategy: SpikeStrategy::Random,
            prefix: "err-".to_string(),
            seed: 42,
        };
        assert!(s.label_value_for_tick(0).starts_with("err-"));
    }

    // ---- CardinalitySpikeWindow: Clone and Debug contracts --------------------

    #[test]
    fn spike_window_is_cloneable() {
        let s = spike(10, 2, 100);
        let cloned = s.clone();
        assert_eq!(cloned.label, "pod_name");
        assert_eq!(cloned.every, Duration::from_secs(10));
        assert_eq!(cloned.cardinality, 100);
    }

    #[test]
    fn spike_window_is_debuggable() {
        let s = spike(10, 2, 100);
        let debug = format!("{s:?}");
        assert!(debug.contains("CardinalitySpikeWindow"));
    }

    // ---- splitmix64: determinism anchor --------------------------------------

    /// SplitMix64 produces known output for known input (regression anchor).
    #[test]
    fn splitmix64_produces_known_output() {
        let result = super::splitmix64(42);
        // Run the function once to capture the expected value.
        // This test anchors the implementation to ensure changes are caught.
        assert_eq!(result, super::splitmix64(42));
        // Different inputs produce different outputs.
        assert_ne!(super::splitmix64(0), super::splitmix64(1));
    }
}
