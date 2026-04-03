//! Jitter wrapper — adds deterministic uniform noise to any value generator.
//!
//! The wrapper decorates a `Box<dyn ValueGenerator>`, adding noise in the range
//! `[-half_range, +half_range]` to each generated value. The noise is
//! deterministic: given the same seed and tick, the output is always identical.
//!
//! Determinism is achieved by hashing `seed ^ tick ^ JITTER_MAGIC` through the
//! shared [`splitmix64`](crate::util::splitmix64) mixer. The `JITTER_MAGIC`
//! constant prevents correlation with `UniformRandom` generators that also use
//! `splitmix64(seed ^ tick)`.

use super::ValueGenerator;
use crate::util::splitmix64;

/// XOR constant mixed into the seed to decorrelate jitter noise from
/// `UniformRandom` generators that use the same `splitmix64(seed ^ tick)` pattern.
const JITTER_MAGIC: u64 = 0xa076_1d64_78bd_642f;

/// Wraps an inner [`ValueGenerator`] and adds deterministic uniform noise.
///
/// For each tick, the output is `inner.value(tick) + noise` where noise is
/// uniformly distributed in `[-half_range, +half_range]`.
///
/// # Determinism
///
/// The noise sequence is fully determined by `seed` and `tick`. Two
/// `JitterWrapper` instances with the same seed, half_range, and inner
/// generator will produce identical outputs for every tick.
///
/// # Thread safety
///
/// `JitterWrapper` is `Send + Sync` because `Box<dyn ValueGenerator>` requires
/// `Send + Sync` and all other fields are plain data.
pub struct JitterWrapper {
    inner: Box<dyn ValueGenerator>,
    half_range: f64,
    seed: u64,
}

impl JitterWrapper {
    /// Create a new jitter wrapper.
    ///
    /// # Parameters
    ///
    /// - `inner` — the generator whose output will be perturbed.
    /// - `half_range` — the jitter amplitude. Noise will be in
    ///   `[-half_range, +half_range]`.
    /// - `seed` — determinism seed for the noise sequence.
    pub fn new(inner: Box<dyn ValueGenerator>, half_range: f64, seed: u64) -> Self {
        Self {
            inner,
            half_range,
            seed,
        }
    }
}

impl ValueGenerator for JitterWrapper {
    /// Return the inner generator's value plus deterministic uniform noise.
    ///
    /// The noise for each tick is computed by hashing `seed ^ tick ^ JITTER_MAGIC`
    /// through SplitMix64, mapping the result to `[-half_range, +half_range]`.
    fn value(&self, tick: u64) -> f64 {
        let base = self.inner.value(tick);
        let hash = splitmix64(self.seed ^ tick ^ JITTER_MAGIC);
        let unit = (hash as f64) / (u64::MAX as f64);
        let noise = unit * 2.0 * self.half_range - self.half_range;
        base + noise
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::constant::Constant;

    /// Helper: wrap a constant generator with jitter.
    fn jittered_constant(value: f64, half_range: f64, seed: u64) -> JitterWrapper {
        JitterWrapper::new(Box::new(Constant::new(value)), half_range, seed)
    }

    // ---- Determinism ---------------------------------------------------------

    #[test]
    fn same_seed_and_tick_returns_same_value() {
        let gen = jittered_constant(100.0, 5.0, 42);
        let v1 = gen.value(7);
        let v2 = gen.value(7);
        assert_eq!(v1, v2, "same seed+tick must produce identical output");
    }

    #[test]
    fn determinism_across_instances() {
        let gen_a = jittered_constant(100.0, 5.0, 99);
        let gen_b = jittered_constant(100.0, 5.0, 99);
        for tick in 0..100 {
            assert_eq!(
                gen_a.value(tick),
                gen_b.value(tick),
                "two wrappers with same seed must agree at tick {tick}"
            );
        }
    }

    // ---- Bounds --------------------------------------------------------------

    #[test]
    fn all_values_within_bounds_for_10000_ticks() {
        let base_value = 50.0;
        let half_range = 3.0;
        let gen = jittered_constant(base_value, half_range, 0);
        for tick in 0..10_000 {
            let v = gen.value(tick);
            assert!(
                v >= base_value - half_range && v <= base_value + half_range,
                "value {v} at tick {tick} is outside [{}, {}]",
                base_value - half_range,
                base_value + half_range
            );
        }
    }

    // ---- Noise is non-trivial ------------------------------------------------

    #[test]
    fn at_least_some_values_differ_from_inner() {
        let base_value = 50.0;
        let gen = jittered_constant(base_value, 5.0, 42);
        let any_differ = (0..100).any(|tick| gen.value(tick) != base_value);
        assert!(
            any_differ,
            "jitter must perturb at least some values away from the base"
        );
    }

    // ---- Different seeds produce different noise -----------------------------

    #[test]
    fn different_seeds_produce_different_sequences() {
        let gen_a = jittered_constant(100.0, 5.0, 1);
        let gen_b = jittered_constant(100.0, 5.0, 2);
        let any_differ = (0..100).any(|tick| gen_a.value(tick) != gen_b.value(tick));
        assert!(
            any_differ,
            "different seeds must produce different noise sequences"
        );
    }

    // ---- Zero half_range produces no noise -----------------------------------

    #[test]
    fn zero_half_range_returns_inner_value_exactly() {
        let base_value = 42.0;
        let gen = jittered_constant(base_value, 0.0, 99);
        for tick in 0..100 {
            assert_eq!(
                gen.value(tick),
                base_value,
                "zero half_range must produce no noise at tick {tick}"
            );
        }
    }

    // ---- Composition: jitter wrapping a non-constant generator ---------------

    #[test]
    fn jitter_wrapping_sine_stays_within_bounds() {
        use crate::generator::sine::Sine;

        let sine = Sine::new(20.0, 120.0, 50.0, 1.0);
        let half_range = 3.0;
        let gen = JitterWrapper::new(Box::new(sine), half_range, 42);
        for tick in 0..10_000 {
            let base = crate::generator::sine::Sine::new(20.0, 120.0, 50.0, 1.0).value(tick);
            let jittered = gen.value(tick);
            assert!(
                jittered >= base - half_range && jittered <= base + half_range,
                "jittered sine value {jittered} at tick {tick} outside [{}, {}]",
                base - half_range,
                base + half_range
            );
        }
    }

    // ---- JITTER_MAGIC decorrelates from UniformRandom ------------------------

    #[test]
    fn jitter_noise_differs_from_uniform_random_with_same_seed_and_tick() {
        use crate::generator::uniform::UniformRandom;

        // UniformRandom uses splitmix64(seed ^ tick).
        // JitterWrapper uses splitmix64(seed ^ tick ^ JITTER_MAGIC).
        // For the same seed, these should produce different hash values,
        // meaning different noise patterns.
        let uniform = UniformRandom::new(0.0, 1.0, 42);
        let jitter = jittered_constant(0.0, 0.5, 42);

        // The jitter noise for tick T is: jitter.value(T) - 0.0 (base).
        // The uniform value for tick T is uniform.value(T).
        // These should differ due to the JITTER_MAGIC XOR.
        let any_differ = (0..100).any(|tick| {
            let jitter_noise = jitter.value(tick); // base is 0.0, so this IS the noise
            let uniform_val = uniform.value(tick);
            // Rescale uniform from [0, 1] to [-0.5, 0.5] for comparison
            let uniform_noise = uniform_val - 0.5;
            (jitter_noise - uniform_noise).abs() > 1e-10
        });
        assert!(
            any_differ,
            "JITTER_MAGIC must decorrelate jitter from UniformRandom"
        );
    }
}
