//! Operational vocabulary aliases for generator types.
//!
//! This module provides [`desugar_generator_aliases`], which transforms
//! high-level operational aliases (e.g. `flap`, `steady`, `leak`) into
//! their underlying [`GeneratorConfig`] variants. The desugaring happens
//! at config expansion time — the runtime and generator factory never see
//! alias variants.
//!
//! Aliases that imply jitter (`steady`, `degradation`) set the `jitter` and
//! `jitter_seed` fields on the scenario's [`BaseScheduleConfig`]. This is
//! why desugaring operates at the scenario level rather than the generator
//! level alone.

use crate::config::validate::parse_duration;
use crate::config::{BaseScheduleConfig, ScenarioConfig, ScenarioEntry};
use crate::generator::GeneratorConfig;
use crate::{ConfigError, SondaError};

/// Desugar operational generator aliases in a [`ScenarioEntry`].
///
/// Transforms alias variants (`Flap`, `Saturation`, `Leak`, `Degradation`,
/// `Steady`, `SpikeEvent`) into their underlying `GeneratorConfig` variants,
/// and sets jitter fields on `BaseScheduleConfig` where the alias implies
/// noise.
///
/// Non-alias entries and non-metrics entries are returned unchanged.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if an alias has invalid duration parameters
/// (e.g. non-positive durations).
pub fn desugar_entry(mut entry: ScenarioEntry) -> Result<ScenarioEntry, SondaError> {
    match entry {
        ScenarioEntry::Metrics(ref mut config) => {
            desugar_scenario_config(config)?;
        }
        // Aliases are only supported on metric generators. Log, histogram,
        // and summary entries pass through unchanged.
        ScenarioEntry::Logs(_) | ScenarioEntry::Histogram(_) | ScenarioEntry::Summary(_) => {}
    }
    Ok(entry)
}

/// Desugar operational generator aliases in a [`ScenarioConfig`].
///
/// This is the core desugaring function. It inspects the `generator` field
/// and, if it is an alias variant, replaces it with the equivalent concrete
/// `GeneratorConfig` variant. Aliases that imply jitter also set the
/// `jitter` and `jitter_seed` fields on the scenario's `base` config
/// (only when the user has not already set them explicitly).
///
/// # Errors
///
/// Returns [`SondaError::Config`] if duration strings in the alias
/// parameters are invalid.
pub fn desugar_scenario_config(config: &mut ScenarioConfig) -> Result<(), SondaError> {
    if !config.generator.is_alias() {
        return Ok(());
    }

    let rate = config.base.rate;

    match config.generator.clone() {
        GeneratorConfig::Flap {
            up_duration,
            down_duration,
            up_value,
            down_value,
        } => {
            let up_val = up_value.unwrap_or(1.0);
            let down_val = down_value.unwrap_or(0.0);

            let up_dur = up_duration.as_deref().unwrap_or("10s");
            let down_dur = down_duration.as_deref().unwrap_or("5s");

            let up_secs = duration_to_secs(up_dur)?;
            let down_secs = duration_to_secs(down_dur)?;

            let up_ticks = (up_secs * rate).round() as usize;
            let down_ticks = (down_secs * rate).round() as usize;

            if up_ticks == 0 && down_ticks == 0 {
                return Err(SondaError::Config(ConfigError::invalid(
                    "flap: up_duration and down_duration must produce at least one tick total",
                )));
            }

            let mut values = Vec::with_capacity(up_ticks + down_ticks);
            values.extend(std::iter::repeat_n(up_val, up_ticks.max(1)));
            values.extend(std::iter::repeat_n(down_val, down_ticks.max(1)));

            config.generator = GeneratorConfig::Sequence {
                values,
                repeat: Some(true),
            };
        }
        GeneratorConfig::Saturation {
            baseline,
            ceiling,
            time_to_saturate,
        } => {
            let min = baseline.unwrap_or(0.0);
            let max = ceiling.unwrap_or(100.0);
            let dur = time_to_saturate.as_deref().unwrap_or("5m");
            let period_secs = duration_to_secs(dur)?;

            config.generator = GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            };
        }
        GeneratorConfig::Leak {
            baseline,
            ceiling,
            time_to_ceiling,
        } => {
            let min = baseline.unwrap_or(0.0);
            let max = ceiling.unwrap_or(100.0);
            let dur = time_to_ceiling.as_deref().unwrap_or("10m");
            let period_secs = duration_to_secs(dur)?;

            config.generator = GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            };
        }
        GeneratorConfig::Degradation {
            baseline,
            ceiling,
            time_to_degrade,
            noise,
            noise_seed,
        } => {
            let min = baseline.unwrap_or(0.0);
            let max = ceiling.unwrap_or(100.0);
            let dur = time_to_degrade.as_deref().unwrap_or("5m");
            let period_secs = duration_to_secs(dur)?;

            config.generator = GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            };

            // Apply jitter from the noise parameter, but only if the user
            // hasn't explicitly set jitter on the base config.
            apply_jitter_if_unset(&mut config.base, noise.unwrap_or(1.0), noise_seed);
        }
        GeneratorConfig::Steady {
            center,
            amplitude,
            period,
            noise,
            noise_seed,
        } => {
            let offset = center.unwrap_or(50.0);
            let amp = amplitude.unwrap_or(10.0);
            let dur = period.as_deref().unwrap_or("60s");
            let period_secs = duration_to_secs(dur)?;

            config.generator = GeneratorConfig::Sine {
                amplitude: amp,
                period_secs,
                offset,
            };

            apply_jitter_if_unset(&mut config.base, noise.unwrap_or(1.0), noise_seed);
        }
        GeneratorConfig::SpikeEvent {
            baseline,
            spike_height,
            spike_duration,
            spike_interval,
        } => {
            let base_val = baseline.unwrap_or(0.0);
            let magnitude = spike_height.unwrap_or(100.0);
            let dur = spike_duration.as_deref().unwrap_or("10s");
            let interval = spike_interval.as_deref().unwrap_or("30s");
            let duration_secs = duration_to_secs(dur)?;
            let interval_secs = duration_to_secs(interval)?;

            config.generator = GeneratorConfig::Spike {
                baseline: base_val,
                magnitude,
                duration_secs,
                interval_secs,
            };
        }
        // Non-alias variants handled by the early return above.
        _ => {}
    }

    Ok(())
}

