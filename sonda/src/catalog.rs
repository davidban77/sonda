//! Unified catalog view over scenarios and packs.
//!
//! Spec §6.3 replaces `sonda scenarios` + `sonda packs` with a single
//! `sonda catalog` subcommand tree. This module provides the unified row
//! iterator — it adapts the existing [`ScenarioCatalog`][crate::scenarios]
//! and [`PackCatalog`][crate::packs] into a common [`CatalogRow`] shape
//! without owning their storage. Neither source catalog is modified.
//!
//! The list surface ordering is: scenarios first (in their native order),
//! then packs. `--type` and `--category` filter this merged stream in
//! [`catalog_rows`].

use crate::packs::{PackCatalog, PackEntry};
use crate::scenarios::ScenarioCatalog;
use sonda_core::BuiltinScenario;

/// What kind of entry a catalog row represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogKind {
    /// A standalone scenario YAML — self-contained and runnable without
    /// additional labels.
    Scenario,
    /// A metric pack — reusable schema bundle. Requires `--label` values
    /// to be meaningful when run.
    Pack,
}

impl CatalogKind {
    /// Lowercase string form used in list output and `--type` matching.
    pub fn as_str(self) -> &'static str {
        match self {
            CatalogKind::Scenario => "scenario",
            CatalogKind::Pack => "pack",
        }
    }
}

/// Filter for the `--type scenario|pack` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogTypeFilter {
    /// Only scenarios.
    Scenario,
    /// Only packs.
    Pack,
}

impl CatalogTypeFilter {
    /// Parse the string supplied by the user. Case-insensitive on the
    /// canonical singular forms.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "scenario" | "scenarios" => Ok(CatalogTypeFilter::Scenario),
            "pack" | "packs" => Ok(CatalogTypeFilter::Pack),
            other => Err(anyhow::anyhow!(
                "unknown --type {other:?}; valid values: scenario, pack"
            )),
        }
    }
}

/// A single row of the unified catalog view.
///
/// Constructed on-demand from the source catalogs so there is no double
/// storage. Field lifetimes are tied to the source catalogs — callers that
/// need an owned DTO should project into a serialized form (as
/// [`to_list_dto`] does).
#[derive(Debug)]
pub struct CatalogRow<'a> {
    /// Unique identifier (kebab-case for scenarios, snake_case for packs).
    pub name: &'a str,
    /// Whether this entry is a scenario or pack.
    pub kind: CatalogKind,
    /// Category grouping (`infrastructure`, `network`, ...).
    pub category: &'a str,
    /// Signal type string: `"metrics"`, `"logs"`, `"multi"`, `"histogram"`,
    /// or `"summary"` for scenarios; always `"metrics"` for packs.
    pub signal: &'a str,
    /// One-line human-readable description.
    pub description: &'a str,
    /// Whether this row can be run with no additional input. Scenarios are
    /// `true`; packs are `false` (they need labels supplied via CLI).
    pub runnable: bool,
}

/// Source references for the catalog adapters.
///
/// `CatalogRow` instances borrow either a `BuiltinScenario` or a
/// `PackEntry`; this enum is internal glue that lets a single loop
/// enumerate both source catalogs.
enum Source<'a> {
    Scenario(&'a BuiltinScenario),
    Pack(&'a PackEntry),
}

impl<'a> Source<'a> {
    fn as_row(&self) -> CatalogRow<'a> {
        match self {
            Source::Scenario(s) => CatalogRow {
                name: s.name.as_str(),
                kind: CatalogKind::Scenario,
                category: s.category.as_str(),
                signal: s.signal_type.as_str(),
                description: s.description.as_str(),
                runnable: true,
            },
            Source::Pack(p) => CatalogRow {
                name: p.name.as_str(),
                kind: CatalogKind::Pack,
                category: p.category.as_str(),
                signal: "metrics",
                description: p.description.as_str(),
                runnable: false,
            },
        }
    }
}

