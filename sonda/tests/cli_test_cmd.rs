//! Integration tests for `sonda test`.
//!
//! A minimal in-process HTTP responder plays the role of the Prometheus
//! query API, scripted per test: how many polls return "inactive" before the
//! alert reports firing, and whether it eventually resolves.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::sonda_bin;
use tempfile::TempDir;

const EMPTY_RESULT: &str = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;

/// Serve scripted query responses. `respond(n, request_line)` picks the
/// body and an artificial delay for the n-th request (0-based); the raw
/// request line carries the path and query, so responders distinguish
/// instant queries from `/api/v1/query_range` and script per-alert behavior
/// by matching on the alert name. Each connection is handled on its own
/// thread so a stalled response never blocks the next request. The server
/// runs until the listener drops at test end.
fn mock_prometheus(
    respond: impl Fn(usize, &str) -> (String, std::time::Duration) + Send + Sync + 'static,
) -> (String, Arc<AtomicUsize>) {
    mock_server(move |n, line| {
        let (body, stall) = respond(n, line);
        (body, Vec::new(), stall)
    })
}

/// The shared scripted-HTTP responder behind [`mock_prometheus`] and
/// [`mock_alertmanager`]. `respond` returns the body, any extra response
/// headers, and an artificial delay.
fn mock_server(
    respond: impl Fn(usize, &str) -> (String, Vec<(String, String)>, std::time::Duration)
        + Send
        + Sync
        + 'static,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);
    let respond = Arc::new(respond);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let n = hits_clone.fetch_add(1, Ordering::SeqCst);
            let respond = Arc::clone(&respond);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                // Drain the remaining headers.
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok() {
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    line.clear();
                }
                let (body, headers, stall) = respond(n, &request_line);
                std::thread::sleep(stall);
                let extra: String = headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    (base_url, hits)
}

const NO_STALL: std::time::Duration = std::time::Duration::ZERO;

/// Extract `(start, end, step)` from a `/api/v1/query_range` request line;
/// `None` for instant queries.
fn parse_range_params(request_line: &str) -> Option<(f64, f64, f64)> {
    if !request_line.contains("/api/v1/query_range") {
        return None;
    }
    let target = request_line.split_whitespace().nth(1)?;
    let (_, params) = target.split_once('?')?;
    let (mut start, mut end, mut step) = (None, None, None);
    for pair in params.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "start" => start = value.parse().ok(),
            "end" => end = value.parse().ok(),
            "step" => step = value.parse().ok(),
            _ => {}
        }
    }
    Some((start?, end?, step?))
}

/// Build a range-query matrix body: one firing series (with `host`) whose
/// samples cover the grid points where `unix_ts - anchor_unix` falls inside
/// `[fire_at, resolve_at)` seconds.
fn matrix_body(
    start: f64,
    end: f64,
    step: f64,
    anchor_unix: f64,
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &str,
) -> String {
    let mut values = Vec::new();
    let steps = ((end - start) / step).floor() as u64;
    for index in 0..=steps {
        let ts = start + index as f64 * step;
        let offset = ts - anchor_unix;
        let firing = fire_at.is_some_and(|f| offset >= f) && resolve_at.is_none_or(|r| offset < r);
        if firing {
            values.push(format!("[{ts:.3},\"1\"]"));
        }
    }
    if values.is_empty() {
        return r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#.to_string();
    }
    format!(
        r#"{{"status":"success","data":{{"resultType":"matrix","result":[{{"metric":{{"alertname":"HighCpuUsage","alertstate":"firing","host":"{host}"}},"values":[{}]}}]}}}}"#,
        values.join(",")
    )
}

/// A time-consistent fake alert: firing during `[fire_at, resolve_at)`
/// seconds measured from the mock's first request. Instant and range
/// queries answer from the same clock, so live polling and post-hoc range
/// acquisition see one coherent world.
fn alert_world(
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &'static str,
) -> impl Fn(usize, &str) -> (String, std::time::Duration) + Send + Sync + 'static {
    skewed_alert_world(fire_at, resolve_at, host, 0.0)
}

