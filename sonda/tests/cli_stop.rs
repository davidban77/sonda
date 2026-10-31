//! Integration tests for stopping `sonda run` with SIGINT: the sink holds
//! every event the stop banner counts.
#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::sonda_bin;

/// `examples/file-sink.yaml`: one metric at rate 100 for 10 s, influx line
/// protocol to a file.
fn file_sink_example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/file-sink.yaml")
}

/// Start `sonda run examples/file-sink.yaml --rate 10 --duration 60s` with
/// `sink_args`, stdout and stderr piped.
fn spawn_example(sink_args: &[&str]) -> Child {
    let example = file_sink_example();
    assert!(example.is_file(), "{} is missing", example.display());
    Command::new(sonda_bin())
        .arg("run")
        .arg(&example)
        .args(["--rate", "10", "--duration", "60s"])
        .args(sink_args)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sonda")
}

/// Send SIGINT to `child` after `after`, then wait for it to exit.
fn interrupt_after(child: Child, after: Duration) -> Output {
    std::thread::sleep(after);
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -INT failed");
    child.wait_with_output().expect("wait for sonda")
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

/// SIGINT 1.5 s into a file-sink run: sonda exits 0, and the file holds one
/// line per event the stop banner counts, at least ten of them.
#[test]
fn sigint_leaves_every_counted_event_in_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.txt");
    let child = spawn_example(&["-o", path.to_str().expect("a UTF-8 path")]);
    let output = interrupt_after(child, Duration::from_millis(1500));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sonda exited {:?}:\n{stderr}",
        output.status
    );

    let events = banner_events(&stderr);
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

/// SIGINT 1.5 s into a run to stdout through a pipe: sonda exits 0, and the
/// pipe received one line per event the stop banner counts, at least ten.
#[test]
fn sigint_leaves_every_counted_event_on_piped_stdout() {
    let child = spawn_example(&["--sink", "stdout"]);
    let output = interrupt_after(child, Duration::from_millis(1500));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sonda exited {:?}:\n{stderr}",
        output.status
    );

    let events = banner_events(&stderr);
    assert!(
        events >= 10,
        "only {events} events before the stop:\n{stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count() as u64,
        events,
        "the banner counts {events} events; stdout holds {} bytes",
        stdout.len()
    );
}