/// Apply jitter and jitter_seed to the base config, but only if the user
/// has not already set jitter explicitly.
///
/// This ensures that user-specified jitter always wins over alias defaults.
fn apply_jitter_if_unset(base: &mut BaseScheduleConfig, jitter: f64, seed: Option<u64>) {
    if base.jitter.is_none() {
        base.jitter = Some(jitter);
    }
    if base.jitter_seed.is_none() {
        if let Some(s) = seed {
            base.jitter_seed = Some(s);
        }
    }
}

/// Convert a human-readable duration string to fractional seconds.
///
/// Uses the existing [`parse_duration`] function from `config::validate` and
/// converts the resulting `Duration` to `f64` seconds.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if the duration string is invalid.
fn duration_to_secs(s: &str) -> Result<f64, SondaError> {
    let dur = parse_duration(s)?;
    Ok(dur.as_secs_f64())
}

#[cfg(all(test, feature = "config"))]
mod tests {
    use super::*;
    use crate::config::ScenarioConfig;

    /// Helper to build a ScenarioConfig from YAML.
    fn parse_scenario(yaml: &str) -> ScenarioConfig {
        serde_yaml_ng::from_str(yaml).expect("test YAML must parse")
    }

    // -----------------------------------------------------------------------
    // Flap alias
    // -----------------------------------------------------------------------

    #[test]
    fn flap_defaults_produce_correct_sequence() {
        let yaml = r#"
name: test_flap
rate: 1
generator:
  type: flap
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sequence { values, repeat } => {
                // up_duration=10s at rate=1 => 10 ticks, down_duration=5s => 5 ticks
                assert_eq!(values.len(), 15);
                assert!(values[..10].iter().all(|v| *v == 1.0));
                assert!(values[10..].iter().all(|v| *v == 0.0));
                assert_eq!(*repeat, Some(true));
            }
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    #[test]
    fn flap_custom_values_and_durations() {
        let yaml = r#"
name: test_flap
rate: 2
generator:
  type: flap
  up_duration: "5s"
  down_duration: "3s"
  up_value: 100.0
  down_value: 50.0
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sequence { values, repeat } => {
                // 5s * 2/s = 10 up ticks, 3s * 2/s = 6 down ticks
                assert_eq!(values.len(), 16);
                assert!(values[..10].iter().all(|v| *v == 100.0));
                assert!(values[10..].iter().all(|v| *v == 50.0));
                assert_eq!(*repeat, Some(true));
            }
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Saturation alias
    // -----------------------------------------------------------------------

    #[test]
    fn saturation_defaults_produce_sawtooth() {
        let yaml = r#"
name: test_sat
rate: 1
generator:
  type: saturation
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(*min, 0.0);
                assert_eq!(*max, 100.0);
                assert_eq!(*period_secs, 300.0); // 5m
            }
            other => panic!("expected Sawtooth, got {other:?}"),
        }
    }

