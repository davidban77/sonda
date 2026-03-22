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
pub mod sequence;
pub mod sine;
pub mod uniform;

use std::collections::HashMap;

use serde::Deserialize;

use self::constant::Constant;
use self::log_replay::LogReplayGenerator;
use self::log_template::{LogTemplateGenerator, TemplateEntry};
use self::sawtooth::Sawtooth;
use self::sequence::SequenceGenerator;
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
    /// A generator that steps through an explicit sequence of values.
    #[serde(rename = "sequence")]
    Sequence {
        /// The ordered list of values to step through. Must not be empty.
        values: Vec<f64>,
        /// When true (default), the sequence cycles. When false, the last value
        /// is returned for all ticks beyond the sequence length.
        repeat: Option<bool>,
    },
}

/// Construct a `Box<dyn ValueGenerator>` from the given configuration.
///
/// The `rate` parameter (events per second) is required by time-based generators
/// (`Sine`, `Sawtooth`) to convert `period_secs` into period ticks.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if the generator configuration is invalid
/// (e.g., an empty values list for the sequence generator).
pub fn create_generator(
    config: &GeneratorConfig,
    rate: f64,
) -> Result<Box<dyn ValueGenerator>, SondaError> {
    match config {
        GeneratorConfig::Constant { value } => Ok(Box::new(Constant::new(*value))),
        GeneratorConfig::Uniform { min, max, seed } => {
            Ok(Box::new(UniformRandom::new(*min, *max, seed.unwrap_or(0))))
        }
        GeneratorConfig::Sine {
            amplitude,
            period_secs,
            offset,
        } => Ok(Box::new(Sine::new(*amplitude, *period_secs, *offset, rate))),
        GeneratorConfig::Sawtooth {
            min,
            max,
            period_secs,
        } => Ok(Box::new(Sawtooth::new(*min, *max, *period_secs, rate))),
        GeneratorConfig::Sequence { values, repeat } => Ok(Box::new(SequenceGenerator::new(
            values.clone(),
            repeat.unwrap_or(true),
        )?)),
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
        let gen = create_generator(&config, 100.0).expect("constant factory");
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
        let gen = create_generator(&config, 100.0).expect("uniform factory");
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
        let gen_none = create_generator(&config_none, 1.0).expect("uniform none factory");
        let gen_zero = create_generator(&config_zero, 1.0).expect("uniform zero factory");
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
        let gen = create_generator(&config, 1.0).expect("sine factory");
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
        let gen = create_generator(&config, 1.0).expect("sawtooth factory");
        assert_eq!(
            gen.value(0),
            5.0,
            "sawtooth factory: value(0) must equal min"
        );
    }

    // ---- Sequence factory tests -----------------------------------------------

    #[test]
    fn factory_sequence_repeat_true_creates_working_generator() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0, 3.0],
            repeat: Some(true),
        };
        let gen = create_generator(&config, 1.0).expect("sequence factory repeat=true");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
        assert_eq!(gen.value(3), 1.0, "should wrap around");
    }

    #[test]
    fn factory_sequence_repeat_false_creates_working_generator() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0, 3.0],
            repeat: Some(false),
        };
        let gen = create_generator(&config, 1.0).expect("sequence factory repeat=false");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(4), 3.0, "should clamp to last value");
    }

    #[test]
    fn factory_sequence_repeat_none_defaults_to_true() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0],
            repeat: None,
        };
        let gen = create_generator(&config, 1.0).expect("sequence factory repeat=None");
        // With repeat defaulting to true, tick=2 on a 2-element seq should wrap to index 0
        assert_eq!(
            gen.value(2),
            1.0,
            "repeat=None should default to true (cycling)"
        );
    }

    #[test]
    fn factory_sequence_empty_values_returns_error() {
        let config = GeneratorConfig::Sequence {
            values: vec![],
            repeat: Some(true),
        };
        let result = create_generator(&config, 1.0);
        assert!(result.is_err(), "empty sequence must return an error");
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

    #[test]
    fn deserialize_sequence_config_with_repeat() {
        let yaml = "type: sequence\nvalues: [1.0, 2.0, 3.0]\nrepeat: true\n";
        let config: GeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize sequence with repeat");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![1.0, 2.0, 3.0]);
                assert_eq!(repeat, Some(true));
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[test]
    fn deserialize_sequence_config_without_repeat() {
        let yaml = "type: sequence\nvalues: [10.0, 20.0]\n";
        let config: GeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize sequence without repeat");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![10.0, 20.0]);
                assert_eq!(repeat, None, "repeat should be None when omitted");
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[test]
    fn deserialize_sequence_config_repeat_false() {
        let yaml = "type: sequence\nvalues: [5.0]\nrepeat: false\n";
        let config: GeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize sequence repeat=false");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![5.0]);
                assert_eq!(repeat, Some(false));
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[test]
    fn deserialize_sequence_config_integer_values() {
        // YAML integers should coerce to f64
        let yaml = "type: sequence\nvalues: [10, 20, 30]\nrepeat: true\n";
        let config: GeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize sequence with integer values");
        match config {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values, vec![10.0, 20.0, 30.0]);
                assert_eq!(repeat, Some(true));
            }
            _ => panic!("expected Sequence variant"),
        }
    }

    #[test]
    fn deserialize_example_yaml_scenario_file() {
        // Validate the example file from examples/sequence-alert-test.yaml
        let yaml = "\
name: cpu_spike_test
rate: 1
duration: 80s

generator:
  type: sequence
  values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
  repeat: true

labels:
  instance: server-01
  job: node

encoder:
  type: prometheus_text
sink:
  type: stdout
";
        let config: crate::config::ScenarioConfig =
            serde_yaml::from_str(yaml).expect("example YAML must deserialize");
        assert_eq!(config.name, "cpu_spike_test");
        assert_eq!(config.rate, 1.0);
        assert_eq!(config.duration, Some("80s".to_string()));
        match &config.generator {
            GeneratorConfig::Sequence { values, repeat } => {
                assert_eq!(values.len(), 16);
                assert_eq!(values[0], 10.0);
                assert_eq!(values[5], 95.0);
                assert_eq!(values[10], 10.0);
                assert_eq!(*repeat, Some(true));
            }
            _ => panic!("expected Sequence generator variant in example YAML"),
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
        assert_send_sync::<crate::generator::sequence::SequenceGenerator>();
    }

    // ---- LogGeneratorConfig deserialization tests ----------------------------

    #[test]
    fn deserialize_log_template_config_minimal() {
        let yaml = "\
type: template
templates:
  - message: \"hello {name}\"
    field_pools:
      name:
        - alice
        - bob
";
        let config: LogGeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize template config");
        match config {
            LogGeneratorConfig::Template {
                templates,
                severity_weights,
                seed,
            } => {
                assert_eq!(templates.len(), 1);
                assert_eq!(templates[0].message, "hello {name}");
                assert!(templates[0].field_pools.contains_key("name"));
                assert_eq!(
                    templates[0].field_pools["name"],
                    vec!["alice".to_string(), "bob".to_string()]
                );
                assert!(
                    severity_weights.is_none(),
                    "severity_weights must default to None"
                );
                assert!(seed.is_none(), "seed must default to None");
            }
            _ => panic!("expected Template variant"),
        }
    }

    #[test]
    fn deserialize_log_template_config_with_weights_and_seed() {
        let yaml = "\
type: template
templates:
  - message: \"msg\"
    field_pools: {}
severity_weights:
  info: 0.7
  warn: 0.2
  error: 0.1
seed: 42
";
        let config: LogGeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize template config with weights");
        match config {
            LogGeneratorConfig::Template {
                severity_weights,
                seed,
                ..
            } => {
                let weights = severity_weights.expect("severity_weights should be present");
                assert!((weights["info"] - 0.7).abs() < 1e-10);
                assert!((weights["warn"] - 0.2).abs() < 1e-10);
                assert!((weights["error"] - 0.1).abs() < 1e-10);
                assert_eq!(seed, Some(42));
            }
            _ => panic!("expected Template variant"),
        }
    }

    #[test]
    fn deserialize_log_replay_config() {
        let yaml = "type: replay\nfile: /var/log/app.log\n";
        let config: LogGeneratorConfig =
            serde_yaml::from_str(yaml).expect("deserialize replay config");
        match config {
            LogGeneratorConfig::Replay { file } => {
                assert_eq!(file, "/var/log/app.log");
            }
            _ => panic!("expected Replay variant"),
        }
    }

    // ---- create_log_generator factory tests ----------------------------------

    #[test]
    fn factory_template_config_creates_working_generator() {
        let config = LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "event {id}".into(),
                field_pools: {
                    let mut m = HashMap::new();
                    m.insert("id".into(), vec!["1".into(), "2".into(), "3".into()]);
                    m
                },
            }],
            severity_weights: None,
            seed: Some(0),
        };
        let gen = create_log_generator(&config).expect("template factory must succeed");
        let event = gen.generate(0);
        // Must not contain unresolved placeholder.
        assert!(!event.message.contains('{'));
    }

    #[test]
    fn factory_template_config_seed_none_defaults_correctly() {
        // seed: None should not error and should produce a generator.
        let config = LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "static message".into(),
                field_pools: HashMap::new(),
            }],
            severity_weights: None,
            seed: None,
        };
        let gen = create_log_generator(&config).expect("template with seed=None must succeed");
        assert_eq!(gen.generate(0).message, "static message");
    }

    #[test]
    fn factory_template_invalid_severity_key_returns_error() {
        let config = LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "msg".into(),
                field_pools: HashMap::new(),
            }],
            severity_weights: {
                let mut m = HashMap::new();
                m.insert("bogus".into(), 1.0);
                Some(m)
            },
            seed: None,
        };
        let result = create_log_generator(&config);
        assert!(
            result.is_err(),
            "invalid severity key 'bogus' must produce Err"
        );
    }

    #[test]
    fn factory_replay_config_missing_file_returns_error() {
        let config = LogGeneratorConfig::Replay {
            file: "/this/path/does/not/exist.log".into(),
        };
        let result = create_log_generator(&config);
        assert!(result.is_err(), "missing replay file must produce Err");
    }

    #[test]
    fn factory_replay_config_creates_working_generator() {
        use std::io::Write;
        use tempfile::NamedTempFile;
        let mut tmp = NamedTempFile::new().expect("create temp file");
        writeln!(tmp, "line one").expect("write");
        writeln!(tmp, "line two").expect("write");
        let config = LogGeneratorConfig::Replay {
            file: tmp.path().to_string_lossy().into_owned(),
        };
        let gen =
            create_log_generator(&config).expect("replay factory with real file must succeed");
        assert_eq!(gen.generate(0).message, "line one");
        assert_eq!(gen.generate(1).message, "line two");
        assert_eq!(gen.generate(2).message, "line one");
    }

    #[test]
    fn log_generators_are_send_and_sync() {
        assert_send_sync::<crate::generator::log_template::LogTemplateGenerator>();
        assert_send_sync::<crate::generator::log_replay::LogReplayGenerator>();
    }
}
