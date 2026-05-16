//! v2 scenario file loading.

use std::path::Path;

use anyhow::{bail, Context, Result};

use sonda_core::compiler::compile_after::CompiledFile;
use sonda_core::compiler::parse::detect_version;
use sonda_core::{compile_scenario_file_compiled, CompileError};

use crate::catalog_dir::CatalogPackResolver;

pub fn has_while_clause(file: &CompiledFile) -> bool {
    file.entries.iter().any(|e| e.while_clause.is_some())
}

/// Resolve a scenario reference (path or `@name`) to a [`CompiledFile`].
pub fn load_scenario_compiled(
    scenario_ref: &str,
    catalog_dir: Option<&Path>,
) -> Result<CompiledFile> {
    let yaml = crate::config::resolve_scenario_source(scenario_ref, catalog_dir)?;
    let version = detect_version(&yaml);
    match version {
        Some(2) => compile_v2_yaml_compiled(&yaml, catalog_dir)
            .with_context(|| format!("failed to compile v2 scenario {scenario_ref}")),
        _ => bail!(
            "scenario {scenario_ref} is not a v2 scenario. \
             Sonda only accepts v2 YAML (`version: 2` at the top level). \
             Migrate this file to v2 — see docs/configuration/v2-scenarios.md \
             for the migration guide."
        ),
    }
}

pub fn compile_v2_yaml_compiled(
    yaml: &str,
    catalog_dir: Option<&Path>,
) -> Result<CompiledFile, CompileError> {
    let resolver = CatalogPackResolver::new(catalog_dir);
    compile_scenario_file_compiled(yaml, &resolver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_yaml(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).expect("write fixture");
    }

    #[test]
    fn load_v2_file_succeeds() {
        let dir = TempDir::new().expect("tempdir");
        let yaml = r#"version: 2
kind: runnable
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: m
    signal_type: metrics
    name: x
    generator:
      type: constant
      value: 1.0
"#;
        write_yaml(dir.path(), "s.yaml", yaml);
        let path = dir.path().join("s.yaml");
        let compiled = load_scenario_compiled(path.to_str().unwrap(), None).expect("must load");
        assert_eq!(compiled.entries.len(), 1);
    }

    #[test]
    fn load_v1_file_is_rejected() {
        let dir = TempDir::new().expect("tempdir");
        let yaml = "scenarios:\n  - name: x\n";
        write_yaml(dir.path(), "s.yaml", yaml);
        let path = dir.path().join("s.yaml");
        let err = load_scenario_compiled(path.to_str().unwrap(), None).expect_err("must reject v1");
        assert!(format!("{err}").contains("v2"));
    }

    #[test]
    fn load_at_name_resolves_from_catalog_dir() {
        let dir = TempDir::new().expect("tempdir");
        let yaml = r#"version: 2
kind: runnable
scenario_name: my-scn
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: m
    signal_type: metrics
    name: x
    generator:
      type: constant
      value: 1.0
"#;
        write_yaml(dir.path(), "my-scn.yaml", yaml);
        let compiled =
            load_scenario_compiled("@my-scn", Some(dir.path())).expect("must resolve @name");
        assert_eq!(compiled.entries.len(), 1);
    }
}
