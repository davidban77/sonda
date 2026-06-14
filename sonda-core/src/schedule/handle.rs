//! Lifecycle handle for a running scenario.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::PromMeta;
use crate::schedule::stats::ScenarioStats;
use crate::SondaError;

/// Returned by [`ScenarioHandle::join_timeout`] when the task did not finish
/// within the requested deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinTimeout;

impl std::fmt::Display for JoinTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("scenario task did not exit within join_timeout")
    }
}

impl std::error::Error for JoinTimeout {}

/// A running scenario's lifecycle handle.
#[non_exhaustive]
pub struct ScenarioHandle {
    pub id: String,
    pub name: String,
    pub scenario_name: Option<String>,
    /// Per-handle cancellation token. `stop()` on one handle never affects another.
    pub cancel: CancellationToken,
    /// The tokio task running the scenario. `None` after [`ScenarioHandle::join`] consumes it.
    pub task: Option<JoinHandle<Result<(), SondaError>>>,
    pub started_at: Instant,
    pub stats: Arc<RwLock<ScenarioStats>>,
    pub target_rate: f64,
    /// Lock-free liveness flag flipped to `false` when the runner exits.
    pub alive: Arc<AtomicBool>,
    pub labels: Arc<HashMap<String, String>>,
    pub prometheus_meta: Option<Arc<PromMeta>>,
    pub cleaned_up: Arc<AtomicBool>,
}

