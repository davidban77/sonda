//! Shared test infrastructure for sonda-server integration tests.
//!
//! Provides an RAII `ServerGuard` that kills the child process on drop,
//! a portable `free_port()` helper, and convenience functions for spawning
//! and waiting on the sonda-server binary.

// Each test file compiles its own copy of this module, so not every file
// uses every helper. Suppress the per-file dead_code warnings.
#![allow(dead_code)]

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// RAII guard that kills the child process on drop, ensuring cleanup even on
/// test failure or panic.
pub struct ServerGuard {
    child: Child,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Find a free port by binding to port 0 and returning the OS-assigned port.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("must bind to a free port");
    listener.local_addr().unwrap().port()
}

/// Spawn the sonda-server binary on the given port with optional extra CLI
/// args and environment variables.
///
/// The `SONDA_API_KEY` environment variable is always removed from the
/// inherited environment so that tests running under a shell with the
/// variable set do not accidentally enable authentication.
pub fn spawn_server_with(port: u16, extra_args: &[&str], extra_env: &[(&str, &str)]) -> Child {
    let binary = env!("CARGO_BIN_EXE_sonda-server");

    let mut cmd = Command::new(binary);
    cmd.args(["--port", &port.to_string(), "--bind", "127.0.0.1"])
        .args(extra_args)
        .env("RUST_LOG", "warn")
        .env_remove("SONDA_API_KEY")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    cmd.spawn().expect("failed to spawn sonda-server binary")
}

/// Spawn the sonda-server binary on the given port with default settings
/// (no extra args, no extra env).
pub fn spawn_server(port: u16) -> Child {
    spawn_server_with(port, &[], &[])
}

/// Wait until the server responds to `GET /health` or the timeout elapses.
///
/// Returns `true` if the server became ready within the timeout, `false`
/// otherwise.
pub fn wait_for_server(port: u16, timeout: Duration) -> bool {
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

/// Start the server on a random port with default settings, wrapped in a
/// `ServerGuard` for automatic cleanup.
pub fn start_server() -> (u16, ServerGuard) {
    start_server_with(&[], &[])
}

/// Start the server on a random port with extra CLI args and env vars,
/// wrapped in a `ServerGuard` for automatic cleanup.
pub fn start_server_with(extra_args: &[&str], extra_env: &[(&str, &str)]) -> (u16, ServerGuard) {
    let port = free_port();
    let child = spawn_server_with(port, extra_args, extra_env);
    assert!(
        wait_for_server(port, Duration::from_secs(10)),
        "sonda-server must start accepting connections within 10 seconds on port {port}"
    );
    (port, ServerGuard { child })
}

/// Build a `reqwest::blocking::Client` with a 10-second timeout.
pub fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("must build HTTP client")
}
