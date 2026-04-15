//! Filesystem-based scenario discovery.
//!
//! Scenario YAML files live outside the binary as standalone files. The CLI
//! discovers them via a search path, scans directories for `.yaml`/`.yml`
//! files, and provides a [`ScenarioCatalog`] that caches the results for the
//! duration of one invocation.
//!
//! # Search path (priority order)
//!
//! 1. `--scenario-path` CLI flag (sole path when present — overrides everything)
//! 2. `SONDA_SCENARIO_PATH` environment variable (colon-separated directories)
//! 3. `./scenarios/` relative to the current working directory
//! 4. `~/.sonda/scenarios/` in the user's home directory
//!
//! Non-existent directories are silently skipped. Name collisions across
//! tiers are resolved by first-match-wins (highest-priority path).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sonda_core::BuiltinScenario;

// ---------------------------------------------------------------------------
// Scenario catalog
// ---------------------------------------------------------------------------

/// A cached catalog of discovered scenarios on the filesystem.
///
/// Built once per CLI invocation by scanning the search path. Thread-safe
/// by construction (immutable after build).
#[derive(Debug)]
pub struct ScenarioCatalog {
    /// Scenarios indexed by normalized name, deduplicated (first match wins).
    entries: Vec<BuiltinScenario>,
}

impl ScenarioCatalog {
    /// Build a scenario catalog by scanning the given search path directories.
    ///
    /// Directories that do not exist are silently skipped. Files that fail
    /// to read or parse emit a warning to stderr and are excluded from the
    /// catalog. Duplicate names are resolved by first-match (earlier
    /// directory in the search path wins).
    pub fn discover(search_path: &[PathBuf]) -> Self {
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut entries: Vec<BuiltinScenario> = Vec::new();

        for dir in search_path {
            if !dir.is_dir() {
                continue;
            }

            let read_dir = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(e) => {
                    eprintln!(
                        "warning: cannot read scenario directory {}: {}",
                        dir.display(),
                        e
                    );
                    continue;
                }
            };

            for dir_entry in read_dir {
                let dir_entry = match dir_entry {
                    Ok(de) => de,
                    Err(e) => {
                        eprintln!("warning: error reading entry in {}: {}", dir.display(), e);
                        continue;
                    }
                };

                let path = dir_entry.path();

                // Follow symlinks via metadata (not symlink_metadata).
                let meta = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("warning: cannot stat {}: {}", path.display(), e);
                        continue;
                    }
                };

                if !meta.is_file() {
                    continue;
                }

                let ext = path.extension().and_then(|e| e.to_str());
                if ext != Some("yaml") && ext != Some("yml") {
                    continue;
                }

                let normalized = match normalize_filename(&path) {
                    Some(n) => n,
                    None => continue,
                };

                // Skip duplicates (first match wins).
                if seen.contains_key(&normalized) {
                    continue;
                }

                // Parse lightweight metadata from the YAML.
                match read_scenario_metadata(&path) {
                    Ok(entry) => {
                        seen.insert(normalized, entries.len());
                        entries.push(entry);
                    }
                    Err(e) => {
                        eprintln!("warning: skipping {}: {}", path.display(), e);
                    }
                }
            }
        }

        ScenarioCatalog { entries }
    }

    /// Return all discovered scenarios.
    pub fn list(&self) -> &[BuiltinScenario] {
        &self.entries
    }

    /// Return scenarios matching a category filter.
    pub fn list_by_category(&self, category: &str) -> Vec<&BuiltinScenario> {
        self.entries
            .iter()
            .filter(|e| e.category == category)
            .collect()
    }

    /// Find a scenario by its kebab-case name.
    ///
    /// Name matching normalizes hyphens to underscores for comparison.
    pub fn find(&self, name: &str) -> Option<&BuiltinScenario> {
        let query = name.replace('-', "_");
        self.entries
            .iter()
            .find(|e| e.name.replace('-', "_") == query)
    }

    /// Return all available scenario names, useful for error messages.
    pub fn available_names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Read the raw YAML content for a named scenario.
    ///
    /// Returns `None` if the scenario is not in the catalog, or an error if the
    /// file cannot be read.
    pub fn read_yaml(&self, name: &str) -> Option<Result<String, std::io::Error>> {
        self.find(name)
            .map(|entry| std::fs::read_to_string(&entry.source_path))
    }
}

// ---------------------------------------------------------------------------
// Search path construction
// ---------------------------------------------------------------------------

