//! Spike value generator — outputs a baseline value with periodic spikes.
//!
//! The generator produces a constant baseline value most of the time, but
//! periodically emits `baseline + magnitude` for a configurable duration.
//! This is useful for testing alerting rules and anomaly detection.

use super::ValueGenerator;

/// Generates a baseline value with periodic spikes of configurable magnitude
/// and duration.
///
/// During a spike window the output is `baseline + magnitude`. Outside the
/// window the output is `baseline`. The spike timing is controlled by
/// `interval_ticks` (time between spike starts) and `duration_ticks` (how
/// long each spike lasts), both pre-computed from seconds and rate at
/// construction time.
///
/// A negative `magnitude` produces periodic dips below the baseline, which
/// is a valid use case for testing low-threshold alerts.
pub struct SpikeGenerator {
    baseline: f64,
    magnitude: f64,
    duration_ticks: f64,
    interval_ticks: f64,
}

impl SpikeGenerator {
    /// Construct a new `SpikeGenerator`.
    ///
    /// # Parameters
    /// - `baseline` — the normal output value between spikes.
    /// - `magnitude` — the amount added to baseline during a spike.
    /// - `duration_secs` — how long each spike lasts in seconds.
    /// - `interval_secs` — time between spike starts in seconds.
    /// - `rate` — events per second; used to convert seconds into ticks.
    pub fn new(
        baseline: f64,
        magnitude: f64,
        duration_secs: f64,
        interval_secs: f64,
        rate: f64,
    ) -> Self {
        let duration_ticks = duration_secs * rate;
        let interval_ticks = interval_secs * rate;
        Self {
            baseline,
            magnitude,
            duration_ticks,
            interval_ticks,
        }
    }
}

