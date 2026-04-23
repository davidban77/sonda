//! v2 scenario file loading.
//!
//! The CLI reads every scenario file through the v2 compiler and hands the
//! runtime a [`Vec<ScenarioEntry>`][sonda_core::ScenarioEntry] ready for
//! [`prepare_entries`][sonda_core::prepare_entries]. v1 YAML shapes (flat
//! single-signal configs, top-level `scenarios:` lists without `version: 2`,
//! `pack:` shorthand files) are rejected here with a clear migration hint.
//!
//! [`load_scenario_entries`] is the single CLI entry point. It resolves the
//! scenario source (filesystem path or `@name` shorthand), detects the
//! format via [`detect_version`][sonda_core::compiler::parse::detect_version],
//! and dispatches to
//! [`compile_scenario_file`][sonda_core::compile_scenario_file] with a
//! [`FilesystemPackResolver`] backed by the CLI's pack catalog. Anything
//! that does not declare `version: 2` is treated as v1 and refused.

use std::path::Path;

use anyhow::{bail, Context, Result};

use sonda_core::compiler::expand::{
    classify_pack_reference, PackResolveError, PackResolveOrigin, PackResolver,
};
use sonda_core::compiler::parse::detect_version;
use sonda_core::packs::MetricPackDef;
use sonda_core::{compile_scenario_file, CompileError, ScenarioEntry};

use crate::packs::PackCatalog;
use crate::scenarios::ScenarioCatalog;

/// Compile a v2 scenario YAML string into [`ScenarioEntry`] values using
/// the CLI's filesystem-backed pack resolver.
///
/// This is the shared resolver+compile step used by every CLI entry
/// point that loads a v2 scenario file
/// ([`load_scenario_entries`], the single-entry subcommand loader in
/// `sonda::config`, and the catalog dispatcher
/// [`parse_builtin_scenario`](crate::config::parse_builtin_scenario)).
///
/// Callers are expected to have already performed the `version: 2`
/// check — this helper does not reject v1 YAML shapes. It also does not
/// wrap the returned [`CompileError`] in `anyhow::Context`; each caller
/// attaches its own path-specific context so diagnostics stay accurate
/// to the source that was loaded.
///
/// # Errors
///
/// Returns the raw [`CompileError`] produced by
/// [`sonda_core::compile_scenario_file`] so callers can pattern-match
/// on typed compilation failures or wrap the error with caller-specific
/// context via [`anyhow::Context`].
pub fn compile_v2_yaml(
    yaml: &str,
    pack_catalog: &PackCatalog,
) -> Result<Vec<ScenarioEntry>, CompileError> {
    let resolver = FilesystemPackResolver::new(pack_catalog);
    compile_scenario_file(yaml, &resolver)
}

/// The result of loading a scenario file: the prepared runtime entries
/// plus the detected schema version.
///
/// The runtime takes [`Self::entries`] and feeds it to
/// [`sonda_core::prepare_entries`]; [`Self::version`] steers the caller
/// toward the right `--dry-run` formatter. Post-v1 removal, [`Self::version`]
/// is always `Some(2)` on success — the field is retained so downstream
/// formatter selection keeps a stable API and future schema versions can
/// slot in without another signature churn.
#[derive(Debug)]
pub struct LoadedScenario {
    /// The scenario entries, ready for `prepare_entries`.
    pub entries: Vec<ScenarioEntry>,
    /// The schema version detected in the YAML. Always `Some(2)` for
    /// successful loads; the field is retained for future schema versions
    /// and for the v2 dry-run formatter trigger in `main.rs`.
    pub version: Option<u32>,
}

