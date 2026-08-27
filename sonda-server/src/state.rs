//! Shared application state for the HTTP server.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sonda_core::ScenarioHandle;
use tokio::sync::Semaphore;

use crate::gate_registry::GateBusRegistry;
use crate::locks::RecoveringLock;

const SCENARIOS_LOCK: &str = "scenarios";
const REQUEST_COUNTERS_LOCK: &str = "request_counters";
const REQUEST_HISTOGRAMS_LOCK: &str = "request_histograms";

pub type RouteKey = (String, String, u16);
pub type HistogramKey = (String, String);

pub struct HistogramShard {
    pub buckets: [AtomicU64; 11],
    pub plus_inf: AtomicU64,
    pub sum_bits: AtomicU64,
    pub count: AtomicU64,
}

impl HistogramShard {
    pub const BUCKET_BOUNDS: [f64; 11] = [
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ];

    pub fn new() -> Self {
        Self {
            buckets: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            plus_inf: AtomicU64::new(0),
            sum_bits: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, seconds: f64) {
        use std::sync::atomic::Ordering;
        for (i, bound) in Self::BUCKET_BOUNDS.iter().enumerate() {
            if seconds <= *bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.plus_inf.fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        // Atomic sum tracked as f64 bit-pattern via CAS loop.
        let mut current = self.sum_bits.load(Ordering::Relaxed);
        loop {
            let updated = f64::from_bits(current) + seconds;
            match self.sum_bits.compare_exchange_weak(
                current,
                updated.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        use std::sync::atomic::Ordering;
        HistogramSnapshot {
            buckets: self.buckets.each_ref().map(|a| a.load(Ordering::Relaxed)),
            plus_inf: self.plus_inf.load(Ordering::Relaxed),
            sum: f64::from_bits(self.sum_bits.load(Ordering::Relaxed)),
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

impl Default for HistogramShard {
    fn default() -> Self {
        Self::new()
    }
}

pub struct HistogramSnapshot {
    pub buckets: [u64; 11],
    pub plus_inf: u64,
    pub sum: f64,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepPhase {
    NotConfigured,
    InProgress,
    Finished,
    Failed,
}

impl SweepPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            SweepPhase::NotConfigured => "not_configured",
            SweepPhase::InProgress => "in_progress",
            SweepPhase::Finished => "finished",
            SweepPhase::Failed => "failed",
        }
    }

    pub fn is_ready(self) -> bool {
        matches!(self, SweepPhase::NotConfigured | SweepPhase::Finished)
    }

    fn code(self) -> u8 {
        match self {
            SweepPhase::NotConfigured => 0,
            SweepPhase::InProgress => 1,
            SweepPhase::Finished => 2,
            SweepPhase::Failed => 3,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            0 => SweepPhase::NotConfigured,
            1 => SweepPhase::InProgress,
            2 => SweepPhase::Finished,
            _ => SweepPhase::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepSnapshot {
    pub phase: SweepPhase,
    pub started: usize,
    pub expected: usize,
}

/// Progress of the `--autostart` catalog sweep. `expected` is fixed when the catalog
/// is enumerated, before the listener binds, so probes never read a zero denominator.
pub struct SweepStatus {
    phase: AtomicU8,
    started: AtomicUsize,
    expected: usize,
}

impl SweepStatus {
    pub fn not_configured() -> Self {
        Self {
            phase: AtomicU8::new(SweepPhase::NotConfigured.code()),
            started: AtomicUsize::new(0),
            expected: 0,
        }
    }

    pub fn in_progress(expected: usize) -> Self {
        Self {
            phase: AtomicU8::new(SweepPhase::InProgress.code()),
            started: AtomicUsize::new(0),
            expected,
        }
    }

    pub fn record_started(&self) {
        self.started.fetch_add(1, Ordering::Relaxed);
    }

    pub fn finish(&self) {
        self.phase
            .store(SweepPhase::Finished.code(), Ordering::Release);
    }

    pub fn fail(&self) {
        self.phase
            .store(SweepPhase::Failed.code(), Ordering::Release);
    }

    pub fn snapshot(&self) -> SweepSnapshot {
        // Acquire pairs with finish()'s Release: seeing Finished implies the final count.
        let phase = SweepPhase::from_code(self.phase.load(Ordering::Acquire));
        SweepSnapshot {
            phase,
            started: self.started.load(Ordering::Relaxed),
            expected: self.expected,
        }
    }
}

/// Shared application state for the HTTP server.
///
/// Holds a map of running [`ScenarioHandle`]s keyed by scenario ID, the
/// process-wide [`GateBusRegistry`] backing cross-POST `while:` refs, and
/// an optional API key for bearer-token authentication. No scenario lifecycle
/// logic lives here — all launch and stop operations are delegated to
/// sonda-core.
///
/// The state is wrapped in an [`Arc`] by axum and cloned into each handler
/// automatically via the `State` extractor.
#[derive(Clone)]
pub struct AppState {
    /// Map from scenario ID to its lifecycle handle.
    pub scenarios: Arc<RecoveringLock<HashMap<String, ScenarioHandle>>>,
    /// Optional API key for bearer-token authentication on protected routes.
    pub api_key: Option<Arc<String>>,
    /// Optional catalog directory for resolving `pack:` references in posted bodies.
    pub catalog_dir: Option<Arc<PathBuf>>,
    /// Registry of cross-POST `while:` upstream buses.
    pub gate_bus_registry: Arc<GateBusRegistry>,
    /// Permits gating the `--max-scenarios` row cap.
    pub scenario_permits: Arc<Semaphore>,
    /// Process start time for `sonda_server_uptime_seconds`.
    pub started_at: Instant,
    /// Configured tokio worker thread count exposed via `sonda_server_worker_threads`.
    pub worker_threads: usize,
    /// Configured `--max-scenarios` value exposed via `sonda_server_max_scenarios`.
    pub max_scenarios: usize,
    /// Per-`(route, method, status)` request counters.
    pub request_counters: Arc<RecoveringLock<HashMap<RouteKey, AtomicU64>>>,
    /// Per-`(route, method)` duration histograms.
    pub request_histograms: Arc<RecoveringLock<HashMap<HistogramKey, HistogramShard>>>,
    /// Progress of the `--autostart` catalog sweep.
    pub sweep_status: Arc<SweepStatus>,
}

impl AppState {
    /// Create a new, empty application state with no authentication.
    pub fn new() -> Self {
        Self {
            scenarios: Arc::new(RecoveringLock::new(SCENARIOS_LOCK, HashMap::new())),
            api_key: None,
            catalog_dir: None,
            gate_bus_registry: Arc::new(GateBusRegistry::new()),
            scenario_permits: Arc::new(Semaphore::new(Semaphore::MAX_PERMITS)),
            started_at: Instant::now(),
            worker_threads: 1,
            max_scenarios: 0,
            request_counters: Arc::new(RecoveringLock::new(REQUEST_COUNTERS_LOCK, HashMap::new())),
            request_histograms: Arc::new(RecoveringLock::new(
                REQUEST_HISTOGRAMS_LOCK,
                HashMap::new(),
            )),
            sweep_status: Arc::new(SweepStatus::not_configured()),
        }
    }

    /// Create a new, empty application state with an optional API key.
    #[allow(dead_code)]
    pub fn with_api_key(api_key: Option<String>) -> Self {
        Self {
            scenarios: Arc::new(RecoveringLock::new(SCENARIOS_LOCK, HashMap::new())),
            api_key: api_key.map(Arc::new),
            catalog_dir: None,
            gate_bus_registry: Arc::new(GateBusRegistry::new()),
            scenario_permits: Arc::new(Semaphore::new(Semaphore::MAX_PERMITS)),
            started_at: Instant::now(),
            worker_threads: 1,
            max_scenarios: 0,
            request_counters: Arc::new(RecoveringLock::new(REQUEST_COUNTERS_LOCK, HashMap::new())),
            request_histograms: Arc::new(RecoveringLock::new(
                REQUEST_HISTOGRAMS_LOCK,
                HashMap::new(),
            )),
            sweep_status: Arc::new(SweepStatus::not_configured()),
        }
    }

    /// Recovered poisoned-lock acquisitions, per lock, across every lock the server owns.
    pub fn lock_recoveries(&self) -> Vec<(&'static str, u64)> {
        let mut rows = vec![
            (self.scenarios.name(), self.scenarios.recoveries()),
            (
                self.request_counters.name(),
                self.request_counters.recoveries(),
            ),
            (
                self.request_histograms.name(),
                self.request_histograms.recoveries(),
            ),
        ];
        rows.extend(self.gate_bus_registry.lock_recoveries());
        rows.sort_by_key(|(name, _)| *name);
        rows
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A freshly created AppState has an empty scenarios map.
    #[test]
    fn new_state_has_empty_scenarios() {
        let state = AppState::new();
        let scenarios = state.scenarios.read();
        assert!(
            scenarios.is_empty(),
            "new AppState must have an empty scenarios map"
        );
    }

    /// AppState::new sets api_key to None.
    #[test]
    fn new_state_has_no_api_key() {
        let state = AppState::new();
        assert!(
            state.api_key.is_none(),
            "new AppState must have api_key = None"
        );
    }

    /// AppState::default produces the same result as AppState::new.
    #[test]
    fn default_produces_empty_state() {
        let state = AppState::default();
        let scenarios = state.scenarios.read();
        assert!(
            scenarios.is_empty(),
            "default AppState must have an empty scenarios map"
        );
        assert!(
            state.api_key.is_none(),
            "default AppState must have api_key = None"
        );
    }

    /// with_api_key(Some) stores the key.
    #[test]
    fn with_api_key_some_stores_key() {
        let state = AppState::with_api_key(Some("secret".to_string()));
        let key = state.api_key.expect("api_key must be Some");
        assert_eq!(*key, "secret", "api_key must contain the provided value");
    }

    /// with_api_key(None) results in no authentication.
    #[test]
    fn with_api_key_none_disables_auth() {
        let state = AppState::with_api_key(None);
        assert!(
            state.api_key.is_none(),
            "with_api_key(None) must produce api_key = None"
        );
    }

    /// with_api_key produces an empty scenarios map.
    #[test]
    fn with_api_key_has_empty_scenarios() {
        let state = AppState::with_api_key(Some("key".to_string()));
        let scenarios = state.scenarios.read();
        assert!(
            scenarios.is_empty(),
            "with_api_key must produce an empty scenarios map"
        );
    }

    /// Cloning AppState shares the same underlying Arc (not a deep copy).
    #[test]
    fn clone_shares_same_arc() {
        let state1 = AppState::new();
        let state2 = state1.clone();
        // Both point to the same Arc.
        assert!(
            Arc::ptr_eq(&state1.scenarios, &state2.scenarios),
            "cloned AppState must share the same lock"
        );
    }

    /// Cloning AppState with an API key shares the same key Arc.
    #[test]
    fn clone_shares_api_key_arc() {
        let state1 = AppState::with_api_key(Some("secret".to_string()));
        let state2 = state1.clone();
        assert!(
            Arc::ptr_eq(
                state1.api_key.as_ref().unwrap(),
                state2.api_key.as_ref().unwrap()
            ),
            "cloned AppState must share the same api_key Arc"
        );
    }

    /// AppState::new and with_api_key default catalog_dir to None.
    #[test]
    fn constructors_default_catalog_dir_to_none() {
        assert!(AppState::new().catalog_dir.is_none());
        assert!(AppState::with_api_key(None).catalog_dir.is_none());
        assert!(AppState::with_api_key(Some("k".to_string()))
            .catalog_dir
            .is_none());
    }

    /// A catalog dir set on AppState is carried and survives cloning.
    #[test]
    fn catalog_dir_is_carried_and_shared_on_clone() {
        let mut state = AppState::new();
        state.catalog_dir = Some(Arc::new(PathBuf::from("/scenarios")));
        let clone = state.clone();
        assert!(Arc::ptr_eq(
            state.catalog_dir.as_ref().unwrap(),
            clone.catalog_dir.as_ref().unwrap()
        ));
        assert_eq!(
            clone.catalog_dir.as_ref().unwrap().as_path(),
            std::path::Path::new("/scenarios")
        );
    }

    /// AppState is Send + Sync (required for axum State extractor).
    #[test]
    fn app_state_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AppState>();
    }

    /// AppState is Clone (required for axum State extractor).
    #[test]
    fn app_state_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<AppState>();
    }

    #[test]
    fn new_defaults_semaphore_to_max_permits() {
        let state = AppState::new();
        assert_eq!(
            state.scenario_permits.available_permits(),
            Semaphore::MAX_PERMITS
        );
        assert_eq!(state.max_scenarios, 0);
        assert_eq!(state.worker_threads, 1);
    }

    #[test]
    fn started_at_is_recorded_on_construction() {
        let before = Instant::now();
        let state = AppState::new();
        let after = Instant::now();
        assert!(state.started_at >= before);
        assert!(state.started_at <= after);
    }

    #[test]
    fn not_configured_status_is_ready_with_zero_counts() {
        let snap = SweepStatus::not_configured().snapshot();
        assert_eq!(snap.phase, SweepPhase::NotConfigured);
        assert!(snap.phase.is_ready());
        assert_eq!((snap.started, snap.expected), (0, 0));
    }

    #[test]
    fn in_progress_status_carries_the_expected_count_before_anything_starts() {
        let snap = SweepStatus::in_progress(12).snapshot();
        assert_eq!(snap.phase, SweepPhase::InProgress);
        assert!(!snap.phase.is_ready());
        assert_eq!((snap.started, snap.expected), (0, 12));
    }

    #[test]
    fn record_started_counts_up_to_finish() {
        let status = SweepStatus::in_progress(3);
        status.record_started();
        status.record_started();
        status.finish();

        let snap = status.snapshot();
        assert_eq!(snap.phase, SweepPhase::Finished);
        assert!(snap.phase.is_ready());
        assert_eq!((snap.started, snap.expected), (2, 3));
    }

    #[test]
    fn a_failed_sweep_is_not_ready_and_keeps_its_counts() {
        let status = SweepStatus::in_progress(4);
        status.record_started();
        status.fail();

        let snap = status.snapshot();
        assert_eq!(snap.phase, SweepPhase::Failed);
        assert!(!snap.phase.is_ready());
        assert_eq!((snap.started, snap.expected), (1, 4));
    }

    #[test]
    fn phase_labels_are_stable() {
        assert_eq!(SweepPhase::NotConfigured.as_str(), "not_configured");
        assert_eq!(SweepPhase::InProgress.as_str(), "in_progress");
        assert_eq!(SweepPhase::Finished.as_str(), "finished");
        assert_eq!(SweepPhase::Failed.as_str(), "failed");
    }

    #[test]
    fn phase_codes_round_trip() {
        for phase in [
            SweepPhase::NotConfigured,
            SweepPhase::InProgress,
            SweepPhase::Finished,
            SweepPhase::Failed,
        ] {
            assert_eq!(SweepPhase::from_code(phase.code()), phase);
        }
    }

    #[test]
    fn new_state_reports_no_autostart_sweep() {
        assert_eq!(
            AppState::new().sweep_status.snapshot().phase,
            SweepPhase::NotConfigured
        );
    }

    #[test]
    fn clone_shares_the_same_sweep_status() {
        let state = AppState::new();
        let clone = state.clone();
        state.sweep_status.record_started();
        assert_eq!(clone.sweep_status.snapshot().started, 1);
    }

    #[test]
    fn histogram_shard_observe_records_bucket_count_and_sum() {
        let shard = HistogramShard::new();
        shard.observe(0.004);
        shard.observe(0.5);
        let snap = shard.snapshot();
        assert_eq!(snap.count, 2);
        assert_eq!(snap.plus_inf, 2);
        assert!((snap.sum - 0.504).abs() < 1e-9);
        // 0.004 falls in the 0.005 bucket and every higher bucket.
        assert_eq!(snap.buckets[0], 1);
        // 0.5 also falls in 0.5 bucket and above.
        assert_eq!(snap.buckets[6], 2);
    }
}
