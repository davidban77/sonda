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
