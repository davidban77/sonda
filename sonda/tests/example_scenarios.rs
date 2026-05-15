//! Integration tests for the example scenario YAML files shipped with the project.
//!
//! Every file under `examples/*.yaml` that is a sonda scenario (not an
//! alertmanager or prometheus rules file) must:
//!
//! - Declare `version: 2` at the top level.
//! - Parse through the v2 compiler parser (`sonda_core::compiler::parse::parse`).
//! - Compile end-to-end through `sonda_core::compile_scenario_file` using a
//!   pack resolver pre-loaded with the three built-in packs shipped under
//!   `packs/` at the repo root.
//!
//! Per-file behavioural assertions (metric names, label sets, generator
//! variants, etc.) are preserved for the curated examples that the original
//! Slice 0.8 test suite covered. Generator, encoder, and sink factories are
//! exercised indirectly — a successful end-to-end compile implies every phase
//! (parse → normalize → expand → compile_after → prepare) accepted the file.

use std::path::PathBuf;

use sonda_core::compile_scenario_file;
use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::compiler::parse::{detect_version, parse as parse_v2};
use sonda_core::config::{DynamicLabelStrategy, ScenarioEntry};
use sonda_core::expand_entry;
use sonda_core::generator::GeneratorConfig;
use sonda_core::packs::MetricPackDef;

// ---------------------------------------------------------------------------
// Test-infra helpers
// ---------------------------------------------------------------------------

/// Return an absolute path to a file under the workspace root, regardless of
/// where `cargo test` is invoked from.
fn workspace_file(relative: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR for the `sonda` crate is `<workspace>/sonda`.
    // One parent step takes us to the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonda crate must have a parent directory (workspace root)")
        .join(relative)
}

/// Return the absolute path to the repo-root `examples/` directory.
fn examples_dir() -> PathBuf {
    workspace_file("examples")
}

/// Read and parse a metric pack YAML from the repo-root `packs/` directory.
fn load_repo_pack(file_name: &str) -> MetricPackDef {
    let path = workspace_file("packs").join(file_name);
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read pack {}: {e}", path.display()));
    serde_yaml_ng::from_str::<MetricPackDef>(&yaml)
        .unwrap_or_else(|e| panic!("cannot parse pack {}: {e}", path.display()))
}

/// Build an [`InMemoryPackResolver`] preloaded with the three built-in packs
/// shipped under `packs/` at the repo root. Every example that uses a pack
/// name references one of these three.
fn builtin_pack_resolver() -> InMemoryPackResolver {
    let mut r = InMemoryPackResolver::new();
    for (file, pack_name) in [
        ("telegraf-snmp-interface.yaml", "telegraf_snmp_interface"),
        ("node-exporter-cpu.yaml", "node_exporter_cpu"),
        ("node-exporter-memory.yaml", "node_exporter_memory"),
    ] {
        r.insert(pack_name, load_repo_pack(file));
    }
    r
}

/// Lightweight probe to detect whether a YAML file is a sonda scenario.
///
/// Sonda scenarios always contain one of `scenarios:` (v2 AST root) or
/// `version:` (v2 version tag). Non-sonda files under `examples/` (e.g.
/// alertmanager rule groups) declare unrelated top-level keys and are
/// skipped by the test sweep.
#[derive(serde::Deserialize)]
struct ScenarioProbe {
    version: Option<u32>,
    scenarios: Option<serde_yaml_ng::Value>,
    // Alertmanager / Prometheus rule files have a top-level `groups:` key.
    groups: Option<serde_yaml_ng::Value>,
}

fn is_sonda_scenario(yaml: &str) -> bool {
    let probe: ScenarioProbe = match serde_yaml_ng::from_str(yaml) {
        Ok(p) => p,
        // A YAML parse failure on the probe means this isn't a shape we
        // recognise — treat as non-sonda rather than failing the test.
        Err(_) => return false,
    };
    if probe.groups.is_some() {
        return false;
    }
    probe.version.is_some() || probe.scenarios.is_some()
}

/// Discover every `.yaml` file in `examples/` and classify each as either a
/// sonda scenario or a non-sonda file (e.g. alertmanager rules). Returns the
/// list of sonda-scenario file paths, sorted for deterministic reporting.
fn discover_sonda_example_files() -> Vec<PathBuf> {
    let dir = examples_dir();
    assert!(dir.is_dir(), "examples/ directory must exist at repo root");

    let mut files = Vec::new();
    collect_sonda_scenario_yamls(&dir, &mut files);
    files.sort();
    files
}

