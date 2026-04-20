//! CI validation: every YAML file in `scenarios/` must satisfy the catalog
//! invariants that were previously enforced by compile-time tests in
//! `sonda-core::scenarios::mod`.
//!
//! This test suite replaces the 14 invariant tests that were removed during
//! the externalization refactor (#186). It discovers scenario YAML files from
//! the repo-root `scenarios/` directory and validates:
//!
//! - All names are unique and kebab-case.
//! - Categories belong to a known set.
//! - Signal types belong to a known set.
//! - Descriptions are non-empty.
//! - Each YAML file is non-empty.
//! - Each YAML parses as the appropriate config type based on signal_type.
//! - All scenarios use stdout sink.
//! - All scenarios have a finite duration.

use std::collections::HashSet;
use std::path::PathBuf;

use sonda_core::compiler::parse::{detect_version, parse as parse_v2};
use sonda_core::config::{
    HistogramScenarioConfig, LogScenarioConfig, ScenarioConfig, SummaryScenarioConfig,
};

/// Return the path to the repo-root `scenarios/` directory.
fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonda crate must have a parent directory")
        .join("scenarios")
}

/// Lightweight metadata probe matching the fields written in each scenario
/// YAML header.
#[derive(serde::Deserialize)]
struct ScenarioProbe {
    scenario_name: Option<String>,
    category: Option<String>,
    signal_type: Option<String>,
    description: Option<String>,
}

/// Collect all scenario YAML files with their probed metadata.
struct DiscoveredScenario {
    /// The scenario name (from `scenario_name` field, or derived from filename).
    name: String,
    /// The category (from `category` field, or "uncategorized").
    category: String,
    /// The signal type (from `signal_type` field, or "metrics").
    signal_type: String,
    /// The one-line description.
    description: String,
    /// Full YAML content.
    content: String,
    /// Path to the file (for error messages).
    path: PathBuf,
}

/// Discover all scenario YAML files from the repo-root `scenarios/` directory.
fn discover_all_scenarios() -> Vec<DiscoveredScenario> {
    let dir = scenarios_dir();
    assert!(dir.is_dir(), "scenarios/ directory must exist at repo root");

    let mut scenarios = Vec::new();

    for entry in std::fs::read_dir(&dir).expect("must read scenarios/ directory") {
        let entry = entry.expect("directory entry must be readable");
        let path = entry.path();

        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("yaml") && ext != Some("yml") {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));

        let probe: ScenarioProbe = serde_yaml_ng::from_str(&content)
            .unwrap_or_else(|e| panic!("{}: failed to parse metadata: {}", path.display(), e));

        let filename_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("filename must be valid UTF-8");

        let name = probe
            .scenario_name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| filename_stem.replace('_', "-"));

        let category = probe
            .category
            .unwrap_or_else(|| "uncategorized".to_string());

        let signal_type = probe.signal_type.unwrap_or_else(|| "metrics".to_string());

        let description = probe.description.unwrap_or_default();

        scenarios.push(DiscoveredScenario {
            name,
            category,
            signal_type,
            description,
            content,
            path,
        });
    }

    scenarios
}

// ---------------------------------------------------------------------------
// Catalog structure invariants
// ---------------------------------------------------------------------------

#[test]
fn catalog_is_not_empty() {
    let scenarios = discover_all_scenarios();
    assert!(
        !scenarios.is_empty(),
        "scenario catalog must contain at least one scenario"
    );
}

#[test]
fn all_names_are_unique() {
    let scenarios = discover_all_scenarios();
    let mut seen = HashSet::new();
    for scenario in &scenarios {
        assert!(
            seen.insert(&scenario.name),
            "duplicate scenario name: {:?}",
            scenario.name
        );
    }
}

#[test]
fn all_names_are_kebab_case() {
    let scenarios = discover_all_scenarios();
    for scenario in &scenarios {
        assert!(
            !scenario.name.is_empty(),
            "scenario name must not be empty (file: {})",
            scenario.path.display()
        );
        assert!(
            scenario
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'),
            "scenario name {:?} must be kebab-case (lowercase ASCII + hyphens only)",
            scenario.name
        );
    }
}

#[test]
fn all_categories_are_known() {
    let known = ["infrastructure", "network", "application", "observability"];
    let scenarios = discover_all_scenarios();
    for scenario in &scenarios {
        assert!(
            known.contains(&scenario.category.as_str()),
            "scenario {:?} has unknown category {:?}; expected one of {:?}",
            scenario.name,
            scenario.category,
            known
        );
    }
}

#[test]
fn all_signal_types_are_known() {
    let known = ["metrics", "logs", "multi", "histogram", "summary"];
    let scenarios = discover_all_scenarios();
    for scenario in &scenarios {
        assert!(
            known.contains(&scenario.signal_type.as_str()),
            "scenario {:?} has unknown signal_type {:?}; expected one of {:?}",
            scenario.name,
            scenario.signal_type,
            known
        );
    }
}

#[test]
fn all_descriptions_are_non_empty() {
    let scenarios = discover_all_scenarios();
    for scenario in &scenarios {
        assert!(
            !scenario.description.is_empty(),
            "scenario {:?} must have a non-empty description",
            scenario.name
        );
    }
}

