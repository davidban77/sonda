//! Uniform random value generator — returns a deterministic pseudo-random value
//! within `[min, max]` for each tick.
//!
//! Determinism is achieved via a hash-based approach: the seed and tick are mixed
//! using a series of bit operations (a simplified SplitMix64 finalizer) rather than
//! a stateful RNG. This keeps `ValueGenerator` stateless — `value()` takes `&self`.

use super::ValueGenerator;
use crate::util::splitmix64;

/// Generates uniformly distributed random values in `[min, max]`.
///
/// The output is deterministic: given the same `seed` and `tick`, `value()` always
/// returns the same `f64`. Different seeds produce independent sequences.
///
/// The hash-based approach avoids mutable state, satisfying the `&self` contract of
/// [`ValueGenerator`].
pub struct UniformRandom {
    min: f64,
    max: f64,
    seed: u64,
}

impl UniformRandom {
    /// Construct a new `UniformRandom` generator.
    ///
    /// # Parameters
    /// - `min` — lower bound of the output range (inclusive).
    /// - `max` — upper bound of the output range (inclusive).
    /// - `seed` — determinism seed. Use different seeds for independent sequences.
    pub fn new(min: f64, max: f64, seed: u64) -> Self {
        Self { min, max, seed }
    }
}

impl ValueGenerator for UniformRandom {
    /// Return a deterministic value in `[min, max]` for the given tick.
    ///
    /// The value is derived by hashing `seed XOR tick` through a SplitMix64
    /// finalizer and mapping the result into the target range.
    fn value(&self, tick: u64) -> f64 {
        let hash = splitmix64(self.seed ^ tick);
        // Map the u64 to [0.0, 1.0) then scale to [min, max].
        let unit = (hash as f64) / (u64::MAX as f64);
        self.min + unit * (self.max - self.min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_and_tick_returns_same_value() {
        let gen = UniformRandom::new(0.0, 100.0, 42);
        let v1 = gen.value(7);
        let v2 = gen.value(7);
        assert_eq!(v1, v2, "same seed+tick must produce identical output");
    }

    #[test]
    fn determinism_across_instances() {
        let gen_a = UniformRandom::new(0.0, 100.0, 99);
        let gen_b = UniformRandom::new(0.0, 100.0, 99);
        for tick in 0..100 {
            assert_eq!(
                gen_a.value(tick),
                gen_b.value(tick),
                "two generators with same seed must agree at tick {tick}"
            );
        }
    }

    #[test]
    fn all_values_within_range_for_10000_ticks() {
        let gen = UniformRandom::new(5.0, 10.0, 0);
        for tick in 0..10_000 {
            let v = gen.value(tick);
            assert!(
                v >= 5.0 && v <= 10.0,
                "value {v} at tick {tick} is outside [5.0, 10.0]"
            );
        }
    }

    #[test]
    fn different_seeds_produce_different_sequences() {
        let gen_a = UniformRandom::new(0.0, 1.0, 1);
        let gen_b = UniformRandom::new(0.0, 1.0, 2);
        // With a good hash function, the sequences should differ at every tick.
        // We test that at least some values differ across a run.
        let any_differ = (0..100).any(|tick| gen_a.value(tick) != gen_b.value(tick));
        assert!(
            any_differ,
            "different seeds must produce different sequences"
        );
    }

    #[test]
    fn different_ticks_produce_different_values() {
        let gen = UniformRandom::new(0.0, 1.0, 0);
        // With a quality hash it is extremely unlikely that 100 consecutive ticks
        // all produce the same value.
        let first = gen.value(0);
        let any_differ = (1..100).any(|tick| gen.value(tick) != first);
        assert!(
            any_differ,
            "consecutive ticks must not all produce the same value"
        );
    }

    #[test]
    fn zero_range_returns_min() {
        let gen = UniformRandom::new(7.5, 7.5, 0);
        for tick in 0..10 {
            assert_eq!(
                gen.value(tick),
                7.5,
                "zero-width range should return min at tick {tick}"
            );
        }
    }
}
