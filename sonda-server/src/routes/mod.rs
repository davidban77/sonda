//! HTTP route definitions for sonda-server.

pub mod health;
pub mod scenarios;

use axum::{routing::get, Router};

use crate::state::AppState;

/// Build the application router with all routes wired up.
///
/// The returned [`Router`] is ready to be handed to the axum server. State is
/// injected via [`axum::extract::State`] in each handler.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/scenarios", get(scenarios::list_scenarios))
        .route("/scenarios/:id", get(scenarios::get_scenario))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode};
    use tower::ServiceExt;

    /// Helper: build the router with empty state for test use.
    fn test_router() -> Router {
        router(AppState::new())
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
}
