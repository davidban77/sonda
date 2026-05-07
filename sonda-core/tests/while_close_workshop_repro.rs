//! Workshop bug repro: paused -> finished cascade through `multi_runner`.
//!
//! Mimics the real workshop scenario shape (`primary_flap` + downstream gated
//! by `while: > 1`, `delay: { open: 50ms, close: 0s }`) but compressed in time
//! and pointed at an in-process TCP listener that decodes the snappy-compressed
//! `WriteRequest` payload. Runs through `launch_multi_compiled` so the wire
//! path matches what the released `sonda-server` does at runtime.

#![cfg(feature = "config")]
#![cfg(feature = "remote-write")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use prost::Message;

use sonda_core::compile_scenario_file_compiled;
use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::encoder::remote_write::{TimeSeries, WriteRequest, PROMETHEUS_STALE_NAN};
use sonda_core::schedule::multi_runner::launch_multi_compiled;

/// Spawn an HTTP listener that accepts POSTs, snappy-decodes the body, parses
/// the protobuf `WriteRequest`, and pushes every `TimeSeries` (with arrival
/// timestamp) into a shared vector.
fn spawn_capture_listener() -> (
    String,
    Arc<Mutex<Vec<(Instant, TimeSeries)>>>,
    Arc<AtomicBool>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/api/v1/write");
    let captured: Arc<Mutex<Vec<(Instant, TimeSeries)>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    let captured_for_thread = Arc::clone(&captured);
    let stop_for_thread = Arc::clone(&stop);
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");

    thread::spawn(move || {
        loop {
            if stop_for_thread.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();

                    // Read headers and parse Content-Length.
                    let mut buf = Vec::with_capacity(4096);
                    let mut tmp = [0u8; 1024];
                    let mut content_length: Option<usize> = None;
                    let mut header_end: Option<usize> = None;
                    loop {
                        let n = match stream.read(&mut tmp) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(_) => break,
                        };
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(idx) = find_double_crlf(&buf) {
                            header_end = Some(idx + 4);
                            let header_str = std::str::from_utf8(&buf[..idx]).unwrap_or("");
                            for line in header_str.split("\r\n") {
                                let lower = line.to_ascii_lowercase();
                                if let Some(rest) = lower.strip_prefix("content-length:") {
                                    content_length = rest.trim().parse().ok();
                                }
                            }
                            break;
                        }
                    }
                    let header_end = header_end.unwrap_or(buf.len());
                    let cl = content_length.unwrap_or(0);
                    while buf.len() < header_end + cl {
                        let n = match stream.read(&mut tmp) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(_) => break,
                        };
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body = &buf[header_end..header_end + cl.min(buf.len() - header_end)];

                    if let Ok(uncompressed) = snap::raw::Decoder::new().decompress_vec(body) {
                        if let Ok(req) = WriteRequest::decode(uncompressed.as_slice()) {
                            let now = Instant::now();
                            let mut g = captured_for_thread.lock().unwrap();
                            for ts in req.timeseries {
                                g.push((now, ts));
                            }
                        } else {
                            eprintln!("listener: WriteRequest decode failed");
                        }
                    } else {
                        eprintln!("listener: snappy decode failed (body len {})", body.len());
                    }

                    let _ = stream.write_all(
                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    let _ = stream.flush();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    });

    (url, captured, stop)
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn label_value<'a>(ts: &'a TimeSeries, name: &str) -> Option<&'a str> {
    ts.labels
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.value.as_str())
}

