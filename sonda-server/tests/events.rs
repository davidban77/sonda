//! End-to-end tests for `POST /events`.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn mock_loki_listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("local addr").port();
    let url = format!("http://127.0.0.1:{port}");
    (listener, url)
}

fn accept_one_and_respond(listener: TcpListener, status: u16) -> Vec<u8> {
    let (mut stream, _) = listener.accept().expect("accept connection");
    let body = read_http_body(&mut stream);
    let reason = if status < 300 { "OK" } else { "Error" };
    let resp =
        format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    stream.write_all(resp.as_bytes()).ok();
    body
}

fn read_http_body(stream: &mut TcpStream) -> Vec<u8> {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read header line");
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
    reader.read_exact(&mut body).expect("read body");
    body
}

fn spawn_loki_responder(listener: TcpListener, status: u16) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let body = accept_one_and_respond(listener, status);
        let _ = tx.send(body);
    });
    rx
}

#[test]
fn post_events_logs_happy_path_returns_200() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let mut path = std::env::temp_dir();
    path.push(format!("sonda-events-logs-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let body = serde_json::json!({
        "signal_type": "logs",
        "labels": {"event": "deploy_start"},
        "log": {
            "severity": "info",
            "message": "deploy started",
            "fields": {"actor": "ci"}
        },
        "encoder": {"type": "json_lines"},
        "sink": {"type": "file", "path": path.to_string_lossy()},
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("POST /events must succeed");

    assert_eq!(resp.status().as_u16(), 200, "happy path must return 200");
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["sent"], true);
    assert_eq!(json["signal_type"], "logs");
    assert!(
        json["latency_ms"].is_number(),
        "latency_ms must be present and numeric, got: {json}"
    );

    let contents = std::fs::read_to_string(&path).expect("read sink file");
    let _ = std::fs::remove_file(&path);
    assert!(
        contents.contains("\"deploy started\""),
        "sink file must contain encoded message, got: {contents}"
    );
}

#[test]
fn post_events_metrics_happy_path_returns_200() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let mut path = std::env::temp_dir();
    path.push(format!("sonda-events-metrics-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let body = serde_json::json!({
        "signal_type": "metrics",
        "labels": {"event": "deploy_start"},
        "metric": {
            "name": "deploy_event_total",
            "value": 1.0,
        },
        "encoder": {"type": "prometheus_text"},
        "sink": {"type": "file", "path": path.to_string_lossy()},
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("POST /events must succeed");

    assert_eq!(resp.status().as_u16(), 200);
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["sent"], true);
    assert_eq!(json["signal_type"], "metrics");
    assert!(json["latency_ms"].is_number());

    let contents = std::fs::read_to_string(&path).expect("read sink file");
    let _ = std::fs::remove_file(&path);
    assert!(
        contents.contains("deploy_event_total"),
        "sink file must contain metric name, got: {contents}"
    );
}

#[test]
fn post_events_malformed_json_returns_400() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body("{not valid json")
        .send()
        .expect("HTTP must succeed");

    assert_eq!(resp.status().as_u16(), 400);
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["error"], "bad_request");
}

#[test]
fn post_events_unknown_signal_type_returns_400() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let body = serde_json::json!({
        "signal_type": "traces",
        "encoder": {"type": "json_lines"},
        "sink": {"type": "stdout"},
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("HTTP must succeed");

    assert_eq!(resp.status().as_u16(), 400);
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["error"], "bad_request");
    let detail = json["detail"].as_str().expect("detail must be a string");
    assert!(
        detail.contains("traces") || detail.to_lowercase().contains("variant"),
        "detail must reference the bad tag, got: {detail}"
    );
}

#[test]
fn post_events_missing_required_log_field_returns_400() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let body = serde_json::json!({
        "signal_type": "logs",
        "log": {"severity": "info"},
        "encoder": {"type": "json_lines"},
        "sink": {"type": "stdout"},
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("HTTP must succeed");

    assert_eq!(resp.status().as_u16(), 400);
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["error"], "bad_request");
    assert!(json["detail"]
        .as_str()
        .map(|d| d.to_lowercase().contains("message"))
        .unwrap_or(false));
}

#[test]
fn post_events_invalid_sink_config_returns_422() {
    let (port, _guard) = common::start_server();
    let client = common::http_client();

    let body = serde_json::json!({
        "signal_type": "logs",
        "log": {"severity": "info", "message": "x"},
        "encoder": {"type": "json_lines"},
        "sink": {
            "type": "tcp",
            "address": "127.0.0.1:1",
            "retry": {
                "max_attempts": 0,
                "initial_backoff": "100ms",
                "max_backoff": "5s"
            }
        },
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("HTTP must succeed");

    assert_eq!(resp.status().as_u16(), 422);
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["error"], "unprocessable_entity");
}

fn long_timeout_http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build long-timeout HTTP client")
}

#[test]
fn post_events_sink_push_5xx_returns_502() {
    let (port, _guard) = common::start_server();
    let client = long_timeout_http_client();

    let (listener, base_url) = mock_loki_listener();
    let _resp_rx = spawn_loki_responder(listener, 502);

    let body = serde_json::json!({
        "signal_type": "logs",
        "labels": {"job": "sonda"},
        "log": {"severity": "info", "message": "deploy started"},
        "encoder": {"type": "json_lines"},
        "sink": {
            "type": "loki",
            "url": base_url,
            "batch_size": 1
        },
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("HTTP request must succeed at the transport level");

    assert_eq!(
        resp.status().as_u16(),
        502,
        "5xx from upstream sink must surface as 502"
    );
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["error"], "bad_gateway");
}

#[test]
fn post_events_without_auth_returns_401() {
    let (port, _guard) = common::start_server_with(&["--api-key", "test-secret"], &[]);
    let client = common::http_client();

    let body = serde_json::json!({
        "signal_type": "logs",
        "log": {"severity": "info", "message": "x"},
        "encoder": {"type": "json_lines"},
        "sink": {"type": "stdout"},
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("HTTP must succeed");

    assert_eq!(resp.status().as_u16(), 401);
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    assert_eq!(json["error"], "unauthorized");
}

#[test]
fn post_events_loopback_sink_attaches_warning() {
    let (port, _guard) = common::start_server();
    let client = long_timeout_http_client();

    let (listener, _) = mock_loki_listener();
    let actual_port = listener.local_addr().expect("local addr").port();
    let loopback_url = format!("http://127.0.0.1:{actual_port}");
    let _resp_rx = spawn_loki_responder(listener, 204);

    let body = serde_json::json!({
        "signal_type": "logs",
        "labels": {"job": "sonda"},
        "log": {"severity": "info", "message": "deploy started"},
        "encoder": {"type": "json_lines"},
        "sink": {
            "type": "loki",
            "url": loopback_url,
            "batch_size": 1
        },
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/events"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .expect("HTTP must succeed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "happy delivery, warnings present"
    );
    let json: serde_json::Value = resp.json().expect("body must be JSON");
    let warnings = json["warnings"]
        .as_array()
        .expect("warnings must be present and an array");
    assert!(!warnings.is_empty(), "loopback URL must surface a warning");
    let first = warnings[0].as_str().expect("warning is a string");
    assert!(
        first.contains("127.0.0.1") && first.contains("loki"),
        "warning must mention the loopback host and sink, got: {first}"
    );
}
