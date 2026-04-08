//! Filesystem-based metric pack discovery.
//!
//! Pack YAML files live outside the binary as standalone files. The CLI
//! discovers them via a search path, scans directories for `.yaml`/`.yml`
//! files, and provides a [`PackCatalog`] that caches the results for the
//! duration of one invocation.
//!
//! # Search path (priority order)
//!
//! 1. `--pack-path` CLI flag (sole path when present — overrides everything)
//! 2. `SONDA_PACK_PATH` environment variable (colon-separated directories)
//! 3. `./packs/` relative to the current working directory
//! 4. `~/.sonda/packs/` in the user's home directory
//!
//! Non-existent directories are silently skipped. Name collisions across
//! tiers are resolved by first-match-wins (highest-priority path).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Pack catalog entry (lightweight metadata, no full YAML parse)
// ---------------------------------------------------------------------------

/// Metadata for a discovered pack on the filesystem.
///
/// Populated by reading only the `name`, `description`, `category`, and
/// `metrics` count from the YAML file. The full [`MetricPackDef`] is parsed
/// lazily when the pack is actually loaded.
///
/// [`MetricPackDef`]: sonda_core::packs::MetricPackDef
#[derive(Debug, Clone)]
pub struct PackEntry {
    /// Normalized pack name (hyphens replaced with underscores, extension stripped).
    pub name: String,
    /// Human-readable description (from YAML `description` field).
    pub description: String,
    /// Category grouping (from YAML `category` field).
    pub category: String,
    /// Number of metric specs in the pack.
    pub metric_count: usize,
    /// Absolute path to the YAML file on disk.
    pub source_path: PathBuf,
}

/// A cached catalog of discovered metric packs.
///
/// Built once per CLI invocation by scanning the search path. Thread-safe
/// by construction (immutable after build).
#[derive(Debug)]
pub struct PackCatalog {
    /// Packs indexed by normalized name, deduplicated (first match wins).
    entries: Vec<PackEntry>,
}

