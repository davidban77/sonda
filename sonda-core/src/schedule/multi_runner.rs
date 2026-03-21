//! Multi-scenario runner: runs multiple scenarios concurrently on separate threads.
//!
//! Each scenario runs on its own OS thread. All threads share a single shutdown
//! flag so that Ctrl+C (or any external signal) stops all scenarios cleanly.
//! Thread errors are collected and returned after all threads have finished.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::{MultiScenarioConfig, ScenarioEntry};
use crate::schedule::log_runner::run_logs_with_sink;
use crate::schedule::runner::run_with_sink;
use crate::sink::create_sink;
use crate::SondaError;

/// Run all scenarios in `config` concurrently, one OS thread per scenario.
///
/// Each thread runs until either:
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

    for entry in config.scenarios {
        let shutdown_clone = Arc::clone(&shutdown);

        let handle = std::thread::spawn(move || -> Result<(), SondaError> {
            match entry {
                ScenarioEntry::Metrics(scenario_config) => {
                    let mut sink = create_sink(&scenario_config.sink)?;
                    run_with_sink(
                        &scenario_config,
                        sink.as_mut(),
                        Some(shutdown_clone.as_ref()),
                    )
                }
                ScenarioEntry::Logs(log_config) => {
                    let mut sink = create_sink(&log_config.sink)?;
                    run_logs_with_sink(&log_config, sink.as_mut(), Some(shutdown_clone.as_ref()))
                }
            }
        });

        handles.push(handle);
    }

    // Collect results from all threads.
    let mut errors: Vec<String> = Vec::new();
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => errors.push(e.to_string()),
            Err(_) => errors.push("scenario thread panicked".to_string()),
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
