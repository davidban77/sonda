//! Smoke tests for critical 1.9 examples.
//!
//! Runs the real sonda binary against example YAMLs.
//! Long-running examples use --duration to keep tests under ~20s.

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::sonda_bin;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonda crate must have a parent workspace directory")
        .to_path_buf()
}

fn example(name: &str) -> PathBuf {
    workspace_root().join("examples").join(name)
}

fn packs_dir() -> PathBuf {
    workspace_root().join("packs")
}

/// Every `.yaml`/`.yml` file under `dir`, at any depth, sorted, with its text.
fn yaml_files(dir: &std::path::Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(yaml_files(&path));
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml" | "yml")
        ) {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            found.push((path, text));
        }
    }
    found.sort();
    found
}

/// Whether `text` is a YAML mapping with a top-level `scenarios:` or `kind:`
/// key, the keys a scenario file has. Panics when `text` is not YAML.
fn has_scenario_keys(path: &std::path::Path, text: &str) -> bool {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .unwrap_or_else(|e| panic!("{} is not YAML: {e}", path.display()));
    value
        .as_mapping()
        .is_some_and(|map| map.contains_key("scenarios") || map.contains_key("kind"))
}

// ---- every example ------------------------------------------------------------

/// Examples whose `tls:` files are made locally and never committed, each
/// with the first file its dry-run reads. A build with the gnmi feature
/// reads that file at dry-run, so these examples are dry-run from a
/// directory without it.
#[cfg(feature = "gnmi")]
const GENERATED_TLS_FILES: &[(&str, &str)] = &[(
    "gnmi-telegraf-tls.yaml",
    "examples/telegraf/certs/server.pem",
)];

/// Every scenario in `examples/` passes `--dry-run`, and its display names
/// each generator, encoder and sink: no field falls back to `unknown (...)`.
///
/// A scenario is a YAML file whose top level declares `version: 2`.
/// `examples/` also holds Compose files, alert rules and collector configs,
/// which declare none; a file without it that has a top-level `scenarios:`
/// or `kind:` key fails here instead of being left out.
#[test]
fn every_example_dry_runs_and_names_each_kind() {
    let (scenarios, others): (Vec<_>, Vec<_>) = yaml_files(&workspace_root().join("examples"))
        .into_iter()
        .partition(|(_, text)| sonda_core::compiler::parse::detect_version(text) == Some(2));
    for (path, text) in &others {
        assert!(
            !has_scenario_keys(path, text),
            "{} has scenario keys but no `version: 2`, so it would not be dry-run",
            path.display()
        );
    }
    #[allow(unused_mut)]
    let mut examples: Vec<PathBuf> = scenarios.into_iter().map(|(path, _)| path).collect();
    assert!(!examples.is_empty(), "no v2 scenario files under examples/");
    for name in ["gnmi-telegraf.yaml", "gnmi-pack.yaml"] {
        assert!(
            examples.contains(&example(name)),
            "{name} is not among the scenarios found: {examples:?}"
        );
    }
    #[cfg(feature = "gnmi")]
    for (name, _) in GENERATED_TLS_FILES {
        let path = example(name);
        assert!(
            examples.contains(&path),
            "{name} is not among the scenarios found: {examples:?}"
        );
        examples.retain(|p| *p != path);
    }

    for path in &examples {
        let output = Command::new(sonda_bin())
            .current_dir(workspace_root())
            .arg("--catalog")
            .arg(packs_dir())
            .args(["--dry-run", "run"])
            .arg(path)
            .output()
            .expect("sonda binary must launch");
        let shown = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "{} must dry-run clean; output: {shown}",
            path.display()
        );
        assert!(
            shown.contains("[config] file:"),
            "{} printed no dry-run display; output: {shown}",
            path.display()
        );
        assert!(
            !shown.contains("unknown ("),
            "{} has a field displayed as unknown: {shown}",
            path.display()
        );
    }
}

/// An example whose `tls:` files are made locally fails `--dry-run` where
/// they are absent, with an error naming the field and the file.
#[cfg(feature = "gnmi")]
#[test]
fn an_example_without_its_generated_tls_files_names_the_missing_file() {
    let empty = tempfile::tempdir().expect("a temporary directory");
    for (name, file) in GENERATED_TLS_FILES {
        let output = Command::new(sonda_bin())
            .current_dir(empty.path())
            .args(["--dry-run", "run"])
            .arg(example(name))
            .output()
            .expect("sonda binary must launch");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{name} dry-runs without {file}; stderr: {stderr}"
        );
        assert!(
            stderr.contains(&format!("gnmi_target tls cert {file:?} cannot be read")),
            "{name}: {stderr}"
        );
    }
}