/// [`alert_world`] whose *server clock* runs `skew` seconds ahead of the
/// local one (negative = behind). `time()` queries, matrix sample
/// timestamps, and range-window interpretation all live on that server
/// clock; alert behaviour stays fixed relative to first contact. A world
/// with skew is the discriminating fixture for verdict-anchor bugs: the
/// reported transition times must not move with the skew.
fn skewed_alert_world(
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &'static str,
    skew: f64,
) -> impl Fn(usize, &str) -> (String, std::time::Duration) + Send + Sync + 'static {
    let anchor: Arc<std::sync::OnceLock<(std::time::Instant, f64)>> =
        Arc::new(std::sync::OnceLock::new());
    move |_, request_line| {
        let (mono0, unix0_local) = *anchor.get_or_init(|| {
            (
                std::time::Instant::now(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("epoch")
                    .as_secs_f64(),
            )
        });
        let unix0_server = unix0_local + skew;
        if request_line.contains("query=time") {
            let server_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs_f64()
                + skew;
            (
                format!(
                    r#"{{"status":"success","data":{{"resultType":"scalar","result":[{server_now:.3},"{server_now:.3}"]}}}}"#
                ),
                NO_STALL,
            )
        } else if let Some((start, end, step)) = parse_range_params(request_line) {
            (
                matrix_body(start, end, step, unix0_server, fire_at, resolve_at, host),
                NO_STALL,
            )
        } else {
            let offset = mono0.elapsed().as_secs_f64();
            let firing =
                fire_at.is_some_and(|f| offset >= f) && resolve_at.is_none_or(|r| offset < r);
            // Carry the same labels the matrix body does. Real Prometheus
            // returns a series' full label set on an instant ALERTS query;
            // a fixture that returns only alertname/alertstate cannot
            // exercise anything that reads labels off the live poll.
            (
                (if firing {
                    firing_result(host)
                } else {
                    EMPTY_RESULT.to_string()
                }),
                NO_STALL,
            )
        }
    }
}

// --- Mock Alertmanager -------------------------------------------------
//
// The Alertmanager acquisition asks a different question of the same alert
// world: not "is the rule firing?" but "did the notification arrive?".
// These fixtures speak the v2 `GET /api/v2/alerts` shape and are driven by
// the same clock as [`alert_world`], so the parity test can put one world
// through both acquisitions and demand the same verdict.

/// Every request line the mock received, in order — so tests can assert on
/// the *wire* form of the filters, which is the only place a mis-escaped or
/// mis-encoded matcher is visible.
type RequestLog = Arc<std::sync::Mutex<Vec<String>>>;

/// A scripted responder: request index and request line in, body + extra
/// response headers + artificial delay out.
type Responder =
    dyn Fn(usize, &str) -> (String, Vec<(String, String)>, std::time::Duration) + Send + Sync;

/// An instant-query vector for a firing `ALERTS` series carrying `host`.
fn firing_result(host: &str) -> String {
    format!(
        r#"{{"status":"success","data":{{"resultType":"vector","result":[{{"metric":{{"alertname":"HighCpuUsage","alertstate":"firing","severity":"critical","host":"{host}"}},"value":[1,"1"]}}]}}}}"#
    )
}

