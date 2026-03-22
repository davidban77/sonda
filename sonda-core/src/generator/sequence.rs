//! Sequence value generator -- steps through an explicit list of values.

use super::ValueGenerator;
use crate::SondaError;

/// A value generator that steps through an explicit sequence of values.
///
/// When `repeat` is true (the default), the sequence cycles: `values[tick % len]`.
/// When `repeat` is false, returns the last value for all ticks beyond the
/// sequence length. This enables modeling real incident patterns like
/// `[0, 0, 0, 95, 95, 95, 0, 0]` for a CPU spike.
///
/// # Examples
///
/// ```
/// use sonda_core::generator::sequence::SequenceGenerator;
/// use sonda_core::generator::ValueGenerator;
///
/// // Repeating sequence: cycles through values
/// let gen = SequenceGenerator::new(vec![10.0, 20.0, 30.0], true).unwrap();
/// assert_eq!(gen.value(0), 10.0);
/// assert_eq!(gen.value(3), 10.0); // wraps around
///
/// // Non-repeating: clamps to last value
/// let gen = SequenceGenerator::new(vec![1.0, 2.0], false).unwrap();
/// assert_eq!(gen.value(5), 2.0); // clamped to last
/// ```
pub struct SequenceGenerator {
    values: Vec<f64>,
    repeat: bool,
}

impl SequenceGenerator {
    /// Create a new sequence generator.
    ///
    /// # Arguments
    ///
    /// * `values` - The sequence of values to step through. Must not be empty.
    /// * `repeat` - When true, the sequence cycles. When false, the last value
    ///   is returned for all ticks beyond the sequence length.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Config`] if `values` is empty.
    pub fn new(values: Vec<f64>, repeat: bool) -> Result<Self, SondaError> {
        if values.is_empty() {
            return Err(SondaError::Config(
                "sequence generator requires at least one value".to_string(),
            ));
        }
        Ok(Self { values, repeat })
    }
}

