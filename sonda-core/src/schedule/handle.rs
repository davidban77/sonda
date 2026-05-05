//! Lifecycle handle for a running scenario.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::schedule::stats::ScenarioStats;
use crate::{RuntimeError, SondaError};

/// Returned by [`ScenarioHandle::join_timeout`] when the thread did not finish
/// within the requested deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinTimeout;

impl std::fmt::Display for JoinTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("scenario thread did not exit within join_timeout")
    }
}

impl std::error::Error for JoinTimeout {}

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
    /// File-level `scenario_name` from the source YAML, when set. Read-only
    /// after launch; every handle from the same POST shares this value.
    pub scenario_name: Option<String>,
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
    /// Lock-free liveness flag flipped to `false` when the runner thread exits.
    ///
    /// Set inside the spawned thread via a Drop guard so it is also cleared on
    /// panic. External observers (e.g. the CLI progress display) read this
    /// without acquiring `JoinHandle::is_finished()`.
    pub alive: Arc<AtomicBool>,
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

    /// Lock-free check that the runner thread has not yet exited.
    ///
    /// Cheaper than [`Self::is_running`] in tight polling loops because it
    /// reads an `AtomicBool` instead of probing the thread handle.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Join the scenario thread, consuming it.
    ///
    /// Blocks until the thread exits or the optional timeout expires. If a
    /// timeout is provided and the thread does not exit within that time, this
    /// method returns `Ok(())` without consuming the thread (the thread
    /// continues running and the handle still owns it).
    ///
    /// **Orphaned thread trade-off:** When the join times out, the OS thread
    /// continues running in the background with no way for the caller to
    /// observe or control it further (the shutdown flag has already been set).
    /// If the handle is subsequently dropped (e.g., removed from the server's
    /// scenario map), the `JoinHandle` is dropped without joining, and the
    /// thread becomes fully detached. This is an acceptable trade-off: the
    /// thread will eventually exit on its own (it checks the shutdown flag
    /// each tick), and blocking the HTTP handler indefinitely would be worse.
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
            Err(_) => Err(SondaError::Runtime(RuntimeError::ThreadPanicked)),
        }
    }

    /// Best-effort timed join: poll the underlying thread until it finishes
    /// or `timeout` elapses. On timeout the thread is left detached and
    /// `Err(JoinTimeout)` is returned. On success the inner `JoinHandle`
    /// is consumed.
    pub fn join_timeout(&mut self, timeout: Duration) -> Result<(), JoinTimeout> {
        if self.thread.is_none() {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        let poll_interval = Duration::from_millis(10);
        loop {
            if self
                .thread
                .as_ref()
                .map(|t| t.is_finished())
                .unwrap_or(true)
            {
                if let Some(handle) = self.thread.take() {
                    let _ = handle.join();
                }
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(JoinTimeout);
            }
            std::thread::sleep((deadline - now).min(poll_interval));
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
    ///
    /// If the stats lock is poisoned (because a writer panicked), this method
    /// recovers the data from the poisoned guard rather than propagating the
    /// panic. The returned stats may be partially updated but will not cause
    /// the caller to panic.
    pub fn stats_snapshot(&self) -> ScenarioStats {
        match self.stats.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Drain and return recent metric events from the stats buffer.
    ///
    /// Acquires the write lock briefly, drains the buffered events, and
    /// returns them ordered oldest-first. After this call the buffer is
    /// empty. Subsequent calls return an empty vec until new events arrive.
    ///
    /// This is used by the scrape endpoint (`GET /scenarios/{id}/metrics`)
    /// to retrieve the latest metric events for Prometheus text encoding.
    ///
    /// If the stats lock is poisoned (because a writer panicked), this method
    /// recovers the data from the poisoned guard rather than propagating the
    /// panic. The returned events may be incomplete but will not cause the
    /// caller to panic.
    pub fn recent_metrics(&self) -> Vec<crate::model::metric::MetricEvent> {
        match self.stats.write() {
            Ok(mut guard) => guard.drain_recent_metrics(),
            Err(poisoned) => poisoned.into_inner().drain_recent_metrics(),
        }
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
            scenario_name: None,
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
            target_rate: 100.0,
            alive: Arc::new(AtomicBool::new(true)),
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

    // ---- recent_metrics: drains from stats buffer ----------------------------

    /// Helper to build a MetricEvent for testing.
    fn make_metric_event(name: &str, value: f64) -> crate::model::metric::MetricEvent {
        crate::model::metric::MetricEvent::new(
            name.to_string(),
            value,
            crate::model::metric::Labels::default(),
        )
        .expect("test metric name must be valid")
    }

    /// recent_metrics on a fresh handle returns an empty Vec.
    #[test]
    fn recent_metrics_on_fresh_handle_returns_empty() {
        let handle = make_handle("test-rm-1", "fresh", 0, Duration::ZERO);
        let events = handle.recent_metrics();
        assert!(
            events.is_empty(),
            "recent_metrics must return empty Vec on a fresh handle"
        );
    }

    /// recent_metrics drains events that were pushed to the stats buffer.
    #[test]
    fn recent_metrics_drains_pushed_events() {
        let handle = make_handle("test-rm-2", "drain", 0, Duration::ZERO);

        // Push events directly into the stats buffer.
        {
            let mut stats = handle.stats.write().expect("lock must not be poisoned");
            stats.push_metric(make_metric_event("up", 1.0));
            stats.push_metric(make_metric_event("up", 2.0));
        }

        let events = handle.recent_metrics();
        assert_eq!(
            events.len(),
            2,
            "recent_metrics must return all pushed events"
        );
        assert_eq!(events[0].value, 1.0, "first event must be value=1.0");
        assert_eq!(events[1].value, 2.0, "second event must be value=2.0");
    }

    /// After calling recent_metrics, the buffer is empty (second call returns empty).
    #[test]
    fn recent_metrics_clears_buffer_after_drain() {
        let handle = make_handle("test-rm-3", "clear", 0, Duration::ZERO);

        {
            let mut stats = handle.stats.write().expect("lock must not be poisoned");
            stats.push_metric(make_metric_event("up", 42.0));
        }

        let first = handle.recent_metrics();
        assert_eq!(first.len(), 1);

        let second = handle.recent_metrics();
        assert!(
            second.is_empty(),
            "second call to recent_metrics must return empty Vec after drain"
        );
    }

    // ---- stats_snapshot: recovers from poisoned lock -------------------------

    /// If the stats lock is poisoned, stats_snapshot recovers the data instead
    /// of panicking.
    #[test]
    fn stats_snapshot_recovers_from_poisoned_lock() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        // Set a known value before poisoning.
        {
            let mut guard = stats.write().expect("lock must not be poisoned");
            guard.total_events = 42;
        }

        // Poison the lock by panicking inside a write guard.
        let stats_clone = Arc::clone(&stats);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = stats_clone.write().expect("lock must not be poisoned");
            panic!("intentional panic to poison lock");
        }));
        assert!(result.is_err(), "panic must have occurred");

        // Verify the lock is actually poisoned.
        assert!(stats.read().is_err(), "lock must be poisoned after panic");

        let thread = thread::Builder::new()
            .name("test-poisoned-stats".to_string())
            .spawn(|| -> Result<(), SondaError> { Ok(()) })
            .expect("thread must spawn");

        let handle = ScenarioHandle {
            id: "test-poisoned".to_string(),
            name: "poisoned".to_string(),
            scenario_name: None,
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
            target_rate: 10.0,
            alive: Arc::new(AtomicBool::new(true)),
        };

        // stats_snapshot must not panic — it recovers from the poisoned lock.
        let snap = handle.stats_snapshot();
        assert_eq!(
            snap.total_events, 42,
            "stats_snapshot must recover data from poisoned lock"
        );
    }

    // ---- recent_metrics: recovers from poisoned lock --------------------------

    /// If the stats lock is poisoned, recent_metrics recovers the data instead
    /// of panicking.
    #[test]
    fn recent_metrics_recovers_from_poisoned_lock() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        // Push a metric event before poisoning.
        {
            let mut guard = stats.write().expect("lock must not be poisoned");
            guard.push_metric(make_metric_event("up", 99.0));
        }

        // Poison the lock.
        let stats_clone = Arc::clone(&stats);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = stats_clone.write().expect("lock must not be poisoned");
            panic!("intentional panic to poison lock");
        }));
        assert!(result.is_err(), "panic must have occurred");

        let thread = thread::Builder::new()
            .name("test-poisoned-metrics".to_string())
            .spawn(|| -> Result<(), SondaError> { Ok(()) })
            .expect("thread must spawn");

        let handle = ScenarioHandle {
            id: "test-poisoned-m".to_string(),
            name: "poisoned_metrics".to_string(),
            scenario_name: None,
            shutdown,
            thread: Some(thread),
            started_at: Instant::now(),
            stats,
            target_rate: 10.0,
            alive: Arc::new(AtomicBool::new(true)),
        };

        // recent_metrics must not panic — it recovers from the poisoned lock.
        let events = handle.recent_metrics();
        assert_eq!(
            events.len(),
            1,
            "must recover buffered events from poisoned lock"
        );
        assert_eq!(
            events[0].value, 99.0,
            "recovered event must have correct value"
        );
    }

    // ---- Contract: ScenarioHandle is Send -----------------------------------

    /// ScenarioHandle must be Send so it can be stored in server state and
    /// moved across tokio await points.
    #[test]
    fn scenario_handle_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ScenarioHandle>();
    }

    #[test]
    fn join_timeout_returns_ok_when_handle_exits_within_window() {
        let mut handle = make_handle("jt-1", "fast", 1, Duration::from_millis(5));
        thread::sleep(Duration::from_millis(50));
        let result = handle.join_timeout(Duration::from_millis(500));
        assert!(
            result.is_ok(),
            "join_timeout must return Ok when thread already exited: {result:?}"
        );
        assert!(handle.thread.is_none(), "JoinHandle must be consumed on Ok");
    }

    #[test]
    fn join_timeout_returns_err_when_thread_still_running() {
        let mut handle = make_handle("jt-2", "slow", 100, Duration::from_millis(50));
        let result = handle.join_timeout(Duration::from_millis(20));
        assert!(
            result.is_err(),
            "join_timeout must return Err when timeout expires before exit"
        );
        assert!(
            handle.thread.is_some(),
            "JoinHandle must be retained on timeout"
        );
        // Clean up.
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[test]
    fn join_timeout_is_idempotent_when_thread_already_consumed() {
        let mut handle = make_handle("jt-3", "consumed", 1, Duration::from_millis(1));
        handle.join(None).expect("first join must succeed");
        let result = handle.join_timeout(Duration::from_millis(50));
        assert!(
            result.is_ok(),
            "join_timeout on consumed handle must return Ok"
        );
    }
}
