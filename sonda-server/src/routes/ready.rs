//! Readiness endpoint.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::state::AppState;

/// `GET /ready` — 200 once the startup sweep has nothing left to start, 503 while
/// it is still running or after it failed.
pub async fn ready(State(state): State<AppState>) -> Response {
    let sweep = state.sweep_status.snapshot();
    let status = if sweep.phase.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(json!({
            "status": sweep.phase.as_str(),
            "autostart_started": sweep.started,
            "autostart_expected": sweep.expected,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SweepStatus;
    use http_body_util::BodyExt;
    use std::sync::Arc;

    async fn probe(status: SweepStatus) -> (StatusCode, serde_json::Value) {
        let mut state = AppState::new();
        state.sweep_status = Arc::new(status);
        let response = ready(State(state)).await;
        let code = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body must collect")
            .to_bytes();
        (
            code,
            serde_json::from_slice(&bytes).expect("body must be JSON"),
        )
    }

    #[tokio::test]
    async fn no_autostart_is_ready() {
        let (code, body) = probe(SweepStatus::not_configured()).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "status": "not_configured",
                "autostart_started": 0,
                "autostart_expected": 0,
            })
        );
    }

    #[tokio::test]
    async fn a_running_sweep_is_not_ready() {
        let status = SweepStatus::in_progress(12);
        status.record_started();

        let (code, body) = probe(status).await;

        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            json!({
                "status": "in_progress",
                "autostart_started": 1,
                "autostart_expected": 12,
            })
        );
    }

    #[tokio::test]
    async fn a_finished_sweep_is_ready() {
        let status = SweepStatus::in_progress(2);
        status.record_started();
        status.record_started();
        status.finish();

        let (code, body) = probe(status).await;

        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "status": "finished",
                "autostart_started": 2,
                "autostart_expected": 2,
            })
        );
    }

    #[tokio::test]
    async fn a_sweep_that_skipped_entries_is_still_ready() {
        let status = SweepStatus::in_progress(12);
        status.record_started();
        status.finish();

        let (code, body) = probe(status).await;

        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "status": "finished",
                "autostart_started": 1,
                "autostart_expected": 12,
            })
        );
    }

    #[tokio::test]
    async fn a_failed_sweep_is_not_ready() {
        let status = SweepStatus::in_progress(4);
        status.record_started();
        status.fail();

        let (code, body) = probe(status).await;

        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            json!({
                "status": "failed",
                "autostart_started": 1,
                "autostart_expected": 4,
            })
        );
    }
}
