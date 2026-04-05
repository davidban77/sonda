//! Summary generator — produces quantile values, count, and sum for
//! simulating Prometheus-style summary metrics.
//!
//! Unlike [`ValueGenerator`](super::ValueGenerator) which produces a single
//! `f64` per tick, `SummaryGenerator` holds cumulative state and produces a
//! [`SummarySample`] containing quantile pairs, count, and sum per tick.
//!
//! Each tick, the generator samples observations from a configurable
//! distribution, sorts them, and computes quantile values. Count and sum are
//! cumulative across ticks.
//!
//! The generator uses deterministic, seeded RNG (SplitMix64) so that the same
//! seed always produces the same observations.

use crate::util::splitmix64;

use super::histogram::Distribution;

/// Default summary quantile targets.
///
/// These match common Prometheus summary quantile configurations.
pub const DEFAULT_SUMMARY_QUANTILES: &[f64] = &[0.5, 0.9, 0.95, 0.99];

/// A single summary sample produced by [`SummaryGenerator::observe`].
///
/// Count and sum are cumulative — they never decrease across successive ticks.
/// Quantile values are computed fresh each tick from that tick's observations.
#[derive(Debug, Clone)]
pub struct SummarySample {
    /// Quantile target-value pairs. Each entry is `(quantile_target, computed_value)`.
    /// For example, `(0.99, 0.245)` means the 99th percentile value is 0.245.
    pub quantiles: Vec<(f64, f64)>,
    /// Total number of observations across all ticks.
    pub count: u64,
    /// Cumulative sum of all observed values.
    pub sum: f64,
}

/// Generates summary samples by sampling from a configurable distribution.
///
/// Each call to [`observe`](SummaryGenerator::observe) draws
/// `observations_per_tick` samples from the configured distribution, sorts
/// them, computes quantile values, and updates the cumulative count and sum.
///
/// The generator is deterministic: given the same seed, tick sequence, and
/// configuration, it always produces the same output.
pub struct SummaryGenerator {
    /// Quantile targets to compute (e.g., [0.5, 0.9, 0.95, 0.99]).
    quantiles: Vec<f64>,
    /// Total observation count (cumulative).
    count: u64,
    /// Cumulative sum of all observations.
    sum: f64,
    /// Distribution model for generating observations.
    distribution: Distribution,
    /// Number of observations to draw per tick.
    observations_per_tick: u64,
    /// Linear drift applied to the distribution center per second.
    mean_shift_per_sec: f64,
    /// Base seed for deterministic RNG.
    seed: u64,
    /// Rate from the scenario config, used to convert ticks to seconds.
    rate: f64,
    /// Monotonically increasing tick counter for RNG state advancement.
    tick_counter: u64,
}

impl SummaryGenerator {
    /// Create a new summary generator.
    ///
    /// # Parameters
    ///
    /// * `quantiles` — sorted quantile targets in `(0, 1)`. Use
    ///   [`DEFAULT_SUMMARY_QUANTILES`] when `None` is provided in config.
    /// * `distribution` — the probability distribution to sample from.
    /// * `observations_per_tick` — how many samples to draw each tick.
    /// * `mean_shift_per_sec` — linear drift per second for the distribution center.
    /// * `seed` — determinism seed for the RNG.
    /// * `rate` — scenario event rate (events/sec), used to convert tick index to elapsed seconds.
    pub fn new(
        quantiles: Vec<f64>,
        distribution: Distribution,
        observations_per_tick: u64,
        mean_shift_per_sec: f64,
        seed: u64,
        rate: f64,
    ) -> Self {
        Self {
            quantiles,
            count: 0,
            sum: 0.0,
            distribution,
            observations_per_tick,
            mean_shift_per_sec,
            seed,
            rate,
            tick_counter: 0,
        }
    }

    /// Return a reference to the quantile targets.
    pub fn quantiles(&self) -> &[f64] {
        &self.quantiles
    }