// ---- basic-metrics.yaml -------------------------------------------------------

#[test]
fn example_basic_metrics_runs_and_emits_prometheus_text() {
    let output = Command::new(sonda_bin())
        .current_dir(workspace_root())
        .args(["--quiet", "run"])
        .arg(example("basic-metrics.yaml"))
        .args(["--duration", "200ms"])
        .output()
        .expect("sonda binary must launch");
    assert!(
        output.status.success(),
        "exit 0 expected; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "basic-metrics must emit to stdout");
    assert!(
        stdout.contains("interface_oper_state"),
        "must emit metric name in stdout; got: {stdout}"
    );
    let data_lines = stdout
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count();
    assert!(data_lines > 0, "must emit at least one metric line");
}

// ---- csv-replay-metrics.yaml --------------------------------------------------

#[test]
fn example_csv_replay_metrics_runs_and_emits_csv_values() {
    let output = Command::new(sonda_bin())
        .current_dir(workspace_root())
        .args(["--quiet", "run"])
        .arg(example("csv-replay-metrics.yaml"))
        .args(["--duration", "15s"])
        .output()
        .expect("sonda binary must launch");
    assert!(
        output.status.success(),
        "exit 0 expected; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "csv-replay-metrics must emit to stdout");
    assert!(
        stdout.contains("cpu_replay"),
        "must emit cpu_replay metric; got: {stdout}"
    );
    // The CSV starts at 12.3 then 14.1 -- verify actual values are replayed.
    assert!(
        stdout.contains("12.3") || stdout.contains("14.1"),
        "must replay CSV values (12.3, 14.1, ...); got: {stdout}"
    );
}

// ---- log-csv-replay.yaml ------------------------------------------------------

#[test]
fn example_log_csv_replay_runs_and_emits_json_lines() {
    let output = Command::new(sonda_bin())
        .current_dir(workspace_root())
        .args(["--quiet", "run"])
        .arg(example("log-csv-replay.yaml"))
        .args(["--duration", "10s"])
        .output()
        .expect("sonda binary must launch");
    assert!(
        output.status.success(),
        "exit 0 expected; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "log-csv-replay must emit to stdout; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for line in stdout.lines().filter(|l| !l.is_empty()) {
        let v: serde_json::Value =
            serde_json::from_str(line).expect("each log line must be valid JSON");
        assert!(v.get("timestamp").is_some(), "must have timestamp: {line}");
        assert!(v.get("message").is_some(), "must have message: {line}");
        assert!(v.get("severity").is_some(), "must have severity: {line}");
    }
    assert!(
        stdout.contains("GET /api/v1/health") || stdout.contains("POST /api/v1/events"),
        "must replay CSV messages; got: {stdout}"
    );
}

// ---- network-link-failure.yaml ------------------------------------------------

#[test]
fn example_network_link_failure_runs_multi_scenario_and_emits_all_metrics() {
    let output = Command::new(sonda_bin())
        .current_dir(workspace_root())
        .args(["--quiet", "run"])
        .arg(example("network-link-failure.yaml"))
        .args(["--duration", "3s"])
        .output()
        .expect("sonda binary must launch");
    assert!(
        output.status.success(),
        "exit 0 expected; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "network-link-failure must emit to stdout"
    );
    // All 6 scenarios must emit at least one line.
    assert!(
        stdout.contains("interface_oper_state"),
        "must emit interface_oper_state"
    );
    assert!(
        stdout.contains("interface_in_octets"),
        "must emit interface_in_octets"
    );
    assert!(
        stdout.contains("interface_errors"),
        "must emit interface_errors"
    );
    assert!(
        stdout.contains("device_cpu_percent"),
        "must emit device_cpu_percent"
    );
    // Label propagation: the device label must appear in the output.
    assert!(
        stdout.contains("rtr-core-01"),
        "must include device rtr-core-01 in labels"
    );
}

// ---- pack-scenario.yaml via catalog -------------------------------------------

#[test]
fn example_pack_scenario_expands_via_catalog_and_emits_all_pack_metrics() {
    let catalog = packs_dir();
    assert!(
        catalog.exists(),
        "packs dir must exist at: {}",
        catalog.display()
    );

    let output = Command::new(sonda_bin())
        .current_dir(workspace_root())
        .args(["--quiet", "--catalog"])
        .arg(&catalog)
        .args(["run"])
        .arg(example("pack-scenario.yaml"))
        .args(["--duration", "3s"])
        .output()
        .expect("sonda binary must launch");

    assert!(
        output.status.success(),
        "exit 0 expected; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "pack-scenario must emit to stdout");
    // All 5 metrics from telegraf_snmp_interface must appear.
    assert!(stdout.contains("ifOperStatus"), "must emit ifOperStatus");
    assert!(stdout.contains("ifHCInOctets"), "must emit ifHCInOctets");
    assert!(stdout.contains("ifHCOutOctets"), "must emit ifHCOutOctets");
    assert!(stdout.contains("ifInErrors"), "must emit ifInErrors");
    assert!(stdout.contains("ifOutErrors"), "must emit ifOutErrors");
    // Labels from the pack entry must appear.
    assert!(
        stdout.contains("rtr-edge-01"),
        "must include rtr-edge-01 device label"
    );
    assert!(
        stdout.contains("snmp"),
        "must include job=snmp from pack shared_labels"
    );
    // The step generator produces strictly increasing values for ifHCInOctets.
    let step_values: Vec<f64> = stdout
        .lines()
        .filter(|l| l.contains("ifHCInOctets"))
        .filter_map(|l| {
            l.split("} ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        })
        .collect();
    assert!(
        step_values.len() >= 2,
        "must emit at least 2 ifHCInOctets samples; got: {step_values:?}"
    );
    assert!(
        step_values.windows(2).all(|w| w[1] > w[0]),
        "ifHCInOctets must produce strictly increasing values; got: {step_values:?}"
    );
}

// ---- histogram.yaml -----------------------------------------------------------

#[test]
fn example_histogram_emits_prometheus_histogram_triplet() {
    // histogram.yaml has duration: 10s -- run it to completion.
    let output = Command::new(sonda_bin())
        .current_dir(workspace_root())
        .args(["--quiet", "run"])
        .arg(example("histogram.yaml"))
        .output()
        .expect("sonda binary must launch");

    assert!(
        output.status.success(),
        "exit 0 expected; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "histogram must emit to stdout");

    let bucket_lines: usize = stdout
        .lines()
        .filter(|l| l.contains("http_request_duration_seconds_bucket"))
        .count();
    let count_lines: usize = stdout
        .lines()
        .filter(|l| l.contains("http_request_duration_seconds_count"))
        .count();
    let sum_lines: usize = stdout
        .lines()
        .filter(|l| l.contains("http_request_duration_seconds_sum"))
        .count();

    assert!(bucket_lines > 0, "must emit _bucket series");
    assert!(count_lines > 0, "must emit _count series");
    assert!(sum_lines > 0, "must emit _sum series");

    // +Inf bucket value must equal _count at every tick.
    let inf_values: Vec<u64> = stdout
        .lines()
        .filter(|l| l.contains("+Inf"))
        .filter_map(|l| {
            l.split("} ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        })
        .collect();
    let count_values: Vec<u64> = stdout
        .lines()
        .filter(|l| l.contains("http_request_duration_seconds_count"))
        .filter_map(|l| {
            l.split("} ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        })
        .collect();
    assert!(
        !inf_values.is_empty() && inf_values == count_values,
        "+Inf bucket must equal _count; inf={inf_values:?} count={count_values:?}"
    );
}

// ---- log-template.yaml --------------------------------------------------------

#[test]
fn example_log_template_emits_structured_json_logs() {
    let output = Command::new(sonda_bin())
        .current_dir(workspace_root())
        .args(["--quiet", "run"])
        .arg(example("log-template.yaml"))
        .args(["--duration", "500ms"])
        .output()
        .expect("sonda binary must launch");

    assert!(
        output.status.success(),
        "exit 0 expected; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "log-template must emit to stdout");
    let mut line_count = 0usize;
    for line in stdout.lines().filter(|l| !l.is_empty()) {
        line_count += 1;
        let v: serde_json::Value =
            serde_json::from_str(line).expect("each log line must be valid JSON");
        assert!(v.get("timestamp").is_some(), "must have timestamp: {line}");
        assert!(v.get("severity").is_some(), "must have severity: {line}");
        assert!(v.get("message").is_some(), "must have message: {line}");
    }
    assert!(line_count > 0, "must emit at least one log event");
}
