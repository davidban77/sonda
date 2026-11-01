//! Integration tests for stopping `sonda run` with SIGINT: the sink holds
//! every event the stop banner counts.
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use common::sonda_bin;

/// How long a run may take to print its start banner.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a run emits after its start banner before it is interrupted.
const RUN_BEFORE_SIGINT: Duration = Duration::from_millis(1500);

/// `examples/file-sink.yaml`: one metric at rate 100 for 10 s, influx line
/// protocol to a file.
fn file_sink_example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/file-sink.yaml")
}

/// A running `sonda run`, its stderr read line by line as it arrives.
struct Run {
    child: Child,
    stderr_lines: Receiver<String>,
    stderr: JoinHandle<String>,
    stdout: JoinHandle<String>,
}

/// What a finished run left behind.
struct Finished {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// Start `sonda run examples/file-sink.yaml --rate 10 --duration 60s` with
/// `sink_args`, stdout and stderr piped and read on their own threads.
fn spawn_example(sink_args: &[&str]) -> Run {
    let example = file_sink_example();
    assert!(example.is_file(), "{} is missing", example.display());
    let mut child = Command::new(sonda_bin())
        .arg("run")
        .arg(&example)
        .args(["--rate", "10", "--duration", "60s"])
        .args(sink_args)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sonda");
    let stderr_pipe = child.stderr.take().expect("piped stderr");
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let (lines, stderr_lines) = mpsc::channel();
    let stderr = thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stderr_pipe).lines() {
            let line = line.expect("read stderr");
            all.push_str(&line);
            all.push('\n');
            let _ = lines.send(line);
        }
        all
    });
    let stdout = thread::spawn(move || {
        let mut all = String::new();
        stdout_pipe.read_to_string(&mut all).expect("read stdout");
        all
    });
    Run {
        child,
        stderr_lines,
        stderr,
        stdout,
    }
}

/// Wait for `run`'s start banner (the `▶ … signal_type:` line), let it emit
/// for [`RUN_BEFORE_SIGINT`], send SIGINT, and wait for it to exit.
fn interrupt_after_start(mut run: Run) -> Finished {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match run.stderr_lines.recv_timeout(left) {
            Ok(line) if line.contains('\u{25b6}') && line.contains("signal_type:") => break,
            Ok(_) => {}
            Err(_) => {
                let _ = run.child.kill();
                let _ = run.child.wait();
                let stderr = run.stderr.join().expect("stderr reader");
                panic!("no start banner within {START_TIMEOUT:?}; stderr:\n{stderr}");
            }
        }
    }
    thread::sleep(RUN_BEFORE_SIGINT);
    let sent = Command::new("kill")
        .args(["-INT", &run.child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(sent.success(), "kill -INT failed");
    let status = run.child.wait().expect("wait for sonda");
    Finished {
        status,
        stdout: run.stdout.join().expect("stdout reader"),
        stderr: run.stderr.join().expect("stderr reader"),
    }
}

/// The event count of the stop banner in `stderr`: the number after
/// `events:` on its `completed in` line.
fn banner_events(stderr: &str) -> u64 {
    let line = stderr
        .lines()
        .find(|line| line.contains("completed in") && line.contains("events:"))
        .unwrap_or_else(|| panic!("no stop banner in stderr:\n{stderr}"));
    let after = line.split("events:").nth(1).expect("text after events:");
    after
        .split_whitespace()
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("no event count in {line:?}"))
}

/// SIGINT 1.5 s after the start banner of a file-sink run: sonda exits 0,
/// and the file holds one line per event the stop banner counts, at least
/// ten of them.
#[test]
fn sigint_leaves_every_counted_event_in_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.txt");
    let run = spawn_example(&["-o", path.to_str().expect("a UTF-8 path")]);
    let finished = interrupt_after_start(run);
    let stderr = &finished.stderr;
    assert!(
        finished.status.success(),
        "sonda exited {:?}:\n{stderr}",
        finished.status
    );

    let events = banner_events(stderr);
    assert!(
        events >= 10,
        "only {events} events before the stop:\n{stderr}"
    );
    let on_disk = std::fs::read_to_string(&path).expect("the file exists");
    assert_eq!(
        on_disk.lines().count() as u64,
        events,
        "the banner counts {events} events; the file holds {} bytes",
        on_disk.len()
    );
}

/// SIGINT 1.5 s after the start banner of a run to stdout through a pipe:
/// sonda exits 0, and the pipe received one line per event the stop banner
/// counts, at least ten.
#[test]
fn sigint_leaves_every_counted_event_on_piped_stdout() {
    let run = spawn_example(&["--sink", "stdout"]);
    let finished = interrupt_after_start(run);
    let stderr = &finished.stderr;
    assert!(
        finished.status.success(),
        "sonda exited {:?}:\n{stderr}",
        finished.status
    );

    let events = banner_events(stderr);
    assert!(
        events >= 10,
        "only {events} events before the stop:\n{stderr}"
    );
    assert_eq!(
        finished.stdout.lines().count() as u64,
        events,
        "the banner counts {events} events; stdout holds {} bytes",
        finished.stdout.len()
    );
}