/// Format an instant as Alertmanager renders `startsAt` / `endsAt`.
fn rfc3339(unix: f64) -> String {
    chrono::DateTime::from_timestamp(unix.trunc() as i64, (unix.fract() * 1e9) as u32)
        .expect("representable timestamp")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// An Alertmanager whose alert is present from `fire_at` and carries an
/// `endsAt` of `resolve_at` — deliberately *keeping* the resolved alert in
/// the response. A real Alertmanager (measured: 0.28.1) removes it instead,
/// which makes this the harder fixture of the two: resolution here can only
/// be detected by reading `endsAt`, not by the alert disappearing.
///
/// `extra_labels` is appended verbatim inside the label object (leading
/// comma included) and `state` names the `status.state` to report.
///
/// `keep_after_resolve == false` models what a real Alertmanager does —
/// drop the alert once it resolves — so both resolution shapes are covered.
fn am_world(
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &'static str,
    state: &'static str,
    keep_after_resolve: bool,
) -> (Box<Responder>, RequestLog) {
    let log: RequestLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&log);
    let anchor: Arc<std::sync::OnceLock<(std::time::Instant, f64)>> =
        Arc::new(std::sync::OnceLock::new());
    let responder = Box::new(move |_: usize, request_line: &str| {
        if let Ok(mut lines) = seen.lock() {
            lines.push(request_line.to_string());
        }
        let (mono0, unix0) = *anchor.get_or_init(|| {
            (
                std::time::Instant::now(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("epoch")
                    .as_secs_f64(),
            )
        });
        let offset = mono0.elapsed().as_secs_f64();
        let headers = vec![(
            "Date".to_string(),
            chrono::Utc::now()
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string(),
        )];
        let gone = !keep_after_resolve && resolve_at.is_some_and(|r| offset >= r);
        let body = match fire_at {
            Some(fire) if offset >= fire && !gone => {
                // Alertmanager gives a live alert an endsAt in the future
                // (the resend deadline) and rewrites it to the resolution
                // time when the alert ends.
                let ends = resolve_at.map_or(unix0 + offset + 300.0, |r| unix0 + r);
                format!(
                    r#"[{{"labels":{{"alertname":"HighCpuUsage","host":"{host}"}},
                        "annotations":{{}},
                        "startsAt":"{}","endsAt":"{}","updatedAt":"{}",
                        "status":{{"state":"{state}","silencedBy":[],"inhibitedBy":[]}},
                        "receivers":[{{"name":"webhook"}}],"fingerprint":"deadbeef"}}]"#,
                    rfc3339(unix0 + fire),
                    rfc3339(ends),
                    rfc3339(unix0 + offset),
                )
            }
            _ => "[]".to_string(),
        };
        (body, headers, NO_STALL)
    });
    (responder, log)
}

/// An Alertmanager that keeps the resolved alert with a past `endsAt`.
fn mock_alertmanager(
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &'static str,
    state: &'static str,
) -> (String, RequestLog) {
    let (responder, log) = am_world(fire_at, resolve_at, host, state, true);
    let (url, _hits) = mock_server(responder);
    (url, log)
}

/// An Alertmanager that drops the alert on resolution, as real ones do.
fn mock_alertmanager_dropping(
    fire_at: Option<f64>,
    resolve_at: Option<f64>,
    host: &'static str,
) -> (String, RequestLog) {
    let (responder, log) = am_world(fire_at, resolve_at, host, "active", false);
    let (url, _hits) = mock_server(responder);
    (url, log)
}

/// The `filter=` values the mock actually received, percent-decoded.
fn filters_seen(log: &RequestLog) -> Vec<String> {
    let lines = log.lock().expect("request log");
    let mut filters = Vec::new();
    for line in lines.iter() {
        let Some(target) = line.split_whitespace().nth(1) else {
            continue;
        };
        let Some((_, params)) = target.split_once('?') else {
            continue;
        };
        for pair in params.split('&') {
            if let Some(value) = pair.strip_prefix("filter=") {
                filters.push(percent_decode(value));
            }
        }
    }
    filters
}

/// Decode a percent-encoded query value. Deliberately hand-rolled: the
/// point of the escaping tests is to read exactly what went on the wire,
/// not to trust the same library that put it there.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn scenario_with_expect(expect_block: &str) -> String {
    format!(
        "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 300ms
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
{expect_block}"
    )
}

fn write_scenario(dir: &TempDir, yaml: &str) -> std::path::PathBuf {
    let path = dir.path().join("scenario.yaml");
    std::fs::write(&path, yaml).expect("write scenario");
    path
}

#[test]
fn passes_when_alert_fires_and_resolves() {
    // Firing from the first instant, resolving one second in — shortly
    // after the 300ms scenario ends.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.0), Some(1.0), "sonda-test"));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 5s
      resolves_within: 5s
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {stderr}",
        output.status
    );
    assert!(stderr.contains("firing after"), "stderr: {stderr}");
    assert!(stderr.contains("resolved after"), "stderr: {stderr}");
    assert!(
        stderr.contains("1 alert expectation(s) verified"),
        "stderr: {stderr}"
    );
}

