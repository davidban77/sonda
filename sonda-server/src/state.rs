//! Shared application state for the HTTP server.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sonda_core::ScenarioHandle;

/// Shared application state for the HTTP server.
///
/// Holds a map of running [`ScenarioHandle`]s keyed by scenario ID and an
/// optional API key for bearer-token authentication. No scenario lifecycle
/// logic lives here — this is only the container. All launch and stop
/// operations are delegated to sonda-core.
///
/// The state is wrapped in an [`Arc`] by axum and cloned into each handler
/// automatically via the `State` extractor.
#[derive(Clone)]
pub struct AppState {
    /// Map from scenario ID to its lifecycle handle.
    pub scenarios: Arc<RwLock<HashMap<String, ScenarioHandle>>>,
    /// Optional API key for bearer-token authentication on protected routes.
    ///
    /// When `None`, all routes are publicly accessible (backwards compatible).
    /// When `Some`, requests to `/scenarios/*` must include a valid
    /// `Authorization: Bearer <key>` header.
    pub api_key: Option<Arc<String>>,
}

impl AppState {
    /// Create a new, empty application state with no authentication.
    pub fn new() -> Self {
        Self {
            scenarios: Arc::new(RwLock::new(HashMap::new())),
            api_key: None,
        }
    }

    /// Create a new, empty application state with an optional API key.
    ///
    /// When `api_key` is `Some`, all `/scenarios/*` endpoints require a
    /// matching `Authorization: Bearer <key>` header. When `None`, auth is
    /// disabled and the server behaves identically to [`AppState::new`].
    pub fn with_api_key(api_key: Option<String>) -> Self {
        Self {
            scenarios: Arc::new(RwLock::new(HashMap::new())),
            api_key: api_key.map(Arc::new),
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
        let scenarios = state.scenarios.read().expect("RwLock must not be poisoned");
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
        let scenarios = state.scenarios.read().expect("RwLock must not be poisoned");
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
            "cloned AppState must share the same Arc<RwLock<...>>"
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
