//! Shared utility functions used across sonda-core modules.
//!
//! This module contains small, reusable primitives that do not belong to any
//! single domain module (generators, encoders, sinks, schedules).

/// The constant [`splitmix64`] adds first.
const SPLITMIX64_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
/// The odd constant [`splitmix64`] multiplies by after its first xor-shift.
const SPLITMIX64_MIX1: u64 = 0xbf58_476d_1ce4_e5b9;
/// The odd constant [`splitmix64`] multiplies by after its second xor-shift.
const SPLITMIX64_MIX2: u64 = 0x94d0_49bb_1331_11eb;
/// The inverse of [`SPLITMIX64_MIX1`] modulo 2^64.
const SPLITMIX64_MIX1_INVERSE: u64 = inverse_of_odd(SPLITMIX64_MIX1);
/// The inverse of [`SPLITMIX64_MIX2`] modulo 2^64.
const SPLITMIX64_MIX2_INVERSE: u64 = inverse_of_odd(SPLITMIX64_MIX2);

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
pub(crate) fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(SPLITMIX64_GAMMA);
    z = (z ^ (z >> 30)).wrapping_mul(SPLITMIX64_MIX1);
    z = (z ^ (z >> 27)).wrapping_mul(SPLITMIX64_MIX2);
    z ^ (z >> 31)
}

/// The inverse of [`splitmix64`]: `splitmix64_inverse(splitmix64(z)) == z`
/// for every `z`. Each step of [`splitmix64`] is undone in reverse order:
/// an xor-shift by [`unxorshift`], a multiplication by the constant's
/// inverse modulo 2^64, and the addition by a subtraction.
pub(crate) fn splitmix64_inverse(mut z: u64) -> u64 {
    z = unxorshift(z, 31).wrapping_mul(SPLITMIX64_MIX2_INVERSE);
    z = unxorshift(z, 27).wrapping_mul(SPLITMIX64_MIX1_INVERSE);
    unxorshift(z, 30).wrapping_sub(SPLITMIX64_GAMMA)
}

/// The `x` with `x ^ (x >> shift) == z`, for `shift` in `1..64`.
fn unxorshift(z: u64, shift: u32) -> u64 {
    let mut x = z;
    let mut known = shift;
    while known < u64::BITS {
        x = z ^ (x >> shift);
        known += shift;
    }
    x
}

/// The inverse of odd `a` modulo 2^64, by Newton's iteration: `a` is its
/// own inverse modulo 8, and each step doubles the number of correct bits.
const fn inverse_of_odd(a: u64) -> u64 {
    let mut x = a;
    let mut step = 0;
    while step < 5 {
        x = x.wrapping_mul(2u64.wrapping_sub(a.wrapping_mul(x)));
        step += 1;
    }
    x
}

/// The number behind the value at `index` of a `random` cardinality spike:
/// `splitmix64(seed ^ index)`.
pub(crate) fn random_spike_hash(seed: u64, index: u64) -> u64 {
    splitmix64(seed ^ index)
}

/// The index whose [`random_spike_hash`] under `seed` is `hash`:
/// `splitmix64_inverse(hash) ^ seed`. A spike of `cardinality` values holds
/// the value with that hash exactly when the index is below `cardinality`.
pub(crate) fn random_spike_index(seed: u64, hash: u64) -> u64 {
    splitmix64_inverse(hash) ^ seed
}

/// The value at `index` of a `random` cardinality spike: `prefix` followed by
/// [`random_spike_hash`] as 16 lowercase hexadecimal digits.
pub(crate) fn random_spike_value(prefix: &str, seed: u64, index: u64) -> String {
    format!("{prefix}{:016x}", random_spike_hash(seed, index))
}

/// Installs rustls's ring provider as the process-wide default crypto
/// provider when no default is installed yet, and leaves an installed one,
/// whoever installed it, in place.
///
/// The default is process state: once set, every rustls configuration in
/// the process that does not name a provider uses it, and it cannot be
/// replaced. If another thread installs one between the check and the
/// install, its provider stays the default. Release builds compile ring
/// alone; a build that compiles both of rustls's providers has no default
/// until one is installed.
#[cfg(any(feature = "kafka", feature = "gnmi"))]
pub(crate) fn ensure_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

