//! Unified scenario launch API.
//!
//! This module is the single authoritative location for the "validate →
//! create sink → spawn runner → manage lifecycle" pattern. Both the CLI and
//! sonda-server call [`launch_scenario`]; neither duplicates this logic.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::config::validate::{validate_config, validate_log_config};
use crate::config::ScenarioEntry;
use crate::schedule::handle::ScenarioHandle;
use crate::schedule::log_runner::run_logs_with_sink;
use crate::schedule::runner::run_with_sink;
use crate::schedule::stats::ScenarioStats;
use crate::sink::create_sink;
use crate::SondaError;

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
) -> Result<ScenarioHandle, SondaError> {
    let stats = Arc::new(RwLock::new(ScenarioStats::default()));
    let stats_for_thread = Arc::clone(&stats);
    let shutdown_for_thread = Arc::clone(&shutdown);

    // Extract the name before moving `entry` into the thread closure.
    let name = match &entry {
        ScenarioEntry::Metrics(c) => c.name.clone(),
        ScenarioEntry::Logs(c) => c.name.clone(),
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
            match entry {
                ScenarioEntry::Metrics(config) => {
                    let mut sink = create_sink(&config.sink)?;
                    run_with_sink(
                        &config,
                        sink.as_mut(),
                        Some(shutdown_for_thread.as_ref()),
                        Some(Arc::clone(&stats_for_thread)),
                    )
                }
                ScenarioEntry::Logs(config) => {
                    let mut sink = create_sink(&config.sink)?;
                    run_logs_with_sink(
                        &config,
                        sink.as_mut(),
                        Some(shutdown_for_thread.as_ref()),
                        Some(Arc::clone(&stats_for_thread)),
                    )
                }
            }
        })
        .map_err(|e| SondaError::Config(format!("failed to spawn scenario thread: {e}")))?;

    Ok(ScenarioHandle {
        id,
        name,
        shutdown,
        thread: Some(thread),
        started_at,
        stats,
    })
}
