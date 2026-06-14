//! `GET /server/metrics` — process-level RED and saturation telemetry.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sonda_core::ScenarioState;

use crate::state::{AppState, HistogramShard};

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub async fn get_server_metrics(State(state): State<AppState>) -> Result<Response, Response> {
    let mut buf = String::with_capacity(4096);

    write_active_scenarios(&state, &mut buf).map_err(internal)?;
    write_scenarios_finished_total(&state, &mut buf).map_err(internal)?;
    write_worker_threads(&state, &mut buf).map_err(internal)?;
    write_max_scenarios(&state, &mut buf).map_err(internal)?;
    write_requests_total(&state, &mut buf).map_err(internal)?;
    write_request_duration_seconds(&state, &mut buf).map_err(internal)?;
    write_sink_errors_total(&state, &mut buf).map_err(internal)?;
    write_uptime_seconds(&state, &mut buf).map_err(internal)?;
    write_build_info(&mut buf).map_err(internal)?;

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        buf,
    )
        .into_response())
}

fn internal(_: std::fmt::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "failed to emit server metrics",
    )
        .into_response()
}

fn write_active_scenarios(state: &AppState, buf: &mut String) -> std::fmt::Result {
    let mut by_state: BTreeMap<&'static str, u64> = BTreeMap::new();
    for op in ScenarioState::operational_states() {
        by_state.insert(op.as_label(), 0);
    }
    {
        let scenarios = match state.scenarios.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        for handle in scenarios.values() {
            let snap = handle.stats_snapshot();
            if snap.state == ScenarioState::Finished {
                continue;
            }
            let label = snap.state.as_label();
            *by_state.entry(label).or_insert(0) += 1;
        }
    }

    writeln!(
        buf,
        "# HELP sonda_server_active_scenarios Scenarios currently held in each operational state."
    )?;
    writeln!(buf, "# TYPE sonda_server_active_scenarios gauge")?;
    for op in ScenarioState::operational_states() {
        let label = op.as_label();
        let value = by_state.get(label).copied().unwrap_or(0);
        writeln!(
            buf,
            "sonda_server_active_scenarios{{state=\"{label}\"}} {value}"
        )?;
    }
    Ok(())
}

fn write_scenarios_finished_total(state: &AppState, buf: &mut String) -> std::fmt::Result {
    let mut finished: u64 = 0;
    {
        let scenarios = match state.scenarios.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        for handle in scenarios.values() {
            if handle.stats_snapshot().state == ScenarioState::Finished {
                finished += 1;
            }
        }
    }
    writeln!(
        buf,
        "# HELP sonda_server_scenarios_finished_total Scenarios that have reached the Finished state and not yet been deleted."
    )?;
    writeln!(buf, "# TYPE sonda_server_scenarios_finished_total counter")?;
    writeln!(buf, "sonda_server_scenarios_finished_total {finished}")
}

fn write_worker_threads(state: &AppState, buf: &mut String) -> std::fmt::Result {
    writeln!(
        buf,
        "# HELP sonda_server_worker_threads Configured tokio multi-thread worker count."
    )?;
    writeln!(buf, "# TYPE sonda_server_worker_threads gauge")?;
    writeln!(buf, "sonda_server_worker_threads {}", state.worker_threads)
}

fn write_max_scenarios(state: &AppState, buf: &mut String) -> std::fmt::Result {
    writeln!(
        buf,
        "# HELP sonda_server_max_scenarios Configured scenario row cap (0 means unlimited)."
    )?;
    writeln!(buf, "# TYPE sonda_server_max_scenarios gauge")?;
    writeln!(buf, "sonda_server_max_scenarios {}", state.max_scenarios)
}