/// Load scenario entries from a scenario reference (file path or `@name`).
///
/// Resolves the YAML via
/// [`resolve_scenario_source`][crate::config::resolve_scenario_source] and
/// compiles it through [`compile_scenario_file`][sonda_core::compile_scenario_file].
/// Any file that does not declare `version: 2` is rejected with a migration
/// hint pointing at the v2 scenario guide.
///
/// # Errors
///
/// Returns an error if the scenario source cannot be resolved, the YAML
/// fails to declare `version: 2`, or any v2 compilation phase rejects the
/// input. Compile errors are wrapped with [`anyhow::Context`] carrying the
/// source path so the user can locate the offending file.
pub fn load_scenario_entries(
    scenario_ref: &Path,
    scenario_catalog: &ScenarioCatalog,
    pack_catalog: &PackCatalog,
) -> Result<LoadedScenario> {
    let yaml = crate::config::resolve_scenario_source(scenario_ref, scenario_catalog)?;
    let version = detect_version(&yaml);

    match version {
        Some(2) => {
            let entries = compile_v2_yaml(&yaml, pack_catalog).with_context(|| {
                format!(
                    "failed to compile v2 scenario file {}",
                    scenario_ref.display()
                )
            })?;
            Ok(LoadedScenario { entries, version })
        }
        _ => bail!(
            "scenario file {} is not a v2 scenario. \
             Sonda only accepts v2 YAML (`version: 2` at the top level). \
             Migrate this file to v2 — see docs/configuration/v2-scenarios.md \
             for the migration guide.",
            scenario_ref.display()
        ),
    }
}

/// A [`PackResolver`] that looks pack references up against the CLI's
/// filesystem [`PackCatalog`], falling back to direct path reads for
/// references classified as [`PackResolveOrigin::FilePath`].
///
/// The resolver honors the spec §2.4 classification rules:
///
/// - References containing `/` or starting with `.` are treated as paths
///   and read directly from disk.
/// - All other references are looked up by name in the catalog.
///
/// Errors carry the classification so callers can tell "unknown pack name"
/// apart from "pack file not found" without string parsing.
pub struct FilesystemPackResolver<'a> {
    catalog: &'a PackCatalog,
}

impl<'a> FilesystemPackResolver<'a> {
    /// Construct a resolver backed by `catalog`.
    pub fn new(catalog: &'a PackCatalog) -> Self {
        Self { catalog }
    }
}

