//! Scheduling: rate control, duration, gap windows, burst windows.
//!
//! The scheduler controls *when* events are emitted. It does not know
//! *what* is being emitted — that is the generator and encoder's job.

pub(crate) mod core_loop;
pub mod handle;
pub mod histogram_runner;
pub mod launch;
pub mod log_runner;
pub mod multi_runner;
pub mod runner;
pub mod stats;
pub mod summary_runner;

use std::time::Duration;

use crate::config::{DynamicLabelStrategy, SpikeStrategy};
use crate::util::splitmix64;

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
    /// For the `Random` strategy, computes an index as `tick % cardinality`,
    /// then deterministically maps that index to a 16-char hex string via
    /// `splitmix64(seed ^ index)`. This ensures exactly `cardinality` unique
    /// values while keeping them hash-like rather than sequential.
    pub fn label_value_for_tick(&self, tick: u64) -> String {
        let index = tick % self.cardinality;
        match self.strategy {
            SpikeStrategy::Counter => {
                format!("{}{}", self.prefix, index)
            }
            SpikeStrategy::Random => {
                let mixed = splitmix64(self.seed ^ index);
                format!("{}{:016x}", self.prefix, mixed)
            }
        }
    }
}

/// Resolved configuration for a dynamic label — an always-on rotating label.
///
/// Built from a [`DynamicLabelConfig`](crate::config::DynamicLabelConfig)
/// at runner initialization time. Unlike [`CardinalitySpikeWindow`], dynamic
/// labels are never gated by a time window — they produce a value on every tick.
#[derive(Debug, Clone)]
pub struct DynamicLabel {
    /// The label key to inject on every tick.
    pub key: String,
    /// Prefix for counter-strategy values (e.g. `"host-"`).
    /// Empty string when using the values-list strategy.
    pub prefix: String,
    /// Number of distinct values in the cycle.
    ///
    /// For counter strategy this is the configured `cardinality`.
    /// For values-list strategy this is `values.len()`.
    pub cardinality: u64,
    /// Explicit values list (empty for counter strategy).
    pub values: Vec<String>,
}

impl DynamicLabel {
    /// Generate the label value for the given tick.
    ///
    /// For the counter strategy, returns `"{prefix}{tick % cardinality}"`.
    /// For the values-list strategy, returns `values[tick % values.len()]`.
    ///
    /// This method is pure: it has no side effects and is deterministic for a
    /// given tick.
    pub fn label_value_for_tick(&self, tick: u64) -> String {
        let index = tick % self.cardinality;
        if self.values.is_empty() {
            // Counter strategy
            format!("{}{}", self.prefix, index)
        } else {
            // Values-list strategy
            self.values[index as usize].clone()
        }
    }
}

/// Resolved schedule configuration parsed from a [`BaseScheduleConfig`].
///
/// Holds the parsed `Duration` values for gap, burst, and spike windows,
/// and resolved dynamic labels.
/// This is the shared input to the [`core_loop::run_schedule_loop`] function,
/// eliminating the need for each signal runner to duplicate the parsing logic.
///
/// Constructed via [`ParsedSchedule::from_base_config`].
#[derive(Debug, Clone)]
pub(crate) struct ParsedSchedule {
    /// Total run duration. `None` means run indefinitely.
    pub total_duration: Option<Duration>,
    /// Optional recurring gap window.
    pub gap_window: Option<GapWindow>,
    /// Optional recurring burst window.
    pub burst_window: Option<BurstWindow>,
    /// Resolved cardinality spike windows.
    pub spike_windows: Vec<CardinalitySpikeWindow>,
    /// Resolved dynamic labels (always-on, every tick).
    pub dynamic_labels: Vec<DynamicLabel>,
}