#[test]
fn workshop_paused_finished_cascade_emits_stale_marker_via_multi_runner() {
    let (url, captured, stop_listener) = spawn_capture_listener();

    // Workshop YAML compressed: 1500ms run, 200ms up / 400ms down (cycle 600ms).
    // delay.open: 50ms, delay.close: 0s. batch_size: 1 so each write flushes.
    //
    // Timeline (ms):
    //   0..200    primary UP   (value=1.0). Gate `>1` => CLOSED. downstream Pending->Paused.
    //   200..600  primary DOWN (value=2.0). Gate OPEN. delay.open=50ms debounce.
    //   ~250      open commits. downstream Paused->Running. accumulates recent_metrics.
    //   600..800  primary UP. Gate CLOSE edge. delay.close=0 => commit immediately,
    //             close-emit fires (stale marker) and downstream goes Paused.
    //   800..1200 primary DOWN. Gate OPEN. open-commit at ~850. Running again.
    //   1200..1500 primary UP. Gate CLOSE. close-emit again at ~1200.
    //   1500ms    duration expires. Top-of-loop catches -> DurationExpired.
    //             Tail invokes close-emit (drained-empty buffer => no-op).
    //
    // Expected: at least one stale-NaN sample for `bgp_oper_state`.
    let yaml = format!(
        r#"
version: 2
scenario_name: workshop-paused-finished-repro
defaults:
  rate: 50
  duration: 1500ms
  encoder:
    type: remote_write
  sink:
    type: remote_write
    url: "{url}"
    batch_size: 1
scenarios:
  - id: primary_flap
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 200ms
      down_duration: 400ms
      enum: oper_state
  - id: bgp_oper_state_down
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 2.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
"#
    );

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(&yaml, &resolver).expect("compile must succeed");

    let shutdown = Arc::new(AtomicBool::new(true));
    let handles =
        launch_multi_compiled(compiled, Arc::clone(&shutdown)).expect("launch must succeed");
    assert_eq!(handles.len(), 2, "must launch primary + downstream");

    // Wait for both threads to finish (they exit when duration expires).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut handles = handles;
    while Instant::now() < deadline && handles.iter().any(|h| h.is_alive()) {
        thread::sleep(Duration::from_millis(50));
    }
    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("thread join");
    }

    // Give the listener a beat to drain in-flight POSTs.
    thread::sleep(Duration::from_millis(200));
    stop_listener.store(true, std::sync::atomic::Ordering::SeqCst);

    let captured = captured.lock().unwrap().clone();
    eprintln!("captured {} timeseries total", captured.len());

    let mut bgp_count = 0usize;
    let mut bgp_stale_count = 0usize;
    let mut primary_count = 0usize;
    for (arrival, ts) in &captured {
        let name = label_value(ts, "__name__").unwrap_or("(no __name__)");
        let is_stale = ts
            .samples
            .iter()
            .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits());
        if name == "bgp_oper_state" {
            bgp_count += 1;
            if is_stale {
                bgp_stale_count += 1;
            }
            eprintln!(
                "  bgp_oper_state arrival={:?} samples={} stale={} values={:?}",
                arrival.elapsed(),
                ts.samples.len(),
                is_stale,
                ts.samples.iter().map(|s| s.value).collect::<Vec<_>>()
            );
        } else if name == "interface_oper_state" {
            primary_count += 1;
        }
    }

    eprintln!(
        "primary_flap series count: {}, bgp_oper_state series count: {}, bgp_stale_count: {}",
        primary_count, bgp_count, bgp_stale_count
    );

    assert!(
        bgp_count > 0,
        "expected at least one bgp_oper_state sample to reach the sink \
         (downstream did become Running). got 0 — gate never opened?"
    );
    assert!(
        bgp_stale_count > 0,
        "BUG REPRO: expected >=1 stale-NaN bgp_oper_state sample to reach the sink \
         (close-emit on the running->paused commit). got 0 stale among {bgp_count} bgp samples. \
         primary count: {primary_count}, total captured: {}",
        captured.len()
    );
}

