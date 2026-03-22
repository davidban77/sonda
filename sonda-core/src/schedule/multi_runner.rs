//! Multi-scenario runner: runs multiple scenarios concurrently on separate threads.
//!
//! Each scenario runs on its own OS thread via [`launch_scenario`]. All threads
//! share a single shutdown flag so that Ctrl+C (or any external signal) stops
//! all scenarios cleanly. Thread errors are collected and returned after all
//! threads have finished.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::MultiScenarioConfig;
use crate::schedule::launch::{launch_scenario, validate_entry};
use crate::SondaError;

/// Run all scenarios in `config` concurrently, one OS thread per scenario.
///
/// Each scenario thread runs until either:
/// - The scenario's own duration expires, or
/// - The shared `shutdown` flag is set to `false`.
///
/// The main thread blocks until all scenario threads have finished. If any
/// thread returns an error, those errors are collected and returned as a
/// combined [`SondaError::Config`] message. Errors from all threads are
/// reported, not just the first one.
///
/// # Parameters
///
/// * `config` — the multi-scenario configuration, containing one entry per concurrent scenario.
/// * `shutdown` — shared shutdown flag. Set to `false` to stop all running scenarios.
///   Each scenario thread polls this flag on every tick.
///
/// # Errors
///
/// Returns [`SondaError`] if any scenario thread encounters an error during
/// setup (sink creation, config parsing) or during the event loop (encoding,
/// I/O). All thread errors are collected and formatted into a single error.
pub fn run_multi(config: MultiScenarioConfig, shutdown: Arc<AtomicBool>) -> Result<(), SondaError> {
    let mut handles = Vec::with_capacity(config.scenarios.len());

    for (i, entry) in config.scenarios.into_iter().enumerate() {
        // Validate before spawning so errors are caught synchronously.
        if let Err(e) = validate_entry(&entry) {
            return Err(SondaError::Config(format!("scenario[{i}]: {e}")));
        }

        let id = format!("multi-{i}");
        let handle = launch_scenario(id, entry, Arc::clone(&shutdown))?;
        handles.push(handle);
    }

    // Collect results from all threads.
    let mut errors: Vec<String> = Vec::new();
    for mut handle in handles {
        match handle.join(None) {
            Ok(()) => {}
            Err(e) => errors.push(e.to_string()),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SondaError::Config(errors.join("; ")))
    }
}

