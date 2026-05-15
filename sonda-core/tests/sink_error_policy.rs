//! Integration tests for the `on_sink_error` policy applied at scenario level.
//!
//! Verifies that under `Warn` the runner thread keeps ticking past sink failures
//! and accumulates `total_sink_failures`, while under `Fail` the thread exits
//! with `Err(SondaError::Sink(_))`.

#![cfg(all(feature = "config", feature = "http"))]

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use sonda_core::config::{BaseScheduleConfig, LogScenarioConfig, OnSinkError, ScenarioEntry};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{LogGeneratorConfig, TemplateConfig};
use sonda_core::schedule::launch::launch_scenario;
use sonda_core::sink::SinkConfig;
use sonda_core::SondaError;

fn mock_loki_listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("local addr").port();
    let url = format!("http://127.0.0.1:{port}");
    (listener, url)
}

fn read_http_body(stream: &mut TcpStream) -> Vec<u8> {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return Vec::new();
        }
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("content-length:") {
            let val = lower["content-length:".len()..].trim().to_string();
            content_length = val.parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok();
    body
}

fn always_500_listener_thread(listener: TcpListener, stop: Arc<AtomicBool>) {
    listener
        .set_nonblocking(true)
        .expect("set non-blocking on listener");
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).ok();
                let _ = read_http_body(&mut stream);
                let resp = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\
                            Connection: close\r\n\r\n";
                stream.write_all(resp.as_bytes()).ok();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn build_log_entry(name: &str, sink: SinkConfig, policy: OnSinkError) -> ScenarioEntry {
    ScenarioEntry::Logs(LogScenarioConfig {
        base: BaseScheduleConfig {
            name: name.to_string(),
            rate: 50.0,
            duration: Some("1500ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: policy,
        },
        generator: LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "policy probe".to_string(),
                field_pools: BTreeMap::new(),
            }],
            severity_weights: None,
            seed: Some(0),
        },
        encoder: EncoderConfig::JsonLines { precision: None },
    })
}

#[test]
fn warn_policy_keeps_thread_alive_under_persistent_sink_failure() {
    let (listener, url) = mock_loki_listener();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_listener = Arc::clone(&stop);
    let listener_thread =
        thread::spawn(move || always_500_listener_thread(listener, stop_listener));

    let entry = build_log_entry(
        "warn_persistent",
        SinkConfig::Loki {
            url,
            batch_size: Some(5),
            max_buffer_age: Some("0s".to_string()),
            retry: None,
        },
        OnSinkError::Warn,
    );

    let shutdown = Arc::new(AtomicBool::new(true));
    let mut handle = launch_scenario(
        "warn-persistent".to_string(),
        entry,
        Arc::clone(&shutdown),
        None,
    )
    .expect("launch must succeed");

    thread::sleep(Duration::from_millis(800));
    let snap_mid = handle.stats_snapshot();
    assert!(
        handle.is_alive(),
        "thread must still be alive under Warn policy"
    );
    assert!(
        snap_mid.total_sink_failures > 0,
        "Warn policy must already be recording sink failures mid-run, got {}",
        snap_mid.total_sink_failures
    );

    handle.join(Some(Duration::from_secs(3))).expect("join Ok");

    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    listener_thread.join().ok();

    let snap = handle.stats_snapshot();
    assert!(
        snap.total_sink_failures > 0,
        "Warn policy must record total_sink_failures > 0, got {}",
        snap.total_sink_failures
    );
    assert!(
        snap.last_sink_error.is_some(),
        "last_sink_error must be Some"
    );
    assert!(
        snap.consecutive_failures > 0,
        "consecutive_failures must be > 0"
    );
}

#[test]
fn warn_policy_keeps_delivery_health_gated_on_real_flush() {
    // Bind then drop a listener so the port is guaranteed free — every flush
    // against it will fail to connect.
    let dead_url = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let port = listener.local_addr().expect("local addr").port();
        format!("http://127.0.0.1:{port}")
    };

    let entry = build_log_entry(
        "warn_dead_url",
        SinkConfig::Loki {
            url: dead_url,
            batch_size: Some(500),
            max_buffer_age: Some("250ms".to_string()),
            retry: None,
        },
        OnSinkError::Warn,
    );

    let shutdown = Arc::new(AtomicBool::new(true));
    let mut handle = launch_scenario(
        "warn-dead-url".to_string(),
        entry,
        Arc::clone(&shutdown),
        None,
    )
    .expect("launch must succeed");

    handle.join(Some(Duration::from_secs(3))).expect("join Ok");

    let snap = handle.stats_snapshot();
    assert!(
        snap.total_sink_failures > 0,
        "flushes against a dead URL must record sink failures, got {}",
        snap.total_sink_failures
    );
    assert!(
        snap.last_successful_write_at.is_none(),
        "no write ever delivered — buffered writes must not advance last_successful_write_at, got {:?}",
        snap.last_successful_write_at
    );
    assert!(
        snap.consecutive_failures > 0,
        "a buffered write must not reset consecutive_failures, got {}",
        snap.consecutive_failures
    );
}

#[test]
fn fail_policy_exits_thread_with_sink_error() {
    let (listener, url) = mock_loki_listener();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_listener = Arc::clone(&stop);
    let listener_thread =
        thread::spawn(move || always_500_listener_thread(listener, stop_listener));

    let entry = build_log_entry(
        "fail_propagates",
        SinkConfig::Loki {
            url,
            batch_size: Some(5),
            max_buffer_age: Some("0s".to_string()),
            retry: None,
        },
        OnSinkError::Fail,
    );

    let shutdown = Arc::new(AtomicBool::new(true));
    let mut handle = launch_scenario(
        "fail-propagates".to_string(),
        entry,
        Arc::clone(&shutdown),
        None,
    )
    .expect("launch must succeed");

    let join_result = handle.join(Some(Duration::from_secs(5)));

    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    listener_thread.join().ok();

    assert!(
        matches!(join_result, Err(SondaError::Sink(_))),
        "Fail policy must surface SondaError::Sink: {join_result:?}"
    );
    assert!(
        !handle.is_alive(),
        "alive flag must be false after thread exits"
    );
}
