//! Sine wave value generator — produces values following a sine curve.

use std::f64::consts::PI;

use super::ValueGenerator;

/// Generates values on a sine wave: `offset + amplitude * sin(2π * tick / period_ticks)`.
///
/// `period_ticks` is pre-computed from `period_secs * rate` at construction time,
/// so the hot `value()` path performs only a single sine call and a few arithmetic
/// operations.
pub struct Sine {
    amplitude: f64,
    period_ticks: f64,
    offset: f64,
}

impl Sine {
    /// Construct a new `Sine` generator.
    ///
    /// # Parameters
    /// - `amplitude` — half the peak-to-peak swing of the wave.
    /// - `period_secs` — how long (in seconds) one full cycle takes.
    /// - `offset` — vertical offset applied to every sample (the wave's midpoint).
    /// - `rate` — events per second; used to convert `period_secs` into ticks.
    pub fn new(amplitude: f64, period_secs: f64, offset: f64, rate: f64) -> Self {
        let period_ticks = period_secs * rate;
        Self {
            amplitude,
            period_ticks,
            offset,
        }
    }
}

impl ValueGenerator for Sine {
    /// Return `offset + amplitude * sin(2π * tick / period_ticks)`.
    fn value(&self, tick: u64) -> f64 {
        self.offset + self.amplitude * (2.0 * PI * tick as f64 / self.period_ticks).sin()
    }
}
