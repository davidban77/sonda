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
        let guard = match state.request_counters.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(counter) = guard.get(&key) {
            counter.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    // Slow path: another writer may have created the entry between the read
    // drop and the write acquire — entry().or_insert_with handles both cases.
    let mut guard = match state.request_counters.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);
}

fn observe_histogram(state: &AppState, route: String, method: String, seconds: f64) {
    let key = (route, method);
    {
        let guard = match state.request_histograms.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(shard) = guard.get(&key) {
            shard.observe(seconds);
            return;
        }
    }
    let mut guard = match state.request_histograms.write() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.entry(key).or_default().observe(seconds);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poison<T: Send + Sync>(lock: &std::sync::RwLock<T>) {
        std::thread::scope(|s| {
            let _ = s
                .spawn(|| {
                    let _g = lock.write().expect("first write must succeed");
                    panic!("intentional poison");
                })
                .join();
        });
    }

    #[test]
    fn increment_counter_survives_poisoned_request_counters_lock() {
        let state = AppState::new();
        poison(&state.request_counters);
        assert!(state.request_counters.is_poisoned());

        increment_counter(&state, "/test".to_string(), "GET".to_string(), 200);
        increment_counter(&state, "/test".to_string(), "GET".to_string(), 200);

        let guard = match state.request_counters.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let key = ("/test".to_string(), "GET".to_string(), 200u16);
        let counter = guard.get(&key).expect("entry must exist");
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn observe_histogram_survives_poisoned_request_histograms_lock() {
        let state = AppState::new();
        poison(&state.request_histograms);
        assert!(state.request_histograms.is_poisoned());

        observe_histogram(&state, "/test".to_string(), "GET".to_string(), 0.123);
        observe_histogram(&state, "/test".to_string(), "GET".to_string(), 0.456);

        let guard = match state.request_histograms.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let key = ("/test".to_string(), "GET".to_string());
        let snap = guard.get(&key).expect("entry must exist").snapshot();
        assert_eq!(snap.count, 2);
    }
}
