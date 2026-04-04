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
use crate::{ConfigError, RuntimeError, SondaError};

/// Run all scenarios in `config` concurrently, one OS thread per scenario.
///
/// Each scenario thread runs until either:
/// - The scenario's own duration expires, or
/// - The shared `shutdown` flag is set to `false`.
///
/// The main thread blocks until all scenario threads have finished. If any
/// thread returns an error, those errors are collected and returned as a
/// combined [`SondaError::Runtime`] with the
/// [`RuntimeError::ScenariosFailed`] variant. Errors from all threads are
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
/// Returns [`SondaError::Config`] for synchronous validation failures
/// (invalid config fields, bad phase_offset). Returns
/// [`SondaError::Runtime`] if any scenario thread encounters an error during
/// setup (sink creation) or during the event loop (encoding, I/O). All
/// thread errors are collected and formatted into a single
/// [`RuntimeError::ScenariosFailed`] error.
pub fn run_multi(config: MultiScenarioConfig, shutdown: Arc<AtomicBool>) -> Result<(), SondaError> {
    let mut handles = Vec::with_capacity(config.scenarios.len());

    for (i, entry) in config.scenarios.into_iter().enumerate() {
        // Validate before spawning so errors are caught synchronously.
        if let Err(e) = validate_entry(&entry) {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "scenario[{i}]: {e}"
            ))));
        }

        // Parse the optional phase_offset into a Duration for the launcher.
        let start_delay = match entry.phase_offset() {
            Some(offset) => crate::config::validate::parse_phase_offset(offset).map_err(|e| {
                SondaError::Config(ConfigError::invalid(format!(
                    "scenario[{i}] phase_offset: {e}"
                )))
            })?,
            None => None,
        };

        let id = format!("multi-{i}");
        let handle = launch_scenario(id, entry, Arc::clone(&shutdown), start_delay)?;
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
        Err(SondaError::Runtime(RuntimeError::ScenariosFailed(
            errors.join("; "),
        )))
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

    use crate::config::{
        BaseScheduleConfig, LogScenarioConfig, MultiScenarioConfig, ScenarioConfig, ScenarioEntry,
    };
    use crate::encoder::EncoderConfig;
    use crate::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
    use crate::sink::SinkConfig;

    use super::{run_multi, signal_shutdown};

    /// Build a minimal metrics `ScenarioEntry` that writes to stdout.
    /// Duration of "100ms" ensures the thread exits quickly.
    fn metrics_entry_stdout(name: &str) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: name.to_string(),
                rate: 10.0,
                duration: Some("100ms".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })
    }

    /// Build a minimal logs `ScenarioEntry` that writes to stdout.
    /// Duration of "100ms" ensures the thread exits quickly.
    fn logs_entry_stdout(name: &str) -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: name.to_string(),
                rate: 10.0,
                duration: Some("100ms".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test log event".to_string(),
                    field_pools: std::collections::BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(42),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
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
                    base: BaseScheduleConfig {
                        name: "shutdown_test_metric".to_string(),
                        rate: 10.0,
                        duration: None, // indefinite
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: None,
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
                ScenarioEntry::Logs(LogScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "shutdown_test_logs".to_string(),
                        rate: 10.0,
                        duration: None, // indefinite
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: None,
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: LogGeneratorConfig::Template {
                        templates: vec![TemplateConfig {
                            message: "shutdown test".to_string(),
                            field_pools: std::collections::BTreeMap::new(),
                        }],
                        severity_weights: None,
                        seed: Some(0),
                    },
                    encoder: EncoderConfig::JsonLines { precision: None },
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
                base: BaseScheduleConfig {
                    name: "error_test".to_string(),
                    rate: 10.0,
                    duration: Some("100ms".to_string()),
                    gaps: None,
                    bursts: None,
                    cardinality_spikes: None,
                    dynamic_labels: None,
                    labels: None,
                    sink: SinkConfig::File {
                        path: "/proc/sonda_test_cannot_create_this_file_27.txt".to_string(),
                    },
                    phase_offset: None,
                    clock_group: None,
                    jitter: None,
                    jitter_seed: None,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
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
                    base: BaseScheduleConfig {
                        name: "err_a".to_string(),
                        rate: 10.0,
                        duration: Some("100ms".to_string()),
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::File {
                            path: "/proc/sonda_err_a_27.txt".to_string(),
                        },
                        phase_offset: None,
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
                ScenarioEntry::Metrics(ScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "err_b".to_string(),
                        rate: 10.0,
                        duration: Some("100ms".to_string()),
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::File {
                            path: "/proc/sonda_err_b_27.txt".to_string(),
                        },
                        phase_offset: None,
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
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

    #[test]
    fn run_multi_thread_errors_produce_runtime_not_config_variant() {
        // A file sink pointing to an invalid path will fail inside the thread.
        // The collected error must be Runtime::ScenariosFailed, not Config.
        let config = MultiScenarioConfig {
            scenarios: vec![ScenarioEntry::Metrics(ScenarioConfig {
                base: BaseScheduleConfig {
                    name: "variant_test".to_string(),
                    rate: 10.0,
                    duration: Some("100ms".to_string()),
                    gaps: None,
                    bursts: None,
                    cardinality_spikes: None,
                    dynamic_labels: None,
                    labels: None,
                    sink: SinkConfig::File {
                        path: "/proc/sonda_variant_test_27.txt".to_string(),
                    },
                    phase_offset: None,
                    clock_group: None,
                    jitter: None,
                    jitter_seed: None,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            })],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(result.is_err(), "invalid sink must produce an error");
        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                crate::SondaError::Runtime(crate::RuntimeError::ScenariosFailed(_))
            ),
            "thread join errors must be Runtime::ScenariosFailed, not Config; got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Config: MultiScenarioConfig and ScenarioEntry deserialization
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
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
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        assert!(
            matches!(config.scenarios[0], ScenarioEntry::Metrics(_)),
            "first entry should be a Metrics variant"
        );
    }

    #[cfg(feature = "config")]
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
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        assert!(
            matches!(config.scenarios[0], ScenarioEntry::Logs(_)),
            "first entry should be a Logs variant"
        );
    }

    #[cfg(feature = "config")]
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
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 2);
        assert!(matches!(config.scenarios[0], ScenarioEntry::Metrics(_)));
        assert!(matches!(config.scenarios[1], ScenarioEntry::Logs(_)));
    }

    #[cfg(feature = "config")]
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
        let result: Result<MultiScenarioConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "unknown signal_type should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn multi_scenario_config_missing_scenarios_key_returns_error() {
        let yaml = r#"
name: no_scenarios_key
rate: 10
"#;
        let result: Result<MultiScenarioConfig, _> = serde_yaml_ng::from_str(yaml);
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
    #[cfg(feature = "config")]
    #[test]
    fn multi_scenario_example_file_deserializes_correctly() {
        let yaml = include_str!("../../../examples/multi-scenario.yaml");
        let config: Result<MultiScenarioConfig, _> = serde_yaml_ng::from_str(yaml);
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

    // -----------------------------------------------------------------------
    // phase_offset in multi-scenario mode
    // -----------------------------------------------------------------------

    /// A scenario with a minimal phase_offset ("1ms") emits events almost immediately.
    #[test]
    fn run_multi_with_minimal_phase_offset_emits_almost_immediately() {
        let config = MultiScenarioConfig {
            scenarios: vec![ScenarioEntry::Metrics(ScenarioConfig {
                base: BaseScheduleConfig {
                    name: "minimal_offset".to_string(),
                    rate: 10.0,
                    duration: Some("200ms".to_string()),
                    gaps: None,
                    bursts: None,
                    cardinality_spikes: None,
                    dynamic_labels: None,
                    labels: None,
                    sink: SinkConfig::Stdout,
                    phase_offset: Some("1ms".to_string()),
                    clock_group: None,
                    jitter: None,
                    jitter_seed: None,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            })],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let start = Instant::now();
        let result = run_multi(config, shutdown);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "minimal phase_offset should complete ok");
        // Should complete roughly within duration + small overhead.
        assert!(
            elapsed < Duration::from_secs(2),
            "minimal phase_offset must not add significant delay, took {:?}",
            elapsed
        );
    }

    /// BUG EXPOSURE: phase_offset "0s" fails because parse_duration rejects
    /// zero-valued durations. The example YAML
    /// (examples/multi-metric-correlation.yaml) uses phase_offset: "0s" which
    /// would fail at runtime.
    #[test]
    fn run_multi_accepts_zero_phase_offset() {
        let config = MultiScenarioConfig {
            scenarios: vec![ScenarioEntry::Metrics(ScenarioConfig {
                base: BaseScheduleConfig {
                    name: "zero_offset".to_string(),
                    rate: 10.0,
                    duration: Some("200ms".to_string()),
                    gaps: None,
                    bursts: None,
                    cardinality_spikes: None,
                    dynamic_labels: None,
                    labels: None,
                    sink: SinkConfig::Stdout,
                    phase_offset: Some("0s".to_string()),
                    clock_group: None,
                    jitter: None,
                    jitter_seed: None,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            })],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        // "0s" is treated as no delay — parse_phase_offset returns None.
        assert!(
            result.is_ok(),
            "phase_offset '0s' should succeed (treated as no delay): {:?}",
            result.err()
        );
    }

    /// A scenario with no phase_offset (None) preserves existing behavior.
    #[test]
    fn run_multi_with_no_phase_offset_preserves_behavior() {
        let config = MultiScenarioConfig {
            scenarios: vec![metrics_entry_stdout("no_offset")],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_ok(),
            "scenario without phase_offset should work as before"
        );
    }

    /// Two scenarios where the second has a 500ms phase_offset: the second
    /// starts later, so total run time is at least 500ms.
    #[test]
    fn run_multi_respects_phase_offset_between_scenarios() {
        let config = MultiScenarioConfig {
            scenarios: vec![
                ScenarioEntry::Metrics(ScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "first_immediate".to_string(),
                        rate: 10.0,
                        duration: Some("100ms".to_string()),
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: None,
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
                ScenarioEntry::Metrics(ScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "second_delayed".to_string(),
                        rate: 10.0,
                        duration: Some("100ms".to_string()),
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: Some("500ms".to_string()),
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 2.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
            ],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let start = Instant::now();
        let result = run_multi(config, shutdown);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "phase_offset multi-scenario should succeed");
        // The second scenario must wait 500ms before its 100ms run, so total
        // should be at least ~500ms.
        assert!(
            elapsed >= Duration::from_millis(400),
            "total run time must include the phase_offset delay, took {:?}",
            elapsed
        );
    }

    /// Shutdown during phase_offset delay exits all scenarios cleanly.
    #[test]
    fn run_multi_shutdown_during_phase_offset_exits_cleanly() {
        let config = MultiScenarioConfig {
            scenarios: vec![
                // First scenario runs indefinitely.
                ScenarioEntry::Metrics(ScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "immediate_indef".to_string(),
                        rate: 10.0,
                        duration: None,
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: None,
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
                // Second scenario has a long delay — we'll shut down before it starts.
                ScenarioEntry::Metrics(ScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "long_delay".to_string(),
                        rate: 10.0,
                        duration: None,
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: Some("10s".to_string()),
                        clock_group: None,
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 2.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
            ],
        };

        let shutdown = Arc::new(AtomicBool::new(true));
        let shutdown_for_thread = Arc::clone(&shutdown);

        // Signal shutdown after 100ms.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            signal_shutdown(&shutdown_for_thread);
        });

        let start = Instant::now();
        let result = run_multi(config, shutdown);
        let elapsed = start.elapsed();

        assert!(
            result.is_ok(),
            "shutdown during phase_offset should not produce an error"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "run_multi must exit promptly when shutdown during phase_offset, took {:?}",
            elapsed
        );
    }

    /// An invalid phase_offset string causes run_multi to return an error
    /// synchronously before spawning threads.
    #[test]
    fn run_multi_rejects_invalid_phase_offset() {
        let config = MultiScenarioConfig {
            scenarios: vec![ScenarioEntry::Metrics(ScenarioConfig {
                base: BaseScheduleConfig {
                    name: "bad_offset".to_string(),
                    rate: 10.0,
                    duration: Some("100ms".to_string()),
                    gaps: None,
                    bursts: None,
                    cardinality_spikes: None,
                    dynamic_labels: None,
                    labels: None,
                    sink: SinkConfig::Stdout,
                    phase_offset: Some("not_a_duration".to_string()),
                    clock_group: None,
                    jitter: None,
                    jitter_seed: None,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            })],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_err(),
            "invalid phase_offset must cause run_multi to return Err"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("phase_offset"),
            "error message should mention phase_offset, got: {err_msg}"
        );
    }

    /// Two correlated scenarios with different phase_offsets run concurrently.
    /// (Uses "1ms" instead of "0s" to avoid the parse_duration zero-rejection bug.)
    #[cfg(feature = "config")]
    #[test]
    fn multi_metric_correlation_example_runs_concurrently() {
        let yaml = r#"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 10
    duration: 200ms
    phase_offset: "1ms"
    clock_group: alert-test
    generator:
      type: constant
      value: 95.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
  - signal_type: metrics
    name: memory_usage_percent
    rate: 10
    duration: 200ms
    phase_offset: "100ms"
    clock_group: alert-test
    generator:
      type: constant
      value: 88.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
"#;
        let config: MultiScenarioConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_ok(),
            "multi-metric correlation example should run successfully"
        );
    }

    /// Scenarios with the same clock_group and different phase_offsets both complete.
    #[test]
    fn run_multi_with_clock_group_and_offsets() {
        let config = MultiScenarioConfig {
            scenarios: vec![
                ScenarioEntry::Metrics(ScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "grouped_a".to_string(),
                        rate: 10.0,
                        duration: Some("100ms".to_string()),
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: None,
                        clock_group: Some("test-group".to_string()),
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 1.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
                ScenarioEntry::Metrics(ScenarioConfig {
                    base: BaseScheduleConfig {
                        name: "grouped_b".to_string(),
                        rate: 10.0,
                        duration: Some("100ms".to_string()),
                        gaps: None,
                        bursts: None,
                        cardinality_spikes: None,
                        dynamic_labels: None,
                        labels: None,
                        sink: SinkConfig::Stdout,
                        phase_offset: Some("200ms".to_string()),
                        clock_group: Some("test-group".to_string()),
                        jitter: None,
                        jitter_seed: None,
                    },
                    generator: GeneratorConfig::Constant { value: 2.0 },
                    encoder: EncoderConfig::PrometheusText { precision: None },
                }),
            ],
        };
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(config, shutdown);
        assert!(
            result.is_ok(),
            "scenarios with clock_group and offsets should complete"
        );
    }
}