/// Produce an ordered [`CatalogRow`] iterator over the merged catalog,
/// applying the optional filters.
///
/// Ordering is source-order: all scenarios first, then all packs, both in
/// whatever order the underlying catalog returned them (filesystem order
/// with search-path priority collapse).
pub fn catalog_rows<'a>(
    scenarios: &'a ScenarioCatalog,
    packs: &'a PackCatalog,
    type_filter: Option<CatalogTypeFilter>,
    category: Option<&str>,
) -> Vec<CatalogRow<'a>> {
    let include_scenarios = !matches!(type_filter, Some(CatalogTypeFilter::Pack));
    let include_packs = !matches!(type_filter, Some(CatalogTypeFilter::Scenario));

    let mut sources: Vec<Source<'a>> = Vec::new();
    if include_scenarios {
        sources.extend(scenarios.list().iter().map(Source::Scenario));
    }
    if include_packs {
        sources.extend(packs.list().iter().map(Source::Pack));
    }

    sources
        .into_iter()
        .map(|s| s.as_row())
        .filter(|row| match category {
            Some(cat) => row.category == cat,
            None => true,
        })
        .collect()
}

/// JSON DTO for `sonda catalog list --json`.
///
/// Stable shape; field names match the metadata table in spec §6.3 and are
/// sorted at emit time via `BTreeMap` so output is deterministic.
#[derive(Debug, serde::Serialize)]
pub struct CatalogListDto<'a> {
    /// Name (unique identifier).
    pub name: &'a str,
    /// `scenario` or `pack`.
    #[serde(rename = "type")]
    pub kind: &'a str,
    /// Category grouping.
    pub category: &'a str,
    /// Signal type: `metrics`, `logs`, `multi`, `histogram`, `summary`.
    pub signal: &'a str,
    /// One-line description.
    pub description: &'a str,
    /// Whether the entry is runnable without extra input.
    pub runnable: bool,
}

impl<'a> From<&'a CatalogRow<'a>> for CatalogListDto<'a> {
    fn from(row: &'a CatalogRow<'a>) -> Self {
        CatalogListDto {
            name: row.name,
            kind: row.kind.as_str(),
            category: row.category,
            signal: row.signal,
            description: row.description,
            runnable: row.runnable,
        }
    }
}

