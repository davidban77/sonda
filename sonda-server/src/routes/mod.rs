//! HTTP route definitions for sonda-server.
//!
//! Routes are split into two sub-routers:
//! - **Public**: `/health` — always accessible, no authentication required.
//! - **Protected**: `/scenarios/*` — guarded by the [`require_api_key`] middleware
//!   when an API key is configured. When no key is configured, these routes are
//!   also publicly accessible (backwards compatible).
//!
//! The `route_layer` approach ensures that only matched routes run through the
//! auth middleware. Unmatched paths get a plain 404 from the router, not a 401.

pub mod health;
pub mod scenarios;

use axum::middleware;
use axum::routing::get;
use axum::Router;

use crate::auth::require_api_key;
use crate::state::AppState;

/// Build the application router with all routes wired up.
///
/// The returned [`Router`] is ready to be handed to the axum server. State is
/// injected via [`axum::extract::State`] in each handler.
///
/// The router is composed of two sub-trees:
/// - A **public** router for endpoints that must always be accessible (health
///   checks, readiness probes).
/// - A **protected** router for scenario management endpoints, guarded by
///   bearer-token authentication when an API key is configured.
pub fn router(state: AppState) -> Router {
    let public = Router::new().route("/health", get(health::health));

    let protected = Router::new()
        .route(
            "/scenarios",
            get(scenarios::list_scenarios).post(scenarios::post_scenario),
        )
        .route(
            "/scenarios/{id}",
            get(scenarios::get_scenario).delete(scenarios::delete_scenario),
        )
        .route("/scenarios/{id}/stats", get(scenarios::get_scenario_stats))
        .route(
            "/scenarios/{id}/metrics",
            get(scenarios::get_scenario_metrics),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_key,
        ));

    public.merge(protected).with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode};
    use tower::ServiceExt;

    /// Helper: build the router with empty state (no auth) for test use.
    fn test_router() -> Router {
        router(AppState::new())
    }

    /// Helper: build the router with API key authentication enabled.
    fn test_router_with_key(key: &str) -> Router {
        router(AppState::with_api_key(Some(key.to_string())))
    }

    /// GET /health returns HTTP 200.
    #[tokio::test]
    async fn get_health_returns_200() {
        let app = test_router();
        let request = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET /health must return 200 OK"
        );
    }

    /// GET /health returns JSON body with {"status": "ok"}.
    #[tokio::test]
    async fn get_health_returns_status_ok_json() {
        let app = test_router();
        let request = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body must be valid JSON");

        assert_eq!(
            body,
            serde_json::json!({ "status": "ok" }),
            "GET /health must return {{\"status\": \"ok\"}}"
        );
    }

    /// GET /health sets Content-Type to application/json.
    #[tokio::test]
    async fn get_health_sets_json_content_type() {
        let app = test_router();
        let request = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let ct = response
            .headers()
            .get("content-type")
            .expect("response must have Content-Type header")
            .to_str()
            .unwrap();

        assert!(
            ct.contains("application/json"),
            "Content-Type must be application/json, got: {ct}"
        );
    }

    /// An unknown route returns HTTP 404.
    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = test_router();
        let request = Request::builder()
            .uri("/nonexistent")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "unknown route must return 404"
        );
    }

    /// POST /health returns 405 Method Not Allowed (only GET is registered).
    #[tokio::test]
    async fn post_health_returns_405() {
        let app = test_router();
        let request = Request::builder()
            .method("POST")
            .uri("/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "POST /health must return 405 Method Not Allowed"
        );
    }

    /// A deeply nested unknown path returns 404.
    #[tokio::test]
    async fn deeply_nested_unknown_path_returns_404() {
        let app = test_router();
        let request = Request::builder()
            .uri("/a/b/c/d/e/f")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "deeply nested unknown path must return 404"
        );
    }

    // ---- Authentication integration tests -----------------------------------

    /// When auth is enabled, GET /scenarios without Authorization returns 401.
    #[tokio::test]
    async fn auth_enabled_no_header_returns_401() {
        let app = test_router_with_key("test-secret");
        let request = Request::builder()
            .uri("/scenarios")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "GET /scenarios without auth must return 401"
        );

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["error"], "unauthorized");
    }

    /// When auth is enabled, GET /scenarios with wrong key returns 401.
    #[tokio::test]
    async fn auth_enabled_wrong_key_returns_401() {
        let app = test_router_with_key("correct-key");
        let request = Request::builder()
            .uri("/scenarios")
            .header("authorization", "Bearer wrong-key")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "GET /scenarios with wrong key must return 401"
        );

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["detail"], "invalid API key");
    }

    /// When auth is enabled, GET /scenarios with correct key passes through.
    #[tokio::test]
    async fn auth_enabled_correct_key_returns_200() {
        let app = test_router_with_key("my-secret");
        let request = Request::builder()
            .uri("/scenarios")
            .header("authorization", "Bearer my-secret")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET /scenarios with correct key must return 200"
        );
    }

    /// When auth is enabled, GET /health is still public (no auth required).
    #[tokio::test]
    async fn auth_enabled_health_remains_public() {
        let app = test_router_with_key("secret");
        let request = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET /health must return 200 even when auth is enabled"
        );
    }

    /// When no API key is configured, GET /scenarios is publicly accessible.
    #[tokio::test]
    async fn no_auth_scenarios_accessible() {
        let app = test_router();
        let request = Request::builder()
            .uri("/scenarios")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET /scenarios must return 200 when no auth is configured"
        );
    }

    /// When auth is enabled, unknown routes still return 404, not 401.
    #[tokio::test]
    async fn auth_enabled_unknown_route_returns_404() {
        let app = test_router_with_key("secret");
        let request = Request::builder()
            .uri("/nonexistent")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "unknown route must return 404 even when auth is enabled"
        );
    }

    /// When auth is enabled, DELETE /scenarios/{id} without auth returns 401.
    #[tokio::test]
    async fn auth_enabled_delete_scenario_returns_401() {
        let app = test_router_with_key("secret");
        let request = Request::builder()
            .method("DELETE")
            .uri("/scenarios/some-id")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "DELETE /scenarios/{{id}} without auth must return 401"
        );
    }

    /// When auth is enabled, POST /scenarios without auth returns 401.
    #[tokio::test]
    async fn auth_enabled_post_scenario_returns_401() {
        let app = test_router_with_key("secret");
        let request = Request::builder()
            .method("POST")
            .uri("/scenarios")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "POST /scenarios without auth must return 401"
        );
    }
}
