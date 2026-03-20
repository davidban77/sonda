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

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-10;

    /// rate=1 means 1 event/sec, so period_ticks == period_secs.
    fn sine_rate1(amplitude: f64, period_secs: f64, offset: f64) -> Sine {
        Sine::new(amplitude, period_secs, offset, 1.0)
    }

    #[test]
    fn sine_at_tick_zero_returns_offset() {
        let gen = sine_rate1(5.0, 40.0, 10.0);
        // sin(0) == 0, so value(0) == offset
        assert!(
            (gen.value(0) - 10.0).abs() < EPSILON,
            "value at tick 0 must equal offset"
        );
    }

    #[test]
    fn sine_at_quarter_period_returns_offset_plus_amplitude() {
        // At tick = period/4, sin(2π * 1/4) = sin(π/2) = 1.0
        let amplitude = 5.0;
        let period_secs = 40.0;
        let offset = 10.0;
        let gen = sine_rate1(amplitude, period_secs, offset);
        let quarter_tick = (period_secs / 4.0) as u64; // 10
        let expected = offset + amplitude;
        assert!(
            (gen.value(quarter_tick) - expected).abs() < EPSILON,
            "value at quarter-period tick {} must be ~{} but got {}",
            quarter_tick,
            expected,
            gen.value(quarter_tick)
        );
    }

    #[test]
    fn sine_at_half_period_returns_offset() {
        // At tick = period/2, sin(π) = 0.0
        let amplitude = 5.0;
        let period_secs = 40.0;
        let offset = 10.0;
        let gen = sine_rate1(amplitude, period_secs, offset);
        let half_tick = (period_secs / 2.0) as u64; // 20
        assert!(
            (gen.value(half_tick) - offset).abs() < EPSILON,
            "value at half-period tick must equal offset"
        );
    }

    #[test]
    fn sine_symmetry_around_offset() {
        // value(t) + value(t + half_period) ≈ 2 * offset
        // because sin(x) + sin(x + π) = 0
        let amplitude = 5.0;
        let period_secs = 40.0;
        let offset = 10.0;
        let gen = sine_rate1(amplitude, period_secs, offset);
        let half_period = (period_secs / 2.0) as u64;
        for t in 0..10u64 {
            let sum = gen.value(t) + gen.value(t + half_period);
            assert!(
                (sum - 2.0 * offset).abs() < EPSILON,
                "symmetry violated at tick {t}: sum={sum}, expected {}",
                2.0 * offset
            );
        }
    }

    #[test]
    fn sine_at_full_period_returns_offset() {
        // After one full period, sin returns to 0.
        let amplitude = 5.0;
        let period_secs = 40.0;
        let offset = 10.0;
        let gen = sine_rate1(amplitude, period_secs, offset);
        let full_tick = period_secs as u64; // 40
        assert!(
            (gen.value(full_tick) - offset).abs() < EPSILON,
            "value at full period must equal offset"
        );
    }

    #[test]
    fn sine_period_ticks_pre_computed_from_rate() {
        // rate=10 means period_ticks = period_secs * rate = 4 * 10 = 40.
        // Quarter period is tick 10.
        let amplitude = 3.0;
        let period_secs = 4.0;
        let offset = 0.0;
        let rate = 10.0;
        let gen = Sine::new(amplitude, period_secs, offset, rate);
        // At tick 10 (quarter of 40) → sin(π/2) = 1 → amplitude
        assert!(
            (gen.value(10) - amplitude).abs() < EPSILON,
            "rate-adjusted quarter-period must hit amplitude"
        );
    }
}
