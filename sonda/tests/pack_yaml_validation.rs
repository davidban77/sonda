//! CI validation: every YAML file in `packs/` must parse as a valid
//! `MetricPackDef`.
//!
//! This test replaces the compile-time guarantee that `include_str!()` +
//! parse tests provided when packs were embedded in `sonda-core`. It runs
//! as part of `cargo test --workspace` and catches schema drift or broken
//! YAML files before they reach users.

use std::path::PathBuf;

use sonda_core::packs::MetricPackDef;

/// Return the path to the repo-root `packs/` directory.
fn packs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sonda crate must have a parent directory")
        .join("packs")
}

#[test]
fn all_pack_yamls_parse_as_metric_pack_def() {
    let dir = packs_dir();
    assert!(dir.is_dir(), "packs/ directory must exist at repo root");

    let mut count = 0;
    for entry in std::fs::read_dir(&dir).expect("must read packs/ directory") {
        let entry = entry.expect("directory entry must be readable");
        let path = entry.path();

        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("yaml") && ext != Some("yml") {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));

        let def: MetricPackDef = serde_yaml_ng::from_str(&content).unwrap_or_else(|e| {
            panic!("{} failed to parse as MetricPackDef: {}", path.display(), e)
        });

        assert!(
            !def.name.is_empty(),
            "{}: name must not be empty",
            path.display()
        );
        assert!(
            !def.metrics.is_empty(),
            "{}: metrics list must not be empty",
            path.display()
        );
        assert!(
            !def.description.is_empty(),
            "{}: description must not be empty",
            path.display()
        );
        assert!(
            !def.category.is_empty(),
            "{}: category must not be empty",
            path.display()
        );

        count += 1;
    }

    assert!(
        count >= 3,
        "expected at least 3 pack YAML files, found {count}"
    );
}

#[test]
fn all_pack_names_are_snake_case() {
    let dir = packs_dir();
    for entry in std::fs::read_dir(&dir).expect("must read packs/ directory") {
        let entry = entry.expect("directory entry must be readable");
        let path = entry.path();

        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("yaml") && ext != Some("yml") {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));

        let def: MetricPackDef = serde_yaml_ng::from_str(&content)
            .unwrap_or_else(|e| panic!("{} failed to parse: {}", path.display(), e));

        assert!(
            def.name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
            "{}: name {:?} must be snake_case",
            path.display(),
            def.name
        );
    }
}

#[test]
fn all_pack_categories_are_known() {
    let known = ["infrastructure", "network", "application", "observability"];
    let dir = packs_dir();
    for entry in std::fs::read_dir(&dir).expect("must read packs/ directory") {
        let entry = entry.expect("directory entry must be readable");
        let path = entry.path();

        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("yaml") && ext != Some("yml") {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));

        let def: MetricPackDef = serde_yaml_ng::from_str(&content)
            .unwrap_or_else(|e| panic!("{} failed to parse: {}", path.display(), e));

        assert!(
            known.contains(&def.category.as_str()),
            "{}: category {:?} not in known list {:?}",
            path.display(),
            def.category,
            known
        );
    }
}
