//! End-to-end tests for API key authentication.
//!
//! These tests spawn the actual `sonda-server` binary with various
//! authentication configurations and verify behaviour with real HTTP requests.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// RAII guard that kills the child process on drop, ensuring cleanup even on
/// test failure or panic.
struct ServerGuard {
    child: Child,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Find a free port by binding to port 0 and returning the assigned port.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("must bind to a free port");
    listener.local_addr().unwrap().port()
}

/// Spawn the sonda-server binary on the given port with optional extra CLI args
/// and environment variables.
fn spawn_server_with(port: u16, extra_args: &[&str], extra_env: &[(&str, &str)]) -> Child {
    let binary = env!("CARGO_BIN_EXE_sonda-server");

    let mut cmd = Command::new(binary);
    cmd.args(["--port", &port.to_string(), "--bind", "127.0.0.1"])
        .args(extra_args)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    cmd.spawn().expect("failed to spawn sonda-server binary")
}

/// Wait until the server is responding to HTTP requests on /health (or timeout).
fn wait_for_server(port: u16, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("must build reqwest client");
    while std::time::Instant::now() < deadline {
        if client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .is_ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Start the server with the given extra args and env, wrapped in a ServerGuard.
fn start_server_with(extra_args: &[&str], extra_env: &[(&str, &str)]) -> (u16, ServerGuard) {
    let port = free_port();
    let child = spawn_server_with(port, extra_args, extra_env);
    assert!(
        wait_for_server(port, Duration::from_secs(10)),
        "sonda-server must start accepting connections within 10 seconds on port {port}"
    );
    (port, ServerGuard { child })
}

// ---- Tests: API key via --api-key flag -------------------------------------

/// GET /health returns 200 even when --api-key is set.
#[test]
fn health_public_with_api_key_set() {
    let (port, _guard) = start_server_with(&["--api-key", "test-secret"], &[]);
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/health"))
        .expect("GET /health must succeed");
    assert_eq!(resp.status().as_u16(), 200, "GET /health must return 200");
}

/// GET /scenarios without Authorization header returns 401 when key is set.
#[test]
fn scenarios_without_auth_returns_401() {
    let (port, _guard) = start_server_with(&["--api-key", "test-secret"], &[]);
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/scenarios"))
        .expect("GET /scenarios must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "GET /scenarios without auth must return 401"
    );

    let body: serde_json::Value = resp.json().expect("body must be valid JSON");
    assert_eq!(body["error"], "unauthorized");
}

/// GET /scenarios with wrong key returns 401.
#[test]
fn scenarios_wrong_key_returns_401() {
    let (port, _guard) = start_server_with(&["--api-key", "correct-key"], &[]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/scenarios"))
        .header("Authorization", "Bearer wrong-key")
        .send()
        .expect("request must succeed at HTTP level");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "GET /scenarios with wrong key must return 401"
    );

    let body: serde_json::Value = resp.json().expect("body must be valid JSON");
    assert_eq!(body["detail"], "invalid API key");
}

/// GET /scenarios with correct key returns 200.
#[test]
fn scenarios_correct_key_returns_200() {
    let (port, _guard) = start_server_with(&["--api-key", "my-secret-key"], &[]);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/scenarios"))
        .header("Authorization", "Bearer my-secret-key")
        .send()
        .expect("request must succeed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET /scenarios with correct key must return 200"
    );
}

// ---- Tests: API key via SONDA_API_KEY env var ------------------------------

/// SONDA_API_KEY env var enables authentication (same as --api-key flag).
#[test]
fn env_var_enables_auth() {
    let (port, _guard) = start_server_with(&[], &[("SONDA_API_KEY", "env-secret")]);

    // Without auth header -> 401.
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/scenarios"))
        .expect("request must succeed at HTTP level");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "GET /scenarios without auth must return 401 when SONDA_API_KEY is set"
    );

    // With correct auth header -> 200.
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/scenarios"))
        .header("Authorization", "Bearer env-secret")
        .send()
        .expect("request must succeed");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET /scenarios with correct env-based key must return 200"
    );
}

// ---- Tests: No key configured (backwards compatibility) --------------------

/// When no API key is set, all endpoints are publicly accessible.
#[test]
fn no_key_all_endpoints_public() {
    let (port, _guard) = start_server_with(&[], &[]);
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/scenarios"))
        .expect("GET /scenarios must succeed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET /scenarios must return 200 when no API key is configured"
    );
}

/// When no API key is set, /health is also accessible (baseline sanity check).
#[test]
fn no_key_health_accessible() {
    let (port, _guard) = start_server_with(&[], &[]);
    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/health"))
        .expect("GET /health must succeed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET /health must return 200 when no API key is configured"
    );
}