impl PackCatalog {
    /// Build a pack catalog by scanning the given search path directories.
    ///
    /// Directories that do not exist are silently skipped. Files that fail
    /// to read or parse emit a warning to stderr and are excluded from the
    /// catalog. Duplicate names are resolved by first-match (earlier
    /// directory in the search path wins).
    pub fn discover(search_path: &[PathBuf]) -> Self {
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut entries: Vec<PackEntry> = Vec::new();

        for dir in search_path {
            if !dir.is_dir() {
                continue;
            }

            let read_dir = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(e) => {
                    eprintln!(
                        "warning: cannot read pack directory {}: {}",
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
                match read_pack_metadata(&path) {
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

        PackCatalog { entries }
    }

    /// Return all discovered packs.
    pub fn list(&self) -> &[PackEntry] {
        &self.entries
    }

    /// Return packs matching a category filter.
    pub fn list_by_category(&self, category: &str) -> Vec<&PackEntry> {
        self.entries
            .iter()
            .filter(|e| e.category == category)
            .collect()
    }

    /// Find a pack by its normalized name.
    ///
    /// Name matching is exact after normalization (hyphens become underscores).
    pub fn find(&self, name: &str) -> Option<&PackEntry> {
        let query = name.replace('-', "_");
        self.entries.iter().find(|e| e.name == query)
    }

    /// Return all available pack names, useful for error messages.
    pub fn available_names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Read the raw YAML content for a named pack.
    ///
    /// Returns `None` if the pack is not in the catalog, or an error if the
    /// file cannot be read.
    pub fn read_yaml(&self, name: &str) -> Option<Result<String, std::io::Error>> {
        self.find(name)
            .map(|entry| std::fs::read_to_string(&entry.source_path))
    }
}

// ---------------------------------------------------------------------------
// Search path construction
// ---------------------------------------------------------------------------

/// Build the pack search path from CLI flag, environment variable, and defaults.
///
/// When `cli_pack_path` is `Some`, it is the **sole** entry in the search path
/// (overrides env and defaults). Otherwise the path is assembled from:
///
/// 1. `SONDA_PACK_PATH` env var (colon-separated)
/// 2. `./packs/` relative to CWD
/// 3. `~/.sonda/packs/`
pub fn build_search_path(cli_pack_path: Option<&Path>) -> Vec<PathBuf> {
    if let Some(p) = cli_pack_path {
        return vec![p.to_path_buf()];
    }

    let mut dirs: Vec<PathBuf> = Vec::new();

    // SONDA_PACK_PATH env var (colon-separated).
    if let Ok(env_val) = std::env::var("SONDA_PACK_PATH") {
        for segment in env_val.split(':') {
            let trimmed = segment.trim();
            if !trimmed.is_empty() {
                dirs.push(PathBuf::from(trimmed));
            }
        }
    }

    // ./packs/ relative to CWD.
    dirs.push(PathBuf::from("./packs"));

    // ~/.sonda/packs/
    if let Some(home) = home_dir() {
        dirs.push(home.join(".sonda").join("packs"));
    }

    dirs
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Normalize a YAML filename to a pack name.
///
/// Strips the `.yaml`/`.yml` extension and replaces hyphens with underscores.
fn normalize_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    Some(stem.replace('-', "_"))
}

/// Lightweight YAML metadata probe.
///
/// Parses only the top-level fields needed for catalog display without fully
/// constructing a [`MetricPackDef`] (which would require the `config` feature
/// and `serde`). Uses `serde_yaml_ng` directly since the CLI crate already
/// depends on it.
fn read_pack_metadata(path: &Path) -> Result<PackEntry, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("cannot read file: {e}"))?;

    #[derive(serde::Deserialize)]
    struct Probe {
        name: Option<String>,
        description: Option<String>,
        category: Option<String>,
        metrics: Option<Vec<serde_yaml_ng::Value>>,
    }

    let probe: Probe =
        serde_yaml_ng::from_str(&content).map_err(|e| format!("invalid YAML: {e}"))?;

    let normalized = normalize_filename(path)
        .ok_or_else(|| "cannot determine pack name from filename".to_string())?;

    // Use the YAML `name` field if present, otherwise fall back to filename.
    let name = probe
        .name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| normalized.clone());
    // Normalize the YAML name too (hyphens to underscores).
    let name = name.replace('-', "_");

    let description = probe.description.unwrap_or_default();

    let category = probe
        .category
        .unwrap_or_else(|| "uncategorized".to_string());

    let metric_count = probe.metrics.as_ref().map(|m| m.len()).unwrap_or(0);

    let source_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    Ok(PackEntry {
        name,
        description,
        category,
        metric_count,
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
    fn temp_pack_dir(suffix: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sonda-packs-test-{suffix}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("must create temp dir");
        dir
    }

    fn write_pack(dir: &Path, filename: &str, content: &str) {
        fs::write(dir.join(filename), content).expect("must write pack");
    }

    fn valid_pack_yaml(name: &str) -> String {
        format!(
            r#"name: {name}
description: "Test pack"
category: test
metrics:
  - name: metric_a
    generator:
      type: constant
      value: 1.0
"#
        )
    }

    // ---- normalize_filename ---------------------------------------------------

    #[test]
    fn normalize_filename_strips_yaml_extension() {
        assert_eq!(
            normalize_filename(Path::new("telegraf-snmp-interface.yaml")),
            Some("telegraf_snmp_interface".to_string())
        );
    }

    #[test]
    fn normalize_filename_strips_yml_extension() {
        assert_eq!(
            normalize_filename(Path::new("node-exporter-cpu.yml")),
            Some("node_exporter_cpu".to_string())
        );
    }

    #[test]
    fn normalize_filename_preserves_underscores() {
        assert_eq!(
            normalize_filename(Path::new("already_snake.yaml")),
            Some("already_snake".to_string())
        );
    }

    // ---- PackCatalog::discover -------------------------------------------------

    #[test]
    fn discover_empty_directory_produces_empty_catalog() {
        let dir = temp_pack_dir("empty");
        let catalog = PackCatalog::discover(&[dir.clone()]);
        assert!(catalog.list().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_valid_pack_found() {
        let dir = temp_pack_dir("valid");
        write_pack(&dir, "my-pack.yaml", &valid_pack_yaml("my_pack"));
        let catalog = PackCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].name, "my_pack");
        assert_eq!(catalog.list()[0].category, "test");
        assert_eq!(catalog.list()[0].metric_count, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_skips_non_yaml_files() {
        let dir = temp_pack_dir("non-yaml");
        write_pack(&dir, "readme.txt", "not a pack");
        write_pack(&dir, "data.json", "{}");
        write_pack(&dir, "good.yaml", &valid_pack_yaml("good"));
        let catalog = PackCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].name, "good");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_skips_invalid_yaml_without_crashing() {
        let dir = temp_pack_dir("invalid-yaml");
        write_pack(&dir, "bad.yaml", "not: valid: yaml: :::");
        write_pack(&dir, "good.yaml", &valid_pack_yaml("good"));
        let catalog = PackCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].name, "good");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_nonexistent_directory_silently_skipped() {
        let catalog = PackCatalog::discover(&[PathBuf::from("/nonexistent/path/for/testing")]);
        assert!(catalog.list().is_empty());
    }

    #[test]
    fn discover_name_collision_first_match_wins() {
        let dir1 = temp_pack_dir("prio-high");
        let dir2 = temp_pack_dir("prio-low");
        write_pack(
            &dir1,
            "my-pack.yaml",
            &format!(
                r#"name: my_pack
description: "high priority"
category: high
metrics:
  - name: metric_a
"#
            ),
        );
        write_pack(
            &dir2,
            "my-pack.yaml",
            &format!(
                r#"name: my_pack
description: "low priority"
category: low
metrics:
  - name: metric_a
"#
            ),
        );
        let catalog = PackCatalog::discover(&[dir1.clone(), dir2.clone()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].description, "high priority");
        let _ = fs::remove_dir_all(&dir1);
        let _ = fs::remove_dir_all(&dir2);
    }

    // ---- PackCatalog::find ----------------------------------------------------

    #[test]
    fn find_by_normalized_name() {
        let dir = temp_pack_dir("find");
        write_pack(&dir, "my-test-pack.yaml", &valid_pack_yaml("my_test_pack"));
        let catalog = PackCatalog::discover(&[dir.clone()]);
        // Find with underscores.
        assert!(catalog.find("my_test_pack").is_some());
        // Find with hyphens (normalized to underscores).
        assert!(catalog.find("my-test-pack").is_some());
        // Not found.
        assert!(catalog.find("nonexistent").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- PackCatalog::list_by_category ----------------------------------------

    #[test]
    fn list_by_category_filters_correctly() {
        let dir = temp_pack_dir("category");
        write_pack(
            &dir,
            "pack-a.yaml",
            &format!(
                r#"name: pack_a
description: "A"
category: network
metrics:
  - name: m
"#
            ),
        );
        write_pack(
            &dir,
            "pack-b.yaml",
            &format!(
                r#"name: pack_b
description: "B"
category: infra
metrics:
  - name: m
"#
            ),
        );
        let catalog = PackCatalog::discover(&[dir.clone()]);
        let network = catalog.list_by_category("network");
        assert_eq!(network.len(), 1);
        assert_eq!(network[0].name, "pack_a");
        let infra = catalog.list_by_category("infra");
        assert_eq!(infra.len(), 1);
        assert_eq!(infra[0].name, "pack_b");
        let empty = catalog.list_by_category("nonexistent");
        assert!(empty.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- PackCatalog::read_yaml -----------------------------------------------

    #[test]
    fn read_yaml_returns_file_content() {
        let dir = temp_pack_dir("read-yaml");
        let yaml = valid_pack_yaml("read_test");
        write_pack(&dir, "read-test.yaml", &yaml);
        let catalog = PackCatalog::discover(&[dir.clone()]);
        let content = catalog
            .read_yaml("read_test")
            .expect("pack must be in catalog")
            .expect("file must be readable");
        assert!(content.contains("read_test"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_yaml_unknown_name_returns_none() {
        let catalog = PackCatalog::discover(&[]);
        assert!(catalog.read_yaml("nonexistent").is_none());
    }

    // ---- build_search_path ----------------------------------------------------

    #[test]
    fn build_search_path_cli_flag_overrides_all() {
        let path = build_search_path(Some(Path::new("/custom/packs")));
        assert_eq!(path, vec![PathBuf::from("/custom/packs")]);
    }

    #[test]
    fn build_search_path_default_includes_cwd_packs() {
        // When no CLI flag and no env var, defaults should include ./packs.
        // We can't fully test env var isolation here, but we can check the
        // structure.
        let path = build_search_path(None);
        assert!(
            path.iter().any(|p| p.ends_with("packs")),
            "default search path must include a 'packs' directory"
        );
    }

    // ---- available_names ------------------------------------------------------

    #[test]
    fn available_names_matches_catalog_count() {
        let dir = temp_pack_dir("avail-names");
        write_pack(&dir, "a.yaml", &valid_pack_yaml("a"));
        write_pack(&dir, "b.yaml", &valid_pack_yaml("b"));
        let catalog = PackCatalog::discover(&[dir.clone()]);
        assert_eq!(catalog.available_names().len(), catalog.list().len());
        let _ = fs::remove_dir_all(&dir);
    }
}