#[test]
fn workshop_paused_finished_cascade_default_batch_size_emits_stale_marker() {
    // Same scenario as above but WITHOUT explicit batch_size — falls to
    // DEFAULT_BATCH_SIZE = 5. close-emit writes one stale TimeSeries which
    // alone does NOT trigger auto-flush. Verifies invoke_close_emit's
    // post-write flush actually delivers it.
    let (url, captured, stop_listener) = spawn_capture_listener();

    let yaml = format!(
        r#"
version: 2
scenario_name: workshop-paused-finished-default-batch
defaults:
  rate: 50
  duration: 1500ms
  encoder:
    type: remote_write
  sink:
    type: remote_write
    url: "{url}"
scenarios:
  - id: primary_flap
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 200ms
      down_duration: 400ms
      enum: oper_state
  - id: bgp_oper_state_down
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 2.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
"#
    );

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(&yaml, &resolver).expect("compile must succeed");

    let shutdown = Arc::new(AtomicBool::new(true));
    let handles =
        launch_multi_compiled(compiled, Arc::clone(&shutdown)).expect("launch must succeed");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut handles = handles;
    while Instant::now() < deadline && handles.iter().any(|h| h.is_alive()) {
        thread::sleep(Duration::from_millis(50));
    }
    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("thread join");
    }

    thread::sleep(Duration::from_millis(200));
    stop_listener.store(true, std::sync::atomic::Ordering::SeqCst);

    let captured = captured.lock().unwrap().clone();
    let bgp_stale_count = captured
        .iter()
        .filter(|(_, ts)| {
            label_value(ts, "__name__") == Some("bgp_oper_state")
                && ts
                    .samples
                    .iter()
                    .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    let bgp_total: usize = captured
        .iter()
        .filter(|(_, ts)| label_value(ts, "__name__") == Some("bgp_oper_state"))
        .count();

    eprintln!(
        "default-batch run: {} bgp_oper_state samples, {} stale",
        bgp_total, bgp_stale_count
    );

    assert!(
        bgp_stale_count > 0,
        "DEFAULT-BATCH BUG REPRO: expected >=1 stale-NaN bgp_oper_state sample with default batch_size. \
         got {bgp_stale_count} stale among {bgp_total} bgp samples"
    );
}

/// Test A — multi-entry workshop cascade at compressed time.
///
/// Mirrors the workshop's actual cascade: 1 primary + 7 downstream gated metrics
/// (the 6 BGP counters from the workshop YAML + the operational status) all
/// gated on the same `primary_flap` upstream. Each entry has its own GateBus
/// subscriber, its own debounce, its own close-emit closure, its own
/// recent_metrics buffer. If there's a multi-subscriber race in the GateBus
/// broadcast or a per-entry stats-buffer issue, this exposes it.
///
/// Expectation: every gated metric has >=1 stale-NaN sample reach the wire.
#[test]
fn workshop_paused_finished_multi_entry_cascade_each_metric_emits_stale_marker() {
    let (url, captured, stop_listener) = spawn_capture_listener();

    // Same compressed timing as Test workshop_paused_finished_cascade_emits_stale_marker_via_multi_runner
    // (1500ms run, 200ms up / 400ms down, delay.open=50ms / delay.close=0s, batch_size=1)
    // but with all 7 gated metrics from the workshop YAML.
    let yaml = format!(
        r#"
version: 2
scenario_name: workshop-multi-entry-cascade
defaults:
  rate: 50
  duration: 1500ms
  encoder:
    type: remote_write
  sink:
    type: remote_write
    url: "{url}"
    batch_size: 1
scenarios:
  - id: primary_flap
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 200ms
      down_duration: 400ms
      enum: oper_state
  - id: bgp_oper_state_down
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 2.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
  - id: bgp_active_routes_zero
    signal_type: metrics
    name: bgp_active_routes
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
  - id: bgp_pfx_out_zero
    signal_type: metrics
    name: bgp_pfx_out
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
  - id: bgp_pfx_received_zero
    signal_type: metrics
    name: bgp_pfx_received
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
  - id: bgp_pfx_sent_zero
    signal_type: metrics
    name: bgp_pfx_sent
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
  - id: bgp_msg_rcvd_zero
    signal_type: metrics
    name: bgp_msg_rcvd
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
  - id: bgp_msg_sent_zero
    signal_type: metrics
    name: bgp_msg_sent
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
"#
    );

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(&yaml, &resolver).expect("compile must succeed");

    let shutdown = Arc::new(AtomicBool::new(true));
    let handles =
        launch_multi_compiled(compiled, Arc::clone(&shutdown)).expect("launch must succeed");
    assert_eq!(handles.len(), 8, "must launch primary + 7 downstream");

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut handles = handles;
    while Instant::now() < deadline && handles.iter().any(|h| h.is_alive()) {
        thread::sleep(Duration::from_millis(50));
    }
    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("thread join");
    }

    thread::sleep(Duration::from_millis(200));
    stop_listener.store(true, std::sync::atomic::Ordering::SeqCst);

    let captured = captured.lock().unwrap().clone();
    eprintln!("captured {} timeseries total", captured.len());

    let metric_names = [
        "bgp_oper_state",
        "bgp_active_routes",
        "bgp_pfx_out",
        "bgp_pfx_received",
        "bgp_pfx_sent",
        "bgp_msg_rcvd",
        "bgp_msg_sent",
    ];

    let mut totals = std::collections::BTreeMap::new();
    let mut stales = std::collections::BTreeMap::new();
    for name in &metric_names {
        totals.insert(*name, 0usize);
        stales.insert(*name, 0usize);
    }

    for (_arrival, ts) in &captured {
        let name = label_value(ts, "__name__").unwrap_or("(no __name__)");
        for known in &metric_names {
            if name == *known {
                *totals.get_mut(known).unwrap() += 1;
                let is_stale = ts
                    .samples
                    .iter()
                    .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits());
                if is_stale {
                    *stales.get_mut(known).unwrap() += 1;
                }
            }
        }
    }

    for name in &metric_names {
        eprintln!(
            "metric {} total={} stale={}",
            name, totals[name], stales[name]
        );
    }

    let missing_stale: Vec<&&str> = metric_names.iter().filter(|n| stales[*n] == 0).collect();

    assert!(
        missing_stale.is_empty(),
        "MULTI-ENTRY RACE BUG REPRO: these metrics never received a stale-NaN sample: {:?}. \
         Per-metric breakdown: {:?}",
        missing_stale,
        totals
            .iter()
            .map(|(k, v)| (*k, *v, stales[k]))
            .collect::<Vec<_>>()
    );
}