#[test]
fn fails_when_alert_never_fires() {
    let (url, _hits) = mock_prometheus(alert_world(None, None, "sonda-test"));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 1s
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "must exit non-zero\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("did not fire within 1s"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("1 alert expectation(s) failed"),
        "stderr: {stderr}"
    );
}

#[test]
fn rejects_scenario_without_expect_block() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(""));

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", "http://127.0.0.1:1"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("no `expect:` block"), "stderr: {stderr}");
}

#[test]
fn dry_run_validates_without_polling() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 2m
",
        ),
    );

    // Unreachable URL proves --dry-run never talks to Prometheus.
    let output = Command::new(sonda_bin())
        .args(["--dry-run", "test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", "http://127.0.0.1:1"])
        .output()
        .expect("run sonda test --dry-run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("1 alert expectation(s) parsed OK"),
        "stderr: {stderr}"
    );
}

#[test]
fn fails_when_alert_fires_after_deadline() {
    // #516 round-1 blocker 1, reproduction B: the alert fires, but only
    // after firing_within has passed — must be a failure, not a pass. The
    // world fires at 300ms against a 200ms deadline; the range grid has
    // pre-deadline coverage (inactive samples) and then the late firing
    // sample, so the verdict is Late.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.3), None, "sonda-test"));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 200ms
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "late firing must fail\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("later than firing_within 200ms"),
        "stderr: {stderr}"
    );
}

#[test]
fn fails_when_second_resolution_is_delayed_past_deadline() {
    // A genuinely blown resolution deadline must be a failure: both alerts
    // stay firing until 1200ms after the first request, so MustResolveFast
    // is still firing through its whole 200ms window while SlowToResolve's
    // 4s deadline passes. The world is anchored on the mock's first
    // request, not process spawn (#527 review M5).
    let (url, _hits) = mock_prometheus(alert_world(Some(0.0), Some(1.2), "sonda-test"));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: SlowToResolve
      firing_within: 5s
      resolves_within: 4s
    - alert: MustResolveFast
      firing_within: 5s
      resolves_within: 200ms
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "blown second resolution deadline must fail\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("MustResolveFast still firing 200ms after scenario end"),
        "stderr: {stderr}"
    );
    // The first expectation's generous deadline still passes.
    assert!(
        stderr.contains("SlowToResolve resolved after"),
        "stderr: {stderr}"
    );
}

#[test]
fn parallel_resolutions_do_not_blind_each_others_windows() {
    // #516 round-2 blocker heritage: SlowLane keeps firing for 1.5s against
    // a 3s resolution deadline while FastLane resolves right at scenario
    // end against a 300ms one. Both must PASS — no expectation's window may
    // be blinded by another's wait, whether by concurrent polling or by the
    // range-acquired verdict timelines.
    let slow = alert_world(Some(0.0), Some(1.5), "sonda-test");
    let fast = alert_world(Some(0.0), Some(0.4), "sonda-test");
    let (url, _hits) = mock_prometheus(move |n, request_line| {
        if request_line.contains("SlowLane") {
            slow(n, request_line)
        } else {
            fast(n, request_line)
        }
    });
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: SlowLane
      firing_within: 5s
      resolves_within: 3s
    - alert: FastLane
      firing_within: 5s
      resolves_within: 300ms
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "both resolutions meet their own deadlines and must pass\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("2 alert expectation(s) verified"),
        "stderr: {stderr}"
    );
}

#[test]
fn bounded_failure_when_endpoint_stalls_before_answering() {
    // Review blocker 2, reproduction C: an endpoint that accepts the
    // connection and never answers in time must produce a bounded non-zero
    // exit (preflight timeout), not a SIGINT-proof hang.
    let (url, _hits) =
        mock_prometheus(|_, _| (EMPTY_RESULT.to_string(), std::time::Duration::from_secs(60)));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 200ms
",
        ),
    );

    let started = std::time::Instant::now();
    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url])
        .args(["--interval", "100ms", "--query-timeout", "300ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "stall must fail\nstderr: {stderr}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "must fail in bounded time, took {:?}",
        started.elapsed()
    );
    assert!(stderr.contains("preflight"), "stderr: {stderr}");
}