/// Build the JSON DTO vector from a slice of rows. Helper around the
/// `From` impl so the main.rs dispatch stays a single line.
pub fn to_list_dto<'a>(rows: &'a [CatalogRow<'a>]) -> Vec<CatalogListDto<'a>> {
    rows.iter().map(CatalogListDto::from).collect()
}

/// Resolve a catalog row by name. Returns `None` when the name does not
/// match any scenario or pack. Scenario match takes precedence over pack
/// match on name collision (rare, but possible when users name a pack
/// after a scenario).
pub fn find_row<'a>(
    scenarios: &'a ScenarioCatalog,
    packs: &'a PackCatalog,
    name: &str,
) -> Option<CatalogRow<'a>> {
    if let Some(s) = scenarios.find(name) {
        return Some(Source::Scenario(s).as_row());
    }
    if let Some(p) = packs.find(name) {
        return Some(Source::Pack(p).as_row());
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonda-catalog-{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("must create temp dir");
        dir
    }

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).expect("must write fixture");
    }

    fn scenario_yaml(name: &str, category: &str) -> String {
        format!(
            r#"scenario_name: {name}
category: {category}
signal_type: metrics
description: "Test scenario"

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
        )
    }

    fn pack_yaml(name: &str, category: &str) -> String {
        format!(
            r#"name: {name}
description: "Test pack"
category: {category}
metrics:
  - name: metric_a
    generator:
      type: constant
      value: 1.0
"#
        )
    }

    #[test]
    fn catalog_rows_merges_scenarios_and_packs() {
        let scn_dir = temp_dir("merge-scn");
        write(&scn_dir, "a.yaml", &scenario_yaml("a", "network"));
        let scenarios = ScenarioCatalog::discover(&[scn_dir.clone()]);

        let pk_dir = temp_dir("merge-pk");
        write(&pk_dir, "p.yaml", &pack_yaml("p", "infrastructure"));
        let packs = PackCatalog::discover(&[pk_dir.clone()]);

        let rows = catalog_rows(&scenarios, &packs, None, None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, CatalogKind::Scenario);
        assert_eq!(rows[1].kind, CatalogKind::Pack);

        let _ = fs::remove_dir_all(&scn_dir);
        let _ = fs::remove_dir_all(&pk_dir);
    }

    #[test]
    fn catalog_rows_type_filter_scenarios_only() {
        let scn_dir = temp_dir("type-scn");
        write(&scn_dir, "a.yaml", &scenario_yaml("a", "network"));
        let scenarios = ScenarioCatalog::discover(&[scn_dir.clone()]);

        let pk_dir = temp_dir("type-pk");
        write(&pk_dir, "p.yaml", &pack_yaml("p", "network"));
        let packs = PackCatalog::discover(&[pk_dir.clone()]);

        let rows = catalog_rows(&scenarios, &packs, Some(CatalogTypeFilter::Scenario), None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, CatalogKind::Scenario);

        let _ = fs::remove_dir_all(&scn_dir);
        let _ = fs::remove_dir_all(&pk_dir);
    }

    #[test]
    fn catalog_rows_type_filter_packs_only() {
        let scn_dir = temp_dir("type-scn-p");
        write(&scn_dir, "a.yaml", &scenario_yaml("a", "network"));
        let scenarios = ScenarioCatalog::discover(&[scn_dir.clone()]);

        let pk_dir = temp_dir("type-pk-p");
        write(&pk_dir, "p.yaml", &pack_yaml("p", "network"));
        let packs = PackCatalog::discover(&[pk_dir.clone()]);

        let rows = catalog_rows(&scenarios, &packs, Some(CatalogTypeFilter::Pack), None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, CatalogKind::Pack);

        let _ = fs::remove_dir_all(&scn_dir);
        let _ = fs::remove_dir_all(&pk_dir);
    }

    #[test]
    fn catalog_rows_category_filter_is_case_sensitive() {
        let scn_dir = temp_dir("cat-scn");
        write(&scn_dir, "a.yaml", &scenario_yaml("a", "network"));
        write(&scn_dir, "b.yaml", &scenario_yaml("b", "infrastructure"));
        let scenarios = ScenarioCatalog::discover(&[scn_dir.clone()]);
        let packs = PackCatalog::discover(&[]);

        let rows = catalog_rows(&scenarios, &packs, None, Some("network"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "a");

        let rows_upper = catalog_rows(&scenarios, &packs, None, Some("Network"));
        assert!(
            rows_upper.is_empty(),
            "category match must be case-sensitive"
        );

        let _ = fs::remove_dir_all(&scn_dir);
    }

    #[test]
    fn catalog_rows_unknown_category_yields_empty() {
        let scn_dir = temp_dir("cat-unknown");
        write(&scn_dir, "a.yaml", &scenario_yaml("a", "network"));
        let scenarios = ScenarioCatalog::discover(&[scn_dir.clone()]);
        let packs = PackCatalog::discover(&[]);

        let rows = catalog_rows(&scenarios, &packs, None, Some("quantum"));
        assert!(rows.is_empty());

        let _ = fs::remove_dir_all(&scn_dir);
    }

    #[test]
    fn find_row_prefers_scenario_on_name_collision() {
        let scn_dir = temp_dir("find-scn");
        write(&scn_dir, "shared.yaml", &scenario_yaml("shared", "test"));
        let scenarios = ScenarioCatalog::discover(&[scn_dir.clone()]);

        let pk_dir = temp_dir("find-pk");
        write(&pk_dir, "shared.yaml", &pack_yaml("shared", "test"));
        let packs = PackCatalog::discover(&[pk_dir.clone()]);

        let row = find_row(&scenarios, &packs, "shared").expect("must find");
        assert_eq!(row.kind, CatalogKind::Scenario);

        let _ = fs::remove_dir_all(&scn_dir);
        let _ = fs::remove_dir_all(&pk_dir);
    }

    #[test]
    fn find_row_returns_none_when_unknown() {
        let scenarios = ScenarioCatalog::discover(&[]);
        let packs = PackCatalog::discover(&[]);
        assert!(find_row(&scenarios, &packs, "nope").is_none());
    }

    #[test]
    fn type_filter_parses_case_insensitive_singular_and_plural() {
        assert_eq!(
            CatalogTypeFilter::parse("scenario").unwrap(),
            CatalogTypeFilter::Scenario
        );
        assert_eq!(
            CatalogTypeFilter::parse("Scenarios").unwrap(),
            CatalogTypeFilter::Scenario
        );
        assert_eq!(
            CatalogTypeFilter::parse("PACK").unwrap(),
            CatalogTypeFilter::Pack
        );
        assert_eq!(
            CatalogTypeFilter::parse("packs").unwrap(),
            CatalogTypeFilter::Pack
        );
    }

    #[test]
    fn type_filter_rejects_unknown_value() {
        let err = CatalogTypeFilter::parse("both").expect_err("must reject");
        assert!(format!("{err}").contains("both"));
    }
}