/// Whether `a` and `b` hold the same bytes. Slices of different lengths
/// are unequal at once; slices of one length are compared in full, every
/// byte pair XORed into one accumulator, so the time taken does not depend
/// on where they first differ.
#[cfg(feature = "gnmi")]
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a
        .iter()
        .zip(b)
        .fold(0u8, |acc, (x, y)| std::hint::black_box(acc | (x ^ y)));
    diff == 0
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

    /// The inputs of the inverse tests: fixed edge values, then 10 000 values
    /// of an xorshift64 stream with a fixed seed.
    fn inverse_inputs() -> Vec<u64> {
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut inputs = vec![0, 42, u64::MAX, 1 << 63];
        inputs.extend((0..10_000).map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }));
        inputs
    }

    #[test]
    fn inverse_of_odd_inverts_both_multipliers() {
        assert_eq!(SPLITMIX64_MIX1.wrapping_mul(SPLITMIX64_MIX1_INVERSE), 1);
        assert_eq!(SPLITMIX64_MIX2.wrapping_mul(SPLITMIX64_MIX2_INVERSE), 1);
        for a in [1, 3, u64::MAX] {
            assert_eq!(a.wrapping_mul(inverse_of_odd(a)), 1, "{a:#x}");
        }
    }

    #[test]
    fn splitmix64_inverse_undoes_splitmix64() {
        let inputs = inverse_inputs();
        let distinct: std::collections::BTreeSet<u64> = inputs.iter().copied().collect();
        assert!(distinct.len() > 10_000, "the stream yields distinct inputs");
        for x in inputs {
            assert_eq!(splitmix64_inverse(splitmix64(x)), x, "{x:#x}");
            assert_eq!(splitmix64(splitmix64_inverse(x)), x, "{x:#x}");
        }
    }

    #[test]
    fn random_spike_index_inverts_random_spike_hash() {
        for seed in [0, 7, 42, 1 << 20, u64::MAX] {
            for index in (0..1_000).chain([1 << 32, 1 << 63, u64::MAX]) {
                assert_eq!(
                    random_spike_index(seed, random_spike_hash(seed, index)),
                    index,
                    "seed {seed:#x}, index {index:#x}"
                );
            }
        }
    }

    #[test]
    fn random_spike_hash_mixes_seed_and_index() {
        assert_eq!(random_spike_hash(42, 0), splitmix64(42));
        assert_eq!(random_spike_hash(42, 1), splitmix64(43));
        assert_eq!(random_spike_hash(0, 7), random_spike_hash(7, 0));
    }

    #[test]
    fn random_spike_value_is_prefix_and_sixteen_hex_digits() {
        assert_eq!(random_spike_value("err-", 42, 0), "err-bdd732262feb6e95");
        assert_eq!(random_spike_value("err-", 42, 1), "err-ba69ec90eb4fef88");
        assert_eq!(random_spike_value("", 0, 0), "e220a8397b1dcdaf");
    }

    #[test]
    fn random_spike_value_pads_to_sixteen_digits() {
        let index = (0..u64::MAX)
            .find(|&i| random_spike_hash(0, i) >> 60 == 0)
            .expect("some index in range hashes below 2^60");
        let value = random_spike_value("p", 0, index);
        assert_eq!(value.len(), 17, "{value}");
        assert!(value.starts_with("p0"), "{value}");
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

    /// `constant_time_eq` answers as `==` does on 1 000 pairs of random
    /// bytes, a third of them equal and a third differing in one byte, and
    /// on slices of different lengths.
    #[cfg(feature = "gnmi")]
    #[test]
    fn constant_time_eq_agrees_with_eq() {
        let byte = |seed: u64, i: usize| splitmix64(seed ^ i as u64) as u8;
        let (mut equal, mut unequal) = (0, 0);
        for pair in 0..1_000u64 {
            let len = (splitmix64(pair) % 33) as usize;
            let a: Vec<u8> = (0..len).map(|i| byte(pair, i)).collect();
            let mut b = match pair % 3 {
                0 => a.clone(),
                _ => (0..len).map(|i| byte(!pair, i)).collect(),
            };
            if pair % 3 == 1 && len > 0 {
                b = a.clone();
                b[(splitmix64(!pair) % len as u64) as usize] ^= 1;
            }
            assert_eq!(constant_time_eq(&a, &b), a == b, "{a:?} and {b:?}");
            if a == b {
                equal += 1;
            } else {
                unequal += 1;
            }
        }
        assert!(
            equal > 300 && unequal > 600,
            "the premise: both outcomes occur ({equal} equal, {unequal} unequal)"
        );

        assert!(!constant_time_eq(b"secret", b"secret!"));
        assert!(!constant_time_eq(b"secret!", b"secret"));
        assert!(!constant_time_eq(b"", b"s"));
        assert!(constant_time_eq(b"", b""));
    }
}
