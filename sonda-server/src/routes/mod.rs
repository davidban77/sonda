//! HTTP route definitions for sonda-server.

pub mod health;

use axum::{routing::get, Router};

use crate::state::AppState;

/// Build the application router with all routes wired up.
///
/// The returned [`Router`] is ready to be handed to the axum server. State is
/// injected via [`axum::extract::State`] in each handler.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .with_state(state)
}