fn write_requests_total(state: &AppState, buf: &mut String) -> std::fmt::Result {
    writeln!(
        buf,
        "# HELP sonda_server_requests_total HTTP requests served, per matched route, method, and status."
    )?;
    writeln!(buf, "# TYPE sonda_server_requests_total counter")?;
    let snapshot: Vec<((String, String, u16), u64)> = {
        let guard = match state.request_counters.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut rows: Vec<_> = guard
            .iter()
            .map(|((r, m, s), c)| ((r.clone(), m.clone(), *s), c.load(Ordering::Relaxed)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    };
    for ((route, method, status), value) in &snapshot {
        writeln!(
            buf,
            "sonda_server_requests_total{{route=\"{}\",method=\"{}\",status=\"{}\"}} {}",
            escape_label(route),
            escape_label(method),
            status,
            value
        )?;
    }
    Ok(())
}

fn write_request_duration_seconds(state: &AppState, buf: &mut String) -> std::fmt::Result {
    writeln!(
        buf,
        "# HELP sonda_server_request_duration_seconds HTTP request duration in seconds, per matched route and method."
    )?;
    writeln!(
        buf,
        "# TYPE sonda_server_request_duration_seconds histogram"
    )?;
    let snapshots: Vec<((String, String), _)> = {
        let guard = match state.request_histograms.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut rows: Vec<_> = guard
            .iter()
            .map(|(k, shard)| (k.clone(), shard.snapshot()))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    };
    for ((route, method), snap) in &snapshots {
        let route_esc = escape_label(route);
        let method_esc = escape_label(method);
        for (i, bound) in HistogramShard::BUCKET_BOUNDS.iter().enumerate() {
            writeln!(
                buf,
                "sonda_server_request_duration_seconds_bucket{{route=\"{route_esc}\",method=\"{method_esc}\",le=\"{bound}\"}} {}",
                snap.buckets[i]
            )?;
        }
        writeln!(
            buf,
            "sonda_server_request_duration_seconds_bucket{{route=\"{route_esc}\",method=\"{method_esc}\",le=\"+Inf\"}} {}",
            snap.plus_inf
        )?;
        writeln!(
            buf,
            "sonda_server_request_duration_seconds_sum{{route=\"{route_esc}\",method=\"{method_esc}\"}} {}",
            snap.sum
        )?;
        writeln!(
            buf,
            "sonda_server_request_duration_seconds_count{{route=\"{route_esc}\",method=\"{method_esc}\"}} {}",
            snap.count
        )?;
    }
    Ok(())
}

fn write_sink_errors_total(state: &AppState, buf: &mut String) -> std::fmt::Result {
    writeln!(
        buf,
        "# HELP sonda_server_sink_errors_total Lifetime sink-write failures, summed across scenarios per sink_type."
    )?;
    writeln!(buf, "# TYPE sonda_server_sink_errors_total counter")?;
    let mut totals: BTreeMap<&'static str, u64> = BTreeMap::new();
    {
        let scenarios = match state.scenarios.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        for handle in scenarios.values() {
            let snap = handle.stats_snapshot();
            *totals.entry(handle.sink_type()).or_insert(0) += snap.total_sink_failures;
        }
    }
    for (sink_type, value) in totals {
        writeln!(
            buf,
            "sonda_server_sink_errors_total{{sink_type=\"{sink_type}\"}} {value}"
        )?;
    }
    Ok(())
}

fn write_uptime_seconds(state: &AppState, buf: &mut String) -> std::fmt::Result {
    let uptime = state.started_at.elapsed().as_secs_f64();
    writeln!(
        buf,
        "# HELP sonda_server_uptime_seconds Seconds since the server process started."
    )?;
    writeln!(buf, "# TYPE sonda_server_uptime_seconds gauge")?;
    writeln!(buf, "sonda_server_uptime_seconds {uptime}")
}

fn write_build_info(buf: &mut String) -> std::fmt::Result {
    let version = env!("CARGO_PKG_VERSION");
    let git_sha = env!("SONDA_GIT_SHA");
    writeln!(
        buf,
        "# HELP sonda_server_build_info Build-time version and git SHA. Constant gauge always set to 1."
    )?;
    writeln!(buf, "# TYPE sonda_server_build_info gauge")?;
    writeln!(
        buf,
        "sonda_server_build_info{{version=\"{}\",git_sha=\"{}\"}} 1",
        escape_label(version),
        escape_label(git_sha)
    )
}

fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_label_handles_backslash_quote_and_newline() {
        assert_eq!(escape_label(r#"a\b"c"#), r#"a\\b\"c"#);
        assert_eq!(escape_label("line\nbreak"), "line\\nbreak");
        assert_eq!(escape_label("plain"), "plain");
    }
}
