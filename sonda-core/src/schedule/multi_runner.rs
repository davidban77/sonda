//! Multi-scenario runner: runs multiple scenarios concurrently on separate threads.
//!
//! Each scenario owns its own shutdown flag. [`run_multi`] and
//! [`run_multi_compiled`] accept a master `shutdown` flag and use a watchdog
//! thread to fan a transition out into each handle's per-scenario flag.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::ScenarioEntry;
use crate::schedule::launch::{launch_scenario, prepare_entries};
use crate::{RuntimeError, SondaError};

#[cfg(feature = "config")]
use crate::compiler::compile_after::CompiledFile;
#[cfg(feature = "config")]
use crate::compiler::prepare::translate_entry;
#[cfg(feature = "config")]
use crate::config::aliases::desugar_entry;
#[cfg(feature = "config")]
use crate::config::expand_entry;
#[cfg(feature = "config")]
use crate::schedule::core_loop::GateContext;
#[cfg(feature = "config")]
use crate::schedule::gate_bus::{
    GateBus, GateBusResolver, GateReceiver, InitialState, PendingResolution, SubscriptionSpec,
    WhileSpec,
};
#[cfg(feature = "config")]
use crate::schedule::launch::{launch_scenario_with_gates, validate_entry};
#[cfg(feature = "config")]
use std::collections::HashMap;
#[cfg(feature = "config")]
use tokio::sync::watch;

/// Run all scenarios in `entries` concurrently, one OS thread per scenario.
/// Set `shutdown` to `false` to stop all running scenarios.
pub fn run_multi(entries: Vec<ScenarioEntry>, shutdown: Arc<AtomicBool>) -> Result<(), SondaError> {
    // Expand, validate, and resolve phase offsets for all entries atomically.
    let prepared = prepare_entries(entries)?;

    let mut handles = Vec::with_capacity(prepared.len());
    let mut per_handle_shutdowns: Vec<Arc<AtomicBool>> = Vec::with_capacity(prepared.len());
    for (i, prepared_entry) in prepared.into_iter().enumerate() {
        let id = format!("multi-{i}");
        let scenario_shutdown = Arc::new(AtomicBool::new(true));
        let handle = launch_scenario(
            id,
            prepared_entry.entry,
            Arc::clone(&scenario_shutdown),
            prepared_entry.start_delay,
        )?;
        per_handle_shutdowns.push(scenario_shutdown);
        handles.push(handle);
    }

    let done = Arc::new(AtomicBool::new(false));
    let watchdog = spawn_shutdown_watchdog(shutdown, per_handle_shutdowns, Arc::clone(&done));

    let mut errors: Vec<String> = Vec::new();
    for mut handle in handles {
        match handle.join(None) {
            Ok(()) => {}
            Err(e) => errors.push(e.to_string()),
        }
    }

    done.store(true, Ordering::SeqCst);
    let _ = watchdog.join();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SondaError::Runtime(RuntimeError::ScenariosFailed(
            errors.join("; "),
        )))
    }
}

/// Mirror `master`'s false transition into each per-handle shutdown; exit when `master` flips or `done` flips.
fn spawn_shutdown_watchdog(
    master: Arc<AtomicBool>,
    per_handle: Vec<Arc<AtomicBool>>,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("sonda-shutdown-watchdog".to_string())
        .spawn(move || {
            let poll = std::time::Duration::from_millis(100);
            loop {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                if !master.load(Ordering::SeqCst) {
                    for flag in &per_handle {
                        flag.store(false, Ordering::SeqCst);
                    }
                    return;
                }
                std::thread::sleep(poll);
            }
        })
        .expect("watchdog thread must spawn")
}

/// Set the shutdown flag, signalling all running scenarios to stop.
///
/// This is a convenience wrapper that stores `false` with `SeqCst` ordering,
/// matching the ordering used by the signal handler in the CLI.
pub fn signal_shutdown(shutdown: &AtomicBool) {
    shutdown.store(false, Ordering::SeqCst);
}

