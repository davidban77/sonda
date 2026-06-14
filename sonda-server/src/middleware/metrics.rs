//! RED-metric instrumentation middleware.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::state::AppState;

pub async fn record_request_metrics(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "<unmatched>".to_string());
    let method = request.method().as_str().to_string();
    let started = Instant::now();

    let response = next.run(request).await;

    let status = response.status().as_u16();
    let elapsed = started.elapsed().as_secs_f64();

    increment_counter(&state, route.clone(), method.clone(), status);
    observe_histogram(&state, route, method, elapsed);

    response
}

fn increment_counter(state: &AppState, route: String, method: String, status: u16) {
    let key = (route, method, status);
    {
        let guard = state
            .request_counters
            .read()
            .expect("request_counters lock poisoned");
        if let Some(counter) = guard.get(&key) {
            counter.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    // Slow path: another writer may have created the entry between the read
    // drop and the write acquire — entry().or_insert_with handles both cases.
    let mut guard = state
        .request_counters
        .write()
        .expect("request_counters lock poisoned");
    guard
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

fn observe_histogram(state: &AppState, route: String, method: String, seconds: f64) {
    let key = (route, method);
    {
        let guard = state
            .request_histograms
            .read()
            .expect("request_histograms lock poisoned");
        if let Some(shard) = guard.get(&key) {
            shard.observe(seconds);
            return;
        }
    }
    let mut guard = state
        .request_histograms
        .write()
        .expect("request_histograms lock poisoned");
    guard.entry(key).or_default().observe(seconds);
}
