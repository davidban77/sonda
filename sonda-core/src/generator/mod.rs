//! Value generators produce f64 values for each tick.
//!
//! All generators implement the `ValueGenerator` trait and are constructed
//! via `create_generator()` which returns `Box<dyn ValueGenerator>`.
//!
//! Log generators implement the `LogGenerator` trait and produce `LogEvent`
//! values. They are constructed via `create_log_generator()`.

pub mod constant;
pub mod log_replay;
pub mod log_template;
pub mod sawtooth;
pub mod sine;
pub mod uniform;

use std::collections::HashMap;

use serde::Deserialize;

use self::constant::Constant;
use self::log_replay::LogReplayGenerator;
use self::log_template::{LogTemplateGenerator, TemplateEntry};
use self::sawtooth::Sawtooth;
use self::sine::Sine;
use self::uniform::UniformRandom;
use crate::model::log::{LogEvent, Severity};
use crate::SondaError;

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

// ---------------------------------------------------------------------------
// Log generators
// ---------------------------------------------------------------------------

/// A log generator produces a `LogEvent` for a given tick index.
///
/// Implementations must be deterministic for a given configuration and tick.
/// Side effects are not allowed in `generate()`.
pub trait LogGenerator: Send + Sync {
    /// Produce a `LogEvent` for the given tick index (0-based, monotonically increasing).
    fn generate(&self, tick: u64) -> LogEvent;
}

/// Configuration for one message template used by [`LogGeneratorConfig::Template`].
///
/// The `message` field may contain `{placeholder}` tokens that are resolved
/// using the corresponding value pool from `field_pools`.
///
/// # Example YAML
///
/// ```yaml
/// message: "Request from {ip} to {endpoint}"
/// field_pools:
///   ip:
///     - "10.0.0.1"
///     - "10.0.0.2"
///   endpoint:
///     - "/api"
///     - "/health"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateConfig {
    /// The message template. Use `{field_name}` for dynamic placeholders.
    pub message: String,
    /// Maps placeholder names to their value pools.
    #[serde(default)]
    pub field_pools: HashMap<String, Vec<String>>,
}

/// Configuration for a log generator, used for YAML deserialization.
///
/// The `type` field selects which generator to instantiate.
///
/// # Example YAML — template generator
///
/// ```yaml
/// generator:
///   type: template
///   templates:
///     - message: "Request from {ip} to {endpoint}"
///       field_pools:
///         ip: ["10.0.0.1", "10.0.0.2"]
///         endpoint: ["/api", "/health"]
///   severity_weights:
///     info: 0.7
///     warn: 0.2
///     error: 0.1
///   seed: 42
/// ```
///
/// # Example YAML — replay generator
///
/// ```yaml
/// generator:
///   type: replay
///   file: /var/log/app.log
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum LogGeneratorConfig {
    /// Generates events from message templates with randomized field pool values.
    #[serde(rename = "template")]
    Template {
        /// One or more template entries. Templates are selected round-robin by tick.
        templates: Vec<TemplateConfig>,
        /// Optional severity weight map. Keys are severity names (`info`, `warn`, etc.),
        /// values are relative weights. Defaults to `info: 1.0` when absent.
        #[serde(default)]
        severity_weights: Option<HashMap<String, f64>>,
        /// Seed for deterministic replay. Defaults to `0` when absent.
        seed: Option<u64>,
    },
    /// Replays lines from a file, cycling back to the start when exhausted.
    #[serde(rename = "replay")]
    Replay {
        /// Path to the file containing log lines to replay.
        file: String,
    },
}

/// Parse a severity name string into a [`Severity`] variant.
fn parse_severity(s: &str) -> Result<Severity, SondaError> {
    match s.to_lowercase().as_str() {
        "trace" => Ok(Severity::Trace),
        "debug" => Ok(Severity::Debug),
        "info" => Ok(Severity::Info),
        "warn" | "warning" => Ok(Severity::Warn),
        "error" => Ok(Severity::Error),
        "fatal" => Ok(Severity::Fatal),
        other => Err(SondaError::Config(format!(
            "unknown severity {:?}: must be one of trace, debug, info, warn, error, fatal",
            other
        ))),
    }
}

