//! Value generators produce f64 values for each tick.
//!
//! All generators implement the `ValueGenerator` trait and are constructed
//! via `create_generator()` which returns `Box<dyn ValueGenerator>`.

pub mod constant;
pub mod sawtooth;
pub mod sine;
pub mod uniform;

use serde::Deserialize;

use self::constant::Constant;
use self::sawtooth::Sawtooth;
use self::sine::Sine;
use self::uniform::UniformRandom;

/// A generator produces a single f64 value for a given tick index.
///
/// Implementations must be deterministic for a given configuration and tick.
/// Side effects are not allowed in `value()`.
pub trait ValueGenerator: Send + Sync {
    /// Produce a value for the given tick index (0-based, monotonically increasing).
    fn value(&self, tick: u64) -> f64;
}

/// Configuration for a value generator, used for YAML deserialization.
///
/// The `type` field selects which generator to instantiate. Additional fields
/// are specific to each variant.
///
/// # Example YAML
///
/// ```yaml
/// generator:
///   type: sine
///   amplitude: 5.0
///   period_secs: 30
///   offset: 10.0
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum GeneratorConfig {
    /// A generator that always returns the same value.
    #[serde(rename = "constant")]
    Constant {
        /// The fixed value returned on every tick.
        value: f64,
    },
    /// A generator that returns deterministically random values in `[min, max]`.
    #[serde(rename = "uniform")]
    Uniform {
        /// Lower bound of the output range (inclusive).
        min: f64,
        /// Upper bound of the output range (inclusive).
        max: f64,
        /// Optional seed for deterministic replay. Defaults to 0 when absent.
        seed: Option<u64>,
    },
    /// A generator that follows a sine curve.
    #[serde(rename = "sine")]
    Sine {
        /// Half the peak-to-peak swing of the wave.
        amplitude: f64,
        /// Duration of one full cycle in seconds.
        period_secs: f64,
        /// Vertical offset applied to every sample (the wave's midpoint).
        offset: f64,
    },
    /// A generator that linearly ramps from `min` to `max` then resets.
    #[serde(rename = "sawtooth")]
    Sawtooth {
        /// Value at the start of each period.
        min: f64,
        /// Value approached at the end of each period (never reached).
        max: f64,
        /// Duration of one full ramp in seconds.
        period_secs: f64,
    },
}

/// Construct a `Box<dyn ValueGenerator>` from the given configuration.
///
/// The `rate` parameter (events per second) is required by time-based generators
/// (`Sine`, `Sawtooth`) to convert `period_secs` into period ticks.
pub fn create_generator(config: &GeneratorConfig, rate: f64) -> Box<dyn ValueGenerator> {
    match config {
        GeneratorConfig::Constant { value } => Box::new(Constant::new(*value)),
        GeneratorConfig::Uniform { min, max, seed } => {
            Box::new(UniformRandom::new(*min, *max, seed.unwrap_or(0)))
        }
        GeneratorConfig::Sine {
            amplitude,
            period_secs,
            offset,
        } => Box::new(Sine::new(*amplitude, *period_secs, *offset, rate)),
        GeneratorConfig::Sawtooth {
            min,
            max,
            period_secs,
        } => Box::new(Sawtooth::new(*min, *max, *period_secs, rate)),
    }
}