    /// Advance the generator by one tick, sampling observations and computing
    /// quantile values.
    ///
    /// # Parameters
    ///
    /// * `tick` — the current tick index (used for time-varying shift calculation).
    ///
    /// # Returns
    ///
    /// A [`SummarySample`] with quantile values, cumulative count, and sum.
    pub fn observe(&mut self, tick: u64) -> SummarySample {
        let elapsed_secs = tick as f64 / self.rate;
        let shift = self.mean_shift_per_sec * elapsed_secs;

        // Collect observations for this tick.
        let mut observations = Vec::with_capacity(self.observations_per_tick as usize);
        for i in 0..self.observations_per_tick {
            let rng_input = self
                .seed
                .wrapping_mul(0x517c_c1b7_2722_0a95)
                .wrapping_add(self.tick_counter)
                .wrapping_mul(0x6c62_272e_07bb_0142)
                .wrapping_add(i);
            let value = sample_distribution(&self.distribution, rng_input, shift);
            observations.push(value);
            self.count += 1;
            self.sum += value;
        }
        self.tick_counter += 1;

        // Sort observations to compute quantiles.
        observations.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Compute quantile values using nearest-rank method.
        let n = observations.len();
        let quantile_values: Vec<(f64, f64)> = self
            .quantiles
            .iter()
            .map(|&q| {
                let rank = (q * n as f64).ceil() as usize;
                let index = rank.saturating_sub(1).min(n.saturating_sub(1));
                (q, observations[index])
            })
            .collect();

        SummarySample {
            quantiles: quantile_values,
            count: self.count,
            sum: self.sum,
        }
    }
}

