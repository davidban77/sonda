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
