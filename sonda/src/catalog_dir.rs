//! Catalog directory enumeration and `@name` resolution.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sonda_core::compiler::expand::{
    classify_pack_reference, PackResolveError, PackResolveOrigin, PackResolver,
};
use sonda_core::packs::MetricPackDef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Runnable,
    Composable,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Runnable => "runnable",
            EntryKind::Composable => "composable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub name: String,
    pub kind: EntryKind,
    pub description: String,
    pub tags: Vec<String>,
    pub source_path: PathBuf,
}

#[derive(serde::Deserialize)]
struct CatalogEntryHeader {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    scenario_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// Walk `dir` and return one [`CatalogEntry`] per YAML file with a
/// recognized `kind:` header. Files without `kind:` are silently skipped.
pub fn enumerate(dir: &Path) -> Result<Vec<CatalogEntry>> {
    if !dir.is_dir() {
        return Err(anyhow!(
            "catalog dir {} does not exist or is not a directory",
            dir.display()
        ));
    }

    let mut entries: Vec<CatalogEntry> = Vec::new();
    let read_dir = fs::read_dir(dir)
        .with_context(|| format!("failed to read catalog dir {}", dir.display()))?;
    for entry in read_dir {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        if !is_yaml_file(&path) {
            continue;
        }
        if let Some(parsed) = peek_entry(&path)? {
            entries.push(parsed);
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));

    for pair in entries.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(anyhow!(
                "catalog {} contains duplicate entry name {:?}: {} and {}",
                dir.display(),
                pair[0].name,
                pair[0].source_path.display(),
                pair[1].source_path.display(),
            ));
        }
    }

    Ok(entries)
}

/// Resolve `@name` against `dir` and return the source YAML path.
pub fn resolve(dir: &Path, name: &str) -> Result<PathBuf> {
    let all = enumerate(dir)?;
    if let Some(entry) = all.iter().find(|e| e.name == name) {
        return Ok(entry.source_path.clone());
    }
    let names: Vec<String> = all.iter().map(|e| e.name.clone()).collect();
    let available = if names.is_empty() {
        "<empty>".to_string()
    } else {
        names.join(", ")
    };
    Err(anyhow!(
        "unknown catalog entry {:?} in {}; available: {}",
        name,
        dir.display(),
        available
    ))
}

fn is_yaml_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    )
}

fn peek_entry(path: &Path) -> Result<Option<CatalogEntry>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let header: CatalogEntryHeader = match serde_yaml_ng::from_str(&content) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "catalog: skipping unparseable YAML file"
            );
            return Ok(None);
        }
    };
    let Some(raw_kind) = header.kind else {
        return Ok(None);
    };
    let kind = match raw_kind.as_str() {
        "runnable" => EntryKind::Runnable,
        "composable" => EntryKind::Composable,
        _ => return Ok(None),
    };
    let filename_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("cannot derive name from filename {}", path.display()))?
        .to_string();
    let name = header
        .scenario_name
        .or(header.name)
        .unwrap_or_else(|| filename_stem.replace('_', "-"));
    Ok(Some(CatalogEntry {
        name,
        kind,
        description: header.description.unwrap_or_default(),
        tags: header.tags,
        source_path: path.to_path_buf(),
    }))
}

/// [`PackResolver`] backed by a `--catalog <dir>` peek pass.
///
/// Falls back to direct file reads for references containing `/` or
/// starting with `.` (spec §2.4 classification).
pub struct CatalogPackResolver<'a> {
    catalog_dir: Option<&'a Path>,
}

impl<'a> CatalogPackResolver<'a> {
    pub fn new(catalog_dir: Option<&'a Path>) -> Self {
        Self { catalog_dir }
    }
}

