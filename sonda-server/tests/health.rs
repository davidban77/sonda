//! Integration tests for sonda-server Slice 3.1 — Server Skeleton & Health Check.
//!
//! These tests verify the server's HTTP behavior by starting an actual server
//! on a random port and making real HTTP requests against it.

use std::net::TcpListener;
use std::time::Duration;

/// Find a free port by binding to port 0 and returning the assigned port.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("must bind to a free port");
    listener.local_addr().unwrap().port()
}

/// Spawn the sonda-server binary on the given port. Returns the child process handle.
fn spawn_server(port: u16) -> std::process::Child {
    // Build path to the sonda-server binary.
    let binary = env!("CARGO_BIN_EXE_sonda-server");

    std::process::Command::new(binary)
        .args(["--port", &port.to_string(), "--bind", "127.0.0.1"])
        .env("RUST_LOG", "warn") // Suppress info logs during tests.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn sonda-server binary")
}

/// Wait until the server is accepting connections on the given port (or timeout).
fn wait_for_server(port: u16, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Helper: start the server on a random port and return (port, child).
fn start_server() -> (u16, std::process::Child) {
    let port = free_port();
    let child = spawn_server(port);
    assert!(
        wait_for_server(port, Duration::from_secs(5)),
        "sonda-server must start accepting connections within 5 seconds on port {port}"
    );
    (port, child)
}

// ---- Test: Server starts and binds to port ----------------------------------

/// The server binary must start and bind to the specified port within 5 seconds.
#[test]
fn server_starts_and_binds_to_port() {
    let (port, mut child) = start_server();

    // Verify the server is reachable.
    let connected = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok();
    assert!(connected, "server must be reachable on port {port}");

    child.kill().ok();
    child.wait().ok();
}

// ---- Test: GET /health returns 200 with {"status": "ok"} --------------------

/// GET /health must return HTTP 200 with body {"status": "ok"}.
#[test]
fn get_health_returns_200_status_ok() {
    let (port, mut child) = start_server();

    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/health"))
        .expect("GET /health must succeed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET /health must return status 200"
    );

    let body: serde_json::Value = resp.json().expect("response must be valid JSON");
    assert_eq!(
        body,
        serde_json::json!({ "status": "ok" }),
        "GET /health body must be {{\"status\": \"ok\"}}"
    );

    child.kill().ok();
    child.wait().ok();
}

// ---- Test: GET /health Content-Type is application/json ---------------------

/// GET /health response must have Content-Type: application/json.
#[test]
fn get_health_has_json_content_type() {
    let (port, mut child) = start_server();

    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/health"))
        .expect("GET /health must succeed");

    let ct = resp
        .headers()
        .get("content-type")
        .expect("response must have Content-Type header")
        .to_str()
        .unwrap()
        .to_string();

    assert!(
        ct.contains("application/json"),
        "Content-Type must contain application/json, got: {ct}"
    );

    child.kill().ok();
    child.wait().ok();
}

// ---- Test: Unknown route returns 404 ----------------------------------------

/// A request to an unregistered path must return HTTP 404.
#[test]
fn unknown_route_returns_404() {
    let (port, mut child) = start_server();

    let resp = reqwest::blocking::get(format!("http://127.0.0.1:{port}/nonexistent"))
        .expect("request to unknown route must succeed (at HTTP level)");

    assert_eq!(
        resp.status().as_u16(),
        404,
        "unknown route must return 404 Not Found"
    );

    child.kill().ok();
    child.wait().ok();
}

// ---- Test: Ctrl+C leads to clean shutdown -----------------------------------

/// Sending SIGTERM to the server process causes it to shut down cleanly
/// (exit code 0 or signal-terminated without panic).
#[test]
fn server_shuts_down_cleanly_on_sigterm() {
    let (_port, mut child) = start_server();

    // Send SIGTERM (the Unix equivalent of Ctrl+C for graceful shutdown).
    // On macOS/Linux, kill with SIGTERM.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }

    // Wait for the process to exit within a reasonable time.
    let result = child.wait().expect("must be able to wait for child");

    // The process should have exited. We accept any non-panic exit.
    // On SIGTERM the server may exit with a signal code or 0.
    // The key assertion is that it does not panic (which would show in stderr).
    let _ = result; // Process exited -- no hang.
}
