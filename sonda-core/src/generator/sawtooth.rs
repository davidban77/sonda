//! Sawtooth wave value generator — linearly ramps from `min` to `max` then resets.

use super::ValueGenerator;

/// Generates a sawtooth waveform: a linear ramp from `min` to `max` that resets
/// to `min` at each period boundary.
///
/// `period_ticks` is pre-computed at construction from `period_secs * rate`, keeping
/// the hot `value()` path to a single modulo, one subtraction, and a multiply.
pub struct Sawtooth {
    min: f64,
    max: f64,
    period_ticks: f64,
}

impl Sawtooth {
    /// Construct a new `Sawtooth` generator.
    ///
    /// # Parameters
    /// - `min` — value emitted at tick 0 and at every period reset.
    /// - `max` — value approached (but never reached) at the end of a period.
    /// - `period_secs` — duration of one full ramp in seconds.
    /// - `rate` — events per second; used to convert `period_secs` into ticks.
    pub fn new(min: f64, max: f64, period_secs: f64, rate: f64) -> Self {
        let period_ticks = period_secs * rate;
        Self {
            min,
            max,
            period_ticks,
        }
    }
}

impl ValueGenerator for Sawtooth {
    /// Return a value linearly interpolated from `min` to `max` within the current period.
    ///
    /// At `tick % period_ticks == 0` the value resets to `min`. The value approaches
    /// (but never reaches) `max` just before the period boundary.
    fn value(&self, tick: u64) -> f64 {
        let position = (tick as f64) % self.period_ticks;
        let fraction = position / self.period_ticks;
        self.min + fraction * (self.max - self.min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-9;

    /// Helper: rate=1 means period_ticks == period_secs exactly.
    fn saw_rate1(min: f64, max: f64, period_secs: f64) -> Sawtooth {
        Sawtooth::new(min, max, period_secs, 1.0)
    }

    #[test]
    fn sawtooth_at_tick_zero_returns_min() {
        let gen = saw_rate1(2.0, 10.0, 8.0);
        assert_eq!(gen.value(0), 2.0, "value at tick 0 must equal min");
    }

    #[test]
    fn sawtooth_at_period_boundary_resets_to_min() {
        // tick == period_ticks → modulo wraps to 0 → value == min
        let period_secs = 10.0;
        let gen = saw_rate1(2.0, 10.0, period_secs);
        let period_tick = period_secs as u64; // 10
        assert!(
            (gen.value(period_tick) - 2.0).abs() < EPSILON,
            "value at period boundary must reset to min"
        );
    }

    #[test]
    fn sawtooth_approaches_max_near_period_end() {
        // At tick = period - 1, fraction = (period-1)/period → approaches 1.
        let min = 0.0;
        let max = 100.0;
        let period_secs = 100.0;
        let gen = saw_rate1(min, max, period_secs);
        let last_tick = period_secs as u64 - 1; // 99
        let v = gen.value(last_tick);
        // fraction = 99/100 = 0.99 → value = 99.0
        assert!(
            v >= 98.0 && v < 100.0,
            "value near period end should approach max, got {v}"
        );
    }

    #[test]
    fn sawtooth_linear_ramp_between_ticks() {
        // Values should increase monotonically within a period.
        let gen = saw_rate1(0.0, 10.0, 10.0);
        let mut prev = gen.value(0);
        for tick in 1..10u64 {
            let curr = gen.value(tick);
            assert!(
                curr > prev,
                "ramp must be strictly increasing within a period (tick {tick}): prev={prev}, curr={curr}"
            );
            prev = curr;
        }
    }

    #[test]
    fn sawtooth_resets_at_second_period() {
        // tick == 2 * period_ticks resets again to min.
        let period_secs = 10.0;
        let min = 5.0;
        let gen = saw_rate1(min, 20.0, period_secs);
        let two_periods = 2 * period_secs as u64; // 20
        assert!(
            (gen.value(two_periods) - min).abs() < EPSILON,
            "value at second period boundary must reset to min"
        );
    }

    #[test]
    fn sawtooth_period_ticks_pre_computed_from_rate() {
        // rate=10, period_secs=5 → period_ticks=50
        let min = 0.0;
        let max = 50.0;
        let rate = 10.0;
        let gen = Sawtooth::new(min, max, 5.0, rate);
        // At tick 50 (one full period of 50 ticks) → resets to min
        assert!(
            (gen.value(50) - min).abs() < EPSILON,
            "period_ticks pre-computed from rate must reset at tick 50"
        );
    }
}
