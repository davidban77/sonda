//! Integration tests for the `sonda import` subcommand.
//!
//! These tests verify the end-to-end flow: CSV file -> pattern detection ->
//! generate YAML -> validate the YAML is loadable by sonda-core.

use std::io::Write;
use std::path::PathBuf;

use tempfile::NamedTempFile;

fn write_temp_csv(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("create temp file");
    f.write_all(content.as_bytes()).expect("write temp file");
    f.flush().expect("flush temp file");
    f
}

/// Helper: run `sonda import` via CLI and capture exit status + output.
fn sonda_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sonda"))
}

// -----------------------------------------------------------------------
// --analyze: read-only analysis
// -----------------------------------------------------------------------

#[test]
fn import_analyze_plain_csv_exits_zero() {
    let csv = "timestamp,cpu,mem\n1000,50.0,80.0\n2000,49.9,79.5\n3000,50.1,80.1\n";
    let f = write_temp_csv(csv);
    let output = std::process::Command::new(sonda_bin())
        .args(["import", &f.path().display().to_string(), "--analyze"])
        .output()
        .expect("execute sonda import --analyze");

    assert!(
        output.status.success(),
        "exit code: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("detected:"),
        "expected pattern output, got: {stdout}"
    );
    assert!(
        stdout.contains("cpu"),
        "expected metric name in output, got: {stdout}"
    );
}

// -----------------------------------------------------------------------
// -o: generate scenario YAML
// -----------------------------------------------------------------------

#[test]
fn import_output_generates_loadable_single_scenario_yaml() {
    let csv = "timestamp,cpu\n1000,50.0\n2000,50.1\n3000,49.9\n4000,50.2\n5000,49.8\n";
    let csv_file = write_temp_csv(csv);
    let out_file = NamedTempFile::new().expect("create output file");

    let output = std::process::Command::new(sonda_bin())
        .args([
            "import",
            &csv_file.path().display().to_string(),
            "-o",
            &out_file.path().display().to_string(),
        ])
        .output()
        .expect("execute sonda import -o");

    assert!(
        output.status.success(),
        "exit code: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the generated YAML is v2 and compiles cleanly.
    let yaml = std::fs::read_to_string(out_file.path()).expect("read output YAML");
    assert!(
        yaml.starts_with("version: 2\n"),
        "sonda import must emit v2 YAML, got: {yaml}"
    );

    let resolver = sonda_core::compiler::expand::InMemoryPackResolver::new();
    let entries = sonda_core::compile_scenario_file(&yaml, &resolver)
        .expect("generated v2 YAML must compile cleanly");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].base().name, "cpu");
    assert!(entries[0].base().rate > 0.0);
}

