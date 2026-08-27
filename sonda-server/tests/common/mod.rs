//! Shared test infrastructure: spawns sonda-server with `--port 0` and reads
//! the bound port from the stdout announce.

// Each test file compiles its own copy; not every file uses every helper.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

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
    let mut child = server_command(extra_args, extra_env)
        .spawn()
        .expect("failed to spawn sonda-server binary");
    let stdout = child.stdout.take().expect("child stdout must be piped");

    let port = read_announced_port(stdout)
        .unwrap_or_else(|err| panic!("sonda-server announce failed: {err}"));

    (port, child)
}

/// Reads a running child's stderr into a shared buffer so a test can wait for a
/// log line instead of guessing how long the server needs to emit it.
pub struct StderrTail {
    lines: Arc<Mutex<Vec<String>>>,
    reader: Option<JoinHandle<()>>,
}

impl StderrTail {
    fn spawn(stderr: ChildStderr) -> Self {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut buffer) = sink.lock() {
                    buffer.push(line);
                }
            }
        });
        Self {
            lines,
            reader: Some(reader),
        }
    }

    pub fn wait_for(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.text().contains(needle) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Wait until `needle` has appeared on at least `count` lines.
    pub fn wait_for_lines(&self, needle: &str, count: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.count(needle) >= count {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn count(&self, needle: &str) -> usize {
        self.lines
            .lock()
            .expect("stderr buffer lock must not be poisoned")
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    pub fn text(&self) -> String {
        self.lines
            .lock()
            .expect("stderr buffer lock must not be poisoned")
            .join("\n")
    }

    /// Drain everything the child wrote. Call once the child has exited.
    pub fn finish(mut self) -> String {
        if let Some(reader) = self.reader.take() {
            reader.join().expect("stderr reader thread must not panic");
        }
        self.text()
    }
}

/// `spawn_server_with` plus a live view of the child's stderr.
pub fn spawn_server_tailed(
    extra_args: &[&str],
    extra_env: &[(&str, &str)],
) -> (u16, Child, StderrTail) {
    let mut child = server_command(extra_args, extra_env)
        .spawn()
        .expect("failed to spawn sonda-server binary");
    let stdout = child.stdout.take().expect("child stdout must be piped");
    let tail = StderrTail::spawn(child.stderr.take().expect("child stderr must be piped"));

    let port = read_announced_port(stdout)
        .unwrap_or_else(|err| panic!("sonda-server announce failed: {err}\n{}", tail.text()));

    (port, child, tail)
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

/// Output of a server process that was expected to exit on its own.
pub struct ServerExit {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ServerExit {
    pub fn announced_a_port(&self) -> bool {
        self.stdout.contains("sonda_server")
    }
}

/// Spawn the server with arguments that must be rejected at startup and wait for
/// it to exit; panics if it is still alive after `EXIT_TIMEOUT`.
pub fn run_server_expecting_exit(extra_args: &[&str], extra_env: &[(&str, &str)]) -> ServerExit {
    let mut child = server_command(extra_args, extra_env)
        .spawn()
        .expect("failed to spawn sonda-server binary");

    let deadline = std::time::Instant::now() + EXIT_TIMEOUT;
    loop {
        match child.try_wait().expect("failed to poll sonda-server") {
            Some(_) => break,
            None if std::time::Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                panic!("sonda-server did not exit within {EXIT_TIMEOUT:?}");
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }

    let output = child
        .wait_with_output()
        .expect("failed to collect sonda-server output");
    ServerExit {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// SIGTERM the child and wait for it to exit, so the server runs its graceful
/// shutdown path and writes everything it has to say. Kills and panics if it is
/// still alive after `EXIT_TIMEOUT`.
pub fn terminate_gracefully(child: &mut Child) -> Option<i32> {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }

    let deadline = Instant::now() + EXIT_TIMEOUT;
    loop {
        match child.try_wait().expect("failed to poll sonda-server") {
            Some(status) => return status.code(),
            None if Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                panic!("sonda-server did not exit within {EXIT_TIMEOUT:?} of SIGTERM");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn server_command(extra_args: &[&str], extra_env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sonda-server"));
    cmd.args(["--port", "0", "--bind", "127.0.0.1"])
        .args(extra_args)
        .env("RUST_LOG", "warn")
        .env_remove("SONDA_API_KEY")
        .env_remove("SONDA_CATALOG")
        .env_remove("SONDA_AUTOSTART")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd
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
