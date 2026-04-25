//! Shared test infrastructure for sonda-server integration tests.
//!
//! Spawns the sonda-server binary with `--port 0` so the OS picks a free port
//! and the server announces it on stdout. The harness reads the announce —
//! eliminating the bind/spawn race that pre-allocating a port introduces.
//! See `sonda-server/src/main.rs::announce_bound_port` for the contract.

// Each test file compiles its own copy of this module, so not every file
// uses every helper. Suppress the per-file dead_code warnings.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long to wait for the server to print its bound-port announce.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Spawn the sonda-server binary with `--port 0`, read the announced port from
/// stdout, and return both the port and the child handle.
///
/// The `SONDA_API_KEY` environment variable is always removed from the
/// inherited environment so that tests running under a shell with the variable
/// set do not accidentally enable authentication.
///
/// Stdout is piped (and consumed by the announce reader); stderr is piped so
/// callers can collect tracing output for diagnostics on failure.
pub fn spawn_server_with(extra_args: &[&str], extra_env: &[(&str, &str)]) -> (u16, Child) {
    let binary = env!("CARGO_BIN_EXE_sonda-server");

    let mut cmd = Command::new(binary);
    cmd.args(["--port", "0", "--bind", "127.0.0.1"])
        .args(extra_args)
        .env("RUST_LOG", "warn")
        .env_remove("SONDA_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().expect("failed to spawn sonda-server binary");
    let stdout = child
        .stdout
        .take()
        .expect("child stdout must be piped (Stdio::piped above)");

    let port = read_announced_port(stdout)
        .unwrap_or_else(|err| panic!("sonda-server announce failed: {err}"));

    (port, child)
}

/// Spawn the sonda-server binary with default settings.
pub fn spawn_server() -> (u16, Child) {
    spawn_server_with(&[], &[])
}

/// Start the server with default settings, wrapped in a `ServerGuard`.
pub fn start_server() -> (u16, ServerGuard) {
    start_server_with(&[], &[])
}

/// Start the server with extra CLI args and env vars, wrapped in a
/// `ServerGuard` for automatic cleanup.
pub fn start_server_with(extra_args: &[&str], extra_env: &[(&str, &str)]) -> (u16, ServerGuard) {
    let (port, child) = spawn_server_with(extra_args, extra_env);
    (port, ServerGuard { child })
}

/// Build a `reqwest::blocking::Client` with a 10-second timeout.
pub fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("must build HTTP client")
}

/// Read the first line of the child's stdout and parse it as the server's
/// bound-port announce: `{"sonda_server":{"port":N}}`.
///
/// Uses a worker thread + mpsc so the read is bounded by `ANNOUNCE_TIMEOUT` —
/// otherwise a server that never printed would hang the test indefinitely.
fn read_announced_port(stdout: ChildStdout) -> Result<u16, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = match reader.read_line(&mut line) {
            Ok(0) => Err("child stdout closed before announce".to_string()),
            Ok(_) => Ok(line),
            Err(e) => Err(format!("failed to read child stdout: {e}")),
        };
        let _ = tx.send(result);
    });

    let line = rx
        .recv_timeout(ANNOUNCE_TIMEOUT)
        .map_err(|_| format!("no announce within {ANNOUNCE_TIMEOUT:?}"))??;

    let value: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| format!("announce was not valid JSON ({e}): {line:?}"))?;

    let port = value
        .get("sonda_server")
        .and_then(|inner| inner.get("port"))
        .and_then(|p| p.as_u64())
        .ok_or_else(|| format!("announce missing sonda_server.port: {line:?}"))?;

    u16::try_from(port).map_err(|_| format!("announced port out of range: {port}"))
}
