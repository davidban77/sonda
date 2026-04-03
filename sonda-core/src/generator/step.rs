//! Step (monotonic counter) value generator — produces linearly increasing values
//! with optional wrap-around, ideal for testing `rate()` and `increase()` PromQL functions.

use super::ValueGenerator;

/// Generates monotonically increasing counter values: `start + tick * step_size`.
///
/// When `max` is set (and greater than `start`), the value wraps around at the
/// threshold using modular arithmetic, simulating a counter reset. This is useful
/// for testing PromQL `rate()` and `increase()` functions that expect monotonic
/// counters with occasional resets.
///
/// # Examples
///
/// ```
/// use sonda_core::generator::step::StepGenerator;
/// use sonda_core::generator::ValueGenerator;
///
/// // Unbounded linear growth: 0, 1, 2, 3, ...
/// let gen = StepGenerator::new(0.0, 1.0, None);
/// assert_eq!(gen.value(0), 0.0);
/// assert_eq!(gen.value(3), 3.0);
///
/// // Wrap at max=3: 0, 1, 2, 0, 1, 2, ...
/// let gen = StepGenerator::new(0.0, 1.0, Some(3.0));
/// assert_eq!(gen.value(3), 0.0);
/// ```
pub struct StepGenerator {
    start: f64,
    step_size: f64,
    /// When `Some(m)` and `m > start`, the value wraps via modular arithmetic.
    /// When `None` or `m <= start`, growth is unbounded.
    max: Option<f64>,
}

impl StepGenerator {
    /// Construct a new `StepGenerator`.
    ///
    /// # Parameters
    /// - `start` — the initial value at tick 0.
    /// - `step_size` — the increment applied per tick.
    /// - `max` — optional wrap-around threshold. When `Some(m)` and `m > start`,
    ///   the value wraps via `start + ((tick * step_size) % (m - start))`.
    ///   When `None` or `m <= start`, the value grows without bound.
    pub fn new(start: f64, step_size: f64, max: Option<f64>) -> Self {
        Self {
            start,
            step_size,
            max,
        }
    }
}

impl ValueGenerator for StepGenerator {
    /// Return the counter value for the given tick.
    ///
    /// Pure function with no allocations. Computes the value arithmetically
    /// from the tick index — no accumulated state.
    fn value(&self, tick: u64) -> f64 {
        let raw = tick as f64 * self.step_size;
        match self.max {
            Some(m) if m > self.start => {
                let range = m - self.start;
                self.start + (raw.rem_euclid(range))
            }
            _ => self.start + raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_zero_returns_start() {
        let gen = StepGenerator::new(5.0, 1.0, None);
        assert_eq!(gen.value(0), 5.0);
    }

    #[test]
    fn tick_zero_returns_start_with_max() {
        let gen = StepGenerator::new(10.0, 2.0, Some(100.0));
        assert_eq!(gen.value(0), 10.0);
    }

    #[test]
    fn linear_increase_without_max() {
        let gen = StepGenerator::new(0.0, 1.0, None);
        for tick in 0..10u64 {
            assert_eq!(gen.value(tick), tick as f64);
        }
    }

    #[test]
    fn linear_increase_with_nonzero_start() {
        let gen = StepGenerator::new(100.0, 1.0, None);
        assert_eq!(gen.value(0), 100.0);
        assert_eq!(gen.value(1), 101.0);
        assert_eq!(gen.value(5), 105.0);
    }

    #[test]
    fn wrap_around_at_max_boundary() {
        // start=0, step=1, max=3 => 0,1,2,0,1,2,...
        let gen = StepGenerator::new(0.0, 1.0, Some(3.0));
        assert_eq!(gen.value(0), 0.0);
        assert_eq!(gen.value(1), 1.0);
        assert_eq!(gen.value(2), 2.0);
        assert_eq!(gen.value(3), 0.0);
        assert_eq!(gen.value(4), 1.0);
        assert_eq!(gen.value(5), 2.0);
        assert_eq!(gen.value(6), 0.0);
    }

    #[test]
    fn wrap_around_with_nonzero_start() {
        // start=10, step=1, max=13 => 10,11,12,10,11,12,...
        let gen = StepGenerator::new(10.0, 1.0, Some(13.0));
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 11.0);
        assert_eq!(gen.value(2), 12.0);
        assert_eq!(gen.value(3), 10.0);
        assert_eq!(gen.value(4), 11.0);
    }

    #[test]
    fn max_equal_to_start_acts_as_no_wrap() {
        // max == start => division by zero guard, treat as unbounded
        let gen = StepGenerator::new(5.0, 1.0, Some(5.0));
        assert_eq!(gen.value(0), 5.0);
        assert_eq!(gen.value(1), 6.0);
        assert_eq!(gen.value(10), 15.0);
    }

    #[test]
    fn max_less_than_start_acts_as_no_wrap() {
        // max < start => treat as unbounded
        let gen = StepGenerator::new(10.0, 1.0, Some(5.0));
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(5), 15.0);
    }