/// Launch a compiled scenario file with `while:` / `after:` gating wired in,
/// returning the live handles without joining them. When `resolver` is `Some`,
/// cross-POST `while:` references resolve through the registry; otherwise every
/// reference must resolve inside `file`. Each handle owns an independent
/// shutdown flag — see [`run_multi_compiled`] for the master-stop-all path.
#[cfg(feature = "config")]
pub fn launch_multi_compiled(
    file: CompiledFile,
    resolver: Option<Arc<dyn GateBusResolver>>,
) -> Result<Vec<crate::schedule::handle::ScenarioHandle>, SondaError> {
    let CompiledFile {
        scenario_name: file_scenario_name,
        entries,
        ..
    } = file;
    let file_scenario_name_ref = file_scenario_name.as_deref();

    let mut bus_ids = while_upstream_ids(&entries, file_scenario_name_ref);
    if file_scenario_name_ref.is_some() {
        for entry in &entries {
            if entry.signal_type == "metrics" {
                if let Some(id) = entry.id.as_deref() {
                    bus_ids.push(id.to_string());
                }
            }
        }
        bus_ids.sort();
        bus_ids.dedup();
    }
    let mut buses: HashMap<String, Arc<GateBus>> = HashMap::with_capacity(bus_ids.len());
    for id in bus_ids {
        buses.insert(id, Arc::new(GateBus::new()));
    }

    if let (Some(ref r), Some(name)) = (&resolver, file_scenario_name_ref) {
        for (entry_id, bus) in buses.iter() {
            r.register(name, entry_id, Arc::clone(bus)).map_err(|e| {
                SondaError::Config(crate::ConfigError::invalid(format!(
                    "gate bus registry: {e}"
                )))
            })?;
        }
    }

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

        let mut deferred: Option<DeferredSubscription> = None;
        let mut active: Option<ActiveSubscription> = None;
        let gate_ctx = if let Some(ref clause) = while_clause {
            if let Some(cross_scenario) = cross_scenario_target(clause, file_scenario_name_ref) {
                let resolver = resolver.as_ref().ok_or_else(|| {
                    SondaError::Config(crate::ConfigError::invalid(
                        "cross-POST while: reference requires a GateBusResolver; \
                         the CLI does not support cross-POST `while:`",
                    ))
                })?;
                let spec = WhileSpec {
                    op: clause.op,
                    threshold: clause.value,
                };
                let (tx, rx) = watch::channel::<Option<crate::schedule::gate_bus::GateEdge>>(None);
                let if_unresolved = clause.if_unresolved.unwrap_or_default();
                let bus = resolver.lookup(cross_scenario, clause.ref_id.as_str());
                let (gate_rx, initial, start_unresolved) = match bus {
                    Some(bus) => {
                        let init = bus.subscribe_with_while_sender(spec, tx.clone());
                        active = Some(ActiveSubscription {
                            sender: tx,
                            scenario_name: cross_scenario.to_string(),
                            entry_id: clause.ref_id.clone(),
                            if_unresolved,
                            spec,
                        });
                        (GateReceiver::from_while_rx(rx), init, false)
                    }
                    None => {
                        deferred = Some(DeferredSubscription {
                            sender: tx,
                            scenario_name: cross_scenario.to_string(),
                            entry_id: clause.ref_id.clone(),
                            if_unresolved,
                            spec,
                        });
                        (
                            GateReceiver::from_while_rx(rx),
                            InitialState {
                                after_already_fired: false,
                                while_gate_open: None,
                                current_value: f64::NAN,
                            },
                            true,
                        )
                    }
                };
                Some(
                    GateContext::new(gate_rx, initial)
                        .with_delay(delay_clause)
                        .with_has_while(true)
                        .with_if_unresolved(Some(if_unresolved))
                        .with_start_unresolved(start_unresolved),
                )
            } else {
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
                Some(
                    GateContext::new(rx, init)
                        .with_delay(delay_clause)
                        .with_has_while(true),
                )
            }
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
            deferred,
            active,
        });
    }

    let mut handles = Vec::with_capacity(launches.len());
    for (idx, plan) in launches.into_iter().enumerate() {
        let id = plan.id.unwrap_or_else(|| format!("multi-{idx}"));
        let deferred = plan.deferred;
        let active = plan.active;
        let scenario_shutdown = Arc::new(AtomicBool::new(true));
        match launch_scenario_with_gates(
            id.clone(),
            file_scenario_name.clone(),
            plan.entry,
            scenario_shutdown,
            plan.start_delay,
            plan.upstream_bus,
            plan.gate_ctx,
            resolver.clone(),
        ) {
            Ok(handle) => {
                if let (Some(d), Some(r)) = (deferred, resolver.as_ref()) {
                    r.insert_pending(PendingResolution {
                        handle_id: handle.id.clone(),
                        stats: Arc::downgrade(&handle.stats),
                        edge_sender: d.sender,
                        scenario_name: d.scenario_name,
                        entry_id: d.entry_id,
                        if_unresolved: d.if_unresolved,
                        registered_at: std::time::Instant::now(),
                        attempts: 0,
                        spec: d.spec,
                    });
                }
                if let (Some(a), Some(r)) = (active, resolver.as_ref()) {
                    r.track_subscriber(PendingResolution {
                        handle_id: handle.id.clone(),
                        stats: Arc::downgrade(&handle.stats),
                        edge_sender: a.sender,
                        scenario_name: a.scenario_name,
                        entry_id: a.entry_id,
                        if_unresolved: a.if_unresolved,
                        registered_at: std::time::Instant::now(),
                        attempts: 0,
                        spec: a.spec,
                    });
                }
                handles.push(handle);
            }
            Err(e) => {
                for handle in &handles {
                    handle.stop();
                }
                for mut handle in handles {
                    let _ = handle.join_timeout(std::time::Duration::from_secs(1));
                }
                return Err(e);
            }
        }
    }

    if let Some(ref r) = resolver {
        r.sweep_pending();
    }

    Ok(handles)
}

