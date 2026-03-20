//! Uniform random value generator — returns a deterministic pseudo-random value
//! within `[min, max]` for each tick.
//!
//! Determinism is achieved via a hash-based approach: the seed and tick are mixed
//! using a series of bit operations (a simplified SplitMix64 finalizer) rather than
//! a stateful RNG. This keeps `ValueGenerator` stateless — `value()` takes `&self`.

use super::ValueGenerator;

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

    /// Mix a `u64` through a SplitMix64 finalizer to produce a well-distributed value.
    ///
    /// This is a stateless hash: same input always produces same output.
    fn mix(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl ValueGenerator for UniformRandom {
    /// Return a deterministic value in `[min, max]` for the given tick.
    ///
    /// The value is derived by hashing `seed XOR tick` through a SplitMix64
    /// finalizer and mapping the result into the target range.
    fn value(&self, tick: u64) -> f64 {
        let hash = Self::mix(self.seed ^ tick);
        // Map the u64 to [0.0, 1.0) then scale to [min, max].
        let unit = (hash as f64) / (u64::MAX as f64);
        self.min + unit * (self.max - self.min)
    }
}