impl<'a> PackResolver for CatalogPackResolver<'a> {
    fn resolve(&self, reference: &str) -> Result<MetricPackDef, PackResolveError> {
        let origin = classify_pack_reference(reference);
        let yaml = match origin {
            PackResolveOrigin::FilePath => fs::read_to_string(reference).map_err(|e| {
                PackResolveError::new(format!("cannot read pack file {reference:?}: {e}"), origin)
            })?,
            PackResolveOrigin::Name => {
                let dir = self.catalog_dir.ok_or_else(|| {
                    PackResolveError::new(
                        format!(
                            "pack {reference:?} referenced by name but --catalog <dir> not provided"
                        ),
                        origin,
                    )
                })?;
                let entries = enumerate(dir).map_err(|e| {
                    PackResolveError::new(
                        format!("cannot enumerate catalog dir {}: {e}", dir.display()),
                        origin,
                    )
                })?;
                let entry = entries
                    .iter()
                    .find(|e| e.name == reference && e.kind == EntryKind::Composable)
                    .ok_or_else(|| {
                        let composable: Vec<&str> = entries
                            .iter()
                            .filter(|e| e.kind == EntryKind::Composable)
                            .map(|e| e.name.as_str())
                            .collect();
                        let available = if composable.is_empty() {
                            "<none>".to_string()
                        } else {
                            composable.join(", ")
                        };
                        PackResolveError::new(
                            format!(
                                "unknown pack {reference:?} in catalog {}; composable entries: {available}",
                                dir.display()
                            ),
                            origin,
                        )
                    })?;
                fs::read_to_string(&entry.source_path).map_err(|e| {
                    PackResolveError::new(
                        format!("cannot read pack file {}: {e}", entry.source_path.display()),
                        origin,
                    )
                })?
            }
        };
        serde_yaml_ng::from_str::<MetricPackDef>(&yaml).map_err(|e| {
            PackResolveError::new(
                format!("cannot parse pack definition for {reference:?}: {e}"),
                origin,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).expect("create file");
        f.write_all(content.as_bytes()).expect("write file");
    }

    fn temp_catalog() -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        write(
            dir.path(),
            "cpu-spike.yaml",
            r#"version: 2
kind: runnable
scenario_name: cpu-spike
description: CPU spike test
tags: [infrastructure, cpu]

defaults:
  rate: 1
  duration: 1s

scenarios:
  - id: a
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
"#,
        );
        write(
            dir.path(),
            "tiny-pack.yaml",
            r#"version: 2
kind: composable
scenario_name: tiny_pack
description: A small pack
tags: [network]

name: tiny_pack
category: network
metrics:
  - name: pack_metric_a
    generator:
      type: constant
      value: 1
"#,
        );
        write(dir.path(), "not-a-scenario.txt", "ignored");
        write(dir.path(), "missing-kind.yaml", "version: 2\n");
        dir
    }

    #[test]
    fn enumerate_returns_runnable_and_composable_sorted_by_name() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["cpu-spike", "tiny_pack"]);
    }

    #[test]
    fn enumerate_skips_files_without_kind() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        assert!(entries.iter().all(|e| e.name != "missing-kind"));
    }

    #[test]
    fn enumerate_preserves_tags() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        let cpu = entries.iter().find(|e| e.name == "cpu-spike").unwrap();
        assert_eq!(cpu.tags, vec!["infrastructure", "cpu"]);
    }

    #[test]
    fn enumerate_classifies_runnable_and_composable_kinds() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        let cpu = entries.iter().find(|e| e.name == "cpu-spike").unwrap();
        let pack = entries.iter().find(|e| e.name == "tiny_pack").unwrap();
        assert_eq!(cpu.kind, EntryKind::Runnable);
        assert_eq!(pack.kind, EntryKind::Composable);
    }

    #[test]
    fn resolve_returns_path_for_known_name() {
        let tmp = temp_catalog();
        let resolved = resolve(tmp.path(), "cpu-spike").expect("must resolve");
        assert_eq!(resolved.file_name().unwrap(), "cpu-spike.yaml");
    }

    #[test]
    fn resolve_returns_error_for_unknown_name() {
        let tmp = temp_catalog();
        let err = resolve(tmp.path(), "missing").expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("missing"), "got: {msg}");
        assert!(msg.contains("cpu-spike"), "must list candidates: {msg}");
    }

    #[test]
    fn enumerate_errors_on_nonexistent_dir() {
        let err = enumerate(Path::new("/nonexistent/sonda/catalog")).expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("does not exist"), "got: {msg}");
    }
}