    #[test]
    fn max_none_is_unbounded() {
        let gen = StepGenerator::new(0.0, 1.0, None);
        assert_eq!(gen.value(1_000_000), 1_000_000.0);
    }

    #[test]
    fn fractional_step_size() {
        let gen = StepGenerator::new(0.0, 0.5, None);
        assert_eq!(gen.value(0), 0.0);
        assert_eq!(gen.value(1), 0.5);
        assert_eq!(gen.value(2), 1.0);
        assert_eq!(gen.value(3), 1.5);
    }

    #[test]
    fn fractional_step_with_wrap() {
        // start=0, step=0.5, max=2 => range=2, wraps every 4 ticks
        let gen = StepGenerator::new(0.0, 0.5, Some(2.0));
        assert_eq!(gen.value(0), 0.0);
        assert_eq!(gen.value(1), 0.5);
        assert_eq!(gen.value(2), 1.0);
        assert_eq!(gen.value(3), 1.5);
        assert_eq!(gen.value(4), 0.0);
    }

    #[test]
    fn determinism_same_tick_same_value() {
        let gen = StepGenerator::new(0.0, 1.0, Some(100.0));
        let v1 = gen.value(42);
        let v2 = gen.value(42);
        assert_eq!(v1, v2, "same tick must always produce the same value");
    }

    #[test]
    fn large_tick_values_do_not_panic() {
        let gen = StepGenerator::new(0.0, 1.0, Some(1000.0));
        // Should not panic or produce NaN/Inf
        let v = gen.value(u64::MAX / 2);
        assert!(v.is_finite(), "large tick must produce a finite value");
    }

    #[test]
    fn large_tick_unbounded_does_not_panic() {
        let gen = StepGenerator::new(0.0, 1.0, None);
        let v = gen.value(1_000_000_000);
        assert_eq!(v, 1_000_000_000.0);
    }

    #[test]
    fn negative_step_size() {
        // Negative step produces decreasing values
        let gen = StepGenerator::new(100.0, -1.0, None);
        assert_eq!(gen.value(0), 100.0);
        assert_eq!(gen.value(1), 99.0);
        assert_eq!(gen.value(5), 95.0);
    }

    #[test]
    fn negative_step_with_wrap_stays_in_range() {
        let gen = StepGenerator::new(0.0, -1.0, Some(3.0));
        for tick in 0..20 {
            let v = gen.value(tick);
            assert!(
                v >= 0.0 && v < 3.0,
                "value {v} at tick {tick} must be in [0.0, 3.0)"
            );
        }
    }

    #[test]
    fn zero_step_size_returns_start() {
        let gen = StepGenerator::new(42.0, 0.0, None);
        assert_eq!(gen.value(0), 42.0);
        assert_eq!(gen.value(100), 42.0);
    }
}