impl ValueGenerator for SpikeGenerator {
    /// Return `baseline + magnitude` when the tick falls within a spike
    /// window, or `baseline` otherwise.
    ///
    /// A tick is within a spike window when
    /// `(tick as f64) % interval_ticks < duration_ticks`.
    fn value(&self, tick: u64) -> f64 {
        let position = (tick as f64) % self.interval_ticks;
        if position < self.duration_ticks {
            self.baseline + self.magnitude
        } else {
            self.baseline
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-10;

    /// Helper: rate=1 means ticks == seconds.
    fn spike_rate1(
        baseline: f64,
        magnitude: f64,
        duration_secs: f64,
        interval_secs: f64,
    ) -> SpikeGenerator {
        SpikeGenerator::new(baseline, magnitude, duration_secs, interval_secs, 1.0)
    }

    #[test]
    fn tick_during_baseline_period_returns_baseline() {
        // interval=60, duration=10 → ticks 10..59 are baseline
        let gen = spike_rate1(50.0, 200.0, 10.0, 60.0);
        assert!((gen.value(15) - 50.0).abs() < EPSILON);
        assert!((gen.value(30) - 50.0).abs() < EPSILON);
        assert!((gen.value(59) - 50.0).abs() < EPSILON);
    }

    #[test]
    fn tick_during_spike_period_returns_baseline_plus_magnitude() {
        // interval=60, duration=10 → ticks 0..9 are spike
        let gen = spike_rate1(50.0, 200.0, 10.0, 60.0);
        let expected = 50.0 + 200.0;
        assert!((gen.value(0) - expected).abs() < EPSILON);
        assert!((gen.value(5) - expected).abs() < EPSILON);
        assert!((gen.value(9) - expected).abs() < EPSILON);
    }

    #[test]
    fn exact_boundary_at_spike_start() {
        // tick 0 is the start of the first spike window
        let gen = spike_rate1(10.0, 90.0, 5.0, 20.0);
        assert!(
            (gen.value(0) - 100.0).abs() < EPSILON,
            "tick 0 should be in spike"
        );
    }

    #[test]
    fn exact_boundary_at_spike_end() {
        // duration_ticks=5 → tick 5 is the first tick OUTSIDE the spike
        let gen = spike_rate1(10.0, 90.0, 5.0, 20.0);
        assert!(
            (gen.value(5) - 10.0).abs() < EPSILON,
            "tick at duration boundary should be baseline"
        );
    }

    #[test]
    fn exact_boundary_at_second_interval() {
        // At tick == interval_ticks, a new spike starts
        let gen = spike_rate1(10.0, 90.0, 5.0, 20.0);
        assert!(
            (gen.value(20) - 100.0).abs() < EPSILON,
            "tick at second interval should be in spike"
        );
        assert!(
            (gen.value(25) - 10.0).abs() < EPSILON,
            "tick after second spike should be baseline"
        );
    }

    #[test]
    fn duration_ge_interval_always_spikes() {
        // When duration >= interval, every tick falls within the spike window
        let gen = spike_rate1(10.0, 90.0, 60.0, 60.0);
        for tick in 0..200 {
            assert!(
                (gen.value(tick) - 100.0).abs() < EPSILON,
                "duration >= interval: tick {tick} should always be spike"
            );
        }
    }

    #[test]
    fn duration_greater_than_interval_always_spikes() {
        let gen = spike_rate1(10.0, 90.0, 100.0, 60.0);
        for tick in 0..200 {
            assert!(
                (gen.value(tick) - 100.0).abs() < EPSILON,
                "duration > interval: tick {tick} should always be spike"
            );
        }
    }

    #[test]
    fn duration_zero_always_baseline() {
        // When duration_secs == 0, no tick is ever in the spike window
        let gen = spike_rate1(50.0, 200.0, 0.0, 60.0);
        for tick in 0..200 {
            assert!(
                (gen.value(tick) - 50.0).abs() < EPSILON,
                "duration=0: tick {tick} should always be baseline"
            );
        }
    }

    #[test]
    fn negative_magnitude_dips_below_baseline() {
        let gen = spike_rate1(100.0, -50.0, 5.0, 20.0);
        // During spike: 100 + (-50) = 50
        assert!((gen.value(0) - 50.0).abs() < EPSILON);
        // During baseline: 100
        assert!((gen.value(10) - 100.0).abs() < EPSILON);
    }

    #[test]
    fn determinism_same_tick_same_value() {
        let gen = spike_rate1(50.0, 200.0, 10.0, 60.0);
        for tick in 0..100 {
            assert_eq!(
                gen.value(tick),
                gen.value(tick),
                "tick {tick} must be deterministic"
            );
        }
    }

    #[test]
    fn large_tick_values_do_not_panic() {
        let gen = spike_rate1(50.0, 200.0, 10.0, 60.0);
        // Should not panic at very large tick values
        let _ = gen.value(u64::MAX);
        let _ = gen.value(u64::MAX - 1);
        let _ = gen.value(1_000_000_000);
    }

    #[test]
    fn different_rate_values_produce_different_tick_boundaries() {
        // rate=1: interval_ticks=60, duration_ticks=10
        // At tick 10, rate=1 → baseline; rate=2 → spike (duration_ticks=20)
        let gen_r1 = SpikeGenerator::new(50.0, 200.0, 10.0, 60.0, 1.0);
        let gen_r2 = SpikeGenerator::new(50.0, 200.0, 10.0, 60.0, 2.0);
        // tick 10: rate=1 → 10 % 60 = 10, 10 < 10 is false → baseline
        assert!(
            (gen_r1.value(10) - 50.0).abs() < EPSILON,
            "rate=1, tick=10 should be baseline"
        );
        // tick 10: rate=2 → 10 % 120 = 10, 10 < 20 is true → spike
        assert!(
            (gen_r2.value(10) - 250.0).abs() < EPSILON,
            "rate=2, tick=10 should be spike"
        );
    }

    #[test]
    fn rate_adjusts_period_ticks() {
        // rate=10: interval_ticks = 60*10 = 600, duration_ticks = 10*10 = 100
        // tick 99 should be in spike, tick 100 should be baseline
        let gen = SpikeGenerator::new(50.0, 200.0, 10.0, 60.0, 10.0);
        assert!(
            (gen.value(99) - 250.0).abs() < EPSILON,
            "rate=10, tick=99 should be in spike (99 < 100)"
        );
        assert!(
            (gen.value(100) - 50.0).abs() < EPSILON,
            "rate=10, tick=100 should be baseline (100 >= 100)"
        );
    }
}
