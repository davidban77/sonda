//! Filesystem-based metric pack loading for `sonda-server`.
//!
//! Mirrors the CLI's `SONDA_PACK_PATH` semantics: colon-separated dirs, each
//! containing `*.yaml` / `*.yml` pack definitions. Parsed eagerly at startup
//! into an [`InMemoryPackResolver`] so every `POST /scenarios` body can
//! resolve `pack: <name>` references without filesystem access on the hot path.

use std::path::{Path, PathBuf};

use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::packs::MetricPackDef;
use tracing::{info, warn};

/// Build the search path from `SONDA_PACK_PATH` (colon-separated). Empty when
/// the variable is unset.
pub fn build_search_path() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(env_val) = std::env::var("SONDA_PACK_PATH") {
        for segment in env_val.split(':') {
            let trimmed = segment.trim();
            if !trimmed.is_empty() {
                dirs.push(PathBuf::from(trimmed));
            }
        }
    }
    dirs
}

/// Load every `*.yaml` / `*.yml` file under `search_path` and register it in a
/// fresh [`InMemoryPackResolver`].
///
/// Each pack is registered under two keys:
/// - the pack's own `name` field (e.g. `srlinux_gnmi_bgp`), and
/// - the file stem (e.g. `srlinux-gnmi-bgp.yaml` -> `srlinux-gnmi-bgp`).
///
/// Non-existent dirs and unreadable files emit a warning and are skipped. Name
/// collisions across tiers are first-match-wins.
pub fn load_pack_resolver(search_path: &[PathBuf]) -> InMemoryPackResolver {
    let mut resolver = InMemoryPackResolver::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut count = 0usize;

    for dir in search_path {
        if !dir.is_dir() {
            continue;
        }
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "cannot read pack directory");
                continue;
            }
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("yaml") && ext != Some("yml") {
                continue;
            }
            match load_one(&path) {
                Ok((name, file_stem, pack)) => {
                    if seen.insert(name.clone()) {
                        resolver.insert(name, pack.clone());
                    }
                    if seen.insert(file_stem.clone()) {
                        resolver.insert(file_stem, pack);
                    }
                    count += 1;
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping pack file");
                }
            }
        }
    }

    if count > 0 {
        info!(packs_loaded = count, "loaded pack definitions");
    }
    resolver
}

fn load_one(path: &Path) -> Result<(String, String, MetricPackDef), String> {
    let yaml = std::fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))?;
    let pack: MetricPackDef =
        serde_yaml_ng::from_str(&yaml).map_err(|e| format!("YAML parse failed: {e}"))?;
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    Ok((pack.name.clone(), file_stem, pack))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonda_core::compiler::expand::PackResolver;
    use tempfile::TempDir;

    fn write_pack(dir: &Path, filename: &str, name: &str) {
        let body = format!(
            "name: {name}\ndescription: t\ncategory: test\nmetrics:\n  - name: m\n    generator: {{ type: constant, value: 1.0 }}\n"
        );
        std::fs::write(dir.join(filename), body).expect("write pack");
    }

    #[test]
    fn build_search_path_empty_when_unset() {
        // SAFETY: test-only env mutation; var is unique-to-sonda namespace.
        unsafe {
            std::env::remove_var("SONDA_PACK_PATH");
        }
        assert!(build_search_path().is_empty());
    }

    #[test]
    fn load_resolver_registers_pack_under_name_and_file_stem() {
        let dir = TempDir::new().expect("tmpdir");
        write_pack(dir.path(), "srlinux-gnmi-bgp.yaml", "srlinux_gnmi_bgp");

        let resolver = load_pack_resolver(&[dir.path().to_path_buf()]);

        // Both keys resolve to the same pack.
        assert!(resolver.resolve("srlinux_gnmi_bgp").is_ok());
        assert!(resolver.resolve("srlinux-gnmi-bgp").is_ok());
    }

    #[test]
    fn load_resolver_skips_non_yaml_and_missing_dirs() {
        let dir = TempDir::new().expect("tmpdir");
        std::fs::write(dir.path().join("README.md"), "ignored").expect("md");
        write_pack(dir.path(), "real_pack.yaml", "real_pack");

        let missing = dir.path().join("does-not-exist");
        let resolver = load_pack_resolver(&[missing, dir.path().to_path_buf()]);

        assert!(resolver.resolve("real_pack").is_ok());
    }

    #[test]
    fn load_resolver_first_match_wins_across_dirs() {
        let a = TempDir::new().expect("tmpdir-a");
        let b = TempDir::new().expect("tmpdir-b");
        write_pack(a.path(), "p.yaml", "shared_name");
        write_pack(b.path(), "p.yaml", "shared_name");

        let resolver =
            load_pack_resolver(&[a.path().to_path_buf(), b.path().to_path_buf()]);

        // First-match-wins: only one registration succeeds; resolution still
        // returns a pack.
        assert!(resolver.resolve("shared_name").is_ok());
    }
}