#[cfg(feature = "config")]
fn cross_scenario_target<'a>(
    clause: &'a crate::compiler::WhileClause,
    file_scenario_name: Option<&'a str>,
) -> Option<&'a str> {
    let name = clause.scenario_name.as_deref()?;
    if Some(name) == file_scenario_name {
        None
    } else {
        Some(name)
    }
}

#[cfg(feature = "config")]
struct DeferredSubscription {
    sender: crate::schedule::gate_bus::GateEdgeSender,
    scenario_name: String,
    entry_id: String,
    if_unresolved: crate::compiler::UnresolvedBehavior,
    spec: WhileSpec,
}

#[cfg(feature = "config")]
struct ActiveSubscription {
    sender: crate::schedule::gate_bus::GateEdgeSender,
    scenario_name: String,
    entry_id: String,
    if_unresolved: crate::compiler::UnresolvedBehavior,
    spec: WhileSpec,
}

/// Run a compiled scenario file with `while:` / `after:` gating wired in.
///
/// Spawns every scenario via [`launch_multi_compiled`] and joins the threads.
/// Non-gated entries launch on the existing non-gated path with no per-tick
/// overhead.
#[cfg(feature = "config")]
pub fn run_multi_compiled(file: CompiledFile, shutdown: Arc<AtomicBool>) -> Result<(), SondaError> {
    let handles = launch_multi_compiled(file, None)?;
    let per_handle_shutdowns: Vec<Arc<AtomicBool>> =
        handles.iter().map(|h| Arc::clone(&h.shutdown)).collect();

    let done = Arc::new(AtomicBool::new(false));
    let watchdog = spawn_shutdown_watchdog(shutdown, per_handle_shutdowns, Arc::clone(&done));

    let mut errors: Vec<String> = Vec::new();
    for mut handle in handles {
        match handle.join(None) {
            Ok(()) => {}
            Err(e) => errors.push(e.to_string()),
        }
    }

    done.store(true, Ordering::SeqCst);
    let _ = watchdog.join();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SondaError::Runtime(RuntimeError::ScenariosFailed(
            errors.join("; "),
        )))
    }
}

