//! Multi-scenario runner: runs multiple scenarios concurrently on separate threads.
//!
//! Each scenario runs on its own OS thread via [`launch_scenario`]. All threads
//! share a single shutdown flag so that Ctrl+C (or any external signal) stops
//! all scenarios cleanly. Thread errors are collected and returned after all
//! threads have finished.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::compiler::compile_after::CompiledFile;
use crate::compiler::prepare::translate_entry;
use crate::config::aliases::desugar_entry;
use crate::config::{expand_entry, ScenarioEntry};
use crate::schedule::core_loop::GateContext;
use crate::schedule::gate_bus::{GateBus, SubscriptionSpec, WhileSpec};
use crate::schedule::launch::{
    launch_scenario, launch_scenario_with_gates, prepare_entries, validate_entry,
};
use crate::{RuntimeError, SondaError};

/// Run all scenarios in `entries` concurrently, one OS thread per scenario.
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
/// * `entries` — the scenario entries to run concurrently, typically sourced
///   from [`compile_scenario_file`][crate::compile_scenario_file].
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
pub fn run_multi(entries: Vec<ScenarioEntry>, shutdown: Arc<AtomicBool>) -> Result<(), SondaError> {
    // Expand, validate, and resolve phase offsets for all entries atomically.
    let prepared = prepare_entries(entries)?;

    let mut handles = Vec::with_capacity(prepared.len());
    for (i, prepared_entry) in prepared.into_iter().enumerate() {
        let id = format!("multi-{i}");
        let handle = launch_scenario(
            id,
            prepared_entry.entry,
            Arc::clone(&shutdown),
            prepared_entry.start_delay,
        )?;
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

/// Run a compiled scenario file with `while:` / `after:` gating wired in.
///
/// Pre-builds an `Arc<GateBus>` per metric scenario id, subscribes each
/// downstream to its upstream's bus, and launches every scenario with the
/// matching [`GateContext`]. Non-gated entries launch on the existing
/// non-gated path with no per-tick overhead.
pub fn run_multi_compiled(file: CompiledFile, shutdown: Arc<AtomicBool>) -> Result<(), SondaError> {
    let CompiledFile { entries, .. } = file;

    // Pre-build a bus for every entry that has an explicit id — downstream
    // subscriptions reference upstreams by id. Logs / histogram / summary
    // entries cannot be `while:` upstreams (compile-time NonMetricsTarget
    // check), so we skip them, but a bus on a non-metrics entry is harmless
    // — `tick()` is never called.
    let mut buses: HashMap<String, Arc<GateBus>> = HashMap::new();
    for entry in &entries {
        if let Some(id) = entry.id.clone() {
            buses.insert(id, Arc::new(GateBus::new()));
        }
    }

    // Build (entry, gate_ctx, upstream_bus, start_delay, id) per scenario.
    let mut launches: Vec<LaunchPlan> = Vec::with_capacity(entries.len());
    for compiled_entry in entries.into_iter() {
        let id = compiled_entry.id.clone();
        let while_clause = compiled_entry.while_clause.clone();
        let delay_clause = compiled_entry.delay_clause.clone();
        let phase_offset = compiled_entry.phase_offset.clone();

        let translated = translate_entry(compiled_entry).map_err(|e| {
            SondaError::Config(crate::ConfigError::invalid(format!("compile prepare: {e}")))
        })?;

        // Mirror the expand → desugar → validate pipeline that
        // `prepare_entries` runs for non-gated launches. Skipping it
        // here would let operational aliases (flap, saturation, etc.)
        // reach `create_generator()` un-desugared and panic at runtime.
        let mut expanded = expand_entry(translated)?;
        let translated = match expanded.len() {
            0 => continue,
            1 => expanded.remove(0),
            _ => {
                return Err(SondaError::Config(crate::ConfigError::invalid(format!(
                    "scenario id {:?}: csv_replay multi-column expansion is not supported \
                     when `while:` is in use; specify a single column or remove the gate",
                    id.as_deref().unwrap_or("(anonymous)"),
                ))));
            }
        };
        let translated = desugar_entry(translated)?;
        validate_entry(&translated)?;

        let upstream_bus = id.as_ref().and_then(|name| buses.get(name).cloned());

        let gate_ctx = if let Some(ref clause) = while_clause {
            let upstream = buses.get(&clause.ref_id).ok_or_else(|| {
                SondaError::Config(crate::ConfigError::invalid(format!(
                    "while: ref '{}' not found among scenario ids",
                    clause.ref_id
                )))
            })?;
            let spec = SubscriptionSpec {
                after: None,
                while_: Some(WhileSpec {
                    op: clause.op,
                    threshold: clause.value,
                }),
            };
            let (rx, init) = upstream.subscribe(spec);
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: delay_clause,
                has_after: false,
                has_while: true,
            })
        } else {
            None
        };

        let start_delay = match phase_offset {
            Some(s) => crate::config::validate::parse_phase_offset(&s).map_err(|e| {
                SondaError::Config(crate::ConfigError::invalid(format!("phase_offset: {e}")))
            })?,
            None => None,
        };

        launches.push(LaunchPlan {
            id: id.clone(),
            entry: translated,
            gate_ctx,
            upstream_bus,
            start_delay,
        });
    }

    let mut handles = Vec::with_capacity(launches.len());
    for (idx, plan) in launches.into_iter().enumerate() {
        let id = plan.id.unwrap_or_else(|| format!("multi-{idx}"));
        let handle = launch_scenario_with_gates(
            id,
            plan.entry,
            Arc::clone(&shutdown),
            plan.start_delay,
            plan.upstream_bus,
            plan.gate_ctx,
        )?;
        handles.push(handle);
    }

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

struct LaunchPlan {
    id: Option<String>,
    entry: ScenarioEntry,
    gate_ctx: Option<GateContext>,
    upstream_bus: Option<Arc<GateBus>>,
    start_delay: Option<std::time::Duration>,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::config::{BaseScheduleConfig, LogScenarioConfig, ScenarioConfig, ScenarioEntry};
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
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
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
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
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
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(vec![], shutdown);
        assert!(result.is_ok(), "empty scenario list should return Ok");
    }

    #[test]
    fn run_multi_with_single_metrics_scenario_returns_ok() {
        let entries = vec![metrics_entry_stdout("single_metric")];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
        assert!(
            result.is_ok(),
            "single metrics scenario should complete without error"
        );
    }

    #[test]
    fn run_multi_with_single_logs_scenario_returns_ok() {
        let entries = vec![logs_entry_stdout("single_logs")];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
        assert!(
            result.is_ok(),
            "single logs scenario should complete without error"
        );
    }

    #[test]
    fn run_multi_with_metrics_and_logs_both_complete() {
        // Two scenarios concurrently — both should run to completion within
        // their 100ms durations and return Ok.
        let entries = vec![
            metrics_entry_stdout("concurrent_metrics"),
            logs_entry_stdout("concurrent_logs"),
        ];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
        assert!(
            result.is_ok(),
            "both concurrent scenarios should complete without error"
        );
    }

    #[test]
    fn run_multi_three_concurrent_scenarios_all_complete() {
        let entries = vec![
            metrics_entry_stdout("m1"),
            metrics_entry_stdout("m2"),
            logs_entry_stdout("l1"),
        ];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
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
        let entries = vec![
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
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
        ];

        let shutdown = Arc::new(AtomicBool::new(true));
        let shutdown_for_thread = Arc::clone(&shutdown);

        // Signal shutdown after 50ms from a separate thread.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal_shutdown(&shutdown_for_thread);
        });

        let start = Instant::now();
        let result = run_multi(entries, shutdown);
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
        let entries = vec![ScenarioEntry::Metrics(ScenarioConfig {
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
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
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
        let entries = vec![
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            }),
        ];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
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
        let entries = vec![ScenarioEntry::Metrics(ScenarioConfig {
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
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
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
    // phase_offset in multi-scenario mode
    // -----------------------------------------------------------------------

    /// A scenario with a minimal phase_offset ("1ms") emits events almost immediately.
    #[test]
    fn run_multi_with_minimal_phase_offset_emits_almost_immediately() {
        let entries = vec![ScenarioEntry::Metrics(ScenarioConfig {
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
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })];
        let shutdown = Arc::new(AtomicBool::new(true));
        let start = Instant::now();
        let result = run_multi(entries, shutdown);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "minimal phase_offset should complete ok");
        // Should complete roughly within duration + small overhead.
        assert!(
            elapsed < Duration::from_secs(2),
            "minimal phase_offset must not add significant delay, took {:?}",
            elapsed
        );
    }

    /// `phase_offset: "0s"` is accepted and treated as no delay.
    #[test]
    fn run_multi_accepts_zero_phase_offset() {
        let entries = vec![ScenarioEntry::Metrics(ScenarioConfig {
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
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
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
        let entries = vec![metrics_entry_stdout("no_offset")];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
        assert!(
            result.is_ok(),
            "scenario without phase_offset should work as before"
        );
    }

    /// Two scenarios where the second has a 500ms phase_offset: the second
    /// starts later, so total run time is at least 500ms.
    #[test]
    fn run_multi_respects_phase_offset_between_scenarios() {
        let entries = vec![
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 2.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            }),
        ];
        let shutdown = Arc::new(AtomicBool::new(true));
        let start = Instant::now();
        let result = run_multi(entries, shutdown);
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
        let entries = vec![
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 2.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            }),
        ];

        let shutdown = Arc::new(AtomicBool::new(true));
        let shutdown_for_thread = Arc::clone(&shutdown);

        // Signal shutdown after 100ms.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            signal_shutdown(&shutdown_for_thread);
        });

        let start = Instant::now();
        let result = run_multi(entries, shutdown);
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
        let entries = vec![ScenarioEntry::Metrics(ScenarioConfig {
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
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
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

    /// Scenarios with the same clock_group and different phase_offsets both complete.
    #[test]
    fn run_multi_with_clock_group_and_offsets() {
        let entries = vec![
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
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
                    clock_group_is_auto: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 2.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
            }),
        ];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
        assert!(
            result.is_ok(),
            "scenarios with clock_group and offsets should complete"
        );
    }
}
