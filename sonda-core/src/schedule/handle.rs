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
    /// The configured target rate (events per second) from the scenario config.
    pub target_rate: f64,
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, RwLock};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::schedule::stats::ScenarioStats;
    use crate::SondaError;

    // ---- Helper: build a ScenarioHandle backed by a trivial thread ----------

    /// Build a `ScenarioHandle` whose thread counts to a limit, then exits.
    ///
    /// The thread increments `total_events` on the shared stats arc each
    /// iteration so tests can observe live stat updates without involving
    /// the full runner pipeline.
    fn make_handle(id: &str, name: &str, events: u64, interval: Duration) -> ScenarioHandle {
        let shutdown = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let shutdown_thread = Arc::clone(&shutdown);
        let stats_thread = Arc::clone(&stats);

        let thread = thread::Builder::new()
            .name(format!("test-{name}"))
            .spawn(move || -> Result<(), SondaError> {
                for _ in 0..events {
                    if !shutdown_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(interval);
                    if let Ok(mut st) = stats_thread.write() {
                        st.total_events += 1;
                        st.bytes_emitted += 64;
                    }
                }
                Ok(())
            })
            .expect("thread must spawn");

        ScenarioHandle {
            id: id.to_string(),
            name: name.to_string(),
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
            target_rate: 100.0,
        }
    }

    // ---- is_running: true before stop, false after join ---------------------

    /// A freshly spawned handle must report is_running() == true.
    #[test]
    fn is_running_returns_true_for_live_thread() {
        let mut handle = make_handle("test-1", "live", 50, Duration::from_millis(10));
        assert!(
            handle.is_running(),
            "is_running must return true for a live thread"
        );
        // Clean up.
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).unwrap();
    }

    /// After stop() + join(), is_running() must return false.
    #[test]
    fn is_running_returns_false_after_stop_and_join() {
        let mut handle = make_handle("test-2", "stopped", 1000, Duration::from_millis(5));
        handle.stop();
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("join must succeed");
        assert!(
            !handle.is_running(),
            "is_running must return false after the thread has been joined"
        );
    }

    /// A handle whose JoinHandle has been consumed (None) returns false.
    #[test]
    fn is_running_returns_false_when_thread_is_none() {
        let mut handle = make_handle("test-3", "none", 1, Duration::from_millis(1));
        // Allow thread to finish naturally.
        thread::sleep(Duration::from_millis(50));
        // Consume the JoinHandle directly to mimic a post-join state.
        handle.thread = None;
        assert!(
            !handle.is_running(),
            "is_running must return false when thread is None"
        );
    }

    // ---- stop(): sets the shutdown flag to false ----------------------------

    /// stop() must store false in the shared AtomicBool.
    #[test]
    fn stop_sets_shutdown_flag_to_false() {
        let handle = make_handle("test-4", "stop_flag", 1000, Duration::from_millis(10));
        assert!(
            handle.shutdown.load(Ordering::SeqCst),
            "shutdown flag must be true before stop"
        );
        handle.stop();
        assert!(
            !handle.shutdown.load(Ordering::SeqCst),
            "shutdown flag must be false after stop"
        );
        // Clean up the thread without consuming the handle.
        drop(handle);
    }

    // ---- join(): blocks until thread exits and returns Ok -------------------

    /// join() with None timeout blocks until the thread finishes and returns Ok.
    #[test]
    fn join_none_timeout_waits_for_thread_and_returns_ok() {
        let mut handle = make_handle("test-5", "join_none", 3, Duration::from_millis(10));
        let result = handle.join(None);
        assert!(
            result.is_ok(),
            "join must return Ok when the thread succeeds: {result:?}"
        );
    }

    /// join() is idempotent: calling it a second time returns Ok immediately.
    #[test]
    fn join_is_idempotent_after_first_call() {
        let mut handle = make_handle("test-6", "idempotent", 1, Duration::from_millis(1));
        handle.join(None).expect("first join must succeed");
        let result = handle.join(None);
        assert!(
            result.is_ok(),
            "second join on an already-joined handle must return Ok: {result:?}"
        );
    }

    /// join() with a timeout that expires while thread is still running returns
    /// Ok without consuming the thread (handle still owns it).
    #[test]
    fn join_with_short_timeout_returns_ok_without_consuming_thread() {
        // Thread runs for 10 events × 50ms each = 500ms total.
        let mut handle = make_handle("test-7", "timeout", 10, Duration::from_millis(50));
        // Join with a very short timeout — thread will still be running.
        let result = handle.join(Some(Duration::from_millis(10)));
        assert!(
            result.is_ok(),
            "join with expired timeout must still return Ok"
        );
        // The JoinHandle must NOT have been consumed — is_running may still be true.
        assert!(
            handle.thread.is_some(),
            "thread must not be consumed when timeout expired before thread finished"
        );
        // Clean up.
        handle.stop();
        handle.join(None).ok();
    }

    // ---- elapsed(): grows over time -----------------------------------------

    /// elapsed() must return a positive Duration immediately after creation.
    #[test]
    fn elapsed_returns_positive_duration() {
        let handle = make_handle("test-8", "elapsed", 1, Duration::from_millis(1));
        let d = handle.elapsed();
        // Even with scheduler jitter this should be at least 0 ns.
        assert!(d >= Duration::ZERO, "elapsed must be non-negative: {d:?}");
    }

    /// elapsed() measured after a sleep must be greater than that sleep duration.
    #[test]
    fn elapsed_grows_monotonically_after_sleep() {
        let mut handle = make_handle("test-9", "monotonic", 100, Duration::from_millis(5));
        let before = handle.elapsed();
        thread::sleep(Duration::from_millis(100));
        let after = handle.elapsed();
        assert!(
            after > before,
            "elapsed must grow over time: before={before:?}, after={after:?}"
        );
        handle.stop();
        handle.join(None).ok();
    }

    // ---- stats_snapshot(): returns the current stats atomically -------------

    /// stats_snapshot() on a fresh handle returns all-zero stats.
    #[test]
    fn stats_snapshot_on_fresh_handle_returns_zeros() {
        let handle = make_handle("test-10", "fresh_stats", 0, Duration::ZERO);
        let snap = handle.stats_snapshot();
        assert_eq!(snap.total_events, 0);
        assert_eq!(snap.bytes_emitted, 0);
        assert_eq!(snap.errors, 0);
    }

    /// After the worker thread emits events, stats_snapshot() reflects the updates.
    #[test]
    fn stats_snapshot_returns_nonzero_total_events_after_running() {
        // 5 events × 10ms each = ~50ms of work.
        let mut handle = make_handle("test-11", "nonzero_stats", 5, Duration::from_millis(10));
        // Wait long enough for all 5 events to fire.
        thread::sleep(Duration::from_millis(200));
        let snap = handle.stats_snapshot();
        assert!(
            snap.total_events > 0,
            "stats_snapshot must reflect emitted events, got total_events={}",
            snap.total_events
        );
        assert!(
            snap.bytes_emitted > 0,
            "stats_snapshot must reflect bytes_emitted > 0, got {}",
            snap.bytes_emitted
        );
        handle.join(None).ok();
    }

    // ---- Contract: ScenarioHandle is Send -----------------------------------

    /// ScenarioHandle must be Send so it can be stored in server state and
    /// moved across tokio await points.
    #[test]
    fn scenario_handle_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ScenarioHandle>();
    }
}