/// Recursively walk `dir` and push every `.yaml` file whose contents
/// [`is_sonda_scenario`] accepts.
///
/// The walk is intentionally simple (no `walkdir` dep) since `examples/`
/// is small and the structure is shallow. Recursion catches scenarios
/// nested under subdirectories like `examples/alertmanager/` that the
/// prior single-level `read_dir` glob missed.
fn collect_sonda_scenario_yamls(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!("read {}: {e}", dir.display());
    }) {
        let entry = entry.expect("directory entry must be readable");
        let path = entry.path();

        if path.is_dir() {
            collect_sonda_scenario_yamls(&path, out);
            continue;
        }

        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        if is_sonda_scenario(&content) {
            out.push(path);
        }
    }
}

/// Load the YAML text at `relative` (a workspace-relative path).
fn read_example(relative: &str) -> String {
    let path = workspace_file(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Assert that `yaml` compiles cleanly via the v2 pipeline against the
/// built-in pack resolver. Returns the compiled [`ScenarioEntry`]s.
fn compile_ok(label: &str, yaml: &str) -> Vec<ScenarioEntry> {
    let resolver = builtin_pack_resolver();
    compile_scenario_file(yaml, &resolver)
        .unwrap_or_else(|e| panic!("{label}: v2 compile failed: {e}"))
}

// ---------------------------------------------------------------------------
// Sweeping invariants: every sonda-scenario YAML in examples/ must be v2 and
// compile end-to-end.
// ---------------------------------------------------------------------------

#[test]
fn every_sonda_scenario_yaml_declares_version_2() {
    let files = discover_sonda_example_files();
    assert!(
        !files.is_empty(),
        "expected at least one sonda-scenario YAML under examples/"
    );
    for path in &files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        assert_eq!(
            detect_version(&content),
            Some(2),
            "{} must declare `version: 2` (post-migration)",
            path.display()
        );
    }
}

#[test]
fn every_sonda_scenario_yaml_parses_via_v2_compiler_parser() {
    let files = discover_sonda_example_files();
    for path in &files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        parse_v2(&content).unwrap_or_else(|e| {
            panic!(
                "{} failed to parse via compiler::parse: {e:?}",
                path.display()
            )
        });
    }
}