#[test]
fn all_yamls_are_non_empty() {
    let scenarios = discover_all_scenarios();
    for scenario in &scenarios {
        assert!(
            !scenario.content.trim().is_empty(),
            "{}: YAML file must not be empty",
            scenario.path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// YAML parsing tests -- each signal type parses as the correct config type
//
// Dispatch: v2 files (detected via `version: 2`) are validated through the
// v2 compiler parser (`sonda_core::compiler::parse::parse`) regardless of
// `signal_type`, since v2 uses a single parse path for all entry kinds.
// v1 files (no `version:` key, or `version: 1`) retain the per-signal-type
// parse contract against the legacy `ScenarioConfig`-family types.
// ---------------------------------------------------------------------------

/// Ensure a v2 scenario YAML parses through the v2 compiler parser.
///
/// Panics with a descriptive message including the scenario path when the
/// parse fails. Centralized so every signal-type test reports the same
/// diagnostic.
fn assert_v2_parse_ok(scenario: &DiscoveredScenario) {
    let result = parse_v2(&scenario.content);
    assert!(
        result.is_ok(),
        "v2 scenario {:?} ({}) failed to parse via compiler::parse: {:?}",
        scenario.name,
        scenario.path.display(),
        result.err()
    );
}

#[test]
fn all_metrics_yamls_parse_as_scenario_config() {
    let scenarios = discover_all_scenarios();
    for scenario in scenarios.iter().filter(|s| s.signal_type == "metrics") {
        if detect_version(&scenario.content) == Some(2) {
            assert_v2_parse_ok(scenario);
            continue;
        }
        let result = serde_yaml_ng::from_str::<ScenarioConfig>(&scenario.content);
        assert!(
            result.is_ok(),
            "metrics scenario {:?} ({}) failed to parse as ScenarioConfig: {:?}",
            scenario.name,
            scenario.path.display(),
            result.err()
        );
    }
}

#[test]
fn all_logs_yamls_parse_as_log_scenario_config() {
    let scenarios = discover_all_scenarios();
    for scenario in scenarios.iter().filter(|s| s.signal_type == "logs") {
        if detect_version(&scenario.content) == Some(2) {
            assert_v2_parse_ok(scenario);
            continue;
        }
        let result = serde_yaml_ng::from_str::<LogScenarioConfig>(&scenario.content);
        assert!(
            result.is_ok(),
            "logs scenario {:?} ({}) failed to parse as LogScenarioConfig: {:?}",
            scenario.name,
            scenario.path.display(),
            result.err()
        );
    }
}

#[test]
fn all_multi_yamls_parse_through_v2_compiler() {
    let scenarios = discover_all_scenarios();
    for scenario in scenarios.iter().filter(|s| s.signal_type == "multi") {
        assert_eq!(
            detect_version(&scenario.content),
            Some(2),
            "multi scenario {:?} ({}) must declare `version: 2` — v1 multi \
             YAML is no longer accepted",
            scenario.name,
            scenario.path.display()
        );
        assert_v2_parse_ok(scenario);
    }
}

#[test]
fn all_histogram_yamls_parse_as_histogram_scenario_config() {
    let scenarios = discover_all_scenarios();
    for scenario in scenarios.iter().filter(|s| s.signal_type == "histogram") {
        if detect_version(&scenario.content) == Some(2) {
            assert_v2_parse_ok(scenario);
            continue;
        }
        let result = serde_yaml_ng::from_str::<HistogramScenarioConfig>(&scenario.content);
        assert!(
            result.is_ok(),
            "histogram scenario {:?} ({}) failed to parse as HistogramScenarioConfig: {:?}",
            scenario.name,
            scenario.path.display(),
            result.err()
        );
    }
}

#[test]
fn all_summary_yamls_parse_as_summary_scenario_config() {
    let scenarios = discover_all_scenarios();
    for scenario in scenarios.iter().filter(|s| s.signal_type == "summary") {
        if detect_version(&scenario.content) == Some(2) {
            assert_v2_parse_ok(scenario);
            continue;
        }
        let result = serde_yaml_ng::from_str::<SummaryScenarioConfig>(&scenario.content);
        assert!(
            result.is_ok(),
            "summary scenario {:?} ({}) failed to parse as SummaryScenarioConfig: {:?}",
            scenario.name,
            scenario.path.display(),
            result.err()
        );
    }
}

// ---------------------------------------------------------------------------
// Convention tests: stdout sink and finite duration
// ---------------------------------------------------------------------------

#[test]
fn all_scenarios_use_stdout_sink() {
    let scenarios = discover_all_scenarios();
    for scenario in &scenarios {
        assert!(
            scenario.content.contains("type: stdout"),
            "scenario {:?} ({}) must use stdout sink for zero-config usability",
            scenario.name,
            scenario.path.display()
        );
    }
}

#[test]
fn all_scenarios_have_finite_duration() {
    let scenarios = discover_all_scenarios();
    for scenario in &scenarios {
        assert!(
            scenario.content.contains("duration:"),
            "scenario {:?} ({}) must have a finite duration for self-termination",
            scenario.name,
            scenario.path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Minimum scenario count -- guard against accidental deletion
// ---------------------------------------------------------------------------

#[test]
fn scenario_catalog_has_expected_minimum_count() {
    let scenarios = discover_all_scenarios();
    assert!(
        scenarios.len() >= 11,
        "expected at least 11 scenario YAML files, found {}",
        scenarios.len()
    );
}
