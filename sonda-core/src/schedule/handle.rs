//! Lifecycle handle for a running scenario.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::schedule::stats::ScenarioStats;
use crate::SondaError;

/// A running scenario's lifecycle handle.
///
/// Returned by [`crate::schedule::launch::launch_scenario`]. Provides shutdown,
/// join, and stats access. Used identically by the CLI, multi_runner, and
/// sonda-server.
///
/// The handle is `Send`: the `JoinHandle` is behind an `Option` and the other
/// fields are all `Send`, so the handle can be stored in server state and
/// moved across await points.
pub struct ScenarioHandle {
    /// Unique identifier for this scenario instance.
    pub id: String,
    /// Human-readable scenario name (from config).
    pub name: String,
    /// Shared shutdown flag. Setting this to `false` signals the runner to exit.
    pub shutdown: Arc<AtomicBool>,
    /// The OS thread running the scenario. `None` after [`ScenarioHandle::join`] consumes it.
    pub thread: Option<JoinHandle<Result<(), SondaError>>>,
    /// Wall-clock time when the scenario was launched.
    pub started_at: Instant,
    /// Live statistics updated by the runner thread on each tick.
    pub stats: Arc<RwLock<ScenarioStats>>,
}

impl ScenarioHandle {
    /// Signal the scenario to stop.
    ///
    /// Sets the shutdown flag to `false` with `SeqCst` ordering. The runner
    /// thread will observe this on its next tick and exit cleanly.
    pub fn stop(&self) {
        self.shutdown.store(false, Ordering::SeqCst);
    }

    /// Check whether the scenario thread is still running.
    ///
    /// Returns `true` if the thread is still alive, `false` if it has exited
    /// or if the handle has already been joined.
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .map(|t| !t.is_finished())
            .unwrap_or(false)
    }

    /// Join the scenario thread, consuming it.
    ///
    /// Blocks until the thread exits or the optional timeout expires. If a
    /// timeout is provided and the thread does not exit within that time, this
    /// method returns `Ok(())` without consuming the thread (the thread
    /// continues running and the handle still owns it).
    ///
    /// Returns the thread's result on success, or a [`SondaError`] if the
    /// thread panicked or returned an error.
    pub fn join(&mut self, timeout: Option<Duration>) -> Result<(), SondaError> {
        if self.thread.is_none() {
            // Already joined.
            return Ok(());
        }

        // When a timeout is requested we poll with a short sleep, since
        // `JoinHandle` does not expose a timed-join API on stable Rust.
        if let Some(limit) = timeout {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                if self
                    .thread
                    .as_ref()
                    .map(|t| t.is_finished())
                    .unwrap_or(true)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }

            // If still not finished, return without consuming the handle.
            if !self
                .thread
                .as_ref()
                .map(|t| t.is_finished())
                .unwrap_or(true)
            {
                return Ok(());
            }
        }

        // Consume the JoinHandle.
        let handle = self.thread.take().expect("checked above: thread is Some");
        match handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(SondaError::Config("scenario thread panicked".to_string())),
        }
    }

    /// Elapsed time since the scenario started.
    ///
    /// This is a real-time measurement based on the wall clock recorded at
    /// launch. It continues to grow even after the scenario stops.
    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Read the latest stats snapshot.
    ///
    /// Acquires the read lock briefly, clones the stats, and returns. Does not
    /// block writers for longer than the clone operation.
    pub fn stats_snapshot(&self) -> ScenarioStats {
        self.stats
            .read()
            .expect("ScenarioStats RwLock poisoned")
            .clone()
    }
}