#[test]
fn every_sonda_scenario_yaml_compiles_end_to_end() {
    let files = discover_sonda_example_files();
    let resolver = builtin_pack_resolver();
    for path in &files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let entries = compile_scenario_file(&content, &resolver)
            .unwrap_or_else(|e| panic!("{} failed v2 compile: {e}", path.display()));
        assert!(
            !entries.is_empty(),
            "{} must compile to at least one ScenarioEntry",
            path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// examples/basic-metrics.yaml — metric-name / rate / duration / generator /
// gaps / labels / encoder / sink assertions preserved from the v1 suite.
// ---------------------------------------------------------------------------

#[test]
fn basic_metrics_yaml_compiles_to_single_metric_entry() {
    let yaml = read_example("examples/basic-metrics.yaml");
    let entries = compile_ok("basic-metrics.yaml", &yaml);
    assert_eq!(entries.len(), 1, "basic-metrics must produce one entry");

    let entry = &entries[0];
    match entry {
        ScenarioEntry::Metrics(config) => {
            assert_eq!(config.base.name, "interface_oper_state");
            assert_eq!(config.base.rate, 1000.0, "rate must be 1000 events/sec");
            assert_eq!(config.base.duration.as_deref(), Some("30s"));
            match config.generator {
                GeneratorConfig::Sine {
                    amplitude,
                    period_secs,
                    offset,
                } => {
                    assert_eq!(amplitude, 5.0, "sine amplitude must be 5.0");
                    assert_eq!(period_secs, 30.0, "sine period_secs must be 30");
                    assert_eq!(offset, 10.0, "sine offset must be 10.0");
                }
                ref other => panic!("expected Sine generator, got {other:?}"),
            }

            let gaps = config
                .base
                .gaps
                .as_ref()
                .expect("basic-metrics must define gaps");
            assert_eq!(gaps.every, "2m");
            assert_eq!(gaps.r#for, "20s");

            let labels = config
                .base
                .labels
                .as_ref()
                .expect("basic-metrics must define labels");
            assert_eq!(labels.get("hostname").map(String::as_str), Some("t0-a1"));
            assert_eq!(labels.get("zone").map(String::as_str), Some("eu1"));
        }
        other => panic!("expected Metrics entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/simple-constant.yaml
// ---------------------------------------------------------------------------

#[test]
fn simple_constant_yaml_compiles_to_constant_metric() {
    let yaml = read_example("examples/simple-constant.yaml");
    let entries = compile_ok("simple-constant.yaml", &yaml);
    assert_eq!(entries.len(), 1);

    match &entries[0] {
        ScenarioEntry::Metrics(config) => {
            assert_eq!(config.base.name, "up");
            assert_eq!(config.base.rate, 10.0);
            assert_eq!(config.base.duration.as_deref(), Some("10s"));
            match config.generator {
                GeneratorConfig::Constant { value } => assert_eq!(value, 1.0),
                ref other => panic!("expected Constant generator, got {other:?}"),
            }
            assert!(
                config.base.gaps.is_none(),
                "simple-constant must not define gaps"
            );
        }
        other => panic!("expected Metrics entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/cardinality-spike.yaml
// ---------------------------------------------------------------------------

#[test]
fn cardinality_spike_yaml_compiles_with_spike_config() {
    let yaml = read_example("examples/cardinality-spike.yaml");
    let entries = compile_ok("cardinality-spike.yaml", &yaml);
    assert_eq!(entries.len(), 1);

    match &entries[0] {
        ScenarioEntry::Metrics(config) => {
            assert_eq!(config.base.name, "cardinality_spike_demo");
            let spikes = config
                .base
                .cardinality_spikes
                .as_ref()
                .expect("cardinality_spikes must be present");
            assert_eq!(spikes.len(), 1, "must have exactly one spike entry");
            assert_eq!(spikes[0].label, "pod_name");
            assert_eq!(spikes[0].cardinality, 100);
        }
        other => panic!("expected Metrics entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/dynamic-labels-fleet.yaml
// ---------------------------------------------------------------------------

#[test]
fn dynamic_labels_fleet_yaml_compiles_with_single_dynamic_label() {
    let yaml = read_example("examples/dynamic-labels-fleet.yaml");
    let entries = compile_ok("dynamic-labels-fleet.yaml", &yaml);
    assert_eq!(entries.len(), 1);

    match &entries[0] {
        ScenarioEntry::Metrics(config) => {
            let dls = config
                .base
                .dynamic_labels
                .as_ref()
                .expect("dynamic_labels must be present");
            assert_eq!(dls.len(), 1, "must have exactly one dynamic label");
            assert_eq!(dls[0].key, "hostname");
        }
        other => panic!("expected Metrics entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/dynamic-labels-regions.yaml
// ---------------------------------------------------------------------------

#[test]
fn dynamic_labels_regions_yaml_uses_values_list_strategy() {
    let yaml = read_example("examples/dynamic-labels-regions.yaml");
    let entries = compile_ok("dynamic-labels-regions.yaml", &yaml);
    assert_eq!(entries.len(), 1);

    match &entries[0] {
        ScenarioEntry::Metrics(config) => {
            let dls = config
                .base
                .dynamic_labels
                .as_ref()
                .expect("dynamic_labels must be present");
            assert_eq!(dls.len(), 1, "must have exactly one dynamic label");
            assert_eq!(dls[0].key, "region");
            match &dls[0].strategy {
                DynamicLabelStrategy::ValuesList { values } => {
                    assert_eq!(values.len(), 3, "must have 3 region values");
                }
                other => panic!("expected ValuesList strategy, got {other:?}"),
            }
        }
        other => panic!("expected Metrics entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/dynamic-labels-multi.yaml
// ---------------------------------------------------------------------------

#[test]
fn dynamic_labels_multi_yaml_compiles_with_two_dynamic_labels() {
    let yaml = read_example("examples/dynamic-labels-multi.yaml");
    let entries = compile_ok("dynamic-labels-multi.yaml", &yaml);
    assert_eq!(entries.len(), 1);

    match &entries[0] {
        ScenarioEntry::Metrics(config) => {
            let dls = config
                .base
                .dynamic_labels
                .as_ref()
                .expect("dynamic_labels must be present");
            assert_eq!(dls.len(), 2, "must have two dynamic labels");
            assert_eq!(dls[0].key, "hostname");
            assert_eq!(dls[1].key, "region");
        }
        other => panic!("expected Metrics entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// examples/csv-replay-grafana-auto.yaml — compiles into one entry per CSV
// data column via the multi-column csv_replay expansion.
// ---------------------------------------------------------------------------

#[test]
fn csv_replay_grafana_auto_yaml_expands_to_two_scenarios() {
    // The v2 compile pipeline emits one entry for the multi-column csv_replay
    // scenario; column-level expansion happens at launch time via
    // `expand_entry` (called inside `prepare_entries`). Run expansion here
    // manually so the test can assert on per-column shape. The embedded
    // `file:` path is relative to the repo root, so rewrite it to an
    // absolute workspace path before compiling — tests may run from any cwd.
    let yaml = read_example("examples/csv-replay-grafana-auto.yaml");
    let absolute_csv = workspace_file("examples/grafana-export.csv")
        .to_string_lossy()
        .into_owned();
    let yaml = yaml.replace("examples/grafana-export.csv", &absolute_csv);
    let compiled = compile_ok("csv-replay-grafana-auto.yaml", &yaml);
    assert_eq!(compiled.len(), 1, "compile produces one csv_replay entry");

    let expanded = expand_entry(compiled.into_iter().next().unwrap())
        .expect("csv_replay multi-column expansion must succeed");
    assert_eq!(expanded.len(), 2, "Grafana export has 2 data columns");

    // Both columns should have metric name "up".
    for entry in &expanded {
        match entry {
            ScenarioEntry::Metrics(config) => {
                assert_eq!(config.base.name, "up");
            }
            other => panic!("expected Metrics entry, got {other:?}"),
        }
    }

    // First column: instance=localhost:9090, job=prometheus, env=production.
    let labels0 = expanded[0]
        .base()
        .labels
        .as_ref()
        .expect("labels must exist");
    assert_eq!(
        labels0.get("instance").map(String::as_str),
        Some("localhost:9090")
    );
    assert_eq!(labels0.get("job").map(String::as_str), Some("prometheus"));
    assert_eq!(labels0.get("env").map(String::as_str), Some("production"));

    // Second column: instance=localhost:9100, job=node, env=production.
    let labels1 = expanded[1]
        .base()
        .labels
        .as_ref()
        .expect("labels must exist");
    assert_eq!(
        labels1.get("instance").map(String::as_str),
        Some("localhost:9100")
    );
    assert_eq!(labels1.get("job").map(String::as_str), Some("node"));
    assert_eq!(labels1.get("env").map(String::as_str), Some("production"));
}

// ---------------------------------------------------------------------------
// examples/csv-replay-explicit-labels.yaml — per-column labels merged with
// scenario-level labels (column labels override on key conflict).
// ---------------------------------------------------------------------------

#[test]
fn csv_replay_explicit_labels_yaml_expands_with_per_column_labels() {
    let yaml = read_example("examples/csv-replay-explicit-labels.yaml");
    let absolute_csv = workspace_file("examples/sample-multi-column.csv")
        .to_string_lossy()
        .into_owned();
    let yaml = yaml.replace("examples/sample-multi-column.csv", &absolute_csv);
    let compiled = compile_ok("csv-replay-explicit-labels.yaml", &yaml);
    assert_eq!(compiled.len(), 1, "compile produces one csv_replay entry");

    let expanded = expand_entry(compiled.into_iter().next().unwrap())
        .expect("csv_replay multi-column expansion must succeed");
    assert_eq!(expanded.len(), 3, "should expand to 3 columns");

    let names: Vec<&str> = expanded.iter().map(|e| e.base().name.as_str()).collect();
    assert_eq!(names, ["cpu_percent", "mem_percent", "disk_io_mbps"]);

    // Column 0 (cpu_percent): adds core=0; carries scenario-level instance.
    let labels0 = expanded[0]
        .base()
        .labels
        .as_ref()
        .expect("labels must exist");
    assert_eq!(labels0.get("core").map(String::as_str), Some("0"));
    assert_eq!(
        labels0.get("instance").map(String::as_str),
        Some("prod-server-42")
    );

    // Column 1 (mem_percent): adds type=physical.
    let labels1 = expanded[1]
        .base()
        .labels
        .as_ref()
        .expect("labels must exist");
    assert_eq!(labels1.get("type").map(String::as_str), Some("physical"));

    // Column 2 (disk_io_mbps): only scenario-level labels.
    let labels2 = expanded[2]
        .base()
        .labels
        .as_ref()
        .expect("labels must exist");
    assert!(labels2.get("core").is_none());
    assert!(labels2.get("type").is_none());
    assert_eq!(
        labels2.get("instance").map(String::as_str),
        Some("prod-server-42")
    );
}

// ---------------------------------------------------------------------------
// examples/pack-scenario.yaml — pack reference expands to one entry per
// metric in the referenced pack.
// ---------------------------------------------------------------------------

#[test]
fn pack_scenario_yaml_expands_to_multiple_entries() {
    let yaml = read_example("examples/pack-scenario.yaml");
    let entries = compile_ok("pack-scenario.yaml", &yaml);
    assert!(
        entries.len() > 1,
        "pack expansion must produce more than one entry, got {}",
        entries.len()
    );
    // Every expanded entry carries the user-supplied device label.
    for entry in &entries {
        let labels = entry.base().labels.as_ref().expect("labels must exist");
        assert_eq!(
            labels.get("device").map(String::as_str),
            Some("rtr-edge-01"),
            "every pack entry must carry device=rtr-edge-01"
        );
    }
}
