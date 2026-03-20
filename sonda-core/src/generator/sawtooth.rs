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
