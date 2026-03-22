//! Health check endpoint.

use axum::response::Json;
use serde_json::{json, Value};

/// `GET /health` — returns `{"status": "ok"}` with HTTP 200.
///
/// Used by load balancers and readiness probes to confirm the server is up.
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