/// Build the scenario search path from CLI flag, environment variable, and defaults.
///
/// When `cli_scenario_path` is `Some`, it is the **sole** entry in the search
/// path (overrides env and defaults). Otherwise the path is assembled from:
///
/// 1. `SONDA_SCENARIO_PATH` env var (colon-separated)
/// 2. `./scenarios/` relative to CWD
/// 3. `~/.sonda/scenarios/`
pub fn build_search_path(cli_scenario_path: Option<&Path>) -> Vec<PathBuf> {
    if let Some(p) = cli_scenario_path {
        return vec![p.to_path_buf()];
    }

    let mut dirs: Vec<PathBuf> = Vec::new();

    // SONDA_SCENARIO_PATH env var (colon-separated).
    if let Ok(env_val) = std::env::var("SONDA_SCENARIO_PATH") {
        for segment in env_val.split(':') {
            let trimmed = segment.trim();
            if !trimmed.is_empty() {
                dirs.push(PathBuf::from(trimmed));
            }
        }
    }

    // ./scenarios/ relative to CWD.
    dirs.push(PathBuf::from("./scenarios"));

    // ~/.sonda/scenarios/
    if let Some(home) = home_dir() {
        dirs.push(home.join(".sonda").join("scenarios"));
    }

    dirs
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Normalize a YAML filename to a scenario name.
///
/// Strips the `.yaml`/`.yml` extension and replaces hyphens with underscores.
fn normalize_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    Some(stem.replace('-', "_"))
}

/// Lightweight YAML metadata probe for scenario files.
///
/// Parses only the metadata fields (`scenario_name`, `category`,
/// `signal_type`, `description`) without fully constructing the scenario
/// config. Uses `serde_yaml_ng` directly since the CLI crate already
/// depends on it.
fn read_scenario_metadata(path: &Path) -> Result<BuiltinScenario, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("cannot read file: {e}"))?;

    #[derive(serde::Deserialize)]
    struct Probe {
        scenario_name: Option<String>,
        category: Option<String>,
        signal_type: Option<String>,
        description: Option<String>,
        scenarios: Option<Vec<EntrySignalProbe>>,
    }

    #[derive(serde::Deserialize)]
    struct EntrySignalProbe {
        signal_type: Option<String>,
    }

    let probe: Probe =
        serde_yaml_ng::from_str(&content).map_err(|e| format!("invalid YAML: {e}"))?;

    let filename_stem = normalize_filename(path)
        .ok_or_else(|| "cannot determine scenario name from filename".to_string())?;

    // Use the YAML `scenario_name` field if present, otherwise derive from filename.
    let name = probe
        .scenario_name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| filename_stem.replace('_', "-"));

    let category = probe
        .category
        .unwrap_or_else(|| "uncategorized".to_string());

    // Fallback order for signal_type: root wins (v1 preserved) → first entry's
    // signal_type (v2 migrations) → `"metrics"` default.
    let signal_type = probe
        .signal_type
        .or_else(|| {
            probe
                .scenarios
                .and_then(|entries| entries.into_iter().next())
                .and_then(|entry| entry.signal_type)
        })
        .unwrap_or_else(|| "metrics".to_string());

    let description = probe.description.unwrap_or_default();

    let source_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    Ok(BuiltinScenario {
        name,
        category,
        signal_type,
        description,
        source_path,
    })
}

