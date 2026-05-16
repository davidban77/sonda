use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn sonda_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sonda"))
}

const NGINX_FIVE: &str = r#"192.168.1.10 - - [10/Oct/2024:13:55:36 +0000] "GET /api/users HTTP/1.1" 200 1234 "-" "Mozilla/5.0"
192.168.1.11 - - [10/Oct/2024:13:55:38 +0000] "POST /api/login HTTP/1.1" 401 12 "-" "curl/7.79"
192.168.1.12 - - [10/Oct/2024:13:55:40 +0000] "GET /api/missing HTTP/1.1" 404 0 "-" "Mozilla/5.0"
192.168.1.13 - - [10/Oct/2024:13:55:42 +0000] "GET /api/health HTTP/1.1" 200 5 "-" "checker/1.0"
192.168.1.14 - - [10/Oct/2024:13:55:44 +0000] "POST /api/orders HTTP/1.1" 500 2048 "-" "kit/2.0"
"#;

#[test]
fn parsers_rawlog_nginx_writes_csv_and_yaml() {
    let dir = TempDir::new().expect("tmp dir");
    let input = dir.path().join("nginx.log");
    std::fs::write(&input, NGINX_FIVE).unwrap();
    let yaml = dir.path().join("out.yaml");

    let out = Command::new(sonda_bin())
        .args([
            "parsers",
            "rawlog",
            input.to_str().unwrap(),
            "--format",
            "nginx",
            "-o",
            yaml.to_str().unwrap(),
        ])
        .output()
        .expect("execute sonda parsers rawlog");

    assert!(
        out.status.success(),
        "exit code: {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let csv_path = yaml.parent().unwrap().join("nginx.csv");
    assert!(yaml.exists(), "yaml was not written");
    assert!(csv_path.exists(), "csv was not written at {csv_path:?}");

    let csv = std::fs::read_to_string(&csv_path).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 6);
    assert_eq!(
        lines[0],
        "timestamp,severity,message,method,path,remote_addr,status,user_agent"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("parsed 5 rows"), "stderr: {stderr}");
    assert!(stderr.contains("wrote csv"), "stderr: {stderr}");
    assert!(stderr.contains("wrote yaml"), "stderr: {stderr}");
}

#[test]
fn parsers_rawlog_emitted_yaml_passes_dry_run() {
    let dir = TempDir::new().expect("tmp dir");
    let input = dir.path().join("nginx.log");
    std::fs::write(&input, NGINX_FIVE).unwrap();
    let yaml = dir.path().join("out.yaml");

    let emit = Command::new(sonda_bin())
        .args([
            "parsers",
            "rawlog",
            input.to_str().unwrap(),
            "--format",
            "nginx",
            "-o",
            yaml.to_str().unwrap(),
        ])
        .output()
        .expect("run parser");
    assert!(emit.status.success());

    let dry_run = Command::new(sonda_bin())
        .args(["--dry-run", "run", "--scenario", yaml.to_str().unwrap()])
        .output()
        .expect("run sonda --dry-run");
    assert!(
        dry_run.status.success(),
        "dry-run failed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&dry_run.stdout),
        String::from_utf8_lossy(&dry_run.stderr)
    );
}

#[test]
fn parsers_rawlog_plain_with_delta_writes_synthesized_timestamps() {
    let dir = TempDir::new().expect("tmp dir");
    let input = dir.path().join("p.log");
    std::fs::write(&input, "alpha\nbeta\ngamma\n").unwrap();
    let yaml = dir.path().join("p.yaml");

    let out = Command::new(sonda_bin())
        .args([
            "parsers",
            "rawlog",
            input.to_str().unwrap(),
            "--format",
            "plain",
            "--delta-seconds",
            "5",
            "-o",
            yaml.to_str().unwrap(),
        ])
        .output()
        .expect("execute");
    assert!(out.status.success());
    let csv = std::fs::read_to_string(yaml.parent().unwrap().join("p.csv")).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[1], "1700000000,,alpha");
    assert_eq!(lines[2], "1700000005,,beta");
    assert_eq!(lines[3], "1700000010,,gamma");
}

#[test]
fn parsers_rawlog_unknown_format_exits_nonzero() {
    let dir = TempDir::new().expect("tmp dir");
    let input = dir.path().join("x.log");
    std::fs::write(&input, "x\n").unwrap();
    let yaml = dir.path().join("x.yaml");
    let out = Command::new(sonda_bin())
        .args([
            "parsers",
            "rawlog",
            input.to_str().unwrap(),
            "--format",
            "bogus",
            "-o",
            yaml.to_str().unwrap(),
        ])
        .output()
        .expect("execute");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown format") || stderr.contains("bogus"),
        "expected unknown-format error in stderr, got: {stderr}"
    );
}