#[test]
fn mid_run_stall_surfaces_query_errors_on_missed_deadline() {
    // Stall only after preflight: every poll times out, the deadline passes,
    // and the failure must carry the query error instead of hanging or
    // silently passing.
    let (url, _hits) = mock_prometheus(|n, _| {
        if n == 0 {
            (EMPTY_RESULT.to_string(), NO_STALL)
        } else {
            (EMPTY_RESULT.to_string(), std::time::Duration::from_secs(30))
        }
    });
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 600ms
",
        ),
    );

    let started = std::time::Instant::now();
    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url])
        .args(["--interval", "100ms", "--query-timeout", "300ms"])
        .args(["--query-step", "300ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "must fail in bounded time, took {:?}",
        started.elapsed()
    );
    assert!(
        stderr.contains("did not fire within 600ms") && stderr.contains("last query error"),
        "stderr: {stderr}"
    );
    // With the range API also stalled, the verdict falls back to the live
    // poll timeline — and says so.
    assert!(
        stderr.contains("uses the live poll timeline"),
        "stderr: {stderr}"
    );
}

#[test]
fn firing_verdict_uses_sample_time_not_poll_time() {
    // The range-acquisition contract: transition times come from the stored
    // samples, not from when polling happened to look. The alert fires at
    // 300ms against a 1s deadline, but the live poll interval is a huge 2s
    // — poll-based verdicts would see the firing at ~2s and call it Late.
    // The range grid shows the 400ms sample, so this must PASS. Goes red if
    // verdicts revert to the poll timeline.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.3), None, "sonda-test"));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 1s
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "2s"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sample-time verdict must pass despite the coarse poll interval\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("firing after 400ms (within 1s)"),
        "stderr: {stderr}"
    );
}

#[test]
fn verdicts_survive_a_server_clock_running_ahead() {
    // #528 review blocker, direction 1: the server's clock is 1s ahead of
    // the local one. The alert fires 400ms in (real time) against a 1s
    // deadline — a clear pass. A local-anchored conversion would read the
    // firing sample as landing at 1.4s and flip the verdict to Late.
    let (url, _hits) = mock_prometheus(skewed_alert_world(Some(0.3), None, "sonda-test", 1.0));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 1s
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "server-clock skew must not move the verdict\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("firing after 400ms (within 1s)"),
        "stderr: {stderr}"
    );
}

#[test]
fn verdicts_survive_a_server_clock_running_behind() {
    // #528 review blocker, direction 2 — the dangerous one: the server's
    // clock is 1.5s behind. The alert fires at 2s (real time) against a 1s
    // deadline, so FAIL is the only correct verdict. A local-anchored
    // conversion would see the firing sample at 2 - 1.5 = 0.5s and report
    // a false PASS with the deadline apparently met.
    let (url, _hits) = mock_prometheus(skewed_alert_world(Some(2.0), None, "sonda-test", -1.5));
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 1s
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a missed deadline must not become a pass under negative skew\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("did not fire within 1s"),
        "stderr: {stderr}"
    );
}

#[test]
fn provenance_survives_a_fall_back_to_the_live_poll_timeline() {
    // #567 r4. The verdict timeline normally comes from a range query, which
    // collects firing-series labels for the provenance check. When that query
    // is unusable — no stored sample yet for an alert that fires inside one
    // --query-step, or an API error — the verdict falls back to the live poll
    // timeline, and the provenance check used to have nothing to inspect: the
    // run passed clean while matching an alert from a foreign series.
    //
    // Forcing the range query to fail reproduces that state deterministically,
    // without depending on the race that made it a CI flake.
    let inner = alert_world(Some(0.0), None, "intruder-host");
    let (url, _hits) = mock_prometheus(move |n, line| {
        if line.contains("/api/v1/query_range") {
            return (
                "{\"status\":\"error\"}".to_string(),
                std::time::Duration::ZERO,
            );
        }
        inner(n, line)
    });
    let dir = TempDir::new().expect("tempdir");
    let yaml = "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 300ms
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
    labels:
      host: sonda-test
expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 5s
";
    let path = write_scenario(&dir, yaml);
    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Guards the fixture: if the fallback is not taken, this test proves
    // nothing about the fallback.
    assert!(
        stderr.contains("uses the live poll timeline"),
        "fixture did not force the fallback: {stderr}"
    );
    assert!(
        stderr.contains("WARN") && stderr.contains("host=\"intruder-host\""),
        "the provenance advisory must survive the fallback: {stderr}"
    );
}