/// Retrieve the user's home directory.
///
/// Uses the `HOME` environment variable on Unix and `USERPROFILE` on Windows.
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temporary directory with a unique name for testing.
    fn temp_scenario_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonda-scenarios-test-{suffix}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("must create temp dir");
        dir
    }

    fn write_scenario(dir: &Path, filename: &str, content: &str) {
        fs::write(dir.join(filename), content).expect("must write scenario");
    }

    fn valid_scenario_yaml(name: &str, category: &str, signal_type: &str) -> String {
        format!(
            r#"scenario_name: {name}
category: {category}
signal_type: {signal_type}
description: "Test scenario for {name}"

name: test_metric
rate: 1
duration: 10s

generator:
  type: constant
  value: 1.0

encoder:
  type: prometheus_text

sink:
  type: stdout
"#
        )
    }

    // ---- normalize_filename ---------------------------------------------------

    #[test]
    fn normalize_filename_strips_yaml_extension() {
        assert_eq!(
            normalize_filename(Path::new("cpu-spike.yaml")),
            Some("cpu_spike".to_string())
        );
    }

    #[test]
    fn normalize_filename_strips_yml_extension() {
        assert_eq!(
            normalize_filename(Path::new("memory-leak.yml")),
            Some("memory_leak".to_string())
        );
    }

    #[test]
    fn normalize_filename_preserves_underscores() {
        assert_eq!(
            normalize_filename(Path::new("already_snake.yaml")),
            Some("already_snake".to_string())
        );
    }

    // ---- ScenarioCatalog::discover --------------------------------------------

    #[test]
    fn discover_empty_directory_produces_empty_catalog() {
        let dir = temp_scenario_dir("empty");
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert!(catalog.list().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_valid_scenario_found() {
        let dir = temp_scenario_dir("valid");
        write_scenario(
            &dir,
            "cpu-spike.yaml",
            &valid_scenario_yaml("cpu-spike", "infrastructure", "metrics"),
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].name, "cpu-spike");
        assert_eq!(catalog.list()[0].category, "infrastructure");
        assert_eq!(catalog.list()[0].signal_type, "metrics");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_skips_non_yaml_files() {
        let dir = temp_scenario_dir("non-yaml");
        write_scenario(&dir, "readme.txt", "not a scenario");
        write_scenario(&dir, "data.json", "{}");
        write_scenario(
            &dir,
            "good.yaml",
            &valid_scenario_yaml("good", "test", "metrics"),
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].name, "good");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_skips_invalid_yaml_without_crashing() {
        let dir = temp_scenario_dir("invalid-yaml");
        write_scenario(&dir, "bad.yaml", "not: valid: yaml: :::");
        write_scenario(
            &dir,
            "good.yaml",
            &valid_scenario_yaml("good", "test", "metrics"),
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].name, "good");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_nonexistent_directory_silently_skipped() {
        let catalog =
            ScenarioCatalog::discover(&[PathBuf::from("/nonexistent/path/for/scenario/testing")]);
        assert!(catalog.list().is_empty());
    }

    #[test]
    fn discover_name_collision_first_match_wins() {
        let dir1 = temp_scenario_dir("prio-high");
        let dir2 = temp_scenario_dir("prio-low");
        write_scenario(
            &dir1,
            "my-scenario.yaml",
            &format!(
                r#"scenario_name: my-scenario
description: "high priority"
category: high
signal_type: metrics

name: test
rate: 1
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
"#
            ),
        );
        write_scenario(
            &dir2,
            "my-scenario.yaml",
            &format!(
                r#"scenario_name: my-scenario
description: "low priority"
category: low
signal_type: metrics

name: test
rate: 1
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
"#
            ),
        );
        let catalog = ScenarioCatalog::discover(&[dir1.clone(), dir2.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].description, "high priority");
        let _ = fs::remove_dir_all(&dir1);
        let _ = fs::remove_dir_all(&dir2);
    }

    // ---- ScenarioCatalog::find -------------------------------------------------

    #[test]
    fn find_by_name() {
        let dir = temp_scenario_dir("find");
        write_scenario(
            &dir,
            "cpu-spike.yaml",
            &valid_scenario_yaml("cpu-spike", "infrastructure", "metrics"),
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        // Find with hyphens (exact).
        assert!(catalog.find("cpu-spike").is_some());
        // Find with underscores (normalized).
        assert!(catalog.find("cpu_spike").is_some());
        // Not found.
        assert!(catalog.find("nonexistent").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- ScenarioCatalog::list_by_category ------------------------------------

    #[test]
    fn list_by_category_filters_correctly() {
        let dir = temp_scenario_dir("category");
        write_scenario(
            &dir,
            "scenario-a.yaml",
            &valid_scenario_yaml("scenario-a", "network", "metrics"),
        );
        write_scenario(
            &dir,
            "scenario-b.yaml",
            &valid_scenario_yaml("scenario-b", "application", "logs"),
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        let network = catalog.list_by_category("network");
        assert_eq!(network.len(), 1);
        assert_eq!(network[0].name, "scenario-a");
        let app = catalog.list_by_category("application");
        assert_eq!(app.len(), 1);
        assert_eq!(app[0].name, "scenario-b");
        let empty = catalog.list_by_category("nonexistent");
        assert!(empty.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- ScenarioCatalog::read_yaml -------------------------------------------

    #[test]
    fn read_yaml_returns_file_content() {
        let dir = temp_scenario_dir("read-yaml");
        let yaml = valid_scenario_yaml("read-test", "test", "metrics");
        write_scenario(&dir, "read-test.yaml", &yaml);
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        let content = catalog
            .read_yaml("read-test")
            .expect("scenario must be in catalog")
            .expect("file must be readable");
        assert!(content.contains("read-test"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_yaml_unknown_name_returns_none() {
        let catalog = ScenarioCatalog::discover(&[]);
        assert!(catalog.read_yaml("nonexistent").is_none());
    }

    // ---- build_search_path ----------------------------------------------------

    #[test]
    fn build_search_path_cli_flag_overrides_all() {
        let path = build_search_path(Some(Path::new("/custom/scenarios")));
        assert_eq!(path, vec![PathBuf::from("/custom/scenarios")]);
    }

    #[test]
    fn build_search_path_default_includes_cwd_scenarios() {
        let path = build_search_path(None);
        assert!(
            path.iter().any(|p| p.ends_with("scenarios")),
            "default search path must include a 'scenarios' directory"
        );
    }

    // ---- available_names ------------------------------------------------------

    #[test]
    fn available_names_matches_catalog_count() {
        let dir = temp_scenario_dir("avail-names");
        write_scenario(&dir, "a.yaml", &valid_scenario_yaml("a", "test", "metrics"));
        write_scenario(&dir, "b.yaml", &valid_scenario_yaml("b", "test", "metrics"));
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.available_names().len(), catalog.list().len());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- metadata fallback tests ----------------------------------------------

    #[test]
    fn scenario_without_metadata_uses_filename_defaults() {
        let dir = temp_scenario_dir("no-meta");
        write_scenario(
            &dir,
            "my-scenario.yaml",
            r#"name: test_metric
rate: 1
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
"#,
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        let entry = &catalog.list()[0];
        // Name derived from filename, converting underscores to hyphens.
        assert_eq!(entry.name, "my-scenario");
        assert_eq!(entry.category, "uncategorized");
        assert_eq!(entry.signal_type, "metrics");
        assert!(entry.description.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- signal_type fallback via first scenario entry (v2) -------------------

    #[test]
    fn v2_scenario_first_entry_signal_type_logs_wins_when_root_absent() {
        let dir = temp_scenario_dir("v2-entry-logs");
        write_scenario(
            &dir,
            "log-storm.yaml",
            r#"version: 2
scenario_name: log-storm
category: application
description: "v2 log storm"
scenarios:
  - name: bursty_logs
    signal_type: logs
"#,
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].signal_type, "logs");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_scenario_first_entry_signal_type_histogram_wins_when_root_absent() {
        let dir = temp_scenario_dir("v2-entry-histogram");
        write_scenario(
            &dir,
            "histogram-latency.yaml",
            r#"version: 2
scenario_name: histogram-latency
category: application
description: "v2 histogram latency"
scenarios:
  - name: latency_buckets
    signal_type: histogram
"#,
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].signal_type, "histogram");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_scenario_root_signal_type_wins_over_first_entry() {
        let dir = temp_scenario_dir("v2-root-wins");
        write_scenario(
            &dir,
            "mixed.yaml",
            r#"version: 2
scenario_name: mixed
category: infrastructure
signal_type: metrics
description: "root metrics overrides entry logs"
scenarios:
  - name: some_logs
    signal_type: logs
"#,
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].signal_type, "metrics");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_scenario_root_signal_type_logs_preserved_without_entries() {
        let dir = temp_scenario_dir("v1-root-logs");
        write_scenario(
            &dir,
            "legacy-logs.yaml",
            r#"scenario_name: legacy-logs
category: application
signal_type: logs
description: "v1 log scenario"

name: test_log
rate: 1
generator:
  type: constant
  value: 1.0
encoder:
  type: json
sink:
  type: stdout
"#,
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].signal_type, "logs");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_scenario_without_any_signal_type_defaults_to_metrics() {
        let dir = temp_scenario_dir("v1-no-signal");
        write_scenario(
            &dir,
            "untyped.yaml",
            r#"scenario_name: untyped
category: uncategorized
description: "no signal_type anywhere"

name: test_metric
rate: 1
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
"#,
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].signal_type, "metrics");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_scenario_empty_scenarios_list_defaults_to_metrics() {
        let dir = temp_scenario_dir("v2-empty-scenarios");
        write_scenario(
            &dir,
            "empty.yaml",
            r#"version: 2
scenario_name: empty
category: infrastructure
description: "v2 with empty scenarios list"
scenarios: []
"#,
        );
        let catalog = ScenarioCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].signal_type, "metrics");
        let _ = fs::remove_dir_all(&dir);
    }
}