impl ValueGenerator for SequenceGenerator {
    fn value(&self, tick: u64) -> f64 {
        let len = self.values.len();
        let index = if self.repeat {
            (tick as usize) % len
        } else {
            (tick as usize).min(len - 1)
        };
        self.values[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Construction tests ---------------------------------------------------

    #[test]
    fn new_with_empty_values_returns_config_error() {
        let result = SequenceGenerator::new(vec![], true);
        assert!(result.is_err(), "empty values must be rejected");
        let err = result.err().expect("already confirmed is_err");
        let msg = format!("{err}");
        assert!(
            msg.contains("at least one value"),
            "error message should mention 'at least one value', got: {msg}"
        );
    }

    #[test]
    fn new_with_empty_values_repeat_false_returns_config_error() {
        let result = SequenceGenerator::new(vec![], false);
        assert!(
            result.is_err(),
            "empty values must be rejected even with repeat=false"
        );
    }

    #[test]
    fn new_with_single_value_succeeds() {
        let gen = SequenceGenerator::new(vec![42.0], true).expect("single value should be valid");
        assert_eq!(gen.value(0), 42.0);
    }

    #[test]
    fn new_with_multiple_values_succeeds() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], true)
            .expect("multiple values should be valid");
        assert_eq!(gen.value(0), 1.0);
    }

    // ---- Repeat=true tests (cycling behavior) ---------------------------------

    #[test]
    fn repeat_tick_zero_returns_first_value() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).unwrap();
        assert_eq!(gen.value(0), 1.0);
    }

    #[test]
    fn repeat_tick_one_returns_second_value() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).unwrap();
        assert_eq!(gen.value(1), 2.0);
    }

    #[test]
    fn repeat_tick_two_returns_third_value() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).unwrap();
        assert_eq!(gen.value(2), 3.0);
    }

    #[test]
    fn repeat_wraps_at_sequence_boundary() {
        // Spec: value(3) on a 3-element sequence returns values[0]
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).unwrap();
        assert_eq!(gen.value(3), 1.0, "tick=3 should wrap to index 0");
    }

    #[test]
    fn repeat_wraps_correctly_at_tick_5() {
        // Spec: value(5) on a 3-element sequence returns values[2] (5 % 3 = 2)
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).unwrap();
        assert_eq!(gen.value(5), 3.0, "tick=5 should wrap to index 2");
    }

    #[test]
    fn repeat_multiple_full_cycles() {
        let gen = SequenceGenerator::new(vec![10.0, 20.0], true).unwrap();
        // Two full cycles: ticks 0-3
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
        assert_eq!(gen.value(2), 10.0);
        assert_eq!(gen.value(3), 20.0);
    }

    #[test]
    fn repeat_single_value_always_returns_that_value() {
        let gen = SequenceGenerator::new(vec![7.5], true).unwrap();
        assert_eq!(gen.value(0), 7.5);
        assert_eq!(gen.value(1), 7.5);
        assert_eq!(gen.value(100), 7.5);
        assert_eq!(gen.value(999_999), 7.5);
    }

    // ---- Repeat=false tests (clamped behavior) --------------------------------

    #[test]
    fn no_repeat_tick_zero_returns_first_value() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0], false).unwrap();
        assert_eq!(gen.value(0), 1.0);
    }

    #[test]
    fn no_repeat_tick_one_returns_second_value() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0], false).unwrap();
        assert_eq!(gen.value(1), 2.0);
    }

    #[test]
    fn no_repeat_beyond_length_clamps_to_last() {
        // Spec: value(5) on a 2-element non-repeating sequence returns the last value
        let gen = SequenceGenerator::new(vec![1.0, 2.0], false).unwrap();
        assert_eq!(
            gen.value(5),
            2.0,
            "tick beyond sequence length should clamp to last value"
        );
    }

    #[test]
    fn no_repeat_at_exact_boundary_clamps_to_last() {
        // tick=2 on a 2-element sequence (indices 0,1) should return last
        let gen = SequenceGenerator::new(vec![1.0, 2.0], false).unwrap();
        assert_eq!(gen.value(2), 2.0);
    }

    #[test]
    fn no_repeat_single_value_always_returns_that_value() {
        let gen = SequenceGenerator::new(vec![99.0], false).unwrap();
        assert_eq!(gen.value(0), 99.0);
        assert_eq!(gen.value(1), 99.0);
        assert_eq!(gen.value(1000), 99.0);
    }

    // ---- Large tick values (no panic) -----------------------------------------

    #[test]
    fn repeat_large_tick_does_not_panic() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).unwrap();
        // u64::MAX would cause issues if converted to usize on 32-bit, but on 64-bit
        // it's just a large modulo. Use a large but safe value.
        let large_tick: u64 = 1_000_000_000;
        let val = gen.value(large_tick);
        let expected_index = (large_tick as usize) % 3;
        let expected = [1.0, 2.0, 3.0][expected_index];
        assert_eq!(val, expected);
    }

    #[test]
    fn no_repeat_large_tick_does_not_panic() {
        let gen = SequenceGenerator::new(vec![1.0, 2.0, 3.0], false).unwrap();
        let large_tick: u64 = 1_000_000_000;
        assert_eq!(gen.value(large_tick), 3.0, "should clamp to last value");
    }

    // ---- Determinism ----------------------------------------------------------

    #[test]
    fn determinism_same_tick_returns_same_value() {
        let gen = SequenceGenerator::new(vec![10.0, 20.0, 30.0], true).unwrap();
        for tick in 0..100 {
            let first_call = gen.value(tick);
            let second_call = gen.value(tick);
            assert_eq!(
                first_call, second_call,
                "value must be deterministic: tick={tick} returned {first_call} then {second_call}"
            );
        }
    }

    #[test]
    fn determinism_separate_instances_same_config() {
        let gen1 = SequenceGenerator::new(vec![5.0, 10.0, 15.0], true).unwrap();
        let gen2 = SequenceGenerator::new(vec![5.0, 10.0, 15.0], true).unwrap();
        for tick in 0..100 {
            assert_eq!(
                gen1.value(tick),
                gen2.value(tick),
                "two generators with same config must produce same values at tick={tick}"
            );
        }
    }

    // ---- Send + Sync contract -------------------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn sequence_generator_is_send_and_sync() {
        assert_send_sync::<SequenceGenerator>();
    }

    // ---- Incident pattern modeling (real-world usage) -------------------------

    #[test]
    fn cpu_spike_pattern_produces_expected_values() {
        // Model the example from the spec: baseline at 10, spike to 95, recovery
        let pattern = vec![
            10.0, 10.0, 10.0, 10.0, 10.0, 95.0, 95.0, 95.0, 95.0, 95.0, 10.0, 10.0, 10.0, 10.0,
            10.0, 10.0,
        ];
        let gen = SequenceGenerator::new(pattern.clone(), true).unwrap();

        // First cycle
        for (i, expected) in pattern.iter().enumerate() {
            assert_eq!(
                gen.value(i as u64),
                *expected,
                "first cycle mismatch at tick={i}"
            );
        }

        // Second cycle starts at tick 16
        for (i, expected) in pattern.iter().enumerate() {
            assert_eq!(
                gen.value((i + 16) as u64),
                *expected,
                "second cycle mismatch at tick={}",
                i + 16
            );
        }
    }

    // ---- Floating point edge cases -------------------------------------------

    #[test]
    fn handles_negative_values() {
        let gen = SequenceGenerator::new(vec![-1.0, -2.5, 0.0, 3.14], true).unwrap();
        assert_eq!(gen.value(0), -1.0);
        assert_eq!(gen.value(1), -2.5);
        assert_eq!(gen.value(2), 0.0);
        assert_eq!(gen.value(3), 3.14);
    }

    #[test]
    fn handles_special_float_values() {
        let gen =
            SequenceGenerator::new(vec![f64::INFINITY, f64::NEG_INFINITY, 0.0], true).unwrap();
        assert_eq!(gen.value(0), f64::INFINITY);
        assert_eq!(gen.value(1), f64::NEG_INFINITY);
        assert_eq!(gen.value(2), 0.0);
    }
}