#[test]
fn warns_when_matched_series_carries_foreign_label_values() {
    // #527 review, W2 follow-up: ALERTS is global, so an expectation can
    // match an alert another series caused. When the matched firing series
    // carries a label value the scenario never emits, say so.
    let (url, _hits) = mock_prometheus(alert_world(Some(0.0), None, "intruder-host"));
    let dir = TempDir::new().expect("tempdir");
    let yaml = "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 300ms
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
    labels:
      host: sonda-test
expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 5s
";
    let path = write_scenario(&dir, yaml);

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The firing check itself passes — the warning is advisory.
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("WARN")
            && stderr.contains("host=\"intruder-host\"")
            && stderr.contains("scope `expect.labels`"),
        "stderr: {stderr}"
    );
    // The noun names what this source actually holds — a reader chasing
    // the warning must be pointed at the system that was queried.
    assert!(
        stderr.contains("matched an ALERTS series"),
        "the Prometheus path must name the ALERTS series\nstderr: {stderr}"
    );
}

#[test]
fn alertmanager_provenance_warning_names_alertmanager_not_alerts() {
    // #552 review M1: the provenance check is acquisition-independent, but
    // its message was not — it told an Alertmanager user to go look at a
    // metric this path never queries. This path exists for people whose
    // two systems disagree, so pointing them at the wrong one is worse
    // here than anywhere else.
    let (url, _log) = mock_alertmanager(Some(0.0), None, "intruder-host", "active");
    let dir = TempDir::new().expect("tempdir");
    let yaml = "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 300ms
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
    labels:
      host: sonda-test
expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 5s
";
    let path = write_scenario(&dir, yaml);

    let output = run_against_alertmanager(&path, &url);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("WARN") && stderr.contains("host=\"intruder-host\""),
        "the provenance check must still fire on this path\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("matched an Alertmanager alert"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("ALERTS series"),
        "the Alertmanager path must not name a metric it never queried\nstderr: {stderr}"
    );
}

#[test]
fn dry_run_works_without_prometheus_url() {
    // Review M2: --dry-run never contacts the endpoint, so it must not
    // demand one.
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 2m
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["--dry-run", "test", path.to_str().expect("utf8 path")])
        .env_remove("SONDA_PROMETHEUS_URL")
        .output()
        .expect("run sonda test --dry-run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("1 alert expectation(s) parsed OK"),
        "stderr: {stderr}"
    );
}

// `missing_prometheus_url_is_rejected_outside_dry_run` lived here until
// `--alertmanager-url` arrived. It is superseded by
// `neither_acquisition_url_names_both_options`, which clears *both* env
// vars and asserts the message names both flags — the weaker version could
// pass while the new flag's env var silently supplied a URL.

#[test]
fn failed_scenario_reports_immediately_without_waiting_out_deadlines() {
    // Review W3: a scenario that dies at launch (unreachable TCP sink) must
    // surface its own error promptly instead of blocking behind a long
    // firing_within.
    let (url, _hits) = mock_prometheus(|_, _| (EMPTY_RESULT.to_string(), NO_STALL));
    let dir = TempDir::new().expect("tempdir");
    let yaml = "version: 2
kind: runnable
defaults:
  rate: 10
  duration: 300ms
  encoder:
    type: prometheus_text
  sink:
    type: tcp
    address: 127.0.0.1:1
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 60s
";
    let path = write_scenario(&dir, yaml);

    let started = std::time::Instant::now();
    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &url, "--interval", "100ms"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "run failure must not wait out firing_within, took {:?}",
        started.elapsed()
    );
}