impl ParsedSchedule {
    /// Parse a [`ParsedSchedule`] from a [`BaseScheduleConfig`].
    ///
    /// Converts duration strings into `Duration` values and resolves spike
    /// window defaults. This is the single authoritative location for schedule
    /// parsing — both the metrics and log runners call this.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError`] if any duration string is invalid.
    pub fn from_base_config(
        config: &crate::config::BaseScheduleConfig,
    ) -> Result<Self, crate::SondaError> {
        use crate::config::validate::parse_duration;

        let total_duration: Option<Duration> =
            config.duration.as_deref().map(parse_duration).transpose()?;

        let gap_window: Option<GapWindow> = config
            .gaps
            .as_ref()
            .map(|g| -> Result<GapWindow, crate::SondaError> {
                Ok(GapWindow {
                    every: parse_duration(&g.every)?,
                    duration: parse_duration(&g.r#for)?,
                })
            })
            .transpose()?;

        let burst_window: Option<BurstWindow> = config
            .bursts
            .as_ref()
            .map(|b| -> Result<BurstWindow, crate::SondaError> {
                Ok(BurstWindow {
                    every: parse_duration(&b.every)?,
                    duration: parse_duration(&b.r#for)?,
                    multiplier: b.multiplier,
                })
            })
            .transpose()?;

        let spike_windows: Vec<CardinalitySpikeWindow> = config
            .cardinality_spikes
            .as_ref()
            .map(|spikes| {
                spikes
                    .iter()
                    .map(|s| {
                        Ok(CardinalitySpikeWindow {
                            label: s.label.clone(),
                            every: parse_duration(&s.every)?,
                            duration: parse_duration(&s.r#for)?,
                            cardinality: s.cardinality,
                            strategy: s.strategy,
                            prefix: s.prefix.clone().unwrap_or_else(|| format!("{}_", s.label)),
                            seed: s.seed.unwrap_or(0),
                        })
                    })
                    .collect::<Result<Vec<_>, crate::SondaError>>()
            })
            .transpose()?
            .unwrap_or_default();

        let dynamic_labels: Vec<DynamicLabel> = config
            .dynamic_labels
            .as_ref()
            .map(|dls| {
                dls.iter()
                    .map(|dl| match &dl.strategy {
                        DynamicLabelStrategy::Counter {
                            prefix,
                            cardinality,
                        } => DynamicLabel {
                            key: dl.key.clone(),
                            prefix: prefix.clone().unwrap_or_else(|| format!("{}_", dl.key)),
                            cardinality: *cardinality,
                            values: Vec::new(),
                        },
                        DynamicLabelStrategy::ValuesList { values } => DynamicLabel {
                            key: dl.key.clone(),
                            prefix: String::new(),
                            cardinality: values.len() as u64,
                            values: values.clone(),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            total_duration,
            gap_window,
            burst_window,
            spike_windows,
            dynamic_labels,
        })
    }
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

    /// Random strategy produces deterministic output for the same seed + tick,
    /// with hardcoded expected values as regression anchors.
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
        assert_eq!(
            s.label_value_for_tick(0),
            "err-bdd732262feb6e95",
            "tick 0 must produce the known anchored value"
        );
        assert_eq!(
            s.label_value_for_tick(1),
            "err-ba69ec90eb4fef88",
            "tick 1 must produce the known anchored value"
        );
        // Same tick always produces the same value.
        assert_eq!(s.label_value_for_tick(0), s.label_value_for_tick(0));
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

    /// Random strategy respects cardinality: ticks beyond cardinality wrap around
    /// and produce the same values as their modular equivalents.
    #[test]
    fn label_value_random_respects_cardinality() {
        let s = CardinalitySpikeWindow {
            label: "error_msg".to_string(),
            every: Duration::from_secs(10),
            duration: Duration::from_secs(2),
            cardinality: 1000,
            strategy: SpikeStrategy::Random,
            prefix: "err-".to_string(),
            seed: 42,
        };
        // tick 1000 wraps to index 0, same as tick 0.
        assert_eq!(
            s.label_value_for_tick(0),
            s.label_value_for_tick(1000),
            "tick 0 and tick 1000 must produce the same value (cardinality=1000)"
        );
        // tick 1001 wraps to index 1, same as tick 1.
        assert_eq!(
            s.label_value_for_tick(1),
            s.label_value_for_tick(1001),
            "tick 1 and tick 1001 must produce the same value (cardinality=1000)"
        );
    }

    /// Random strategy with cardinality=1 always returns the same value.
    #[test]
    fn label_value_random_cardinality_one() {
        let s = CardinalitySpikeWindow {
            label: "x".to_string(),
            every: Duration::from_secs(10),
            duration: Duration::from_secs(2),
            cardinality: 1,
            strategy: SpikeStrategy::Random,
            prefix: "v-".to_string(),
            seed: 99,
        };
        assert_eq!(
            s.label_value_for_tick(0),
            s.label_value_for_tick(999),
            "cardinality=1 must always return the same value"
        );
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

    // ---- ParsedSchedule::from_base_config -------------------------------------

    /// Helper to build a minimal `BaseScheduleConfig` for testing.
    fn base_config() -> crate::config::BaseScheduleConfig {
        crate::config::BaseScheduleConfig {
            name: "test".to_string(),
            rate: 10.0,
            duration: None,
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: crate::sink::SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            jitter: None,
            jitter_seed: None,
        }
    }

    #[test]
    fn parsed_schedule_no_optionals() {
        let cfg = base_config();
        let parsed = ParsedSchedule::from_base_config(&cfg).unwrap();
        assert!(parsed.total_duration.is_none());
        assert!(parsed.gap_window.is_none());
        assert!(parsed.burst_window.is_none());
        assert!(parsed.spike_windows.is_empty());
    }

    #[test]
    fn parsed_schedule_with_duration() {
        let mut cfg = base_config();
        cfg.duration = Some("30s".to_string());
        let parsed = ParsedSchedule::from_base_config(&cfg).unwrap();
        assert_eq!(parsed.total_duration, Some(Duration::from_secs(30)));
    }

    #[test]
    fn parsed_schedule_with_gaps() {
        let mut cfg = base_config();
        cfg.gaps = Some(crate::config::GapConfig {
            every: "60s".to_string(),
            r#for: "10s".to_string(),
        });
        let parsed = ParsedSchedule::from_base_config(&cfg).unwrap();
        let gap = parsed.gap_window.unwrap();
        assert_eq!(gap.every, Duration::from_secs(60));
        assert_eq!(gap.duration, Duration::from_secs(10));
    }

    #[test]
    fn parsed_schedule_with_bursts() {
        let mut cfg = base_config();
        cfg.bursts = Some(crate::config::BurstConfig {
            every: "10s".to_string(),
            r#for: "2s".to_string(),
            multiplier: 5.0,
        });
        let parsed = ParsedSchedule::from_base_config(&cfg).unwrap();
        let burst = parsed.burst_window.unwrap();
        assert_eq!(burst.every, Duration::from_secs(10));
        assert_eq!(burst.duration, Duration::from_secs(2));
        assert!((burst.multiplier - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parsed_schedule_spike_defaults_prefix_and_seed() {
        let mut cfg = base_config();
        cfg.cardinality_spikes = Some(vec![crate::config::CardinalitySpikeConfig {
            label: "pod_name".to_string(),
            every: "2m".to_string(),
            r#for: "30s".to_string(),
            cardinality: 50,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: None,
            seed: None,
        }]);
        let parsed = ParsedSchedule::from_base_config(&cfg).unwrap();
        assert_eq!(parsed.spike_windows.len(), 1);
        let sw = &parsed.spike_windows[0];
        assert_eq!(
            sw.prefix, "pod_name_",
            "prefix defaults to label + underscore"
        );
        assert_eq!(sw.seed, 0, "seed defaults to 0");
        assert_eq!(sw.every, Duration::from_secs(120));
        assert_eq!(sw.duration, Duration::from_secs(30));
    }

    #[test]
    fn parsed_schedule_spike_custom_prefix_and_seed() {
        let mut cfg = base_config();
        cfg.cardinality_spikes = Some(vec![crate::config::CardinalitySpikeConfig {
            label: "host".to_string(),
            every: "1m".to_string(),
            r#for: "10s".to_string(),
            cardinality: 10,
            strategy: crate::config::SpikeStrategy::Random,
            prefix: Some("srv-".to_string()),
            seed: Some(42),
        }]);
        let parsed = ParsedSchedule::from_base_config(&cfg).unwrap();
        let sw = &parsed.spike_windows[0];
        assert_eq!(sw.prefix, "srv-");
        assert_eq!(sw.seed, 42);
    }

    #[test]
    fn parsed_schedule_empty_spikes_vec() {
        let mut cfg = base_config();
        cfg.cardinality_spikes = Some(vec![]);
        let parsed = ParsedSchedule::from_base_config(&cfg).unwrap();
        assert!(parsed.spike_windows.is_empty());
    }

    #[test]
    fn parsed_schedule_invalid_duration_returns_error() {
        let mut cfg = base_config();
        cfg.duration = Some("not_a_duration".to_string());
        assert!(ParsedSchedule::from_base_config(&cfg).is_err());
    }

    // ---- splitmix64: determinism anchor --------------------------------------

    /// SplitMix64 produces known output for known input (regression anchor).
    #[test]
    fn splitmix64_produces_known_output() {
        assert_eq!(
            super::splitmix64(42),
            0xbdd732262feb6e95,
            "splitmix64(42) must return the known constant"
        );
        assert_eq!(
            super::splitmix64(0),
            0xe220a8397b1dcdaf,
            "splitmix64(0) must return the known constant"
        );
        // Different inputs produce different outputs.
        assert_ne!(super::splitmix64(0), super::splitmix64(1));
    }

    // ---- DynamicLabel: counter strategy -----------------------------------------

    /// Counter strategy at tick 0 returns prefix + "0".
    #[test]
    fn dynamic_label_counter_tick_zero_returns_first_value() {
        let dl = DynamicLabel {
            key: "hostname".to_string(),
            prefix: "host-".to_string(),
            cardinality: 10,
            values: Vec::new(),
        };
        assert_eq!(dl.label_value_for_tick(0), "host-0");
    }

    /// Counter strategy cycles through cardinality values.
    #[test]
    fn dynamic_label_counter_wraps_at_cardinality() {
        let dl = DynamicLabel {
            key: "hostname".to_string(),
            prefix: "host-".to_string(),
            cardinality: 3,
            values: Vec::new(),
        };
        assert_eq!(dl.label_value_for_tick(0), "host-0");
        assert_eq!(dl.label_value_for_tick(1), "host-1");
        assert_eq!(dl.label_value_for_tick(2), "host-2");
        assert_eq!(dl.label_value_for_tick(3), "host-0");
        assert_eq!(dl.label_value_for_tick(4), "host-1");
    }

    /// Counter strategy with cardinality=1 always returns the same value.
    #[test]
    fn dynamic_label_counter_cardinality_one() {
        let dl = DynamicLabel {
            key: "pod".to_string(),
            prefix: "pod-".to_string(),
            cardinality: 1,
            values: Vec::new(),
        };
        assert_eq!(dl.label_value_for_tick(0), "pod-0");
        assert_eq!(dl.label_value_for_tick(1), "pod-0");
        assert_eq!(dl.label_value_for_tick(999), "pod-0");
    }

    /// Counter strategy with empty prefix produces bare index.
    #[test]
    fn dynamic_label_counter_empty_prefix() {
        let dl = DynamicLabel {
            key: "zone".to_string(),
            prefix: String::new(),
            cardinality: 5,
            values: Vec::new(),
        };
        assert_eq!(dl.label_value_for_tick(0), "0");
        assert_eq!(dl.label_value_for_tick(4), "4");
        assert_eq!(dl.label_value_for_tick(5), "0");
    }

    /// Counter strategy at large tick values still wraps correctly.
    #[test]
    fn dynamic_label_counter_large_tick() {
        let dl = DynamicLabel {
            key: "host".to_string(),
            prefix: "h-".to_string(),
            cardinality: 10,
            values: Vec::new(),
        };
        assert_eq!(dl.label_value_for_tick(1_000_000), "h-0");
        assert_eq!(dl.label_value_for_tick(1_000_007), "h-7");
    }

    // ---- DynamicLabel: values list strategy --------------------------------------

    /// Values-list strategy at tick 0 returns the first value.
    #[test]
    fn dynamic_label_values_tick_zero_returns_first_value() {
        let dl = DynamicLabel {
            key: "region".to_string(),
            prefix: String::new(),
            cardinality: 3,
            values: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        };
        assert_eq!(dl.label_value_for_tick(0), "alpha");
    }

    /// Values-list strategy cycles through the list.
    #[test]
    fn dynamic_label_values_wraps_at_list_length() {
        let dl = DynamicLabel {
            key: "region".to_string(),
            prefix: String::new(),
            cardinality: 3,
            values: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        };
        assert_eq!(dl.label_value_for_tick(0), "alpha");
        assert_eq!(dl.label_value_for_tick(1), "beta");
        assert_eq!(dl.label_value_for_tick(2), "gamma");
        assert_eq!(dl.label_value_for_tick(3), "alpha");
        assert_eq!(dl.label_value_for_tick(4), "beta");
    }

    /// Values-list with a single element always returns that element.
    #[test]
    fn dynamic_label_values_single_element() {
        let dl = DynamicLabel {
            key: "env".to_string(),
            prefix: String::new(),
            cardinality: 1,
            values: vec!["prod".to_string()],
        };
        assert_eq!(dl.label_value_for_tick(0), "prod");
        assert_eq!(dl.label_value_for_tick(100), "prod");
    }

    // ---- DynamicLabel: determinism ----------------------------------------------

    /// Counter strategy is deterministic: same tick always produces same value.
    #[test]
    fn dynamic_label_counter_is_deterministic() {
        let dl = DynamicLabel {
            key: "host".to_string(),
            prefix: "host-".to_string(),
            cardinality: 10,
            values: Vec::new(),
        };
        for tick in 0..100 {
            assert_eq!(dl.label_value_for_tick(tick), dl.label_value_for_tick(tick));
        }
    }

    /// Cardinality ceiling: counter strategy never produces more than `cardinality` distinct values.
    #[test]
    fn dynamic_label_counter_respects_cardinality_ceiling() {
        let dl = DynamicLabel {
            key: "host".to_string(),
            prefix: "host-".to_string(),
            cardinality: 5,
            values: Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        for tick in 0..1000 {
            seen.insert(dl.label_value_for_tick(tick));
        }
        assert_eq!(
            seen.len(),
            5,
            "counter with cardinality=5 must produce exactly 5 distinct values, got {}",
            seen.len()
        );
    }

    /// Cardinality ceiling: values-list strategy never produces more distinct values than list length.
    #[test]
    fn dynamic_label_values_respects_cardinality_ceiling() {
        let dl = DynamicLabel {
            key: "env".to_string(),
            prefix: String::new(),
            cardinality: 3,
            values: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        let mut seen = std::collections::HashSet::new();
        for tick in 0..1000 {
            seen.insert(dl.label_value_for_tick(tick));
        }
        assert_eq!(
            seen.len(),
            3,
            "values list with 3 elements must produce exactly 3 distinct values, got {}",
            seen.len()
        );
    }

    // ---- DynamicLabel: Clone and Debug contracts ---------------------------------

    #[test]
    fn dynamic_label_is_cloneable() {
        let dl = DynamicLabel {
            key: "host".to_string(),
            prefix: "host-".to_string(),
            cardinality: 10,
            values: Vec::new(),
        };
        let cloned = dl.clone();
        assert_eq!(cloned.key, "host");
        assert_eq!(cloned.cardinality, 10);
    }

    #[test]
    fn dynamic_label_is_debuggable() {
        let dl = DynamicLabel {
            key: "host".to_string(),
            prefix: "host-".to_string(),
            cardinality: 10,
            values: Vec::new(),
        };
        let debug = format!("{dl:?}");
        assert!(debug.contains("DynamicLabel"));
    }

    // ---- ParsedSchedule::from_base_config with dynamic_labels -------------------

    /// ParsedSchedule parses dynamic_labels counter strategy from BaseScheduleConfig.
    #[test]
    fn parsed_schedule_parses_dynamic_labels_counter() {
        let mut config = base_config();
        config.dynamic_labels = Some(vec![crate::config::DynamicLabelConfig {
            key: "hostname".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: Some("host-".to_string()),
                cardinality: 10,
            },
        }]);
        let schedule = ParsedSchedule::from_base_config(&config).expect("must parse");
        assert_eq!(schedule.dynamic_labels.len(), 1);
        assert_eq!(schedule.dynamic_labels[0].key, "hostname");
        assert_eq!(schedule.dynamic_labels[0].prefix, "host-");
        assert_eq!(schedule.dynamic_labels[0].cardinality, 10);
        assert!(schedule.dynamic_labels[0].values.is_empty());
    }

    /// ParsedSchedule parses dynamic_labels values list strategy from BaseScheduleConfig.
    #[test]
    fn parsed_schedule_parses_dynamic_labels_values_list() {
        let mut config = base_config();
        config.dynamic_labels = Some(vec![crate::config::DynamicLabelConfig {
            key: "region".to_string(),
            strategy: crate::config::DynamicLabelStrategy::ValuesList {
                values: vec!["us-east".to_string(), "eu-west".to_string()],
            },
        }]);
        let schedule = ParsedSchedule::from_base_config(&config).expect("must parse");
        assert_eq!(schedule.dynamic_labels.len(), 1);
        assert_eq!(schedule.dynamic_labels[0].key, "region");
        assert_eq!(schedule.dynamic_labels[0].cardinality, 2);
        assert_eq!(
            schedule.dynamic_labels[0].values,
            vec!["us-east", "eu-west"]
        );
    }

    /// ParsedSchedule with no dynamic_labels config produces empty vec.
    #[test]
    fn parsed_schedule_no_dynamic_labels_produces_empty_vec() {
        let config = base_config();
        let schedule = ParsedSchedule::from_base_config(&config).expect("must parse");
        assert!(schedule.dynamic_labels.is_empty());
    }

    /// ParsedSchedule: counter strategy defaults prefix to "{key}_" when prefix is None.
    #[test]
    fn parsed_schedule_counter_default_prefix() {
        let mut config = base_config();
        config.dynamic_labels = Some(vec![crate::config::DynamicLabelConfig {
            key: "pod".to_string(),
            strategy: crate::config::DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 5,
            },
        }]);
        let schedule = ParsedSchedule::from_base_config(&config).expect("must parse");
        assert_eq!(
            schedule.dynamic_labels[0].prefix, "pod_",
            "default prefix must be key + underscore"
        );
    }
}