    #[test]
    fn saturation_custom_params() {
        let yaml = r#"
name: test_sat
rate: 1
generator:
  type: saturation
  baseline: 20.0
  ceiling: 95.0
  time_to_saturate: "2m"
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(*min, 20.0);
                assert_eq!(*max, 95.0);
                assert_eq!(*period_secs, 120.0);
            }
            other => panic!("expected Sawtooth, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Leak alias
    // -----------------------------------------------------------------------

    #[test]
    fn leak_defaults_produce_sawtooth() {
        let yaml = r#"
name: test_leak
rate: 1
generator:
  type: leak
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(*min, 0.0);
                assert_eq!(*max, 100.0);
                assert_eq!(*period_secs, 600.0); // 10m
            }
            other => panic!("expected Sawtooth, got {other:?}"),
        }
    }

    #[test]
    fn leak_custom_params() {
        let yaml = r#"
name: test_leak
rate: 1
generator:
  type: leak
  baseline: 40.0
  ceiling: 95.0
  time_to_ceiling: "120s"
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(*min, 40.0);
                assert_eq!(*max, 95.0);
                assert_eq!(*period_secs, 120.0);
            }
            other => panic!("expected Sawtooth, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Degradation alias
    // -----------------------------------------------------------------------

    #[test]
    fn degradation_defaults_produce_sawtooth_with_jitter() {
        let yaml = r#"
name: test_deg
rate: 1
generator:
  type: degradation
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(*min, 0.0);
                assert_eq!(*max, 100.0);
                assert_eq!(*period_secs, 300.0);
            }
            other => panic!("expected Sawtooth, got {other:?}"),
        }
        assert_eq!(config.base.jitter, Some(1.0));
    }

    #[test]
    fn degradation_custom_params_with_noise() {
        let yaml = r#"
name: test_deg
rate: 2
generator:
  type: degradation
  baseline: 0.05
  ceiling: 0.5
  time_to_degrade: "60s"
  noise: 0.02
  noise_seed: 42
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sawtooth {
                min,
                max,
                period_secs,
            } => {
                assert_eq!(*min, 0.05);
                assert_eq!(*max, 0.5);
                assert_eq!(*period_secs, 60.0);
            }
            other => panic!("expected Sawtooth, got {other:?}"),
        }
        assert_eq!(config.base.jitter, Some(0.02));
        assert_eq!(config.base.jitter_seed, Some(42));
    }

    #[test]
    fn degradation_preserves_user_jitter() {
        let yaml = r#"
name: test_deg
rate: 1
generator:
  type: degradation
  noise: 5.0
jitter: 99.0
jitter_seed: 777
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        // User-specified jitter takes precedence over alias noise.
        assert_eq!(config.base.jitter, Some(99.0));
        assert_eq!(config.base.jitter_seed, Some(777));
    }

    // -----------------------------------------------------------------------
    // Steady alias
    // -----------------------------------------------------------------------

    #[test]
    fn steady_defaults_produce_sine_with_jitter() {
        let yaml = r#"
name: test_steady
rate: 1
generator:
  type: steady
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sine {
                amplitude,
                period_secs,
                offset,
            } => {
                assert_eq!(*amplitude, 10.0);
                assert_eq!(*period_secs, 60.0);
                assert_eq!(*offset, 50.0);
            }
            other => panic!("expected Sine, got {other:?}"),
        }
        assert_eq!(config.base.jitter, Some(1.0));
    }

    #[test]
    fn steady_custom_params() {
        let yaml = r#"
name: test_steady
rate: 1
generator:
  type: steady
  center: 75.0
  amplitude: 10.0
  period: "60s"
  noise: 2.0
  noise_seed: 7
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Sine {
                amplitude,
                period_secs,
                offset,
            } => {
                assert_eq!(*amplitude, 10.0);
                assert_eq!(*period_secs, 60.0);
                assert_eq!(*offset, 75.0);
            }
            other => panic!("expected Sine, got {other:?}"),
        }
        assert_eq!(config.base.jitter, Some(2.0));
        assert_eq!(config.base.jitter_seed, Some(7));
    }

    #[test]
    fn steady_preserves_user_jitter() {
        let yaml = r#"
name: test_steady
rate: 1
generator:
  type: steady
  noise: 3.0
jitter: 50.0
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        assert_eq!(config.base.jitter, Some(50.0));
    }

    // -----------------------------------------------------------------------
    // SpikeEvent alias
    // -----------------------------------------------------------------------

    #[test]
    fn spike_event_defaults_produce_spike() {
        let yaml = r#"
name: test_spike
rate: 1
generator:
  type: spike_event
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Spike {
                baseline,
                magnitude,
                duration_secs,
                interval_secs,
            } => {
                assert_eq!(*baseline, 0.0);
                assert_eq!(*magnitude, 100.0);
                assert_eq!(*duration_secs, 10.0);
                assert_eq!(*interval_secs, 30.0);
            }
            other => panic!("expected Spike, got {other:?}"),
        }
    }

    #[test]
    fn spike_event_custom_params() {
        let yaml = r#"
name: test_spike
rate: 1
generator:
  type: spike_event
  baseline: 35.0
  spike_height: 60.0
  spike_duration: "10s"
  spike_interval: "30s"
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Spike {
                baseline,
                magnitude,
                duration_secs,
                interval_secs,
            } => {
                assert_eq!(*baseline, 35.0);
                assert_eq!(*magnitude, 60.0);
                assert_eq!(*duration_secs, 10.0);
                assert_eq!(*interval_secs, 30.0);
            }
            other => panic!("expected Spike, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Non-alias passthrough
    // -----------------------------------------------------------------------

    #[test]
    fn non_alias_generator_passes_through_unchanged() {
        let yaml = r#"
name: test_const
rate: 1
generator:
  type: constant
  value: 42.0
"#;
        let mut config = parse_scenario(yaml);
        desugar_scenario_config(&mut config).expect("desugar must succeed");

        match &config.generator {
            GeneratorConfig::Constant { value } => {
                assert_eq!(*value, 42.0);
            }
            other => panic!("expected Constant, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // desugar_entry for non-metrics entries
    // -----------------------------------------------------------------------

    #[test]
    fn desugar_entry_passes_logs_unchanged() {
        let yaml = r#"
signal_type: logs
name: test_logs
rate: 1
generator:
  type: template
  templates:
    - message: "test"
      field_pools: {}
"#;
        let entry: ScenarioEntry = serde_yaml_ng::from_str(yaml).expect("must parse");
        let result = desugar_entry(entry).expect("must succeed");
        assert!(matches!(result, ScenarioEntry::Logs(_)));
    }

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

    #[test]
    fn flap_with_invalid_duration_returns_error() {
        let yaml = r#"
name: test_flap
rate: 1
generator:
  type: flap
  up_duration: "invalid"
"#;
        let mut config = parse_scenario(yaml);
        let result = desugar_scenario_config(&mut config);
        assert!(result.is_err());
    }

    #[test]
    fn spike_event_with_invalid_interval_returns_error() {
        let yaml = r#"
name: test_spike
rate: 1
generator:
  type: spike_event
  spike_interval: "nope"
"#;
        let mut config = parse_scenario(yaml);
        let result = desugar_scenario_config(&mut config);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // GeneratorConfig::is_alias
    // -----------------------------------------------------------------------

    #[test]
    fn is_alias_returns_true_for_aliases() {
        assert!(GeneratorConfig::Flap {
            up_duration: None,
            down_duration: None,
            up_value: None,
            down_value: None,
        }
        .is_alias());
        assert!(GeneratorConfig::Steady {
            center: None,
            amplitude: None,
            period: None,
            noise: None,
            noise_seed: None,
        }
        .is_alias());
    }

    #[test]
    fn is_alias_returns_false_for_concrete_generators() {
        assert!(!GeneratorConfig::Constant { value: 1.0 }.is_alias());
        assert!(!GeneratorConfig::Sine {
            amplitude: 1.0,
            period_secs: 1.0,
            offset: 0.0,
        }
        .is_alias());
    }

    // -----------------------------------------------------------------------
    // Regression: create_generator rejects undesugared aliases
    // -----------------------------------------------------------------------

    #[test]
    fn create_generator_rejects_undesugared_alias() {
        use crate::generator::create_generator;

        let config = GeneratorConfig::Steady {
            center: None,
            amplitude: None,
            period: None,
            noise: None,
            noise_seed: None,
        };
        let result = create_generator(&config, 1.0);
        assert!(result.is_err());
        let msg = format!("{}", result.err().expect("checked"));
        assert!(
            msg.contains("desugar"),
            "error must mention desugaring, got: {msg}"
        );
    }
}
