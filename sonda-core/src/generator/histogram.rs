//! Histogram generator — produces cumulative bucket counts, count, and sum
//! for simulating Prometheus-style histogram metrics.
//!
//! Unlike [`ValueGenerator`](super::ValueGenerator) which produces a single
//! `f64` per tick, `HistogramGenerator` holds cumulative state and produces a
//! [`HistogramSample`] containing multiple values per tick. This enables
//! realistic histogram simulation where `rate()` and `histogram_quantile()`
//! queries work correctly on the generated data.
//!
//! The generator uses deterministic, seeded RNG (SplitMix64) so that the same
//! seed always produces the same observations.

use crate::config::DistributionConfig;
use crate::util::splitmix64;

/// Default Prometheus histogram bucket boundaries.
///
/// These match the default bucket boundaries used by the Prometheus client
/// libraries when no custom buckets are specified.
pub const DEFAULT_HISTOGRAM_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Distribution model for generating observations.
///
/// Determines how sample values are distributed when the histogram generator
/// produces observations on each tick.
#[derive(Debug, Clone)]
pub enum Distribution {
    /// Exponential distribution with rate parameter lambda.
    ///
    /// Mean = 1/lambda. Models latency distributions where most requests are
    /// fast but some have long tails. Generated via CDF inversion:
    /// `x = -ln(1 - u) / lambda` where `u` is uniform in `[0, 1)`.
    Exponential {
        /// Rate parameter (lambda). Must be strictly positive.
        rate: f64,
    },
    /// Normal (Gaussian) distribution with configurable mean and standard
    /// deviation.
    ///
    /// Generated via the Box-Muller transform on two uniform samples.
    /// Values can be negative; bucket boundaries should be chosen accordingly.
    Normal {
        /// Center of the distribution.
        mean: f64,
        /// Spread of the distribution. Must be strictly positive.
        stddev: f64,
    },
    /// Uniform distribution over `[min, max]`.
    ///
    /// Every value in the range is equally likely.
    Uniform {
        /// Lower bound (inclusive).
        min: f64,
        /// Upper bound (inclusive).
        max: f64,
    },
}

/// Convert a [`DistributionConfig`] into a runtime [`Distribution`].
///
/// This is the single conversion point used by both the histogram and summary
/// runners to translate the deserialized configuration enum into the runtime
/// sampling model.
pub(crate) fn to_distribution(config: &DistributionConfig) -> Distribution {
    match config {
        DistributionConfig::Exponential { rate } => Distribution::Exponential { rate: *rate },
        DistributionConfig::Normal { mean, stddev } => Distribution::Normal {
            mean: *mean,
            stddev: *stddev,
        },
        DistributionConfig::Uniform { min, max } => Distribution::Uniform {
            min: *min,
            max: *max,
        },
    }
}

/// A single histogram sample produced by [`HistogramGenerator::observe`].
///
/// All values are cumulative — they never decrease across successive ticks.
/// This matches Prometheus histogram counter semantics.
#[derive(Debug, Clone)]
pub struct HistogramSample {
    /// Cumulative count per bucket. `bucket_counts[i]` is the number of
    /// observations that fell into bucket `i` or any earlier bucket
    /// (i.e., observations <= `buckets[i]`).
    pub bucket_counts: Vec<u64>,
    /// Total number of observations across all ticks.
    pub count: u64,
    /// Cumulative sum of all observed values.
    pub sum: f64,
}