impl ScenarioHandle {
    /// Construct a `ScenarioHandle` from its raw parts.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        name: String,
        scenario_name: Option<String>,
        cancel: CancellationToken,
        task: Option<JoinHandle<Result<(), SondaError>>>,
        started_at: Instant,
        stats: Arc<RwLock<ScenarioStats>>,
        target_rate: f64,
        alive: Arc<AtomicBool>,
        labels: Arc<HashMap<String, String>>,
        prometheus_meta: Option<Arc<PromMeta>>,
        cleaned_up: Arc<AtomicBool>,
    ) -> Self {
        Self {
            id,
            name,
            scenario_name,
            cancel,
            task,
            started_at,
            stats,
            target_rate,
            alive,
            labels,
            prometheus_meta,
            cleaned_up,
        }
    }

    /// Signal this scenario to stop. Affects only this scenario.
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    /// Check whether the scenario task is still running.
    pub fn is_running(&self) -> bool {
        self.task
            .as_ref()
            .map(|t| !t.is_finished())
            .unwrap_or(false)
    }

    /// Lock-free check that the runner has not yet exited.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Await the scenario task to completion within the optional timeout.
    /// On timeout the handle still owns the task and the caller can retry.
    pub async fn join_async(&mut self, timeout: Option<Duration>) -> Result<(), SondaError> {
        if self.task.is_none() {
            return Ok(());
        }

        if let Some(t) = timeout {
            let deadline = Instant::now() + t;
            loop {
                if self.task.as_ref().map(|t| t.is_finished()).unwrap_or(true) {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok(());
                }
                let remaining = deadline.saturating_duration_since(now);
                tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
            }
        }

        let task = self.task.take().expect("task present at this point");
        match task.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Ok(())
                } else {
                    Err(SondaError::Runtime(crate::RuntimeError::ThreadPanicked))
                }
            }
        }
    }

    /// Synchronously join the scenario task and return its result.
    ///
    /// Returns `Err(RuntimeError::TaskAborted)` if called from an ambient
    /// current_thread runtime — blocking there would deadlock; use [`join_async`](Self::join_async).
    pub fn join(&mut self, timeout: Option<Duration>) -> Result<(), SondaError> {
        if self.task.is_none() {
            return Ok(());
        }

        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if self.task.as_ref().map(|t| t.is_finished()).unwrap_or(true) {
                break;
            }
            match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Ok(());
                    }
                    let remaining = d.saturating_duration_since(now);
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }

        let task = self.task.take().expect("checked above: task is Some");
        let result = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                if matches!(
                    handle.runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::MultiThread
                ) {
                    tokio::task::block_in_place(|| handle.block_on(task))
                } else {
                    task.abort();
                    return Err(SondaError::Runtime(crate::RuntimeError::TaskAborted {
                        reason: "join() called from a current_thread tokio runtime; \
                                 use join_async() instead",
                    }));
                }
            }
            Err(_) => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| SondaError::Runtime(crate::RuntimeError::SpawnFailed(e)))?;
                rt.block_on(task)
            }
        };
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Ok(())
                } else {
                    Err(SondaError::Runtime(crate::RuntimeError::ThreadPanicked))
                }
            }
        }
    }

    /// Best-effort timed join: poll the underlying task until it finishes
    /// or `timeout` elapses.
    pub fn join_timeout(&mut self, timeout: Duration) -> Result<(), JoinTimeout> {
        if self.task.is_none() {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        let poll_interval = Duration::from_millis(10);
        loop {
            if self.task.as_ref().map(|t| t.is_finished()).unwrap_or(true) {
                if let Some(task) = self.task.take() {
                    task.abort();
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
    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Read the latest stats snapshot.
    pub fn stats_snapshot(&self) -> ScenarioStats {
        match self.stats.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Snapshot the current value of each series the scenario is emitting,
    /// sorted by `(name, labels)` for deterministic output.
    pub fn recent_metrics_snapshot(&self) -> Vec<crate::model::metric::MetricEvent> {
        match self.stats.read() {
            Ok(guard) => guard.current_values_snapshot(),
            Err(poisoned) => poisoned.into_inner().current_values_snapshot(),
        }
    }
}

impl Drop for ScenarioHandle {
    fn drop(&mut self) {
        if self.cleaned_up.load(Ordering::SeqCst) {
            return;
        }
        if self.scenario_name.is_none() {
            return;
        }
        eprintln!(
            "sonda: scenario '{}' dropped without unregistering from the gate bus registry",
            self.id
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, RwLock};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::schedule::stats::ScenarioStats;
    use crate::SondaError;

    fn make_handle(id: &str, name: &str, events: u64, interval: Duration) -> ScenarioHandle {
        let cancel = CancellationToken::new();
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let cancel_for_task = cancel.clone();
        let stats_task = Arc::clone(&stats);
        let alive = Arc::new(AtomicBool::new(true));
        let alive_for_task = Arc::clone(&alive);

        let task = tokio::task::spawn(async move {
            for _ in 0..events {
                if cancel_for_task.is_cancelled() {
                    break;
                }
                tokio::time::sleep(interval).await;
                if let Ok(mut st) = stats_task.write() {
                    st.total_events += 1;
                    st.bytes_emitted += 64;
                }
            }
            alive_for_task.store(false, Ordering::SeqCst);
            Ok::<_, SondaError>(())
        });

        ScenarioHandle::new(
            id.to_string(),
            name.to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            100.0,
            alive,
            Arc::new(HashMap::new()),
            Some(Arc::new(PromMeta::new(
                crate::config::PromMetricType::Gauge,
                None,
            ))),
            Arc::new(AtomicBool::new(true)),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn is_running_returns_true_for_live_task() {
        let mut handle = make_handle("test-1", "live", 50, Duration::from_millis(10));
        assert!(handle.is_running());
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn is_running_returns_false_after_stop_and_join() {
        let mut handle = make_handle("test-2", "stopped", 1000, Duration::from_millis(5));
        handle.stop();
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("join must succeed");
        assert!(!handle.is_running());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn is_running_returns_false_when_task_is_none() {
        let mut handle = make_handle("test-3", "none", 1, Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.task = None;
        assert!(!handle.is_running());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_cancels_token() {
        let handle = make_handle("test-4", "stop_flag", 1000, Duration::from_millis(10));
        assert!(!handle.cancel.is_cancelled());
        handle.stop();
        assert!(handle.cancel.is_cancelled());
        drop(handle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_none_timeout_waits_for_task_and_returns_ok() {
        let mut handle = make_handle("test-5", "join_none", 3, Duration::from_millis(10));
        let result = handle.join(None);
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_is_idempotent_after_first_call() {
        let mut handle = make_handle("test-6", "idempotent", 1, Duration::from_millis(1));
        handle.join(None).expect("first join must succeed");
        let result = handle.join(None);
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_with_short_timeout_returns_ok_without_consuming_task() {
        let mut handle = make_handle("test-7", "timeout", 10, Duration::from_millis(50));
        let result = handle.join(Some(Duration::from_millis(10)));
        assert!(result.is_ok());
        assert!(handle.task.is_some());
        handle.stop();
        handle.join(None).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn elapsed_returns_positive_duration() {
        let handle = make_handle("test-8", "elapsed", 1, Duration::from_millis(1));
        let d = handle.elapsed();
        assert!(d >= Duration::ZERO);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn elapsed_grows_monotonically_after_sleep() {
        let mut handle = make_handle("test-9", "monotonic", 100, Duration::from_millis(5));
        let before = handle.elapsed();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = handle.elapsed();
        assert!(after > before);
        handle.stop();
        handle.join(None).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stats_snapshot_on_fresh_handle_returns_zeros() {
        let handle = make_handle("test-10", "fresh_stats", 0, Duration::ZERO);
        let snap = handle.stats_snapshot();
        assert_eq!(snap.total_events, 0);
        assert_eq!(snap.bytes_emitted, 0);
        assert_eq!(snap.errors, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stats_snapshot_returns_nonzero_total_events_after_running() {
        let mut handle = make_handle("test-11", "nonzero_stats", 5, Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let snap = handle.stats_snapshot();
        assert!(snap.total_events > 0);
        assert!(snap.bytes_emitted > 0);
        handle.join(None).ok();
    }

    fn make_metric_event(name: &str, value: f64) -> crate::model::metric::MetricEvent {
        crate::model::metric::MetricEvent::new(
            name.to_string(),
            value,
            crate::model::metric::Labels::default(),
        )
        .expect("test metric name must be valid")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_metrics_snapshot_on_fresh_handle_returns_empty() {
        let handle = make_handle("test-rm-1", "fresh", 0, Duration::ZERO);
        assert!(handle.recent_metrics_snapshot().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_metrics_snapshot_returns_current_values_not_history() {
        let handle = make_handle("test-rm-2", "current", 0, Duration::ZERO);
        {
            let mut stats = handle.stats.write().expect("lock must not be poisoned");
            stats.push_metric(make_metric_event("up", 1.0));
            stats.push_metric(make_metric_event("up", 2.0));
            stats.push_metric(make_metric_event("up", 3.0));
        }
        let events = handle.recent_metrics_snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].value, 3.0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_metrics_snapshot_is_idempotent() {
        let handle = make_handle("test-rm-3", "idempotent", 0, Duration::ZERO);
        {
            let mut stats = handle.stats.write().expect("lock must not be poisoned");
            stats.push_metric(make_metric_event("up", 42.0));
        }
        let first = handle.recent_metrics_snapshot();
        let second = handle.recent_metrics_snapshot();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].value, second[0].value);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stats_snapshot_recovers_from_poisoned_lock() {
        let cancel = CancellationToken::new();
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        {
            let mut guard = stats.write().expect("lock must not be poisoned");
            guard.total_events = 42;
        }

        let stats_clone = Arc::clone(&stats);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = stats_clone.write().expect("lock must not be poisoned");
            panic!("intentional panic to poison lock");
        }));
        assert!(result.is_err());
        assert!(stats.read().is_err());

        let task = tokio::task::spawn(async { Ok::<_, SondaError>(()) });

        let handle = ScenarioHandle::new(
            "test-poisoned".to_string(),
            "poisoned".to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            10.0,
            Arc::new(AtomicBool::new(true)),
            Arc::new(HashMap::new()),
            Some(Arc::new(PromMeta::new(
                crate::config::PromMetricType::Gauge,
                None,
            ))),
            Arc::new(AtomicBool::new(true)),
        );

        let snap = handle.stats_snapshot();
        assert_eq!(snap.total_events, 42);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_metrics_snapshot_recovers_from_poisoned_lock() {
        let cancel = CancellationToken::new();
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        {
            let mut guard = stats.write().expect("lock must not be poisoned");
            guard.push_metric(make_metric_event("up", 99.0));
        }

        let stats_clone = Arc::clone(&stats);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = stats_clone.write().expect("lock must not be poisoned");
            panic!("intentional panic to poison lock");
        }));
        assert!(result.is_err());

        let task = tokio::task::spawn(async { Ok::<_, SondaError>(()) });

        let handle = ScenarioHandle::new(
            "test-poisoned-m".to_string(),
            "poisoned_metrics".to_string(),
            None,
            cancel,
            Some(task),
            Instant::now(),
            stats,
            10.0,
            Arc::new(AtomicBool::new(true)),
            Arc::new(HashMap::new()),
            Some(Arc::new(PromMeta::new(
                crate::config::PromMetricType::Gauge,
                None,
            ))),
            Arc::new(AtomicBool::new(true)),
        );

        let events = handle.recent_metrics_snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].value, 99.0);
    }

    #[test]
    fn scenario_handle_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ScenarioHandle>();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_timeout_returns_ok_when_handle_exits_within_window() {
        let mut handle = make_handle("jt-1", "fast", 1, Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let result = handle.join_timeout(Duration::from_millis(500));
        assert!(result.is_ok(), "{result:?}");
        assert!(handle.task.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_timeout_returns_err_when_task_still_running() {
        let mut handle = make_handle("jt-2", "slow", 100, Duration::from_millis(50));
        let result = handle.join_timeout(Duration::from_millis(20));
        assert!(result.is_err());
        assert!(handle.task.is_some());
        handle.stop();
        handle.join(Some(Duration::from_secs(2))).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_timeout_is_idempotent_when_task_already_consumed() {
        let mut handle = make_handle("jt-3", "consumed", 1, Duration::from_millis(1));
        handle.join(None).expect("first join must succeed");
        let result = handle.join_timeout(Duration::from_millis(50));
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn join_from_current_thread_runtime_returns_task_aborted_err() {
        let mut handle = make_handle("jt-ct", "current_thread", 1, Duration::from_millis(1));
        // Let the task finish before the sync join polls it — `std::thread::sleep`
        // inside join would otherwise starve the only current_thread worker.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let result = handle.join(None);
        assert!(
            matches!(
                result,
                Err(SondaError::Runtime(crate::RuntimeError::TaskAborted { .. }))
            ),
            "join from current_thread runtime must return TaskAborted, got: {result:?}"
        );
    }
}
