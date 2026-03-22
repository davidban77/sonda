//! Health check endpoint.

use axum::response::Json;
use serde_json::{json, Value};

/// `GET /health` — returns `{"status": "ok"}` with HTTP 200.
///
/// Used by load balancers and readiness probes to confirm the server is up.
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The health handler returns a JSON body with `{"status": "ok"}`.
    #[tokio::test]
    async fn health_returns_status_ok() {
        let Json(body) = health().await;
        assert_eq!(
            body,
            json!({ "status": "ok" }),
            "health handler must return {{\"status\": \"ok\"}}"
        );
    }

    /// The health response body contains exactly one key, "status".
    #[tokio::test]
    async fn health_response_has_single_status_key() {
        let Json(body) = health().await;
        let obj = body.as_object().expect("body must be a JSON object");
        assert_eq!(obj.len(), 1, "health response must contain exactly one key");
        assert!(
            obj.contains_key("status"),
            "health response must contain key 'status'"
        );
    }

    /// The "status" field value is the string "ok".
    #[tokio::test]
    async fn health_status_value_is_ok_string() {
        let Json(body) = health().await;
        assert_eq!(
            body["status"], "ok",
            "status field must be the string \"ok\""
        );
    }
}
