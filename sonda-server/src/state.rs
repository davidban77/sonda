//! Shared application state for the HTTP server.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sonda_core::ScenarioHandle;

/// Shared application state for the HTTP server.
///
/// Holds a map of running [`ScenarioHandle`]s keyed by scenario ID. No
/// scenario lifecycle logic lives here — this is only the container. All
/// launch and stop operations are delegated to sonda-core.
///
/// The state is wrapped in an [`Arc`] by axum and cloned into each handler
/// automatically via the `State` extractor.
#[derive(Clone)]
pub struct AppState {
    /// Map from scenario ID to its lifecycle handle.
    pub scenarios: Arc<RwLock<HashMap<String, ScenarioHandle>>>,
}

impl AppState {
    /// Create a new, empty application state.
    pub fn new() -> Self {
        Self {
            scenarios: Arc::new(RwLock::new(HashMap::new())),
        }
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
        let scenarios = state.scenarios.read().expect("RwLock must not be poisoned");
        assert!(
            scenarios.is_empty(),
            "new AppState must have an empty scenarios map"
        );
    }

    /// AppState::default produces the same result as AppState::new.
    #[test]
    fn default_produces_empty_state() {
        let state = AppState::default();
        let scenarios = state.scenarios.read().expect("RwLock must not be poisoned");
        assert!(
            scenarios.is_empty(),
            "default AppState must have an empty scenarios map"
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
            "cloned AppState must share the same Arc<RwLock<...>>"
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
}