#[test]
fn run_still_accepts_files_with_expect_blocks() {
    // The expect block is pure metadata — `sonda run` must not reject it.
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(
        &dir,
        &scenario_with_expect(
            "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 2m
",
        ),
    );

    let output = Command::new(sonda_bin())
        .args(["run", path.to_str().expect("utf8 path")])
        .output()
        .expect("run sonda run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// --- Alertmanager acquisition ------------------------------------------

/// Standard expect block for the Alertmanager tests.
fn am_expect(firing_within: &str, resolves_within: Option<&str>) -> String {
    let resolves = resolves_within
        .map(|d| format!("      resolves_within: {d}\n"))
        .unwrap_or_default();
    format!(
        "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: {firing_within}
{resolves}"
    )
}

fn run_against_alertmanager(path: &std::path::Path, url: &str) -> std::process::Output {
    Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--alertmanager-url", url, "--interval", "100ms"])
        .output()
        .expect("run sonda test")
}

#[test]
fn alertmanager_passes_when_the_notification_arrives_and_clears() {
    // Firing from first contact, ending one second in — just after the
    // 300ms scenario. The mock keeps the ended alert in the response with
    // a past endsAt, so resolution is detected by the expiry check, not by
    // the alert disappearing.
    let (url, _log) = mock_alertmanager(Some(0.0), Some(1.0), "sonda-test", "active");
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("5s", Some("5s"))));

    let output = run_against_alertmanager(&path, &url);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {stderr}",
        output.status
    );
    assert!(stderr.contains("firing after"), "stderr: {stderr}");
    assert!(stderr.contains("resolved after"), "stderr: {stderr}");
    assert!(
        stderr.contains("verified via Alertmanager"),
        "the report must name the acquisition path\nstderr: {stderr}"
    );
}

#[test]
fn alertmanager_detects_resolution_when_the_alert_disappears() {
    // The shape a real Alertmanager produces: a resolved alert is dropped
    // from /api/v2/alerts rather than lingering with a past endsAt. Both
    // shapes must reach the same verdict, because which one you get is the
    // Alertmanager's choice, not the test author's.
    let (url, _log) = mock_alertmanager_dropping(Some(0.0), Some(1.0), "sonda-test");
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("5s", Some("5s"))));

    let output = run_against_alertmanager(&path, &url);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("resolved after"), "stderr: {stderr}");
}

#[test]
fn alertmanager_report_states_its_precision_instead_of_borrowing_prometheus_guarantees() {
    // The likeliest wrong claim on this path is implying the sample-time
    // precision the range-query path earned. The note must be present and
    // must say where the numbers come from.
    let (url, _log) = mock_alertmanager(Some(0.0), None, "sonda-test", "active");
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("5s", None)));

    let output = run_against_alertmanager(&path, &url);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("keeps no queryable history"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("live polling every"), "stderr: {stderr}");
    assert!(
        !stderr.contains("range"),
        "the Alertmanager path must not mention range acquisition\nstderr: {stderr}"
    );
}

#[test]
fn alertmanager_fails_when_no_notification_arrives() {
    let (url, _log) = mock_alertmanager(None, None, "sonda-test", "active");
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("1s", None)));

    let output = run_against_alertmanager(&path, &url);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "must exit non-zero\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("did not fire within 1s"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("verified via Alertmanager"),
        "stderr: {stderr}"
    );
}

#[test]
fn alertmanager_and_prometheus_agree_on_the_same_alert_world() {
    // Parity: one alert world, two acquisitions, one verdict. The point of
    // the Alertmanager path is that it asks a *different question* — it
    // must not answer a different *answer* when both hops are healthy.
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("5s", Some("5s"))));

    let (prom_url, _hits) = mock_prometheus(alert_world(Some(0.0), Some(1.0), "sonda-test"));
    let prometheus = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", &prom_url, "--interval", "100ms"])
        .args(["--query-step", "200ms"])
        .output()
        .expect("run sonda test");

    let (am_url, _log) = mock_alertmanager(Some(0.0), Some(1.0), "sonda-test", "active");
    let alertmanager = run_against_alertmanager(&path, &am_url);

    let prom_err = String::from_utf8_lossy(&prometheus.stderr);
    let am_err = String::from_utf8_lossy(&alertmanager.stderr);
    assert_eq!(
        prometheus.status.success(),
        alertmanager.status.success(),
        "acquisitions disagreed on the same world\nprometheus: {prom_err}\nalertmanager: {am_err}"
    );
    for expected in [
        "firing after",
        "resolved after",
        "1 alert expectation(s) verified",
    ] {
        assert!(prom_err.contains(expected), "prometheus: {prom_err}");
        assert!(am_err.contains(expected), "alertmanager: {am_err}");
    }
}