/// Test B — single-entry at workshop-realistic time scale.
///
/// Workshop: 2m duration, 90s flap cycle, 10s open debounce, 0s close debounce.
/// Compressed-time tests run in microseconds — the workshop runs in seconds.
/// Maybe at real-millisecond magnitudes something different happens around
/// debounce reset, gate-edge ordering, or recent_metrics dedup.
///
/// Compressed here: 5s duration, 600ms up / 1200ms down (cycle 1.8s), 200ms
/// open / 0s close. ~2.7 cycles → at least 2 running→paused transitions.
#[test]
fn workshop_paused_finished_real_timescale_emits_stale_marker() {
    let (url, captured, stop_listener) = spawn_capture_listener();

    let yaml = format!(
        r#"
version: 2
scenario_name: workshop-real-timescale-repro
defaults:
  rate: 5
  duration: 5s
  encoder:
    type: remote_write
  sink:
    type: remote_write
    url: "{url}"
    batch_size: 1
scenarios:
  - id: primary_flap
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 600ms
      down_duration: 1200ms
      enum: oper_state
  - id: bgp_oper_state_down
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 2.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 200ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
"#
    );

    let resolver = InMemoryPackResolver::new();
    let compiled = compile_scenario_file_compiled(&yaml, &resolver).expect("compile must succeed");

    let shutdown = Arc::new(AtomicBool::new(true));
    let handles =
        launch_multi_compiled(compiled, Arc::clone(&shutdown)).expect("launch must succeed");
    assert_eq!(handles.len(), 2, "must launch primary + downstream");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut handles = handles;
    while Instant::now() < deadline && handles.iter().any(|h| h.is_alive()) {
        thread::sleep(Duration::from_millis(100));
    }
    for handle in &mut handles {
        handle
            .join(Some(Duration::from_secs(3)))
            .expect("thread join");
    }

    thread::sleep(Duration::from_millis(300));
    stop_listener.store(true, std::sync::atomic::Ordering::SeqCst);

    let captured = captured.lock().unwrap().clone();
    eprintln!("captured {} timeseries total", captured.len());

    let mut bgp_count = 0usize;
    let mut bgp_stale_count = 0usize;
    let mut primary_count = 0usize;
    for (_arrival, ts) in &captured {
        let name = label_value(ts, "__name__").unwrap_or("(no __name__)");
        let is_stale = ts
            .samples
            .iter()
            .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits());
        if name == "bgp_oper_state" {
            bgp_count += 1;
            if is_stale {
                bgp_stale_count += 1;
            }
        } else if name == "interface_oper_state" {
            primary_count += 1;
        }
    }

    eprintln!(
        "REAL-TIMESCALE — primary={} bgp_total={} bgp_stale={}",
        primary_count, bgp_count, bgp_stale_count
    );

    assert!(
        bgp_count > 0,
        "expected >=1 bgp_oper_state sample. got 0 — gate never opened?"
    );
    assert!(
        bgp_stale_count > 0,
        "REAL-TIMESCALE BUG REPRO: expected >=1 stale-NaN bgp_oper_state sample at \
         workshop-realistic timing scales. got 0 stale among {bgp_count} bgp samples. \
         primary count: {primary_count}, total captured: {}",
        captured.len()
    );
}