impl<'a> PackResolver for FilesystemPackResolver<'a> {
    fn resolve(&self, reference: &str) -> Result<MetricPackDef, PackResolveError> {
        let origin = classify_pack_reference(reference);

        let yaml = match origin {
            PackResolveOrigin::FilePath => std::fs::read_to_string(reference).map_err(|e| {
                PackResolveError::new(format!("cannot read pack file {reference:?}: {e}"), origin)
            })?,
            PackResolveOrigin::Name => {
                let read_result = self.catalog.read_yaml(reference).ok_or_else(|| {
                    let available = self.catalog.available_names().join(", ");
                    PackResolveError::new(
                        format!("unknown pack {reference:?}; available packs: {available}",),
                        origin,
                    )
                })?;
                read_result.map_err(|e| {
                    PackResolveError::new(
                        format!("cannot read pack file for {reference:?}: {e}"),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;

    // -----------------------------------------------------------------------
    // Test fixtures: temp dirs for scenario and pack catalogs
    // -----------------------------------------------------------------------

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sonda-scenario-loader-{prefix}-{}-{}",
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

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).expect("must write fixture");
        path
    }

    fn empty_scenario_catalog() -> ScenarioCatalog {
        ScenarioCatalog::discover(&[])
    }

    fn empty_pack_catalog() -> PackCatalog {
        PackCatalog::discover(&[])
    }

    // -----------------------------------------------------------------------
    // Happy paths
    // -----------------------------------------------------------------------

    /// A v2 inline scenario file dispatches to `compile_scenario_file` and
    /// produces the expected entries.
    #[test]
    fn loads_v2_inline_scenario_file() {
        let dir = temp_dir("v2-inline");
        let path = write(
            &dir,
            "v2.yaml",
            r#"version: 2
defaults:
  rate: 5
  duration: 200ms
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
"#,
        );

        let loaded = load_scenario_entries(&path, &empty_scenario_catalog(), &empty_pack_catalog())
            .expect("v2 inline must compile");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.version, Some(2));
        assert_eq!(loaded.entries[0].base().name, "cpu_usage");
        assert_eq!(loaded.entries[0].base().rate, 5.0);

        let _ = fs::remove_dir_all(&dir);
    }

    /// A v2 pack-backed scenario resolves the pack via the filesystem
    /// resolver, expanding into per-metric entries.
    #[test]
    fn loads_v2_pack_backed_scenario() {
        let pack_dir = temp_dir("v2-pack-catalog");
        write(
            &pack_dir,
            "tiny_pack.yaml",
            r#"name: tiny_pack
description: test
category: test
metrics:
  - name: metric_a
    generator:
      type: constant
      value: 1.0
  - name: metric_b
    generator:
      type: constant
      value: 2.0
"#,
        );
        let pack_catalog = PackCatalog::discover(&[pack_dir.clone()]);

        let scenario_dir = temp_dir("v2-pack-scenario");
        let path = write(
            &scenario_dir,
            "v2-pack.yaml",
            r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: primary
    signal_type: metrics
    pack: tiny_pack
"#,
        );

        let loaded = load_scenario_entries(&path, &empty_scenario_catalog(), &pack_catalog)
            .expect("v2 pack-backed must compile");
        assert_eq!(loaded.entries.len(), 2, "pack expands to two entries");
        assert_eq!(loaded.version, Some(2));

        let _ = fs::remove_dir_all(&pack_dir);
        let _ = fs::remove_dir_all(&scenario_dir);
    }

    /// The `@name` shorthand resolves through the scenario catalog, then
    /// dispatches through the v2 compiler.
    #[test]
    fn resolves_at_name_shorthand() {
        let scenarios_dir = temp_dir("at-name");
        write(
            &scenarios_dir,
            "my-scenario.yaml",
            r#"version: 2
scenario_name: my-scenario
category: test
description: test

defaults:
  rate: 1
  duration: 100ms

scenarios:
  - id: mymetric
    signal_type: metrics
    name: mymetric
    generator:
      type: constant
      value: 1.0
"#,
        );
        let scenario_catalog = ScenarioCatalog::discover(&[scenarios_dir.clone()]);

        let loaded = load_scenario_entries(
            Path::new("@my-scenario"),
            &scenario_catalog,
            &empty_pack_catalog(),
        )
        .expect("@name shorthand must resolve");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].base().name, "mymetric");

        let _ = fs::remove_dir_all(&scenarios_dir);
    }

    // -----------------------------------------------------------------------
    // Error paths
    // -----------------------------------------------------------------------

    /// A v1 flat single-scenario file is rejected with a migration hint.
    #[test]
    fn rejects_v1_flat_scenario_with_migration_hint() {
        let dir = temp_dir("v1-flat-reject");
        let path = write(
            &dir,
            "flat-v1.yaml",
            r#"name: legacy
rate: 1
duration: 100ms
generator:
  type: constant
  value: 1.0
encoder:
  type: prometheus_text
sink:
  type: stdout
"#,
        );

        let err = load_scenario_entries(&path, &empty_scenario_catalog(), &empty_pack_catalog())
            .expect_err("v1 flat file must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("v2"),
            "error must mention v2 requirement, got: {msg}"
        );
        assert!(
            msg.contains("v2-scenarios") || msg.contains("migration") || msg.contains("Migrate"),
            "error must point at the migration guide, got: {msg}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// A v1 multi-scenario file (top-level `scenarios:` without `version: 2`)
    /// is rejected with a migration hint.
    #[test]
    fn rejects_v1_multi_scenario_with_migration_hint() {
        let dir = temp_dir("v1-multi-reject");
        let path = write(
            &dir,
            "multi-v1.yaml",
            r#"scenarios:
  - signal_type: metrics
    name: legacy
    rate: 1
    duration: 100ms
    generator:
      type: constant
      value: 1.0
"#,
        );

        let err = load_scenario_entries(&path, &empty_scenario_catalog(), &empty_pack_catalog())
            .expect_err("v1 multi-scenario must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("v2"),
            "error must mention v2 requirement, got: {msg}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// A v1 pack shorthand file (`pack: <name>` at top level) is rejected
    /// with a migration hint — the v2 replacement is `pack:` inside a
    /// `scenarios:` entry under `version: 2`.
    #[test]
    fn rejects_v1_pack_shorthand_with_migration_hint() {
        let pack_dir = temp_dir("v1-pack-reject-catalog");
        write(
            &pack_dir,
            "tiny_pack.yaml",
            r#"name: tiny_pack
description: test
category: test
metrics:
  - name: metric_a
    generator:
      type: constant
      value: 1.0
"#,
        );
        let pack_catalog = PackCatalog::discover(&[pack_dir.clone()]);

        let scenario_dir = temp_dir("v1-pack-reject-scenario");
        let path = write(
            &scenario_dir,
            "pack-v1.yaml",
            r#"pack: tiny_pack
rate: 1
duration: 100ms
"#,
        );

        let err = load_scenario_entries(&path, &empty_scenario_catalog(), &pack_catalog)
            .expect_err("v1 pack shorthand must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("v2"),
            "error must mention v2 requirement, got: {msg}"
        );

        let _ = fs::remove_dir_all(&pack_dir);
        let _ = fs::remove_dir_all(&scenario_dir);
    }

    /// An unknown `@name` reference surfaces the catalog's "unknown
    /// scenario" diagnostic.
    #[test]
    fn unknown_at_name_surfaces_catalog_error() {
        let err = load_scenario_entries(
            Path::new("@does-not-exist"),
            &empty_scenario_catalog(),
            &empty_pack_catalog(),
        )
        .expect_err("unknown name must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does-not-exist") || msg.contains("unknown scenario"),
            "error must mention the missing name, got: {msg}"
        );
    }

    /// A v2 file that fails compilation (e.g. self-referencing `after:`)
    /// surfaces a `CompileError` wrapped with path context.
    #[test]
    fn v2_compile_error_includes_path_context() {
        let dir = temp_dir("v2-self-ref");
        let path = write(
            &dir,
            "broken.yaml",
            r#"version: 2
defaults:
  rate: 1
scenarios:
  - id: loopy
    signal_type: metrics
    name: loopy
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s
    after:
      ref: loopy
      op: "<"
      value: 1
"#,
        );

        let err = load_scenario_entries(&path, &empty_scenario_catalog(), &empty_pack_catalog())
            .expect_err("self-ref must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("broken.yaml"),
            "error must mention the source path, got: {msg}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // FilesystemPackResolver
    // -----------------------------------------------------------------------

    /// A pack reference that classifies as a name and is missing from the
    /// catalog reports `PackResolveOrigin::Name`.
    #[test]
    fn resolver_missing_name_reports_name_origin() {
        let catalog = empty_pack_catalog();
        let resolver = FilesystemPackResolver::new(&catalog);
        let err = resolver
            .resolve("nonexistent_pack")
            .expect_err("missing name must fail");
        assert_eq!(err.origin, PackResolveOrigin::Name);
    }

    /// A pack reference that classifies as a file path and does not exist
    /// reports `PackResolveOrigin::FilePath`.
    #[test]
    fn resolver_missing_file_reports_file_origin() {
        let catalog = empty_pack_catalog();
        let resolver = FilesystemPackResolver::new(&catalog);
        let err = resolver
            .resolve("./nonexistent_pack.yaml")
            .expect_err("missing file must fail");
        assert_eq!(err.origin, PackResolveOrigin::FilePath);
    }

    /// Resolving by pack name reads the YAML from the catalog and parses
    /// the `MetricPackDef`.
    #[test]
    fn resolver_reads_pack_by_name() {
        let pack_dir = temp_dir("resolver-name");
        write(
            &pack_dir,
            "tiny_pack.yaml",
            r#"name: tiny_pack
description: test
category: test
metrics:
  - name: m1
    generator:
      type: constant
      value: 1.0
"#,
        );
        let catalog = PackCatalog::discover(&[pack_dir.clone()]);
        let resolver = FilesystemPackResolver::new(&catalog);

        let pack = resolver.resolve("tiny_pack").expect("must resolve by name");
        assert_eq!(pack.name, "tiny_pack");
        assert_eq!(pack.metrics.len(), 1);

        let _ = fs::remove_dir_all(&pack_dir);
    }
}
