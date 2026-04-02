//! Unified scenario launch API.
//!
//! This module is the single authoritative location for the "validate →
//! create sink → spawn runner → manage lifecycle" pattern. Both the CLI and
//! sonda-server call [`launch_scenario`]; neither duplicates this logic.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::config::validate::{validate_config, validate_log_config};
use crate::config::ScenarioEntry;
use crate::schedule::handle::ScenarioHandle;
use crate::schedule::log_runner::run_logs_with_sink;
use crate::schedule::runner::run_with_sink;
use crate::schedule::stats::ScenarioStats;
use crate::sink::create_sink;
use crate::{RuntimeError, SondaError};

/// Validate any scenario entry (metrics or logs).
///
/// Dispatches to [`validate_config`] or [`validate_log_config`] based on the
/// entry variant. This centralises the `match ScenarioEntry { ... }` dispatch
/// so that neither the CLI nor the server needs to duplicate it.
///
/// # Errors
///
/// Returns [`SondaError`] if the entry's configuration is invalid.
pub fn validate_entry(entry: &ScenarioEntry) -> Result<(), SondaError> {
    match entry {
        ScenarioEntry::Metrics(config) => validate_config(config),
        ScenarioEntry::Logs(config) => validate_log_config(config),
    }
}

/// Launch a single scenario on a new OS thread.
///
/// Creates the sink, wires up the shutdown flag and the stats arc, spawns the
/// appropriate runner (metrics or logs), and returns a [`ScenarioHandle`] for
/// lifecycle management.
///
/// This is the single function that both the CLI and sonda-server call to
/// start a scenario. No scenario launch logic exists outside this function.
///
/// # Parameters
///
/// * `id` — unique identifier for this scenario instance (e.g. a UUID string).
/// * `entry` — the scenario configuration. The `signal_type` field selects
///   the runner.
/// * `shutdown` — shared shutdown flag. Pass `Arc::new(AtomicBool::new(true))`
///   for a new scenario. The handle's [`ScenarioHandle::stop`] method sets this
///   to `false` to request a clean exit.
/// * `start_delay` — optional delay before the scenario begins emitting events.
///   When `Some`, the spawned thread sleeps for this duration before entering the
///   event loop. This enables temporal correlation in multi-scenario mode: "metric
///   A starts immediately, metric B starts 30s later". The sleep happens inside
///   the spawned thread, so it does not block the caller.
///
/// # Errors
///
/// Returns [`SondaError`] if the sink cannot be created. Thread spawn failures
/// are extremely rare on modern operating systems; if the OS refuses to spawn
/// a thread it is treated as an unrecoverable condition.
pub fn launch_scenario(
    id: String,
    entry: ScenarioEntry,
    shutdown: Arc<AtomicBool>,
    start_delay: Option<Duration>,
) -> Result<ScenarioHandle, SondaError> {
    let stats = Arc::new(RwLock::new(ScenarioStats::default()));
    let stats_for_thread = Arc::clone(&stats);
    let shutdown_for_thread = Arc::clone(&shutdown);

    // Extract the name and target rate before moving `entry` into the thread closure.
    let (name, target_rate) = match &entry {
        ScenarioEntry::Metrics(c) => (c.name.clone(), c.rate),
        ScenarioEntry::Logs(c) => (c.name.clone(), c.rate),
    };

    let started_at = Instant::now();

    // Validate shutdown flag is currently set to `true` (running). The caller
    // is responsible for ensuring this; we document the contract but do not
    // enforce it here to avoid a redundant check on every launch.
    //
    // Ensure `running` ordering is visible from the new thread.
    shutdown.store(true, Ordering::SeqCst);

    let thread = std::thread::Builder::new()
        .name(format!("sonda-{}", name))
        .spawn(move || -> Result<(), SondaError> {
            // If a start delay is configured, sleep before entering the event
            // loop. This enables phase-offset correlation between scenarios
            // launched from the same multi-scenario config. The sleep respects
            // the shutdown flag so that Ctrl+C during the delay exits promptly.
            if let Some(delay) = start_delay {
                let deadline = std::time::Instant::now() + delay;
                while std::time::Instant::now() < deadline {
                    if !shutdown_for_thread.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    // Poll every 50ms to keep shutdown responsive.
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    let sleep_chunk = remaining.min(Duration::from_millis(50));
                    if sleep_chunk > Duration::ZERO {
                        std::thread::sleep(sleep_chunk);
                    }
                }
            }

            match entry {
                ScenarioEntry::Metrics(config) => {
                    let mut sink = create_sink(&config.sink, None)?;
                    run_with_sink(
                        &config,
                        sink.as_mut(),
                        Some(shutdown_for_thread.as_ref()),
                        Some(Arc::clone(&stats_for_thread)),
                    )
                }
                ScenarioEntry::Logs(config) => {
                    let mut sink = create_sink(&config.sink, config.labels.as_ref())?;
                    run_logs_with_sink(
                        &config,
                        sink.as_mut(),
                        Some(shutdown_for_thread.as_ref()),
                        Some(Arc::clone(&stats_for_thread)),
                    )
                }
            }
        })
        .map_err(|e| SondaError::Runtime(RuntimeError::SpawnFailed(e)))?;

    Ok(ScenarioHandle {
        id,
        name,
        shutdown,
        thread: Some(thread),
        started_at,
        stats,
        target_rate,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::config::{LogScenarioConfig, ScenarioConfig, ScenarioEntry};
    use crate::encoder::EncoderConfig;
    use crate::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
    use crate::sink::SinkConfig;

    // ---- Helpers ------------------------------------------------------------

    /// Build a short-lived metrics `ScenarioEntry` (runs for 200ms then stops).
    fn metrics_entry(name: &str) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            name: name.to_string(),
            rate: 50.0,
            duration: Some("200ms".to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        })
    }

    /// Build a short-lived logs `ScenarioEntry` (runs for 200ms then stops).
    fn logs_entry(name: &str) -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            name: name.to_string(),
            rate: 50.0,
            duration: Some("200ms".to_string()),
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test log".to_string(),
                    field_pools: HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::JsonLines { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        })
    }

    /// Build an indefinitely-running metrics entry (no duration).
    fn metrics_entry_indefinite(name: &str) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            name: name.to_string(),
            rate: 100.0,
            duration: None,
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        })
    }

    /// Build an indefinitely-running logs entry (no duration).
    fn logs_entry_indefinite(name: &str) -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            name: name.to_string(),
            rate: 100.0,
            duration: None,
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "indefinite log".to_string(),
                    field_pools: HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(1),
            },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::JsonLines { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        })
    }

    // ---- validate_entry: dispatches to the correct validator ----------------

    /// validate_entry dispatches to validate_config for a Metrics entry.
    #[test]
    fn validate_entry_accepts_valid_metrics_entry() {
        let entry = metrics_entry("valid_metrics");
        let result = validate_entry(&entry);
        assert!(
            result.is_ok(),
            "validate_entry must accept a valid metrics entry: {result:?}"
        );
    }

    /// validate_entry dispatches to validate_log_config for a Logs entry.
    #[test]
    fn validate_entry_accepts_valid_logs_entry() {
        let entry = logs_entry("valid_logs");
        let result = validate_entry(&entry);
        assert!(
            result.is_ok(),
            "validate_entry must accept a valid logs entry: {result:?}"
        );
    }

    /// validate_entry returns Err for a Metrics entry with rate = 0 (invalid).
    #[test]
    fn validate_entry_rejects_metrics_entry_with_zero_rate() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "bad_metrics".to_string(),
            rate: 0.0, // invalid
            duration: Some("1s".to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject a metrics entry with rate=0"
        );
    }

    /// validate_entry returns Err for a Metrics entry with negative rate.
    #[test]
    fn validate_entry_rejects_metrics_entry_with_negative_rate() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "neg_rate".to_string(),
            rate: -5.0,
            duration: Some("1s".to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject negative rate for metrics entry"
        );
    }

    /// validate_entry returns Err for a Logs entry with rate = 0 (invalid).
    #[test]
    fn validate_entry_rejects_logs_entry_with_zero_rate() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            name: "bad_logs".to_string(),
            rate: 0.0, // invalid
            duration: Some("1s".to_string()),
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "msg".to_string(),
                    field_pools: HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::JsonLines { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject a logs entry with rate=0"
        );
    }

    /// validate_entry returns Err for a Metrics entry with an invalid duration.
    #[test]
    fn validate_entry_rejects_metrics_entry_with_bad_duration() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "bad_dur".to_string(),
            rate: 10.0,
            duration: Some("not_a_duration".to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        let result = validate_entry(&entry);
        assert!(
            result.is_err(),
            "validate_entry must reject an invalid duration string"
        );
    }

    // ---- launch_scenario: returns a running handle --------------------------

    /// launch_scenario with a metrics entry returns a handle whose thread is alive.
    #[test]
    fn launch_scenario_metrics_returns_running_handle() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = metrics_entry_indefinite("launch_metrics");

        let mut handle =
            launch_scenario("test-id-1".to_string(), entry, Arc::clone(&shutdown), None)
                .expect("launch must succeed for valid metrics entry");

        // The thread must be alive immediately after launch.
        assert!(
            handle.is_running(),
            "handle must report is_running() == true immediately after launch"
        );

        // Verify the handle fields are populated correctly.
        assert_eq!(handle.id, "test-id-1");
        assert_eq!(handle.name, "launch_metrics");

        // Shut down cleanly.
        handle.stop();
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("join must succeed after stop");
    }

    /// launch_scenario with a logs entry returns a handle whose thread is alive.
    #[test]
    fn launch_scenario_logs_returns_running_handle() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = logs_entry_indefinite("launch_logs");

        let mut handle =
            launch_scenario("test-id-2".to_string(), entry, Arc::clone(&shutdown), None)
                .expect("launch must succeed for valid logs entry");

        assert!(
            handle.is_running(),
            "handle must report is_running() == true for a launched logs scenario"
        );
        assert_eq!(handle.name, "launch_logs");

        handle.stop();
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("join must succeed after stop");
    }

    // ---- stop() + join() exits cleanly --------------------------------------

    /// stop() followed by join() on a metrics scenario exits cleanly (Ok).
    #[test]
    fn stop_then_join_metrics_scenario_returns_ok() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = metrics_entry_indefinite("stop_join_metrics");
        let mut handle = launch_scenario("id-stop-1".to_string(), entry, shutdown, None)
            .expect("launch must succeed");

        handle.stop();
        let result = handle.join(Some(Duration::from_secs(3)));
        assert!(
            result.is_ok(),
            "join after stop must return Ok for metrics: {result:?}"
        );
        assert!(
            !handle.is_running(),
            "is_running must be false after stop + join"
        );
    }

    /// stop() followed by join() on a logs scenario exits cleanly (Ok).
    #[test]
    fn stop_then_join_logs_scenario_returns_ok() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = logs_entry_indefinite("stop_join_logs");
        let mut handle = launch_scenario("id-stop-2".to_string(), entry, shutdown, None)
            .expect("launch must succeed");

        handle.stop();
        let result = handle.join(Some(Duration::from_secs(3)));
        assert!(
            result.is_ok(),
            "join after stop must return Ok for logs: {result:?}"
        );
    }

    /// A finite-duration scenario exits on its own and join() returns Ok.
    #[test]
    fn finite_duration_scenario_exits_naturally_and_join_returns_ok() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = metrics_entry("natural_exit");
        let mut handle = launch_scenario("id-natural".to_string(), entry, shutdown, None)
            .expect("launch must succeed");

        // 200ms duration — wait for it to finish, then join.
        let result = handle.join(Some(Duration::from_secs(3)));
        assert!(
            result.is_ok(),
            "natural exit must result in Ok join: {result:?}"
        );
    }

    // ---- stats_snapshot(): non-zero total_events after brief run ------------

    /// After letting a launched scenario run briefly, stats show non-zero events.
    #[test]
    fn stats_snapshot_shows_nonzero_events_after_brief_run() {
        use std::thread;

        let shutdown = Arc::new(AtomicBool::new(true));
        // High rate so events accumulate quickly.
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "stats_test".to_string(),
            rate: 500.0,
            duration: None, // indefinite — we stop it manually
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });

        let mut handle =
            launch_scenario("id-stats".to_string(), entry, Arc::clone(&shutdown), None)
                .expect("launch must succeed");

        // Wait long enough for at least a few events to be emitted and stats updated.
        thread::sleep(Duration::from_millis(200));

        let snap = handle.stats_snapshot();
        assert!(
            snap.total_events > 0,
            "stats_snapshot must show non-zero total_events after running for 200ms, got {}",
            snap.total_events
        );
        assert!(
            snap.bytes_emitted > 0,
            "stats_snapshot must show non-zero bytes_emitted, got {}",
            snap.bytes_emitted
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    /// Stats are also tracked for logs scenarios.
    #[test]
    fn stats_snapshot_shows_nonzero_events_for_logs_scenario() {
        use std::thread;

        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            name: "logs_stats_test".to_string(),
            rate: 500.0,
            duration: None,
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "stat tracking log".to_string(),
                    field_pools: HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(42),
            },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::JsonLines { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });

        let mut handle = launch_scenario(
            "id-log-stats".to_string(),
            entry,
            Arc::clone(&shutdown),
            None,
        )
        .expect("launch must succeed");

        thread::sleep(Duration::from_millis(200));

        let snap = handle.stats_snapshot();
        assert!(
            snap.total_events > 0,
            "log scenario stats must show non-zero total_events, got {}",
            snap.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    // ---- elapsed(): positive duration after launch --------------------------

    /// elapsed() is positive immediately after launch.
    #[test]
    fn elapsed_is_positive_after_launch() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = metrics_entry_indefinite("elapsed_test");
        let mut handle = launch_scenario("id-elapsed".to_string(), entry, shutdown, None)
            .expect("launch must succeed");

        let d = handle.elapsed();
        assert!(
            d >= Duration::ZERO,
            "elapsed must be non-negative right after launch, got {d:?}"
        );

        handle.stop();
        handle.join(None).ok();
    }

    // ---- shutdown flag is set to true on launch -----------------------------

    /// launch_scenario sets the shared shutdown flag to true (SeqCst), regardless
    /// of what the caller set it to beforehand.
    #[test]
    fn launch_scenario_resets_shutdown_flag_to_true() {
        // Intentionally start the flag as false to verify launch forces it true.
        let shutdown = Arc::new(AtomicBool::new(false));
        let entry = metrics_entry_indefinite("flag_reset");

        let mut handle = launch_scenario("id-flag".to_string(), entry, Arc::clone(&shutdown), None)
            .expect("launch must succeed");

        assert!(
            shutdown.load(Ordering::SeqCst),
            "launch_scenario must reset the shutdown flag to true"
        );

        handle.stop();
        handle.join(None).ok();
    }

    // ---- start_delay: None starts immediately -------------------------------

    /// launch_scenario with start_delay=None starts emitting events immediately.
    #[test]
    fn launch_with_no_start_delay_emits_events_immediately() {
        use std::thread;

        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "no_delay_test".to_string(),
            rate: 500.0,
            duration: None,
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });

        let mut handle =
            launch_scenario("id-nodelay".to_string(), entry, Arc::clone(&shutdown), None)
                .expect("launch must succeed");

        // Wait briefly and check events have already been emitted.
        thread::sleep(Duration::from_millis(200));
        let snap = handle.stats_snapshot();
        assert!(
            snap.total_events > 0,
            "with no start_delay, events should be emitted within 200ms, got {}",
            snap.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    // ---- start_delay: Some(Duration) delays the start -----------------------

    /// launch_scenario with start_delay=Some(500ms) does not emit events for
    /// the first ~400ms (allowing margin for thread scheduling).
    #[test]
    fn launch_with_start_delay_does_not_emit_during_delay() {
        use std::thread;

        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "delay_test".to_string(),
            rate: 500.0,
            duration: Some("1s".to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });

        let delay = Duration::from_millis(500);
        let mut handle = launch_scenario(
            "id-delay".to_string(),
            entry,
            Arc::clone(&shutdown),
            Some(delay),
        )
        .expect("launch must succeed");

        // Check after 100ms — should still be in the delay period.
        thread::sleep(Duration::from_millis(100));
        let snap_early = handle.stats_snapshot();
        assert_eq!(
            snap_early.total_events, 0,
            "during start_delay, total_events should be 0, got {}",
            snap_early.total_events
        );

        // Wait for the delay to expire and some events to be emitted.
        thread::sleep(Duration::from_millis(600));
        let snap_after = handle.stats_snapshot();
        assert!(
            snap_after.total_events > 0,
            "after start_delay expires, events should be emitted, got {}",
            snap_after.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    // ---- Shutdown during start_delay exits cleanly --------------------------

    /// Sending shutdown during start_delay causes the thread to exit cleanly
    /// without hanging.
    #[test]
    fn shutdown_during_start_delay_exits_cleanly() {
        use std::thread;
        use std::time::Instant;

        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = metrics_entry_indefinite("shutdown_delay");

        // Set a long delay (10 seconds) so we can verify the shutdown works.
        let delay = Duration::from_secs(10);
        let mut handle = launch_scenario(
            "id-shutdown-delay".to_string(),
            entry,
            Arc::clone(&shutdown),
            Some(delay),
        )
        .expect("launch must succeed");

        // Wait 100ms then signal shutdown.
        thread::sleep(Duration::from_millis(100));
        handle.stop();

        // Join should succeed quickly, well within 2 seconds.
        let start = Instant::now();
        let result = handle.join(Some(Duration::from_secs(2)));
        let elapsed = start.elapsed();

        assert!(
            result.is_ok(),
            "join after shutdown during delay must return Ok: {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "thread must exit promptly after shutdown during delay, took {:?}",
            elapsed
        );

        // No events should have been emitted since we stopped during the delay.
        let snap = handle.stats_snapshot();
        assert_eq!(
            snap.total_events, 0,
            "no events should be emitted when shutdown during delay, got {}",
            snap.total_events
        );
    }

    // ---- start_delay with logs scenario -------------------------------------

    /// launch_scenario with start_delay works for logs scenarios too.
    #[test]
    fn launch_logs_with_start_delay_does_not_emit_during_delay() {
        use std::thread;

        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            name: "log_delay_test".to_string(),
            rate: 500.0,
            duration: Some("1s".to_string()),
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "delayed log".to_string(),
                    field_pools: HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::JsonLines { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });

        let delay = Duration::from_millis(500);
        let mut handle = launch_scenario(
            "id-log-delay".to_string(),
            entry,
            Arc::clone(&shutdown),
            Some(delay),
        )
        .expect("launch must succeed");

        // Check during the delay.
        thread::sleep(Duration::from_millis(100));
        let snap_early = handle.stats_snapshot();
        assert_eq!(
            snap_early.total_events, 0,
            "logs scenario should not emit during start_delay, got {}",
            snap_early.total_events
        );

        // Wait for delay to expire.
        thread::sleep(Duration::from_millis(600));
        let snap_after = handle.stats_snapshot();
        assert!(
            snap_after.total_events > 0,
            "logs scenario should emit after delay, got {}",
            snap_after.total_events
        );

        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }
}
