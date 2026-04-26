//! Shared test infrastructure: spawns sonda-server with `--port 0` and reads
//! the bound port from the stdout announce.

// Each test file compiles its own copy; not every file uses every helper.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(10);

/// RAII guard: kills the child on drop.
pub struct ServerGuard {
    child: Child,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Spawn `sonda-server --port 0`, read the announced port, return `(port, child)`.
/// Strips `SONDA_API_KEY` from the inherited env so a shell-set key doesn't leak in.
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

pub fn spawn_server() -> (u16, Child) {
    spawn_server_with(&[], &[])
}

pub fn start_server() -> (u16, ServerGuard) {
    start_server_with(&[], &[])
}

/// `spawn_server_with` wrapped in a `ServerGuard`.
pub fn start_server_with(extra_args: &[&str], extra_env: &[(&str, &str)]) -> (u16, ServerGuard) {
    let (port, child) = spawn_server_with(extra_args, extra_env);
    (port, ServerGuard { child })
}

pub fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("must build HTTP client")
}

/// Parse the first stdout line as `{"sonda_server":{"port":N}}`. Worker thread +
/// mpsc bound the read to `ANNOUNCE_TIMEOUT` so a silent server can't hang the test.
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
