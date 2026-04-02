//! Shared utility functions used across sonda-core modules.
//!
//! This module contains small, reusable primitives that do not belong to any
//! single domain module (generators, encoders, sinks, schedules).

/// SplitMix64 mixing function — a deterministic, stateless hash of a `u64` input.
///
/// Produces a well-distributed output from any input value. This is the
/// finalizer from Sebastiano Vigna's SplitMix64 PRNG, used here as a
/// one-shot hash rather than a stateful generator.
///
/// # Properties
///
/// - **Deterministic**: same input always produces the same output.
/// - **Stateless**: no mutable state; suitable for `&self` methods.
/// - **Well-distributed**: avalanche properties ensure small input changes
///   produce large output changes across all 64 bits.
///
/// # Usage
///
/// Typically called with `seed ^ tick` (or similar) to produce a
/// pseudo-random but reproducible value for a given scenario tick.
///
/// ```
/// # use sonda_core::util::splitmix64;
/// let a = splitmix64(42);
/// let b = splitmix64(42);
/// assert_eq!(a, b, "same input always produces the same output");
///
/// let c = splitmix64(43);
/// assert_ne!(a, c, "different inputs produce different outputs");
/// ```
pub fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_input_produces_same_output() {
        assert_eq!(splitmix64(0), splitmix64(0));
        assert_eq!(splitmix64(42), splitmix64(42));
        assert_eq!(splitmix64(u64::MAX), splitmix64(u64::MAX));
    }

    #[test]
    fn different_inputs_produce_different_outputs() {
        // With a quality mixing function, consecutive inputs should never collide.
        let outputs: Vec<u64> = (0..1000).map(splitmix64).collect();
        let mut deduped = outputs.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            outputs.len(),
            deduped.len(),
            "1000 consecutive inputs must all produce distinct outputs"
        );
    }

    #[test]
    fn zero_input_does_not_produce_zero() {
        // The additive constant ensures that input 0 does not map to 0.
        assert_ne!(splitmix64(0), 0);
    }

    #[test]
    fn max_input_does_not_panic() {
        let _ = splitmix64(u64::MAX);
    }

    #[test]
    fn known_output_regression_anchor() {
        // Pin the output for input 0 to catch accidental changes to the constants.
        // This value was computed from the canonical SplitMix64 finalizer.
        let result = splitmix64(0);
        assert_eq!(
            result, 16294208416658607535,
            "splitmix64(0) must match the canonical SplitMix64 output"
        );
    }

    #[test]
    fn output_covers_full_bit_width() {
        // Verify that outputs use high bits, not just low bits.
        let mut any_high_set = false;
        for i in 0..100 {
            let out = splitmix64(i);
            if out & 0xFFFF_FFFF_0000_0000 != 0 {
                any_high_set = true;
                break;
            }
        }
        assert!(
            any_high_set,
            "at least one output in 0..100 must have high 32 bits set"
        );
    }
}
