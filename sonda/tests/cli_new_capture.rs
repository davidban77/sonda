//! `sonda new --from-prometheus` driven as a real process against a real socket.
//!
//! The unit tests in `new/tsdb_reader.rs` cover window resolution. What only a
//! spawned binary can show is that the flags clap accepts reach the capture,
//! that both files land, that the emitted scenario is one `sonda run` will
//! take — and that the token in the environment never reaches either file.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use common::sonda_bin;

/// Serve one canned response and hand back the request that asked for it.
fn mock_tsdb(status: &str, body: &str) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let (tx, rx) = mpsc::channel();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).unwrap_or(0) == 1 {
            seen.push(byte[0]);
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        let _ = tx.send(String::from_utf8_lossy(&seen).into_owned());
    });

    (base, rx)
}

/// Three series over a 10s grid, the middle one with a hole in it.
fn matrix_body() -> String {
    r#"{"status":"success","data":{"resultType":"matrix","result":[
        {"metric":{"__name__":"up","job":"api"},"values":[[0,"1"],[10,"1"],[20,"1"],[30,"1"]]},
        {"metric":{"__name__":"up","job":"db"},"values":[[0,"1"],[30,"0"]]}
    ]}}"#
        .to_string()
}

#[test]
fn a_capture_writes_both_files_and_the_scenario_runs() {
    let (base, _rx) = mock_tsdb("200 OK", &matrix_body());
    let dir = tempfile::tempdir().expect("tempdir");
    let csv = dir.path().join("capture.csv");
    let yaml = dir.path().join("capture.yaml");

    let out = Command::new(sonda_bin())
        .args(["new", "--from-prometheus", &base])
        .args(["--query", "up"])
        .args(["--start", "0", "--end", "30"])
        .args(["--step", "10s"])
        .arg("--out")
        .arg(&csv)
        .arg("-o")
        .arg(&yaml)
        .output()
        .expect("spawn sonda");
    assert!(
        out.status.success(),
        "capture must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let csv_text = std::fs::read_to_string(&csv).expect("the CSV was written");
    assert!(
        csv_text.lines().count() == 5,
        "a header and four grid points:\n{csv_text}"
    );
    // The header is one quoted CSV field per column, so the label quotes are
    // doubled inside it — this is the escaped form, not a second convention.
    assert!(
        csv_text.contains(r#"job=""api"""#) && csv_text.contains(r#"job=""db"""#),
        "both series became columns:\n{csv_text}"
    );
    assert!(
        csv_text.contains("10.000,1,\n"),
        "and the db series' silence is a blank cell, not a zero:\n{csv_text}"
    );

    let yaml_text = std::fs::read_to_string(&yaml).expect("the scenario was written");
    assert!(
        yaml_text.contains("gap_windows:"),
        "the silence in the db series is declared:\n{yaml_text}"
    );

    // The real acceptance test: the binary loads what the binary wrote.
    let dry = Command::new(sonda_bin())
        .arg("--dry-run")
        .arg("run")
        .arg(&yaml)
        .output()
        .expect("spawn sonda");
    assert!(
        dry.status.success(),
        "the emitted scenario must load; stderr:\n{}\nyaml:\n{yaml_text}",
        String::from_utf8_lossy(&dry.stderr)
    );
}

#[test]
fn a_token_in_the_environment_is_sent_and_reaches_neither_file() {
    const TOKEN: &str = "cli-token-must-not-be-written";

    let (base, rx) = mock_tsdb("200 OK", &matrix_body());
    let dir = tempfile::tempdir().expect("tempdir");
    let csv = dir.path().join("capture.csv");
    let yaml = dir.path().join("capture.yaml");

    let out = Command::new(sonda_bin())
        .env("SONDA_PROM_TOKEN", TOKEN)
        .args(["new", "--from-prometheus", &base])
        .args(["--query", "up"])
        .args(["--range", "1h", "--step", "10s"])
        .arg("--out")
        .arg(&csv)
        .arg("-o")
        .arg(&yaml)
        .output()
        .expect("spawn sonda");
    assert!(
        out.status.success(),
        "capture must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Vacuity guard: absence in the files proves nothing unless the token was
    // actually sent. Without this the assertions below pass just as well when
    // the CLI never reads the environment variable at all.
    let request = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    assert!(
        request.contains(&format!("Authorization: Bearer {TOKEN}")),
        "the env token must reach the wire, or the leak checks prove nothing:\n{request}"
    );

    for (what, path) in [("csv", &csv), ("yaml", &yaml)] {
        let text = std::fs::read_to_string(path).expect("written");
        assert!(
            !text.contains(TOKEN),
            "the {what} carries the token:\n{text}"
        );
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(TOKEN),
        "the log carries the token:\n{stderr}"
    );
}

#[test]
fn explicit_headers_are_sent_alongside_the_env_token() {
    const TOKEN: &str = "env-token-alongside-headers";

    let (base, rx) = mock_tsdb("200 OK", &matrix_body());
    let dir = tempfile::tempdir().expect("tempdir");

    let out = Command::new(sonda_bin())
        .env("SONDA_PROM_TOKEN", TOKEN)
        .args(["new", "--from-prometheus", &base])
        .args(["--query", "up"])
        .args(["--range", "1h", "--step", "10s"])
        .args(["--header", "X-Scope-OrgID: tenant-7"])
        .arg("--out")
        .arg(dir.path().join("capture.csv"))
        .output()
        .expect("spawn sonda");
    assert!(
        out.status.success(),
        "capture must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let request = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    assert!(
        request.contains("X-Scope-OrgID: tenant-7"),
        "the explicit header was sent:\n{request}"
    );
    assert!(
        request.contains(&format!("Authorization: Bearer {TOKEN}")),
        "and the env token was not dropped by adding one:\n{request}"
    );
}

#[test]
fn a_timescaled_capture_says_so_and_still_loads() {
    let (base, _rx) = mock_tsdb("200 OK", &matrix_body());
    let dir = tempfile::tempdir().expect("tempdir");
    let csv = dir.path().join("capture.csv");
    let yaml = dir.path().join("capture.yaml");

    let out = Command::new(sonda_bin())
        .args(["new", "--from-prometheus", &base])
        .args(["--query", "up"])
        .args(["--start", "0", "--end", "30", "--step", "10s"])
        .args(["--timescale", "4"])
        .arg("--out")
        .arg(&csv)
        .arg("-o")
        .arg(&yaml)
        .output()
        .expect("spawn sonda");
    assert!(
        out.status.success(),
        "capture must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let yaml_text = std::fs::read_to_string(&yaml).expect("written");
    assert!(
        yaml_text.contains("timescale: 4"),
        "the replay speed is in the scenario:\n{yaml_text}"
    );
    let dry = Command::new(sonda_bin())
        .arg("--dry-run")
        .arg("run")
        .arg(&yaml)
        .output()
        .expect("spawn sonda");
    assert!(
        dry.status.success(),
        "a timescaled capture must load; stderr:\n{}\nyaml:\n{yaml_text}",
        String::from_utf8_lossy(&dry.stderr)
    );
}

#[test]
fn too_many_series_says_how_to_narrow_the_query() {
    // 21 series, one over the cap.
    let results: Vec<String> = (0..21)
        .map(|i| {
            format!(r#"{{"metric":{{"__name__":"up","pod":"p{i}"}},"values":[[0,"1"],[10,"1"]]}}"#)
        })
        .collect();
    let body = format!(
        r#"{{"status":"success","data":{{"resultType":"matrix","result":[{}]}}}}"#,
        results.join(",")
    );

    let (base, _rx) = mock_tsdb("200 OK", &body);
    let dir = tempfile::tempdir().expect("tempdir");
    let csv = dir.path().join("capture.csv");

    let out = Command::new(sonda_bin())
        .args(["new", "--from-prometheus", &base])
        .args(["--query", "up"])
        .args(["--start", "0", "--end", "10", "--step", "10s"])
        .arg("--out")
        .arg(&csv)
        .output()
        .expect("spawn sonda");
    assert!(!out.status.success(), "over the cap must fail");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("21 series"), "it says how many: {stderr}");
    assert!(
        stderr.contains("sum by") || stderr.contains("Aggregate"),
        "and how to narrow it: {stderr}"
    );
    assert!(
        !csv.exists(),
        "and nothing is written when the capture is refused"
    );
}

#[test]
fn the_capture_flags_that_go_together_are_enforced_by_the_parser() {
    let cases: [(&[&str], &str); 4] = [
        (
            &["new", "--from-prometheus", "http://x:9090"],
            "required arguments were not provided",
        ),
        (
            &[
                "new",
                "--from-prometheus",
                "http://x:9090",
                "--query",
                "up",
                "--step",
                "10s",
            ],
            "required arguments were not provided",
        ),
        (
            &["new", "--template", "--from-prometheus", "http://x:9090"],
            "cannot be used with",
        ),
        (
            &[
                "new",
                "--from",
                "a.csv",
                "--from-prometheus",
                "http://x:9090",
            ],
            "cannot be used with",
        ),
    ];
    for (args, needle) in cases {
        let out = Command::new(sonda_bin())
            .args(args)
            .output()
            .expect("spawn sonda");
        assert!(!out.status.success(), "{args:?} must be rejected");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(needle),
            "{args:?} should say {needle:?}, got: {stderr}"
        );
    }
}

#[test]
fn a_window_the_parser_accepts_but_the_capture_cannot_use_is_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Parses fine — no --range and no --start/--end is a clap-legal command.
    let out = Command::new(sonda_bin())
        .args(["new", "--from-prometheus", "http://127.0.0.1:1"])
        .args(["--query", "up", "--step", "10s"])
        .arg("--out")
        .arg(dir.path().join("capture.csv"))
        .output()
        .expect("spawn sonda");
    assert!(!out.status.success(), "a capture with no window must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--range") && stderr.contains("--start"),
        "the error names both ways to give one: {stderr}"
    );
}