/// Generates histogram samples by sampling from a configurable distribution.
///
/// Each call to [`observe`](HistogramGenerator::observe) draws
/// `observations_per_tick` samples from the configured distribution, updates
/// the cumulative bucket counts, count, and sum, and returns a
/// [`HistogramSample`].
///
/// The generator is deterministic: given the same seed, tick sequence, and
/// configuration, it always produces the same output.
pub struct HistogramGenerator {
    /// Sorted bucket upper bounds (exclusive of `+Inf`).
    buckets: Vec<f64>,
    /// Cumulative bucket counts. Length matches `buckets`.
    bucket_counts: Vec<u64>,
    /// Total observation count.
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

impl HistogramGenerator {
    /// Create a new histogram generator.
    ///
    /// # Parameters
    ///
    /// * `buckets` — sorted upper boundaries for histogram buckets. Use
    ///   [`DEFAULT_HISTOGRAM_BUCKETS`] when `None` is provided in config.
    /// * `distribution` — the probability distribution to sample from.
    /// * `observations_per_tick` — how many samples to draw each tick.
    /// * `mean_shift_per_sec` — linear drift per second for the distribution center.
    /// * `seed` — determinism seed for the RNG.
    /// * `rate` — scenario event rate (events/sec), used to convert tick index to elapsed seconds.
    pub fn new(
        buckets: Vec<f64>,
        distribution: Distribution,
        observations_per_tick: u64,
        mean_shift_per_sec: f64,
        seed: u64,
        rate: f64,
    ) -> Self {
        let bucket_counts = vec![0u64; buckets.len()];
        Self {
            buckets,
            bucket_counts,
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

    /// Return a reference to the bucket boundaries.
    pub fn buckets(&self) -> &[f64] {
        &self.buckets
    }

    /// Advance the generator by one tick, sampling observations and updating
    /// cumulative state.
    ///
    /// # Parameters
    ///
    /// * `tick` — the current tick index (used for time-varying shift calculation).
    ///
    /// # Returns
    ///
    /// A [`HistogramSample`] with cumulative bucket counts, count, and sum.
    pub fn observe(&mut self, tick: u64) -> HistogramSample {
        let elapsed_secs = tick as f64 / self.rate;
        let shift = self.mean_shift_per_sec * elapsed_secs;

        for i in 0..self.observations_per_tick {
            // Generate a deterministic observation value.
            // Use tick_counter (monotonically increasing) to ensure unique RNG
            // state even if observe() is called with non-sequential tick values.
            let rng_input = self
                .seed
                .wrapping_mul(0x517c_c1b7_2722_0a95)
                .wrapping_add(self.tick_counter)
                .wrapping_mul(0x6c62_272e_07bb_0142)
                .wrapping_add(i);
            let value = self.sample_distribution(rng_input, shift);

            self.count += 1;
            self.sum += value;

            // Update cumulative bucket counts. Each bucket counts observations
            // that are <= the bucket upper bound.
            for (j, &bound) in self.buckets.iter().enumerate() {
                if value <= bound {
                    self.bucket_counts[j] += 1;
                }
            }
        }
        self.tick_counter += 1;

        HistogramSample {
            bucket_counts: self.bucket_counts.clone(),
            count: self.count,
            sum: self.sum,
        }
    }

    /// Sample a single value from the configured distribution with an optional
    /// mean shift applied.
    fn sample_distribution(&self, rng_input: u64, shift: f64) -> f64 {
        match &self.distribution {
            Distribution::Exponential { rate } => {
                let u = uniform_01(rng_input);
                // CDF inversion: x = -ln(1 - u) / lambda
                // Clamp u away from 1.0 to avoid ln(0).
                let u_clamped = u.min(1.0 - f64::EPSILON);
                let value = -(1.0 - u_clamped).ln() / rate;
                value + shift
            }
            Distribution::Normal { mean, stddev } => {
                // Box-Muller transform using two uniform samples.
                let u1 = uniform_01(rng_input);
                let u2 = uniform_01(splitmix64(rng_input.wrapping_add(1)));
                // Clamp u1 away from 0 to avoid ln(0).
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
}

/// Convert a `u64` hash output to a uniform `f64` in `[0, 1)`.
fn uniform_01(input: u64) -> f64 {
    let hash = splitmix64(input);
    (hash >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a histogram generator with exponential distribution.
    fn exponential_gen(buckets: Vec<f64>, rate: f64, seed: u64) -> HistogramGenerator {
        HistogramGenerator::new(
            buckets,
            Distribution::Exponential { rate },
            100, // observations_per_tick
            0.0, // no mean shift
            seed,
            10.0, // scenario rate
        )
    }

    // ---- Cumulative semantics ------------------------------------------------

    /// Bucket counts never decrease across ticks.
    #[test]
    fn bucket_counts_never_decrease_across_ticks() {
        let mut gen = exponential_gen(DEFAULT_HISTOGRAM_BUCKETS.to_vec(), 10.0, 42);
        let mut prev = gen.observe(0);
        for tick in 1..20 {
            let curr = gen.observe(tick);
            for (i, (&prev_count, &curr_count)) in prev
                .bucket_counts
                .iter()
                .zip(curr.bucket_counts.iter())
                .enumerate()
            {
                assert!(
                    curr_count >= prev_count,
                    "bucket {i} decreased: {prev_count} -> {curr_count} at tick {tick}"
                );
            }
            assert!(curr.count >= prev.count, "count decreased at tick {tick}");
            prev = curr;
        }
    }

    /// The count always equals the total observations per tick times the number of ticks.
    #[test]
    fn count_equals_observations_times_ticks() {
        let mut gen = HistogramGenerator::new(
            DEFAULT_HISTOGRAM_BUCKETS.to_vec(),
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

    // ---- +Inf bucket equals count -------------------------------------------

    /// The last bucket in a histogram with the right boundaries should have
    /// count equal to total count if all observations fall within range.
    /// For exponential, all values are non-negative, and with the +Inf
    /// convention handled by the runner, we verify that the largest bucket
    /// captures most observations.
    #[test]
    fn largest_bucket_captures_most_observations() {
        let mut gen = exponential_gen(vec![0.1, 0.5, 1.0, 5.0, 100.0], 10.0, 42);
        let sample = gen.observe(0);
        // With rate=10 (mean=0.1), the 100.0 bucket should capture nearly all observations.
        assert!(
            sample.bucket_counts[4] >= 95,
            "largest bucket should capture most of 100 observations, got {}",
            sample.bucket_counts[4]
        );
    }

    // ---- Determinism --------------------------------------------------------

    /// Same seed and config produce identical output.
    #[test]
    fn same_seed_produces_identical_output() {
        let mut gen_a = exponential_gen(DEFAULT_HISTOGRAM_BUCKETS.to_vec(), 10.0, 42);
        let mut gen_b = exponential_gen(DEFAULT_HISTOGRAM_BUCKETS.to_vec(), 10.0, 42);
        for tick in 0..10 {
            let a = gen_a.observe(tick);
            let b = gen_b.observe(tick);
            assert_eq!(
                a.bucket_counts, b.bucket_counts,
                "bucket counts must match at tick {tick}"
            );
            assert_eq!(a.count, b.count, "count must match at tick {tick}");
            assert_eq!(a.sum, b.sum, "sum must match at tick {tick}");
        }
    }

    /// Different seeds produce different output.
    #[test]
    fn different_seeds_produce_different_output() {
        let mut gen_a = exponential_gen(DEFAULT_HISTOGRAM_BUCKETS.to_vec(), 10.0, 1);
        let mut gen_b = exponential_gen(DEFAULT_HISTOGRAM_BUCKETS.to_vec(), 10.0, 2);
        let a = gen_a.observe(0);
        let b = gen_b.observe(0);
        assert_ne!(
            a.bucket_counts, b.bucket_counts,
            "different seeds should produce different bucket counts"
        );
    }

    // ---- Default buckets ----------------------------------------------------

    /// DEFAULT_HISTOGRAM_BUCKETS is sorted and matches Prometheus defaults.
    #[test]
    fn default_buckets_are_sorted_and_positive() {
        for window in DEFAULT_HISTOGRAM_BUCKETS.windows(2) {
            assert!(
                window[0] < window[1],
                "default buckets must be strictly sorted: {} >= {}",
                window[0],
                window[1]
            );
        }
        for &b in DEFAULT_HISTOGRAM_BUCKETS {
            assert!(b > 0.0, "default bucket {b} must be positive");
        }
    }

    /// Default bucket count matches expected Prometheus defaults.
    #[test]
    fn default_buckets_have_expected_count() {
        assert_eq!(
            DEFAULT_HISTOGRAM_BUCKETS.len(),
            11,
            "Prometheus default has 11 buckets"
        );
    }

    // ---- Distribution models ------------------------------------------------

    /// Exponential distribution produces non-negative values.
    #[test]
    fn exponential_values_are_non_negative() {
        let mut gen = exponential_gen(vec![0.1, 1.0, 10.0], 5.0, 99);
        let sample = gen.observe(0);
        // Sum should be non-negative for exponential distribution.
        assert!(
            sample.sum >= 0.0,
            "exponential sum must be non-negative, got {}",
            sample.sum
        );
    }

    /// Normal distribution produces values centered around the mean.
    #[test]
    fn normal_distribution_centers_around_mean() {
        let mut gen = HistogramGenerator::new(
            vec![0.0, 0.05, 0.1, 0.15, 0.2, 0.5, 1.0],
            Distribution::Normal {
                mean: 0.1,
                stddev: 0.02,
            },
            10000,
            0.0,
            42,
            10.0,
        );
        let sample = gen.observe(0);
        let average = sample.sum / sample.count as f64;
        assert!(
            (average - 0.1).abs() < 0.01,
            "normal mean should be ~0.1, got {average}"
        );
    }

    /// Uniform distribution produces values within the configured range.
    #[test]
    fn uniform_distribution_within_range() {
        let mut gen = HistogramGenerator::new(
            vec![1.0, 2.0, 3.0, 4.0, 5.0],
            Distribution::Uniform { min: 1.0, max: 5.0 },
            10000,
            0.0,
            42,
            10.0,
        );
        let sample = gen.observe(0);
        let average = sample.sum / sample.count as f64;
        // Uniform [1,5] has mean = 3.0
        assert!(
            (average - 3.0).abs() < 0.1,
            "uniform mean should be ~3.0, got {average}"
        );
    }

    // ---- Time-varying distribution ------------------------------------------

    /// Mean shift changes the distribution over time.
    #[test]
    fn mean_shift_increases_values_over_time() {
        let mut gen = HistogramGenerator::new(
            vec![0.1, 0.5, 1.0, 5.0, 10.0, 50.0],
            Distribution::Exponential { rate: 10.0 },
            1000,
            1.0, // shift 1.0 per second
            42,
            1.0, // 1 tick per second
        );
        let sample_early = gen.observe(0);
        let avg_early = sample_early.sum / 1000.0;

        // Observe at tick 10 (10 seconds later, shift = 10.0)
        for tick in 1..10 {
            gen.observe(tick);
        }
        let sample_late = gen.observe(10);
        let avg_late = (sample_late.sum - sample_early.sum - {
            // Sum from ticks 1..10
            let mut sum = 0.0;
            let mut gen2 = HistogramGenerator::new(
                vec![0.1, 0.5, 1.0, 5.0, 10.0, 50.0],
                Distribution::Exponential { rate: 10.0 },
                1000,
                1.0,
                42,
                1.0,
            );
            for t in 0..10 {
                let s = gen2.observe(t);
                sum = s.sum;
            }
            sum - sample_early.sum
        }) / 1000.0;

        // The late average should be higher due to mean shift.
        // At tick 10, shift = 10.0, so values should be shifted by ~10.
        assert!(
            avg_late > avg_early,
            "late average ({avg_late}) should be higher than early ({avg_early}) due to mean shift"
        );
    }

    // ---- Edge cases ---------------------------------------------------------

    /// Single bucket works correctly.
    #[test]
    fn single_bucket_works() {
        let mut gen = HistogramGenerator::new(
            vec![1.0],
            Distribution::Uniform { min: 0.0, max: 2.0 },
            100,
            0.0,
            42,
            10.0,
        );
        let sample = gen.observe(0);
        assert_eq!(sample.bucket_counts.len(), 1);
        assert_eq!(sample.count, 100);
        // Some observations should be <= 1.0 (approximately half for uniform [0,2])
        assert!(
            sample.bucket_counts[0] > 20,
            "single bucket should capture some observations, got {}",
            sample.bucket_counts[0]
        );
    }

    /// Sum accumulates correctly across ticks.
    #[test]
    fn sum_accumulates_across_ticks() {
        let mut gen = HistogramGenerator::new(
            vec![1.0],
            Distribution::Uniform { min: 0.0, max: 1.0 },
            100,
            0.0,
            42,
            10.0,
        );
        let s1 = gen.observe(0);
        let s2 = gen.observe(1);
        assert!(
            s2.sum > s1.sum,
            "sum must increase across ticks: {} -> {}",
            s1.sum,
            s2.sum
        );
    }

    // ---- uniform_01 ---------------------------------------------------------

    /// uniform_01 returns values in [0, 1).
    #[test]
    fn uniform_01_in_range() {
        for i in 0..10_000u64 {
            let v = uniform_01(i);
            assert!(
                (0.0..1.0).contains(&v),
                "uniform_01({i}) = {v} is outside [0, 1)"
            );
        }
    }

    // ---- Contract: Send + Sync ----------------------------------------------

    #[test]
    fn histogram_generator_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<HistogramGenerator>();
    }

    #[test]
    fn histogram_sample_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HistogramSample>();
    }
}