/// Test C — Workshop scenario through the actual `sonda-server` HTTP binary.
///
/// If Test A (multi-entry) and Test B (real timescale) both pass but the
/// workshop's empirical observation is broken, the divergence has to live in
/// the HTTP server layer or the released binary itself. This test spawns the
/// `sonda-server` binary directly, posts the workshop YAML to `/scenarios`,
/// and walks the captured wire bytes for stale-NaN.
///
/// Skipped automatically if the workspace `sonda-server` binary isn't built
/// with the `remote-write` feature (locating it via target/{profile}/sonda-server).
#[test]
fn workshop_paused_finished_through_server_binary_emits_stale_marker() {
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    // Locate target/debug/sonda-server (or target/release/sonda-server).
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .expect("manifest dir parent")
        .to_path_buf();
    let candidates = [
        workspace_root.join("target/debug/sonda-server"),
        workspace_root.join("target/release/sonda-server"),
    ];
    let binary = match candidates.iter().find(|p| p.exists()) {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "SKIP: sonda-server binary not found in target/{{debug,release}}; \
                 build it first with `cargo build -p sonda-server --features remote-write`"
            );
            return;
        }
    };

    let (sink_url, captured, stop_listener) = spawn_capture_listener();

    // Spawn sonda-server on an ephemeral port. Read the announce.
    let mut child = Command::new(&binary)
        .args(["--port", "0", "--bind", "127.0.0.1"])
        .env_remove("SONDA_API_KEY")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sonda-server");

    let stdout = child.stdout.take().expect("piped stdout");
    let port = {
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read announce");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("announce json");
        v["sonda_server"]["port"].as_u64().expect("port") as u16
    };

    // Use a struct-drop guard so the child is killed even on panic.
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }
    let mut guard = ChildGuard(child);

    let yaml = format!(
        r#"
version: 2
scenario_name: workshop-via-server-binary
defaults:
  rate: 50
  duration: 1500ms
  encoder:
    type: remote_write
  sink:
    type: remote_write
    url: "{sink_url}"
    batch_size: 1
scenarios:
  - id: primary_flap
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 200ms
      down_duration: 400ms
      enum: oper_state
  - id: bgp_oper_state_down
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 2.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
"#
    );

    // POST the YAML (raw TCP write to avoid pulling reqwest/ureq into sonda-core dev-deps).
    let post_body = yaml.as_bytes();
    let request = format!(
        "POST /scenarios HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Content-Type: application/x-yaml\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        post_body.len()
    );
    let mut server_stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to sonda-server");
    server_stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok();
    server_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    server_stream
        .write_all(request.as_bytes())
        .expect("write request headers");
    server_stream
        .write_all(post_body)
        .expect("write request body");
    server_stream.flush().ok();

    // Drain response so server can finalize.
    let mut response = Vec::new();
    server_stream.read_to_end(&mut response).ok();
    let response_str = String::from_utf8_lossy(&response);
    eprintln!(
        "server POST response head: {}",
        &response_str[..response_str.len().min(400)]
    );
    assert!(
        response_str.starts_with("HTTP/1.1 201") || response_str.starts_with("HTTP/1.1 200"),
        "POST /scenarios should return 201/200: {}",
        &response_str[..response_str.len().min(400)]
    );

    // Wait for the scenarios to drive their full duration (1.5s + slack).
    thread::sleep(Duration::from_millis(2500));
    stop_listener.store(true, std::sync::atomic::Ordering::SeqCst);

    // Drop the server.
    drop(&mut guard);
    guard.0.kill().ok();
    guard.0.wait().ok();

    let captured = captured.lock().unwrap().clone();
    eprintln!(
        "SERVER-BINARY: captured {} timeseries total",
        captured.len()
    );

    let mut bgp_count = 0usize;
    let mut bgp_stale_count = 0usize;
    let mut primary_count = 0usize;
    for (_arrival, ts) in &captured {
        let name = label_value(ts, "__name__").unwrap_or("(no __name__)");
        let is_stale = ts
            .samples
            .iter()
            .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits());
        if name == "bgp_oper_state" {
            bgp_count += 1;
            if is_stale {
                bgp_stale_count += 1;
            }
            eprintln!(
                "  bgp_oper_state samples={} stale={} values={:?}",
                ts.samples.len(),
                is_stale,
                ts.samples.iter().map(|s| s.value).collect::<Vec<_>>()
            );
        } else if name == "interface_oper_state" {
            primary_count += 1;
        }
    }

    eprintln!(
        "SERVER-BINARY — primary={} bgp_total={} bgp_stale={}",
        primary_count, bgp_count, bgp_stale_count
    );

    assert!(
        bgp_count > 0,
        "expected >=1 bgp_oper_state sample to reach the sink. got 0 — gate never opened?"
    );
    assert!(
        bgp_stale_count > 0,
        "HTTP-SERVER-LAYER BUG REPRO: expected >=1 stale-NaN bgp_oper_state sample \
         when running via the sonda-server binary. got 0 stale among {bgp_count} bgp samples. \
         primary count: {primary_count}, total captured: {}",
        captured.len()
    );
}