/// Local `while:` upstream ids; cross-POST refs are excluded.
#[cfg(feature = "config")]
fn while_upstream_ids(
    entries: &[crate::compiler::compile_after::CompiledEntry],
    file_scenario_name: Option<&str>,
) -> Vec<String> {
    let mut ids: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            let clause = e.while_clause.as_ref()?;
            if cross_scenario_target(clause, file_scenario_name).is_some() {
                None
            } else {
                Some(clause.ref_id.clone())
            }
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(feature = "config")]
struct LaunchPlan {
    id: Option<String>,
    entry: ScenarioEntry,
    gate_ctx: Option<GateContext>,
    upstream_bus: Option<Arc<GateBus>>,
    start_delay: Option<std::time::Duration>,
    deferred: Option<DeferredSubscription>,
    active: Option<ActiveSubscription>,
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

    #[cfg(feature = "config")]
    use super::launch_multi_compiled;
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
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                start_time: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                    start_time: None,
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
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 2.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 2.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
            metric_type: None,
            help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 1.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
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
                    start_time: None,
                    jitter: None,
                    jitter_seed: None,
                    on_sink_error: crate::OnSinkError::Warn,
                },
                generator: GeneratorConfig::Constant { value: 2.0 },
                encoder: EncoderConfig::PrometheusText { precision: None },
                metric_type: None,
                help: None,
            }),
        ];
        let shutdown = Arc::new(AtomicBool::new(true));
        let result = run_multi(entries, shutdown);
        assert!(
            result.is_ok(),
            "scenarios with clock_group and offsets should complete"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn while_upstream_ids_returns_only_entries_referenced_by_a_while_clause() {
        use super::while_upstream_ids;
        use crate::compile_scenario_file_compiled;
        use crate::compiler::expand::InMemoryPackResolver;

        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 5
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream_a
    signal_type: metrics
    name: upstream_a
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0
  - id: middle_b
    signal_type: metrics
    name: middle_b
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream_a
      op: '>'
      value: 50.0
  - id: lonely_c
    signal_type: metrics
    name: lonely_c
    generator:
      type: constant
      value: 1.0
  - id: lonely_d
    signal_type: metrics
    name: lonely_d
    generator:
      type: constant
      value: 1.0
";
        let resolver = InMemoryPackResolver::new();
        let compiled =
            compile_scenario_file_compiled(yaml, &resolver).expect("compile must succeed");
        let ids = while_upstream_ids(&compiled.entries, compiled.scenario_name.as_deref());
        assert_eq!(
            ids,
            vec!["upstream_a".to_string()],
            "only entries referenced by some while: clause must get a bus, got {ids:?}"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn launch_multi_compiled_partial_cleanup_stops_already_launched_handles() {
        use crate::compile_scenario_file_compiled;
        use crate::compiler::expand::InMemoryPackResolver;

        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 10s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cleanup_a
    signal_type: metrics
    name: cleanup_a
    generator:
      type: constant
      value: 1.0
  - id: cleanup_b
    signal_type: metrics
    name: cleanup_b
    generator:
      type: constant
      value: 2.0
";
        let resolver = InMemoryPackResolver::new();
        let compiled =
            compile_scenario_file_compiled(yaml, &resolver).expect("compile must succeed");

        let mut handles = launch_multi_compiled(compiled, None).expect("launch must succeed");
        assert_eq!(handles.len(), 2, "must launch both entries");
        assert!(
            handles.iter().all(|h| h.is_alive()),
            "both threads must be alive immediately after launch"
        );

        for handle in &handles {
            handle.stop();
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && handles.iter().any(|h| h.is_alive()) {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            handles.iter().all(|h| !h.is_alive()),
            "every handle must exit after stop() — partial-launch cleanup must not leak threads"
        );

        for handle in &mut handles {
            handle
                .join(Some(Duration::from_secs(1)))
                .expect("join must succeed after stop");
        }
    }

    #[cfg(feature = "config")]
    mod cross_post {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, RwLock, Weak};
        use std::thread;
        use std::time::{Duration, Instant};

        use crate::compile_scenario_file_compiled;
        use crate::compiler::expand::InMemoryPackResolver;
        use crate::schedule::gate_bus::{
            GateBus, GateBusResolver, GateEdgeSender, PendingRef, PendingResolution, RegistryError,
        };
        use crate::schedule::handle::ScenarioHandle;
        use crate::schedule::stats::{ScenarioState, ScenarioStats};

        use super::super::launch_multi_compiled;

        struct TestRegistry {
            buses: Mutex<HashMap<(String, String), Arc<GateBus>>>,
            pending: Mutex<Vec<PendingResolution>>,
        }

        impl TestRegistry {
            fn new() -> Arc<Self> {
                Arc::new(Self {
                    buses: Mutex::new(HashMap::new()),
                    pending: Mutex::new(Vec::new()),
                })
            }

            fn pending_len(&self) -> usize {
                self.pending.lock().unwrap().len()
            }
        }

        impl GateBusResolver for TestRegistry {
            fn register(
                &self,
                scenario_name: &str,
                entry_id: &str,
                bus: Arc<GateBus>,
            ) -> Result<(), RegistryError> {
                let key = (scenario_name.to_string(), entry_id.to_string());
                let mut buses = self.buses.lock().unwrap();
                if buses.contains_key(&key) {
                    return Err(RegistryError::DuplicateScenarioName {
                        name: scenario_name.to_string(),
                    });
                }
                buses.insert(key, bus);
                Ok(())
            }

            fn lookup(&self, scenario_name: &str, entry_id: &str) -> Option<Arc<GateBus>> {
                self.buses
                    .lock()
                    .unwrap()
                    .get(&(scenario_name.to_string(), entry_id.to_string()))
                    .cloned()
            }

            fn subscribe(
                &self,
                upstream: (&str, &str),
                _downstream_handle_id: &str,
                _downstream_stats: Weak<RwLock<ScenarioStats>>,
                _edge_sender: GateEdgeSender,
            ) -> Option<Arc<GateBus>> {
                self.buses
                    .lock()
                    .unwrap()
                    .get(&(upstream.0.to_string(), upstream.1.to_string()))
                    .cloned()
            }

            fn unregister(&self, scenario_name: &str) {
                let mut buses = self.buses.lock().unwrap();
                let keys: Vec<_> = buses
                    .keys()
                    .filter(|(s, _)| s == scenario_name)
                    .cloned()
                    .collect();
                for key in keys {
                    if let Some(bus) = buses.remove(&key) {
                        bus.broadcast_upstream_gone();
                    }
                }
            }

            fn sweep_pending(&self) -> usize {
                let mut pending = self.pending.lock().unwrap();
                let buses = self.buses.lock().unwrap();
                let mut promoted = 0;
                pending.retain(|p| {
                    if p.stats.strong_count() == 0 {
                        return false;
                    }
                    let key = (p.scenario_name.clone(), p.entry_id.clone());
                    if let Some(bus) = buses.get(&key) {
                        bus.subscribe_with_while_sender(p.spec, p.edge_sender.clone());
                        promoted += 1;
                        false
                    } else {
                        true
                    }
                });
                promoted
            }

            fn insert_pending(&self, pending: PendingResolution) {
                self.pending.lock().unwrap().push(pending);
            }

            fn pending_for_handle(&self, handle_id: &str) -> Option<PendingRef> {
                let pending = self.pending.lock().unwrap();
                pending
                    .iter()
                    .find(|p| p.handle_id == handle_id)
                    .map(|p| PendingRef {
                        scenario_name: p.scenario_name.clone(),
                        entry_id: p.entry_id.clone(),
                        if_unresolved: p.if_unresolved,
                        #[cfg(feature = "config")]
                        registered_at: chrono::DateTime::<chrono::Utc>::from(
                            std::time::SystemTime::now(),
                        ),
                        attempts: p.attempts,
                    })
            }

            fn scenario_name_in_use(&self, scenario_name: &str) -> bool {
                self.buses
                    .lock()
                    .unwrap()
                    .keys()
                    .any(|(s, _)| s == scenario_name)
            }
        }

        fn downstream_yaml(if_unresolved: &str) -> String {
            format!(
                r#"
version: 2
kind: runnable
scenario_name: downstream_post
defaults:
  rate: 50
  duration: 5s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: dependent
    signal_type: metrics
    name: dependent
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream_metric
      op: ">"
      value: 0
      scenario_name: upstream_post
      if_unresolved: {if_unresolved}
"#
            )
        }

        fn upstream_yaml() -> String {
            r#"
version: 2
kind: runnable
scenario_name: upstream_post
defaults:
  rate: 50
  duration: 5s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream_metric
    signal_type: metrics
    name: upstream_metric
    generator:
      type: constant
      value: 1.0
"#
            .to_string()
        }

        fn wait_for_state(handle: &ScenarioHandle, expected: ScenarioState, timeout: Duration) {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if handle.stats_snapshot().state == expected {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let actual = handle.stats_snapshot().state;
            panic!("timed out waiting for {expected:?}; current state = {actual:?}");
        }

        fn stop_and_join(mut handles: Vec<ScenarioHandle>) {
            for h in &handles {
                h.stop();
            }
            for h in &mut handles {
                let _ = h.join(Some(Duration::from_secs(2)));
            }
        }

        #[test]
        fn t8_downstream_enters_unresolved_then_promoted_on_register_and_sweep() {
            let registry = TestRegistry::new();
            let resolver: Arc<dyn GateBusResolver> = registry.clone();

            let compiled = compile_scenario_file_compiled(
                &downstream_yaml("pending"),
                &InMemoryPackResolver::new(),
            )
            .expect("compile downstream");

            let handles = launch_multi_compiled(compiled, Some(Arc::clone(&resolver)))
                .expect("launch downstream");
            assert_eq!(handles.len(), 1);
            wait_for_state(
                &handles[0],
                ScenarioState::Unresolved,
                Duration::from_secs(2),
            );
            assert_eq!(registry.pending_len(), 1);

            let bus = Arc::new(GateBus::new());
            bus.tick(1.0);
            resolver
                .register("upstream_post", "upstream_metric", bus)
                .expect("register upstream");
            let promoted = resolver.sweep_pending();
            assert_eq!(promoted, 1);

            wait_for_state(&handles[0], ScenarioState::Running, Duration::from_secs(2));
            stop_and_join(handles);
        }

        #[test]
        fn t9_downstream_first_then_upstream_post_promotes_via_sweep() {
            let registry = TestRegistry::new();
            let resolver: Arc<dyn GateBusResolver> = registry.clone();

            let downstream = compile_scenario_file_compiled(
                &downstream_yaml("pending"),
                &InMemoryPackResolver::new(),
            )
            .expect("compile downstream");

            let downstream_handles = launch_multi_compiled(downstream, Some(Arc::clone(&resolver)))
                .expect("launch downstream");
            wait_for_state(
                &downstream_handles[0],
                ScenarioState::Unresolved,
                Duration::from_secs(2),
            );

            let upstream =
                compile_scenario_file_compiled(&upstream_yaml(), &InMemoryPackResolver::new())
                    .expect("compile upstream");
            let upstream_handles = launch_multi_compiled(upstream, Some(Arc::clone(&resolver)))
                .expect("launch upstream");
            wait_for_state(
                &downstream_handles[0],
                ScenarioState::Running,
                Duration::from_secs(2),
            );

            stop_and_join(downstream_handles);
            stop_and_join(upstream_handles);
        }

        // Re-resolution after `unregister` (so a downstream Unresolved can pick up a fresh
        // upstream with the same scenario_name) requires the registry to push affected
        // downstreams back into `pending` on unregister — that mechanism lives with the
        // production `GateBusRegistry` implementation, not the test resolver.
        #[test]
        fn t10_unregister_drives_downstream_back_to_unresolved() {
            let registry = TestRegistry::new();
            let resolver: Arc<dyn GateBusResolver> = registry.clone();

            let upstream =
                compile_scenario_file_compiled(&upstream_yaml(), &InMemoryPackResolver::new())
                    .expect("compile upstream");
            let upstream_handles = launch_multi_compiled(upstream, Some(Arc::clone(&resolver)))
                .expect("launch upstream");

            let downstream = compile_scenario_file_compiled(
                &downstream_yaml("open"),
                &InMemoryPackResolver::new(),
            )
            .expect("compile downstream");
            let downstream_handles = launch_multi_compiled(downstream, Some(Arc::clone(&resolver)))
                .expect("launch downstream");
            wait_for_state(
                &downstream_handles[0],
                ScenarioState::Running,
                Duration::from_secs(2),
            );

            resolver.unregister("upstream_post");
            wait_for_state(
                &downstream_handles[0],
                ScenarioState::Unresolved,
                Duration::from_secs(2),
            );
            for h in &upstream_handles {
                h.stop();
            }
            for mut h in upstream_handles {
                let _ = h.join(Some(Duration::from_secs(1)));
            }

            let bus = Arc::new(GateBus::new());
            bus.tick(1.0);
            resolver
                .register("upstream_post", "upstream_metric", bus)
                .expect("re-register");
            assert_eq!(
                downstream_handles[0].stats_snapshot().state,
                ScenarioState::Unresolved,
            );

            stop_and_join(downstream_handles);
        }

        #[test]
        fn t11_if_unresolved_open_ticks_at_full_rate() {
            let registry = TestRegistry::new();
            let resolver: Arc<dyn GateBusResolver> = registry.clone();

            let compiled = compile_scenario_file_compiled(
                &downstream_yaml("open"),
                &InMemoryPackResolver::new(),
            )
            .expect("compile downstream");
            let handles =
                launch_multi_compiled(compiled, Some(Arc::clone(&resolver))).expect("launch");
            wait_for_state(
                &handles[0],
                ScenarioState::Unresolved,
                Duration::from_secs(2),
            );

            let snapshot_at_t0 = handles[0].stats_snapshot().total_events;
            thread::sleep(Duration::from_millis(400));
            let snapshot_after = handles[0].stats_snapshot().total_events;
            let delta = snapshot_after - snapshot_at_t0;
            assert!(
                delta >= 5,
                "if_unresolved: open must keep ticking; delta = {delta}"
            );

            stop_and_join(handles);
        }

        #[test]
        fn t11b_if_unresolved_open_transitions_to_running_on_upstream_register() {
            let registry = TestRegistry::new();
            let resolver: Arc<dyn GateBusResolver> = registry.clone();

            let compiled = compile_scenario_file_compiled(
                &downstream_yaml("open"),
                &InMemoryPackResolver::new(),
            )
            .expect("compile downstream");
            let handles =
                launch_multi_compiled(compiled, Some(Arc::clone(&resolver))).expect("launch");
            wait_for_state(
                &handles[0],
                ScenarioState::Unresolved,
                Duration::from_secs(2),
            );

            let pre_register = handles[0].stats_snapshot().total_events;

            let bus = Arc::new(GateBus::new());
            bus.tick(1.0);
            resolver
                .register("upstream_post", "upstream_metric", bus)
                .expect("register upstream");
            let promoted = resolver.sweep_pending();
            assert_eq!(promoted, 1);

            wait_for_state(&handles[0], ScenarioState::Running, Duration::from_secs(2));

            let post_transition = handles[0].stats_snapshot().total_events;
            thread::sleep(Duration::from_millis(400));
            let after_running = handles[0].stats_snapshot().total_events;
            assert!(
                post_transition > pre_register,
                "emission must continue across the Unresolved -> Running transition; \
                 pre_register = {pre_register}, post_transition = {post_transition}"
            );
            let delta = after_running - post_transition;
            assert!(
                delta >= 5,
                "Running state must keep ticking after transition; delta = {delta}"
            );

            stop_and_join(handles);
        }

        #[test]
        fn t12_if_unresolved_closed_does_not_emit() {
            let registry = TestRegistry::new();
            let resolver: Arc<dyn GateBusResolver> = registry.clone();

            let compiled = compile_scenario_file_compiled(
                &downstream_yaml("closed"),
                &InMemoryPackResolver::new(),
            )
            .expect("compile downstream");
            let handles =
                launch_multi_compiled(compiled, Some(Arc::clone(&resolver))).expect("launch");
            wait_for_state(
                &handles[0],
                ScenarioState::Unresolved,
                Duration::from_secs(2),
            );

            let snapshot_at_t0 = handles[0].stats_snapshot().total_events;
            thread::sleep(Duration::from_millis(400));
            let snapshot_after = handles[0].stats_snapshot().total_events;
            assert_eq!(
                snapshot_after, snapshot_at_t0,
                "if_unresolved: closed must not emit ticks"
            );

            stop_and_join(handles);
        }

        #[test]
        fn t13_if_unresolved_pending_does_not_emit_and_holds_state() {
            let registry = TestRegistry::new();
            let resolver: Arc<dyn GateBusResolver> = registry.clone();

            let compiled = compile_scenario_file_compiled(
                &downstream_yaml("pending"),
                &InMemoryPackResolver::new(),
            )
            .expect("compile downstream");
            let handles =
                launch_multi_compiled(compiled, Some(Arc::clone(&resolver))).expect("launch");
            wait_for_state(
                &handles[0],
                ScenarioState::Unresolved,
                Duration::from_secs(2),
            );

            let snapshot_at_t0 = handles[0].stats_snapshot().total_events;
            thread::sleep(Duration::from_millis(400));
            let snap = handles[0].stats_snapshot();
            assert_eq!(snap.total_events, snapshot_at_t0);
            assert_eq!(snap.state, ScenarioState::Unresolved);

            stop_and_join(handles);
        }

        #[test]
        fn t_regress_2_local_only_while_path_unchanged() {
            let yaml = r#"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 200ms
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    generator:
      type: constant
      value: 1.0
  - id: dependent
    signal_type: metrics
    name: dependent
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: ">"
      value: 0
"#;
            let compiled = compile_scenario_file_compiled(yaml, &InMemoryPackResolver::new())
                .expect("compile local-only");
            let mut handles = launch_multi_compiled(compiled, None).expect("launch");
            assert_eq!(handles.len(), 2);
            for h in &mut handles {
                let _ = h.join(Some(Duration::from_secs(2)));
            }
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn launch_multi_compiled_gives_each_handle_an_independent_shutdown_flag() {
        use crate::compile_scenario_file_compiled;
        use crate::compiler::expand::InMemoryPackResolver;

        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 10s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: iso_a
    signal_type: metrics
    name: iso_a
    generator:
      type: constant
      value: 1.0
  - id: iso_b
    signal_type: metrics
    name: iso_b
    generator:
      type: constant
      value: 2.0
  - id: iso_c
    signal_type: metrics
    name: iso_c
    generator:
      type: constant
      value: 3.0
";
        let resolver = InMemoryPackResolver::new();
        let compiled =
            compile_scenario_file_compiled(yaml, &resolver).expect("compile must succeed");

        let mut handles = launch_multi_compiled(compiled, None).expect("launch must succeed");
        assert_eq!(handles.len(), 3, "must launch all three entries");

        for i in 0..handles.len() {
            for j in (i + 1)..handles.len() {
                assert!(
                    !Arc::ptr_eq(&handles[i].shutdown, &handles[j].shutdown),
                    "handles {i} and {j} must own independent shutdown Arcs"
                );
            }
        }

        handles[0].stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && handles[0].is_alive() {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!handles[0].is_alive(), "stopped handle must exit");
        assert!(
            handles[1].is_alive() && handles[2].is_alive(),
            "siblings must remain alive — stop() on one handle must not cascade"
        );

        for handle in &handles {
            handle.stop();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && handles.iter().any(|h| h.is_alive()) {
            thread::sleep(Duration::from_millis(20));
        }
        for handle in &mut handles {
            handle
                .join(Some(Duration::from_secs(1)))
                .expect("join must succeed");
        }
    }
}