#[test]
fn alertmanager_suppressed_alert_counts_as_firing_and_warns() {
    // A silence stops the notification, not the alert. The expectation is
    // satisfied — and the operator is told, because asserting on a
    // silenced alert is almost never what was meant.
    let (url, _log) = mock_alertmanager(Some(0.0), None, "sonda-test", "suppressed");
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("5s", None)));

    let output = run_against_alertmanager(&path, &url);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("suppressed (silenced or inhibited)"),
        "stderr: {stderr}"
    );
}

#[test]
fn alertmanager_down_is_a_preflight_failure_not_a_silent_pass() {
    // No listener at all: the preflight gate must name the failure rather
    // than let the run proceed to an unprovable verdict.
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("1s", None)));

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args([
            "--alertmanager-url",
            "http://127.0.0.1:1",
            "--interval",
            "100ms",
        ])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("alertmanager preflight against http://127.0.0.1:1 failed"),
        "stderr: {stderr}"
    );
}

#[test]
fn both_acquisition_urls_is_a_config_error() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("1s", None)));

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .args(["--prometheus-url", "http://127.0.0.1:1"])
        .args(["--alertmanager-url", "http://127.0.0.1:2"])
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("mutually exclusive"), "stderr: {stderr}");
}

#[test]
fn neither_acquisition_url_names_both_options() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_scenario(&dir, &scenario_with_expect(&am_expect("1s", None)));

    let output = Command::new(sonda_bin())
        .args(["test", path.to_str().expect("utf8 path")])
        .env_remove("SONDA_PROMETHEUS_URL")
        .env_remove("SONDA_ALERTMANAGER_URL")
        .output()
        .expect("run sonda test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("--prometheus-url"), "stderr: {stderr}");
    assert!(stderr.contains("--alertmanager-url"), "stderr: {stderr}");
}

#[rustfmt::skip]
#[rstest::rstest]
// Hostile expectation label values must survive TWO layers on the wire:
// Alertmanager matcher quoting (done by sonda) and percent-encoding (done
// by the HTTP layer). Decoding the request line back proves both — a value
// that broke out of its quotes, or one that was never encoded, shows up
// here as a filter that is not the one we meant to send.
#[case::spaces(      "on call",        r#"team="on call""#)]
#[case::double_quote("net\"ops",       r#"team="net\"ops""#)]
#[case::backslash(   "a\\b",           r#"team="a\\b""#)]
#[case::comma(       "a,b",            r#"team="a,b""#)]
#[case::ampersand(   "a&b=c",          r#"team="a&b=c""#)]
#[case::braces(      "{a}",            r#"team="{a}""#)]
#[case::percent(     "100%",           r#"team="100%""#)]
#[case::utf8(        "café-日本",       r#"team="café-日本""#)]
fn alertmanager_filters_survive_the_wire(#[case] value: &str, #[case] expected: &str) {
    let (url, log) = mock_alertmanager(None, None, "sonda-test", "active");
    let dir = TempDir::new().expect("tempdir");
    let expect_block = format!(
        "expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 300ms
      labels:
        team: {}
",
        serde_json::to_string(value).expect("json string")
    );
    let path = write_scenario(&dir, &scenario_with_expect(&expect_block));

    let output = run_against_alertmanager(&path, &url);
    // The alert never fires here; the verdict is beside the point.
    let _ = output.status;
    let filters = filters_seen(&log);
    assert!(
        filters.contains(&r#"alertname="HighCpuUsage""#.to_string()),
        "alertname filter missing from {filters:?}"
    );
    assert!(
        filters.contains(&expected.to_string()),
        "expected filter {expected:?} on the wire, saw {filters:?}"
    );
}
