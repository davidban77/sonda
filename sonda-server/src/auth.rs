//! API key authentication middleware.
//!
//! Provides opt-in bearer-token authentication for protected routes. When an
//! API key is configured via [`AppState::with_api_key`], requests to protected
//! routes must include an `Authorization: Bearer <key>` header. The comparison
//! is performed in constant time using [`subtle::ConstantTimeEq`] to prevent
//! timing side-channels.
//!
//! When no API key is configured, the middleware passes all requests through
//! unchanged, preserving full backwards compatibility.

use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::state::AppState;

/// Build a JSON 401 Unauthorized response with the given detail message.
///
/// Response body format:
/// ```json
/// {"error": "unauthorized", "detail": "<detail>"}
/// ```
pub fn unauthorized(detail: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "unauthorized",
            "detail": detail,
        })),
    )
        .into_response()
}

/// Extract a bearer token from the `Authorization` header.
///
/// Returns `Some(token)` if the header is present and has the form
/// `Bearer <token>` (case-insensitive scheme). Returns `None` if the
/// header is missing, uses a different scheme, or has no token value.
pub fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let mut parts = value.splitn(2, ' ');
    let scheme = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = parts.next()?;
    if token.is_empty() {
        return None;
    }
    Some(token)
}

/// Axum middleware that enforces bearer-token authentication on protected routes.
///
/// This middleware is intended to be applied via
/// [`axum::middleware::from_fn_with_state`] on the protected router sub-tree.
///
/// Behaviour:
/// - If `state.api_key` is `None`, the request passes through unconditionally.
/// - If `state.api_key` is `Some(key)`, the request must carry an
///   `Authorization: Bearer <key>` header with a matching token. Comparison
///   uses constant-time equality to prevent timing attacks.
///
/// # Note on length leakage
///
/// `ConstantTimeEq` compares byte-for-byte in constant time only when both
/// slices have the same length. A length mismatch returns early, which could
/// theoretically leak the key length. For API key use cases (typically 32-64
/// random characters), this is a negligible risk and standard practice.
pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let expected = match &state.api_key {
        Some(key) => key,
        None => return next.run(request).await,
    };

    let provided = match extract_bearer_token(request.headers()) {
        Some(token) => token,
        None => return unauthorized("missing or malformed Authorization header"),
    };

    // Constant-time comparison to prevent timing side-channels.
    if provided.as_bytes().ct_eq(expected.as_bytes()).into() {
        next.run(request).await
    } else {
        unauthorized("invalid API key")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use http_body_util::BodyExt;

    // ---- unauthorized() tests -----------------------------------------------

    /// unauthorized() returns HTTP 401.
    #[tokio::test]
    async fn unauthorized_returns_401() {
        let resp = unauthorized("test detail");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// unauthorized() returns the expected JSON body shape.
    #[tokio::test]
    async fn unauthorized_returns_json_body() {
        let resp = unauthorized("bad token");
        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body must be valid JSON");
        assert_eq!(body["error"], "unauthorized");
        assert_eq!(body["detail"], "bad token");
    }

    /// unauthorized() includes the detail message verbatim.
    #[tokio::test]
    async fn unauthorized_preserves_detail() {
        let resp = unauthorized("custom message");
        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(
            body["detail"], "custom message",
            "detail field must match the provided message"
        );
    }

    // ---- extract_bearer_token() tests ---------------------------------------

    /// Valid Bearer header extracts the token.
    #[test]
    fn extract_valid_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer my-secret"),
        );
        assert_eq!(extract_bearer_token(&headers), Some("my-secret"));
    }

    /// Bearer scheme is case-insensitive.
    #[test]
    fn extract_bearer_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("bearer my-key"));
        assert_eq!(extract_bearer_token(&headers), Some("my-key"));

        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("BEARER my-key"));
        assert_eq!(extract_bearer_token(&headers), Some("my-key"));
    }

    /// Missing Authorization header returns None.
    #[test]
    fn extract_missing_header_returns_none() {
        let headers = HeaderMap::new();
        assert_eq!(extract_bearer_token(&headers), None);
    }

    /// Non-Bearer scheme (e.g. Basic) returns None.
    #[test]
    fn extract_non_bearer_scheme_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert_eq!(extract_bearer_token(&headers), None);
    }

    /// Bearer with no space/token after returns None.
    #[test]
    fn extract_bearer_no_token_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer"));
        assert_eq!(extract_bearer_token(&headers), None);
    }

    /// Bearer with empty token after space returns None.
    #[test]
    fn extract_bearer_empty_token_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer "));
        assert_eq!(extract_bearer_token(&headers), None);
    }

    /// Token with spaces is returned as-is (everything after "Bearer ").
    #[test]
    fn extract_token_with_spaces_preserved() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer token with spaces"),
        );
        assert_eq!(
            extract_bearer_token(&headers),
            Some("token with spaces"),
            "everything after 'Bearer ' should be the token"
        );
    }

    /// Header with only scheme name and no value after returns None.
    #[test]
    fn extract_scheme_only_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer"));
        assert_eq!(extract_bearer_token(&headers), None);
    }
}