#[test]
fn import_output_generates_loadable_multi_scenario_yaml() {
    let csv = "timestamp,cpu,mem\n1000,50.0,80.0\n2000,50.1,79.5\n3000,49.9,80.1\n4000,50.2,79.9\n";
    let csv_file = write_temp_csv(csv);
    let out_file = NamedTempFile::new().expect("create output file");

    let output = std::process::Command::new(sonda_bin())
        .args([
            "import",
            &csv_file.path().display().to_string(),
            "-o",
            &out_file.path().display().to_string(),
        ])
        .output()
        .expect("execute sonda import -o");

    assert!(
        output.status.success(),
        "exit code: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the generated YAML is v2 and compiles into two entries.
    let yaml = std::fs::read_to_string(out_file.path()).expect("read output YAML");
    assert!(
        yaml.starts_with("version: 2\n"),
        "sonda import must emit v2 YAML, got: {yaml}"
    );

    let resolver = sonda_core::compiler::expand::InMemoryPackResolver::new();
    let entries = sonda_core::compile_scenario_file(&yaml, &resolver)
        .expect("generated v2 YAML must compile cleanly");
    assert_eq!(entries.len(), 2);
}

// -----------------------------------------------------------------------
// --columns: column selection
// -----------------------------------------------------------------------

#[test]
fn import_columns_selects_specific_columns() {
    let csv =
        "timestamp,cpu,mem,disk\n1000,50.0,80.0,55.0\n2000,50.1,79.5,56.0\n3000,49.9,80.1,57.0\n";
    let csv_file = write_temp_csv(csv);
    let out_file = NamedTempFile::new().expect("create output file");

    let output = std::process::Command::new(sonda_bin())
        .args([
            "import",
            &csv_file.path().display().to_string(),
            "-o",
            &out_file.path().display().to_string(),
            "--columns",
            "1,3",
        ])
        .output()
        .expect("execute sonda import --columns");

    assert!(
        output.status.success(),
        "exit code: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let yaml = std::fs::read_to_string(out_file.path()).expect("read output YAML");
    assert!(yaml.contains("name: cpu"), "cpu should be included");
    assert!(yaml.contains("name: disk"), "disk should be included");
    assert!(!yaml.contains("name: mem"), "mem should NOT be included");
}

// -----------------------------------------------------------------------
// Grafana CSV: labels preserved
// -----------------------------------------------------------------------

#[test]
fn import_grafana_csv_preserves_labels_in_yaml() {
    let csv = concat!(
        r#""Time","{__name__=""up"", instance=""localhost:9090"", job=""prometheus""}""#,
        "\n",
        "1000,1\n",
        "2000,1\n",
        "3000,1\n",
        "4000,1\n",
        "5000,1\n",
    );
    let csv_file = write_temp_csv(csv);
    let out_file = NamedTempFile::new().expect("create output file");

    let output = std::process::Command::new(sonda_bin())
        .args([
            "import",
            &csv_file.path().display().to_string(),
            "-o",
            &out_file.path().display().to_string(),
        ])
        .output()
        .expect("execute sonda import -o");

    assert!(
        output.status.success(),
        "exit code: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let yaml = std::fs::read_to_string(out_file.path()).expect("read output YAML");
    assert!(yaml.contains("name: up"), "metric name should be 'up'");
    assert!(
        yaml.contains("instance:"),
        "instance label should be preserved"
    );
    assert!(yaml.contains("job:"), "job label should be preserved");
}

// -----------------------------------------------------------------------
// Error cases
// -----------------------------------------------------------------------

#[test]
fn import_nonexistent_file_returns_nonzero_exit() {
    let output = std::process::Command::new(sonda_bin())
        .args(["import", "/nonexistent/file.csv", "--analyze"])
        .output()
        .expect("execute sonda import");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error"),
        "expected error message, got: {stderr}"
    );
}

#[test]
fn import_no_mode_specified_returns_nonzero_exit() {
    let csv = "timestamp,cpu\n1000,50.0\n";
    let csv_file = write_temp_csv(csv);

    let output = std::process::Command::new(sonda_bin())
        .args(["import", &csv_file.path().display().to_string()])
        .output()
        .expect("execute sonda import");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--analyze") || stderr.contains("-o") || stderr.contains("--run"),
        "error should mention available modes, got: {stderr}"
    );
}

// -----------------------------------------------------------------------
// --run: generate and execute (short duration)
// -----------------------------------------------------------------------

#[test]
fn import_run_generates_and_executes_successfully() {
    let csv = "timestamp,cpu\n1000,50.0\n2000,50.1\n3000,49.9\n4000,50.2\n5000,49.8\n";
    let csv_file = write_temp_csv(csv);

    let output = std::process::Command::new(sonda_bin())
        .args([
            "import",
            &csv_file.path().display().to_string(),
            "--run",
            "--duration",
            "1s",
            "--quiet",
        ])
        .output()
        .expect("execute sonda import --run");

    assert!(
        output.status.success(),
        "exit code: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // stdout should contain generated metric output.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cpu"),
        "expected metric name in output, got: {stdout}"
    );
}
