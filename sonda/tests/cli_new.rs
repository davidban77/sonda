//! Integration tests for `sonda new`.

mod common;

use std::io::Write;
use std::process::Command;

use common::sonda_bin;
use tempfile::NamedTempFile;

#[test]
fn new_template_prints_valid_v2_yaml_to_stdout() {
    let output = Command::new(sonda_bin())
        .args(["new", "--template"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "new --template must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("version: 2"), "got: {stdout}");
    assert!(stdout.contains("kind: runnable"), "got: {stdout}");
}

#[test]
fn new_template_output_parses_through_v2_compiler() {
    let output = Command::new(sonda_bin())
        .args(["new", "--template"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8");

    let mut f = NamedTempFile::new().expect("tempfile");
    f.write_all(stdout.as_bytes()).expect("write");
    f.flush().expect("flush");

    let validation = Command::new(sonda_bin())
        .args(["--dry-run", "run"])
        .arg(f.path())
        .output()
        .expect("spawn sonda dry-run");
    assert!(
        validation.status.success(),
        "template YAML must validate via v2 compiler; stderr:\n{}",
        String::from_utf8_lossy(&validation.stderr)
    );
}

#[test]
fn new_from_csv_scaffolds_yaml_with_runnable_kind() {
    let mut csv = NamedTempFile::new().expect("tempfile");
    let content = "timestamp,cpu\n1000,50.0\n2000,50.1\n3000,49.9\n4000,50.2\n5000,49.8\n";
    csv.write_all(content.as_bytes()).expect("write");
    csv.flush().expect("flush");

    let output = Command::new(sonda_bin())
        .args(["new", "--from"])
        .arg(csv.path())
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "new --from <csv> must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("version: 2"));
    assert!(stdout.contains("kind: runnable"));
    assert!(
        stdout.contains("cpu"),
        "metric name from CSV column must appear, got: {stdout}"
    );
}

#[test]
fn new_from_csv_uses_operational_alias() {
    let mut csv = NamedTempFile::new().expect("tempfile");
    // Steady values around 50: pattern detector should pick a steady alias.
    let content = "timestamp,steady_metric\n1000,50.0\n2000,50.1\n3000,49.9\n4000,50.2\n5000,49.8\n6000,50.0\n7000,50.1\n8000,49.9\n9000,50.2\n10000,50.0\n";
    csv.write_all(content.as_bytes()).expect("write");
    csv.flush().expect("flush");

    let output = Command::new(sonda_bin())
        .args(["new", "--from"])
        .arg(csv.path())
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let known_aliases = ["steady", "spike_event", "leak", "flap", "sawtooth", "step"];
    let found = known_aliases
        .iter()
        .any(|alias| stdout.contains(&format!("type: {alias}")));
    assert!(found, "must use an operational alias, got: {stdout}");
}