/// Set the shutdown flag, signalling all running scenarios to stop.
///
/// This is a convenience wrapper that stores `false` with `SeqCst` ordering,
/// matching the ordering used by the signal handler in the CLI.
pub fn signal_shutdown(shutdown: &AtomicBool) {
    shutdown.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::config::{LogScenarioConfig, MultiScenarioConfig, ScenarioConfig, ScenarioEntry};
    use crate::encoder::EncoderConfig;
    use crate::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
    use crate::sink::SinkConfig;

    use super::{run_multi, signal_shutdown};

    /// Build a minimal metrics `ScenarioEntry` that writes to stdout.
    /// Duration of "100ms" ensures the thread exits quickly.
    fn metrics_entry_stdout(name: &str) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            name: name.to_string(),
            rate: 10.0,
            duration: Some("100ms".to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText,
            sink: SinkConfig::Stdout,
        })
    }

    /// Build a minimal logs `ScenarioEntry` that writes to stdout.
    /// Duration of "100ms" ensures the thread exits quickly.
    fn logs_entry_stdout(name: &str) -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            name: name.to_string(),
            rate: 10.0,
            duration: Some("100ms".to_string()),
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test log event".to_string(),
                    field_pools: std::collections::HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(42),
            },
            gaps: None,
            bursts: None,
            encoder: EncoderConfig::JsonLines,
            sink: SinkConfig::Stdout,
        })
    }

    // -----------------------------------------------------------------------
    // Happy path: multiple scenarios complete successfully
    // -----------------------------------------------------------------------

    #[test]
    fn run_multi_with_empty_scenarios_returns_ok() {
        let config = MultiScenarioConfig { scenarios: vec![] };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(result.is_ok(), "empty scenario list should return Ok");
    }

    #[test]
    fn run_multi_with_single_metrics_scenario_returns_ok() {
        let config = MultiScenarioConfig {
            scenarios: vec![metrics_entry_stdout("single_metric")],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_ok(),
            "single metrics scenario should complete without error"
        );
    }

    #[test]
    fn run_multi_with_single_logs_scenario_returns_ok() {
        let config = MultiScenarioConfig {
            scenarios: vec![logs_entry_stdout("single_logs")],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_ok(),
            "single logs scenario should complete without error"
        );
    }

    #[test]
    fn run_multi_with_metrics_and_logs_both_complete() {
        // Two scenarios concurrently — both should run to completion within
        // their 100ms durations and return Ok.
        let config = MultiScenarioConfig {
            scenarios: vec![
                metrics_entry_stdout("concurrent_metrics"),
                logs_entry_stdout("concurrent_logs"),
            ],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_ok(),
            "both concurrent scenarios should complete without error"
        );
    }

    #[test]
    fn run_multi_three_concurrent_scenarios_all_complete() {
        let config = MultiScenarioConfig {
            scenarios: vec![
                metrics_entry_stdout("m1"),
                metrics_entry_stdout("m2"),
                logs_entry_stdout("l1"),
            ],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_ok(),
            "three concurrent scenarios should all complete without error"
        );
    }

    // -----------------------------------------------------------------------
    // Shutdown flag: setting it stops all threads
    // -----------------------------------------------------------------------

    #[test]
    fn run_multi_shutdown_flag_stops_all_threads_within_two_seconds() {
        // Both scenarios have no duration (would run indefinitely). We
        // signal shutdown after a short delay and verify all threads stop
        // well within 2 seconds.
        let config = MultiScenarioConfig {
            scenarios: vec![
                ScenarioEntry::Metrics(ScenarioConfig {
                    name: "shutdown_test_metric".to_string(),
                    rate: 10.0,
                    duration: None, // indefinite
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    gaps: None,
                    bursts: None,
                    labels: None,
                    encoder: EncoderConfig::PrometheusText,
                    sink: SinkConfig::Stdout,
                }),
                ScenarioEntry::Logs(LogScenarioConfig {
                    name: "shutdown_test_logs".to_string(),
                    rate: 10.0,
                    duration: None, // indefinite
                    generator: LogGeneratorConfig::Template {
                        templates: vec![TemplateConfig {
                            message: "shutdown test".to_string(),
                            field_pools: std::collections::HashMap::new(),
                        }],
                        severity_weights: None,
                        seed: Some(0),
                    },
                    gaps: None,
                    bursts: None,
                    encoder: EncoderConfig::JsonLines,
                    sink: SinkConfig::Stdout,
                }),
            ],
        };

        let shutdown = Arc::new(AtomicBool::new(true));
        let shutdown_for_thread = Arc::clone(&shutdown);

        // Signal shutdown after 50ms from a separate thread.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal_shutdown(&shutdown_for_thread);
        });

        let start = Instant::now();
        let result = run_multi(config, shutdown);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "shutdown should not produce an error");
        assert!(
            elapsed < Duration::from_secs(2),
            "run_multi should return within 2 seconds of shutdown signal, took {:?}",
            elapsed
        );
    }

    #[test]
    fn signal_shutdown_stores_false_with_seqcst_ordering() {
        let flag = AtomicBool::new(true);
        signal_shutdown(&flag);
        assert!(
            !flag.load(Ordering::SeqCst),
            "signal_shutdown should set the flag to false"
        );
    }

    // -----------------------------------------------------------------------
    // Error handling: errors from individual threads are collected
    // -----------------------------------------------------------------------

    #[test]
    fn run_multi_with_invalid_sink_config_returns_err() {
        // A file sink pointing to a path that cannot be created will fail
        // during sink construction inside the thread.
        let config = MultiScenarioConfig {
            scenarios: vec![ScenarioEntry::Metrics(ScenarioConfig {
                name: "error_test".to_string(),
                rate: 10.0,
                duration: Some("100ms".to_string()),
                generator: GeneratorConfig::Constant { value: 1.0 },
                gaps: None,
                bursts: None,
                labels: None,
                encoder: EncoderConfig::PrometheusText,
                // /proc is read-only on Linux and not writable on macOS either
                sink: SinkConfig::File {
                    path: "/proc/sonda_test_cannot_create_this_file_27.txt".to_string(),
                },
            })],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_err(),
            "scenario with an invalid sink path should return Err"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.is_empty(),
            "error message should be non-empty, got: {err_msg}"
        );
    }

    #[test]
    fn run_multi_collects_all_thread_errors() {
        // Two scenarios both use an invalid sink — both errors should be reported.
        let config = MultiScenarioConfig {
            scenarios: vec![
                ScenarioEntry::Metrics(ScenarioConfig {
                    name: "err_a".to_string(),
                    rate: 10.0,
                    duration: Some("100ms".to_string()),
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    gaps: None,
                    bursts: None,
                    labels: None,
                    encoder: EncoderConfig::PrometheusText,
                    sink: SinkConfig::File {
                        path: "/proc/sonda_err_a_27.txt".to_string(),
                    },
                }),
                ScenarioEntry::Metrics(ScenarioConfig {
                    name: "err_b".to_string(),
                    rate: 10.0,
                    duration: Some("100ms".to_string()),
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    gaps: None,
                    bursts: None,
                    labels: None,
                    encoder: EncoderConfig::PrometheusText,
                    sink: SinkConfig::File {
                        path: "/proc/sonda_err_b_27.txt".to_string(),
                    },
                }),
            ],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(result.is_err(), "two failing scenarios should return Err");
        // The combined error message should contain both errors separated by "; "
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains(';'),
            "combined error should separate errors with ';', got: {err_msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Config: MultiScenarioConfig and ScenarioEntry deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn multi_scenario_config_deserializes_metrics_entry_from_yaml() {
        let yaml = r#"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 100
    duration: 30s
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
"#;
        let config: MultiScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        assert!(
            matches!(config.scenarios[0], ScenarioEntry::Metrics(_)),
            "first entry should be a Metrics variant"
        );
    }

    #[test]
    fn multi_scenario_config_deserializes_logs_entry_from_yaml() {
        let yaml = r#"
scenarios:
  - signal_type: logs
    name: app_logs
    rate: 10
    duration: 30s
    generator:
      type: template
      templates:
        - message: "test message"
          field_pools: {}
    encoder:
      type: json_lines
    sink:
      type: stdout
"#;
        let config: MultiScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        assert!(
            matches!(config.scenarios[0], ScenarioEntry::Logs(_)),
            "first entry should be a Logs variant"
        );
    }

    #[test]
    fn multi_scenario_config_deserializes_mixed_entries_from_yaml() {
        let yaml = r#"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 100
    generator:
      type: constant
      value: 1.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: logs
    name: app_logs
    rate: 10
    generator:
      type: template
      templates:
        - message: "event"
          field_pools: {}
    encoder:
      type: json_lines
    sink:
      type: stdout
"#;
        let config: MultiScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 2);
        assert!(matches!(config.scenarios[0], ScenarioEntry::Metrics(_)));
        assert!(matches!(config.scenarios[1], ScenarioEntry::Logs(_)));
    }

    #[test]
    fn multi_scenario_config_unknown_signal_type_returns_error() {
        let yaml = r#"
scenarios:
  - signal_type: traces
    name: trace_scenario
    rate: 10
    generator:
      type: constant
      value: 1.0
    sink:
      type: stdout
"#;
        let result: Result<MultiScenarioConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "unknown signal_type should fail deserialization"
        );
    }

    #[test]
    fn multi_scenario_config_missing_scenarios_key_returns_error() {
        let yaml = r#"
name: no_scenarios_key
rate: 10
"#;
        let result: Result<MultiScenarioConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "YAML without top-level 'scenarios:' key should fail"
        );
    }

    #[test]
    fn multi_scenario_config_is_cloneable() {
        let config = MultiScenarioConfig {
            scenarios: vec![metrics_entry_stdout("clone_test")],
        };
        let cloned = config.clone();
        assert_eq!(cloned.scenarios.len(), 1);
    }

    #[test]
    fn multi_scenario_config_is_debuggable() {
        let config = MultiScenarioConfig {
            scenarios: vec![metrics_entry_stdout("debug_test")],
        };
        let s = format!("{config:?}");
        assert!(s.contains("MultiScenarioConfig"));
    }

    #[test]
    fn scenario_entry_metrics_is_debuggable() {
        let entry = metrics_entry_stdout("debug_metrics");
        let s = format!("{entry:?}");
        assert!(s.contains("Metrics"));
    }

    #[test]
    fn scenario_entry_logs_is_debuggable() {
        let entry = logs_entry_stdout("debug_logs");
        let s = format!("{entry:?}");
        assert!(s.contains("Logs"));
    }

    // -----------------------------------------------------------------------
    // Multi-scenario example file: can deserialize the provided example
    // -----------------------------------------------------------------------

    /// Verify the shipped example file parses correctly.
    ///
    /// This catches accidental breakage of the example YAML if the config
    /// types change.
    #[test]
    fn multi_scenario_example_file_deserializes_correctly() {
        let yaml = include_str!("../../../examples/multi-scenario.yaml");
        let config: Result<MultiScenarioConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            config.is_ok(),
            "examples/multi-scenario.yaml should parse without error: {:?}",
            config.err()
        );
        let config = config.unwrap();
        assert_eq!(
            config.scenarios.len(),
            2,
            "example should have exactly 2 scenarios"
        );
        assert!(matches!(config.scenarios[0], ScenarioEntry::Metrics(_)));
        assert!(matches!(config.scenarios[1], ScenarioEntry::Logs(_)));
    }
}