/// Sample a single value from a distribution with an optional mean shift.
fn sample_distribution(dist: &Distribution, rng_input: u64, shift: f64) -> f64 {
    match dist {
        Distribution::Exponential { rate } => {
            let u = uniform_01(rng_input);
            let u_clamped = u.min(1.0 - f64::EPSILON);
            let value = -(1.0 - u_clamped).ln() / rate;
            value + shift
        }
        Distribution::Normal { mean, stddev } => {
            let u1 = uniform_01(rng_input);
            let u2 = uniform_01(splitmix64(rng_input.wrapping_add(1)));
            let u1_clamped = u1.max(f64::EPSILON);
            let z = (-2.0 * u1_clamped.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            (mean + shift) + stddev * z
        }
        Distribution::Uniform { min, max } => {
            let u = uniform_01(rng_input);
            min + u * (max - min) + shift
        }
    }
}

/// Convert a `u64` hash output to a uniform `f64` in `[0, 1)`.
fn uniform_01(input: u64) -> f64 {
    let hash = splitmix64(input);
    (hash >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a summary generator with exponential distribution.
    fn exponential_summary(quantiles: Vec<f64>, rate: f64, seed: u64) -> SummaryGenerator {
        SummaryGenerator::new(
            quantiles,
            Distribution::Exponential { rate },
            100,
            0.0,
            seed,
            10.0,
        )
    }

    // ---- Cumulative count and sum -------------------------------------------

    /// Count and sum never decrease across ticks.
    #[test]
    fn count_and_sum_never_decrease() {
        let mut gen = exponential_summary(DEFAULT_SUMMARY_QUANTILES.to_vec(), 10.0, 42);
        let mut prev = gen.observe(0);
        for tick in 1..20 {
            let curr = gen.observe(tick);
            assert!(curr.count >= prev.count, "count decreased at tick {tick}");
            assert!(curr.sum >= prev.sum, "sum decreased at tick {tick}");
            prev = curr;
        }
    }

    /// Count accumulates correctly.
    #[test]
    fn count_equals_observations_times_ticks() {
        let mut gen = SummaryGenerator::new(
            DEFAULT_SUMMARY_QUANTILES.to_vec(),
            Distribution::Exponential { rate: 5.0 },
            50,
            0.0,
            0,
            10.0,
        );
        for tick in 0..10 {
            let sample = gen.observe(tick);
            assert_eq!(
                sample.count,
                50 * (tick + 1),
                "count must equal observations_per_tick * ticks at tick {tick}"
            );
        }
    }

    // ---- Quantile value ordering -------------------------------------------

    /// Quantile values should be monotonically non-decreasing for sorted quantile targets.
    #[test]
    fn quantile_values_are_non_decreasing() {
        let mut gen = exponential_summary(vec![0.1, 0.5, 0.9, 0.99], 10.0, 42);
        let sample = gen.observe(0);
        for window in sample.quantiles.windows(2) {
            let (q1, v1) = window[0];
            let (q2, v2) = window[1];
            assert!(
                v2 >= v1,
                "quantile {q2} value ({v2}) should be >= quantile {q1} value ({v1})"
            );
        }
    }

    // ---- Determinism --------------------------------------------------------

    /// Same seed produces identical output.
    #[test]
    fn same_seed_produces_identical_output() {
        let mut gen_a = exponential_summary(DEFAULT_SUMMARY_QUANTILES.to_vec(), 10.0, 42);
        let mut gen_b = exponential_summary(DEFAULT_SUMMARY_QUANTILES.to_vec(), 10.0, 42);
        for tick in 0..10 {
            let a = gen_a.observe(tick);
            let b = gen_b.observe(tick);
            assert_eq!(a.count, b.count, "count must match at tick {tick}");
            assert_eq!(a.sum, b.sum, "sum must match at tick {tick}");
            for (qa, qb) in a.quantiles.iter().zip(b.quantiles.iter()) {
                assert_eq!(qa.0, qb.0, "quantile targets must match at tick {tick}");
                assert_eq!(qa.1, qb.1, "quantile values must match at tick {tick}");
            }
        }
    }

    /// Different seeds produce different output.
    #[test]
    fn different_seeds_produce_different_output() {
        let mut gen_a = exponential_summary(DEFAULT_SUMMARY_QUANTILES.to_vec(), 10.0, 1);
        let mut gen_b = exponential_summary(DEFAULT_SUMMARY_QUANTILES.to_vec(), 10.0, 2);
        let a = gen_a.observe(0);
        let b = gen_b.observe(0);
        let any_differ = a
            .quantiles
            .iter()
            .zip(b.quantiles.iter())
            .any(|(qa, qb)| qa.1 != qb.1);
        assert!(
            any_differ,
            "different seeds should produce different quantile values"
        );
    }

    // ---- Default quantiles --------------------------------------------------

    /// Default quantiles are sorted and in (0, 1).
    #[test]
    fn default_quantiles_are_valid() {
        for window in DEFAULT_SUMMARY_QUANTILES.windows(2) {
            assert!(
                window[0] < window[1],
                "default quantiles must be sorted: {} >= {}",
                window[0],
                window[1]
            );
        }
        for &q in DEFAULT_SUMMARY_QUANTILES {
            assert!(q > 0.0 && q < 1.0, "default quantile {q} must be in (0, 1)");
        }
    }

    /// Default quantile count matches expected.
    #[test]
    fn default_quantiles_have_expected_count() {
        assert_eq!(
            DEFAULT_SUMMARY_QUANTILES.len(),
            4,
            "expected 4 default quantiles"
        );
    }

    // ---- Normal distribution -----------------------------------------------

    /// Normal distribution quantiles center around the mean.
    #[test]
    fn normal_distribution_p50_near_mean() {
        let mut gen = SummaryGenerator::new(
            vec![0.5],
            Distribution::Normal {
                mean: 100.0,
                stddev: 5.0,
            },
            10000,
            0.0,
            42,
            10.0,
        );
        let sample = gen.observe(0);
        let p50 = sample.quantiles[0].1;
        assert!(
            (p50 - 100.0).abs() < 1.0,
            "p50 should be near 100.0, got {p50}"
        );
    }

    // ---- Contract: Send + Sync ----------------------------------------------

    #[test]
    fn summary_generator_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SummaryGenerator>();
    }

    #[test]
    fn summary_sample_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SummarySample>();
    }
}