/// Construct a `Box<dyn LogGenerator>` from the given configuration.
///
/// # Errors
/// - Returns [`SondaError::Config`] if severity weight keys are invalid.
/// - Returns [`SondaError::Config`] if the replay file is empty or cannot be parsed.
/// - Returns [`SondaError::Sink`] (wrapping `std::io::Error`) if the replay file
///   cannot be opened.
pub fn create_log_generator(
    config: &LogGeneratorConfig,
) -> Result<Box<dyn LogGenerator>, SondaError> {
    match config {
        LogGeneratorConfig::Template {
            templates,
            severity_weights,
            seed,
        } => {
            let seed = seed.unwrap_or(0);

            // Build severity weight vector from the optional map.
            let weights: Vec<(Severity, f64)> = if let Some(map) = severity_weights {
                let mut pairs = Vec::with_capacity(map.len());
                for (name, weight) in map {
                    let severity = parse_severity(name)?;
                    pairs.push((severity, *weight));
                }
                // Sort by severity ordinal for deterministic ordering.
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                pairs
            } else {
                vec![]
            };

            // Convert TemplateConfig into TemplateEntry.
            let entries: Vec<TemplateEntry> = templates
                .iter()
                .map(|tc| TemplateEntry {
                    message: tc.message.clone(),
                    field_pools: tc.field_pools.clone(),
                })
                .collect();

            Ok(Box::new(LogTemplateGenerator::new(entries, weights, seed)))
        }
        LogGeneratorConfig::Replay { file } => {
            let path = std::path::Path::new(file);
            Ok(Box::new(LogReplayGenerator::from_file(path)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Factory tests -------------------------------------------------------

    #[test]
    fn factory_constant_returns_configured_value() {
        let config = GeneratorConfig::Constant { value: 1.0 };
        let gen = create_generator(&config, 100.0);
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1_000_000), 1.0);
    }

    #[test]
    fn factory_uniform_returns_values_in_range() {
        let config = GeneratorConfig::Uniform {
            min: 0.0,
            max: 1.0,
            seed: Some(7),
        };
        let gen = create_generator(&config, 100.0);
        for tick in 0..1000 {
            let v = gen.value(tick);
            assert!(
                v >= 0.0 && v <= 1.0,
                "uniform value {v} out of [0,1] at tick {tick}"
            );
        }
    }

    #[test]
    fn factory_uniform_seed_none_defaults_to_zero_seed() {
        // When seed is None the factory must behave the same as seed Some(0).
        let config_none = GeneratorConfig::Uniform {
            min: 0.0,
            max: 1.0,
            seed: None,
        };
        let config_zero = GeneratorConfig::Uniform {
            min: 0.0,
            max: 1.0,
            seed: Some(0),
        };
        let gen_none = create_generator(&config_none, 1.0);
        let gen_zero = create_generator(&config_zero, 1.0);
        for tick in 0..100 {
            assert_eq!(
                gen_none.value(tick),
                gen_zero.value(tick),
                "seed=None must equal seed=Some(0) at tick {tick}"
            );
        }
    }

    #[test]
    fn factory_sine_value_at_zero_equals_offset() {
        let config = GeneratorConfig::Sine {
            amplitude: 5.0,
            period_secs: 10.0,
            offset: 3.0,
        };
        let gen = create_generator(&config, 1.0);
        assert!(
            (gen.value(0) - 3.0).abs() < 1e-10,
            "sine factory: value(0) must equal offset"
        );
    }

    #[test]
    fn factory_sawtooth_value_at_zero_equals_min() {
        let config = GeneratorConfig::Sawtooth {
            min: 5.0,
            max: 15.0,
            period_secs: 10.0,
        };
        let gen = create_generator(&config, 1.0);
        assert_eq!(
            gen.value(0),
            5.0,
            "sawtooth factory: value(0) must equal min"
        );
    }

    // ---- Config deserialization tests ----------------------------------------

    #[test]
    fn deserialize_constant_config() {
        let yaml = "type: constant\nvalue: 42.0\n";
        let config: GeneratorConfig = serde_yaml::from_str(yaml).expect("deserialize constant");
        match config {
            GeneratorConfig::Constant { value } => {
                assert_eq!(value, 42.0);
            }
            _ => panic!("expected Constant variant"),
        }
    }

    #[test]
    fn deserialize_uniform_config_with_seed() {
        let yaml = "type: uniform\nmin: 1.0\nmax: 5.0\nseed: 99\n";
        let config: GeneratorConfig = serde_yaml::from_str(yaml).expect("deserialize uniform");
        match config {
            GeneratorConfig::Uniform { min, max, seed } => {
                assert_eq!(min, 1.0);
                assert_eq!(max, 5.0);
                assert_eq!(seed, Some(99));
            }
            _ => panic!("expected Uniform variant"),
        }
    }

    #[test]
    fn deserialize_uniform_config_without_seed() {
        let yaml = "type: uniform\nmin: 0.0\nmax: 10.0\n";
        let config: GeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize uniform no seed");
        match config {
            GeneratorConfig::Uniform { min, max, seed } => {
                assert_eq!(min, 0.0);
                assert_eq!(max, 10.0);
                assert_eq!(seed, None);
            }
            _ => panic!("expected Uniform variant"),
        }
    }

    #[test]
    fn deserialize_sine_config() {
        let yaml = "type: sine\namplitude: 5.0\nperiod_secs: 30\noffset: 10.0\n";
        let config: GeneratorConfig = serde_yaml::from_str(yaml).expect("deserialize sine");
        match config {
            GeneratorConfig::Sine {
                amplitude,
                period_secs,
                offset,
            } => {
                assert_eq!(amplitude, 5.0);
                assert_eq!(period_secs, 30.0);
                assert_eq!(offset, 10.0);
            }
            _ => panic!("expected Sine variant"),
        }
    }

    #[test]
    fn deserialize_sawtooth_config() {
        let yaml = "type: sawtooth\nmin: 0.0\nmax: 100.0\nperiod_secs: 60.0\n";
        let config: GeneratorConfig = serde_yaml::from_str(yaml).expect("deserialize sawtooth");
        match config {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(min, 0.0);
                assert_eq!(max, 100.0);
                assert_eq!(period_secs, 60.0);
            }
            _ => panic!("expected Sawtooth variant"),
        }
    }

    // ---- Send + Sync contract tests ------------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn generators_are_send_and_sync() {
        // These are compile-time checks — if the types don't implement Send+Sync the
        // test binary will not compile.
        assert_send_sync::<crate::generator::uniform::UniformRandom>();
        assert_send_sync::<crate::generator::sine::Sine>();
        assert_send_sync::<crate::generator::sawtooth::Sawtooth>();
        assert_send_sync::<crate::generator::constant::Constant>();
    }
}