/// Test F — workshop's environmental shape: baseline scenarios already running
/// when the cascade is POSTed.
///
/// Workshop POSTs `srl2-metrics.yaml` (~9 entries, expanded via packs into ~30
/// metric streams) to sonda-server first. Those baseline scenarios run at
/// `rate: 0.1` with the **default sink** (Stdout) — they do NOT use
/// `remote_write`. Once they're at steady state, the cascade YAML is POSTed
/// (separate `scenario_name`) with `remote_write` to Prometheus. Workshop
/// observes the cascade's gated metrics never produce a close-emit stale
/// marker on `running -> paused`.
///
/// This test reproduces that two-POST shape against the real `sonda-server`
/// binary: ~30 baseline metric scenarios (file sink to `/dev/null`, mimicking
/// stdout) run concurrently with the workshop cascade (remote_write to TCP
/// capture).
///
/// Compressed timing: baseline rate=20, cascade rate=50, cascade duration
/// 1500ms, flap up=200ms / down=400ms, delay open=50ms / close=0s.
#[test]
fn workshop_cascade_with_baseline_scenarios_emits_stale_marker() {
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .expect("manifest dir parent")
        .to_path_buf();
    let candidates = [
        workspace_root.join("target/debug/sonda-server"),
        workspace_root.join("target/release/sonda-server"),
    ];
    let binary = match candidates.iter().find(|p| p.exists()) {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "SKIP: sonda-server binary not found in target/{{debug,release}}; \
                 build it first with `cargo build -p sonda-server --features remote-write`"
            );
            return;
        }
    };

    let (sink_url, captured, stop_listener) = spawn_capture_listener();

    let mut child = Command::new(&binary)
        .args(["--port", "0", "--bind", "127.0.0.1"])
        .env_remove("SONDA_API_KEY")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sonda-server");

    let stdout = child.stdout.take().expect("piped stdout");
    let port = {
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read announce");
        let v: serde_json::Value = serde_json::from_str(line.trim()).expect("announce json");
        v["sonda_server"]["port"].as_u64().expect("port") as u16
    };

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }
    let mut guard = ChildGuard(child);

    // Helper: POST a YAML body and assert 2xx.
    let post_yaml = |port: u16, yaml: &str, label: &str| {
        let post_body = yaml.as_bytes();
        let request = format!(
            "POST /scenarios HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Content-Type: application/x-yaml\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            post_body.len()
        );
        let mut s =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to sonda-server");
        s.set_write_timeout(Some(Duration::from_secs(5))).ok();
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        s.write_all(request.as_bytes())
            .expect("write request headers");
        s.write_all(post_body).expect("write request body");
        s.flush().ok();
        let mut response = Vec::new();
        s.read_to_end(&mut response).ok();
        let response_str = String::from_utf8_lossy(&response);
        eprintln!(
            "{label}: server response head: {}",
            &response_str[..response_str.len().min(300)]
        );
        assert!(
            response_str.starts_with("HTTP/1.1 201") || response_str.starts_with("HTTP/1.1 200"),
            "{label}: POST /scenarios should return 201/200: {}",
            &response_str[..response_str.len().min(400)]
        );
    };

    // ------------------------------------------------------------------
    // Baseline body — ~30 metric scenarios at rate 20, file sink → /dev/null.
    // Mimics srl2-metrics.yaml's role: pre-existing scenarios using the default
    // (non-remote-write) sink path.
    // ------------------------------------------------------------------
    let mut baseline_yaml = String::from(
        "version: 2\n\
         scenario_name: workshop-baseline-srl2\n\
         defaults:\n  \
           rate: 20\n  \
           duration: 5s\n  \
           sink:\n    \
             type: file\n    \
             path: /dev/null\n\
         scenarios:\n",
    );
    let baseline_metrics = [
        (
            "srl_bgp_neighbor_state_p1",
            "bgp_neighbor_state",
            1.0,
            "10.1.2.1",
        ),
        ("srl_bgp_admin_state_p1", "bgp_admin_state", 1.0, "10.1.2.1"),
        (
            "srl_bgp_oper_state_p1",
            "bgp_oper_state_baseline",
            1.0,
            "10.1.2.1",
        ),
        (
            "srl_bgp_received_routes_p1",
            "bgp_received_routes_baseline",
            10.0,
            "10.1.2.1",
        ),
        (
            "srl_bgp_prefixes_accepted_p1",
            "bgp_prefixes_accepted_baseline",
            10.0,
            "10.1.2.1",
        ),
        (
            "srl_bgp_sent_routes_p1",
            "bgp_sent_routes_baseline",
            10.0,
            "10.1.2.1",
        ),
        (
            "srl_bgp_active_routes_p1",
            "bgp_active_routes_baseline",
            10.0,
            "10.1.2.1",
        ),
        (
            "srl_bgp_neighbor_state_p2",
            "bgp_neighbor_state",
            1.0,
            "10.1.7.1",
        ),
        ("srl_bgp_admin_state_p2", "bgp_admin_state", 1.0, "10.1.7.1"),
        (
            "srl_bgp_oper_state_p2",
            "bgp_oper_state_baseline",
            1.0,
            "10.1.7.1",
        ),
        (
            "srl_bgp_received_routes_p2",
            "bgp_received_routes_baseline",
            10.0,
            "10.1.7.1",
        ),
        (
            "srl_bgp_prefixes_accepted_p2",
            "bgp_prefixes_accepted_baseline",
            10.0,
            "10.1.7.1",
        ),
        (
            "srl_bgp_sent_routes_p2",
            "bgp_sent_routes_baseline",
            10.0,
            "10.1.7.1",
        ),
        (
            "srl_bgp_active_routes_p2",
            "bgp_active_routes_baseline",
            10.0,
            "10.1.7.1",
        ),
        (
            "srl_bgp_neighbor_state_p3",
            "bgp_neighbor_state",
            4.0,
            "10.1.11.1",
        ),
        (
            "srl_bgp_admin_state_p3",
            "bgp_admin_state",
            1.0,
            "10.1.11.1",
        ),
        (
            "srl_bgp_oper_state_p3",
            "bgp_oper_state_baseline",
            5.0,
            "10.1.11.1",
        ),
        (
            "srl_bgp_received_routes_p3",
            "bgp_received_routes_baseline",
            0.0,
            "10.1.11.1",
        ),
        (
            "srl_bgp_prefixes_accepted_p3",
            "bgp_prefixes_accepted_baseline",
            0.0,
            "10.1.11.1",
        ),
        ("srl_intf_admin_e1_1", "intf_admin", 1.0, "ethernet-1/1"),
        ("srl_intf_oper_e1_1", "intf_oper", 1.0, "ethernet-1/1"),
        ("srl_intf_admin_e1_10", "intf_admin", 1.0, "ethernet-1/10"),
        ("srl_intf_oper_e1_10", "intf_oper", 1.0, "ethernet-1/10"),
        ("srl_intf_admin_e1_11", "intf_admin", 1.0, "ethernet-1/11"),
        ("srl_intf_oper_e1_11", "intf_oper", 2.0, "ethernet-1/11"),
        ("ping_result_srl2", "ping_result_code", 0.0, "srl2"),
        ("ping_rtt_srl2", "ping_average_response_ms", 1.5, "srl2"),
        ("system_cpu_srl2", "cpu_used", 30.0, "srl2"),
        ("system_mem_srl2", "memory_utilization", 38.0, "srl2"),
        ("system_uptime_srl2", "device_uptime", 36000.0, "srl2"),
    ];
    assert_eq!(baseline_metrics.len(), 30, "baseline must have 30 entries");

    for (id, name, value, who) in baseline_metrics.iter() {
        baseline_yaml.push_str(&format!(
            "  - id: {id}\n    \
               signal_type: metrics\n    \
               name: {name}\n    \
               generator:\n      \
                 type: constant\n      \
                 value: {value}\n    \
               labels:\n      \
                 source: srl2\n      \
                 endpoint: \"{who}\"\n"
        ));
    }

    post_yaml(port, &baseline_yaml, "BASELINE");

    // Let baseline reach steady state.
    thread::sleep(Duration::from_millis(500));

    // ------------------------------------------------------------------
    // Cascade body — 6 gated BGP metrics on while: primary_flap > 1.
    // remote_write to capture listener.
    // ------------------------------------------------------------------
    let cascade_yaml = format!(
        r#"
version: 2
scenario_name: workshop-cascade-incident
defaults:
  rate: 50
  duration: 1500ms
  encoder:
    type: remote_write
  sink:
    type: remote_write
    url: "{sink_url}"
    batch_size: 1
  labels:
    device: srl1
    pipeline: direct
    collection_type: gnmi
    source: workshop-cascade
scenarios:
  - id: primary_flap
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 200ms
      down_duration: 400ms
      enum: oper_state
    labels:
      name: ethernet-1/1
      intf_role: peer

  - id: bgp_oper_state_down
    signal_type: metrics
    name: bgp_oper_state
    generator:
      type: constant
      value: 2.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
      neighbor_asn: "65102"

  - id: bgp_neighbor_state_down
    signal_type: metrics
    name: bgp_neighbor_state
    generator:
      type: constant
      value: 1.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
      neighbor_asn: "65102"

  - id: bgp_prefixes_accepted_zero
    signal_type: metrics
    name: bgp_prefixes_accepted
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
      neighbor_asn: "65102"

  - id: bgp_received_routes_zero
    signal_type: metrics
    name: bgp_received_routes
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
      neighbor_asn: "65102"

  - id: bgp_sent_routes_zero
    signal_type: metrics
    name: bgp_sent_routes
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
      neighbor_asn: "65102"

  - id: bgp_active_routes_zero
    signal_type: metrics
    name: bgp_active_routes
    generator:
      type: constant
      value: 0.0
    while:
      ref: primary_flap
      op: ">"
      value: 1
    delay:
      open: 50ms
      close: 0s
    labels:
      peer_address: "10.1.2.2"
      neighbor_asn: "65102"
"#
    );

    post_yaml(port, &cascade_yaml, "CASCADE");

    // Wait for cascade to drive its full duration (1.5s + slack), then stop.
    thread::sleep(Duration::from_millis(2500));
    stop_listener.store(true, std::sync::atomic::Ordering::SeqCst);

    drop(&mut guard);
    guard.0.kill().ok();
    guard.0.wait().ok();

    let captured = captured.lock().unwrap().clone();
    eprintln!(
        "TWO-POST: captured {} timeseries total to remote_write listener",
        captured.len()
    );

    let cascade_metric_names = [
        "bgp_oper_state",
        "bgp_neighbor_state",
        "bgp_prefixes_accepted",
        "bgp_received_routes",
        "bgp_sent_routes",
        "bgp_active_routes",
    ];

    let mut totals = std::collections::BTreeMap::new();
    let mut stales = std::collections::BTreeMap::new();
    for name in &cascade_metric_names {
        totals.insert(*name, 0usize);
        stales.insert(*name, 0usize);
    }
    let mut primary_count = 0usize;
    let mut other_count = 0usize;

    for (_arrival, ts) in &captured {
        let name = label_value(ts, "__name__").unwrap_or("(no __name__)");
        if name == "interface_oper_state" {
            primary_count += 1;
            continue;
        }
        let mut matched = false;
        for known in &cascade_metric_names {
            if name == *known {
                matched = true;
                *totals.get_mut(known).unwrap() += 1;
                let is_stale = ts
                    .samples
                    .iter()
                    .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits());
                if is_stale {
                    *stales.get_mut(known).unwrap() += 1;
                }
            }
        }
        if !matched {
            other_count += 1;
        }
    }

    eprintln!(
        "TWO-POST — primary_flap={} other={} (baseline scenarios should NOT \
         appear here; they sink to /dev/null)",
        primary_count, other_count
    );
    for name in &cascade_metric_names {
        eprintln!(
            "  cascade metric {} total={} stale={}",
            name, totals[name], stales[name]
        );
    }

    let missing_stale: Vec<&&str> = cascade_metric_names
        .iter()
        .filter(|n| stales[*n] == 0)
        .collect();

    assert!(
        primary_count > 0,
        "primary_flap never reached the sink — cascade didn't run at all"
    );
    assert!(
        missing_stale.is_empty(),
        "WORKSHOP-ENV BUG REPRO: with ~30 baseline scenarios concurrent, these \
         cascade metrics never received a stale-NaN sample at gate-close: {:?}. \
         Per-metric breakdown: {:?}",
        missing_stale,
        totals
            .iter()
            .map(|(k, v)| (*k, *v, stales[k]))
            .collect::<Vec<_>>()
    );
}
