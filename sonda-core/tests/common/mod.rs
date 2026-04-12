//! Shared helpers for integration tests.
//!
//! Cargo treats `tests/common/mod.rs` as a non-binary test module — the
//! file is compiled once per integration test that declares `mod common;`
//! at its root, so it never produces a standalone `no tests` harness run.
//!
//! This module consolidates the fixture-loading, pack-loading, and
//! compilation-chaining helpers that were previously duplicated across
//! `v2_fixture_examples.rs`, `v2_expand_fixtures.rs`,
//! `v2_compile_after_fixtures.rs`, `v2_story_parity.rs`, and
//! `v2_pack_parity.rs`.
//!
//! Snapshot assertions are handled by [`insta`] directly — this module only
//! produces the value that the caller feeds into `insta::assert_json_snapshot!`.
//!
//! Keep the surface area here deliberately small: every helper either loads
//! a fixture from disk or runs a deterministic compile step. Nothing in
//! this module decides *what* a test expects — that still lives in the caller.

#![cfg(feature = "config")]
#![allow(dead_code)]

use std::path::PathBuf;

use sonda_core::compiler::compile_after::{compile_after, CompiledFile};
use sonda_core::compiler::expand::{expand, ExpandedFile, InMemoryPackResolver};
use sonda_core::compiler::normalize::normalize;
use sonda_core::compiler::parse::parse;
use sonda_core::packs::MetricPackDef;

// -----------------------------------------------------------------------------
// Paths
// -----------------------------------------------------------------------------

/// Return the absolute path to the crate's `tests/fixtures/` directory.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Return the absolute path to the repository root (the workspace dir).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has a parent directory")
        .to_path_buf()
}

// -----------------------------------------------------------------------------
// Fixture loaders
// -----------------------------------------------------------------------------

/// Read a scenario fixture from `tests/fixtures/v2-examples/`.
///
/// Panics with a clear message if the file cannot be read; that is the
/// right behavior for tests — a missing fixture is always a bug.
pub fn example_fixture(name: &str) -> String {
    let path = fixtures_dir().join("v2-examples").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {}", path.display(), e))
}

/// Read a scenario fixture from `tests/fixtures/v2-parity/`.
pub fn parity_fixture(name: &str) -> String {
    let path = fixtures_dir().join("v2-parity").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {}", path.display(), e))
}

/// Load and parse a pack YAML from the repo-root `packs/` directory.
pub fn load_repo_pack(file_name: &str) -> MetricPackDef {
    let path = repo_root().join("packs").join(file_name);
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read pack {}: {}", path.display(), e));
    serde_yaml_ng::from_str::<MetricPackDef>(&yaml)
        .unwrap_or_else(|e| panic!("cannot parse pack {}: {}", path.display(), e))
}

// -----------------------------------------------------------------------------
// Resolvers
// -----------------------------------------------------------------------------

/// Build an [`InMemoryPackResolver`] preloaded with the three built-in
/// packs (telegraf_snmp_interface, node_exporter_cpu, node_exporter_memory),
/// keyed by both the canonical pack name and the typical file-path
/// reference used in test fixtures.
pub fn builtin_pack_resolver() -> InMemoryPackResolver {
    let mut r = InMemoryPackResolver::new();
    for (file, pack_name) in [
        ("telegraf-snmp-interface.yaml", "telegraf_snmp_interface"),
        ("node-exporter-cpu.yaml", "node_exporter_cpu"),
        ("node-exporter-memory.yaml", "node_exporter_memory"),
    ] {
        let pack = load_repo_pack(file);
        r.insert(pack_name, pack.clone());
        r.insert(format!("./packs/{file}"), pack);
    }
    r
}

/// Build an [`InMemoryPackResolver`] containing exactly one pack registered
/// under the given lookup name.
pub fn resolver_with(name: &str, pack: MetricPackDef) -> InMemoryPackResolver {
    let mut r = InMemoryPackResolver::new();
    r.insert(name, pack);
    r
}

// -----------------------------------------------------------------------------
// Compile chain helpers
// -----------------------------------------------------------------------------

/// Run `parse → normalize → expand` on a fixture YAML, panicking on any
/// step's failure. Use this when the fixture is known to expand cleanly.
pub fn compile_to_expanded(yaml: &str, resolver: &InMemoryPackResolver) -> ExpandedFile {
    let parsed = parse(yaml).expect("fixture must parse");
    let normalized = normalize(parsed).expect("fixture must normalize");
    expand(normalized, resolver).expect("fixture must expand")
}

/// Run the full v2 compile pipeline (`parse → normalize → expand →
/// compile_after`), panicking on any step's failure.
pub fn compile_to_compiled(yaml: &str, resolver: &InMemoryPackResolver) -> CompiledFile {
    let expanded = compile_to_expanded(yaml, resolver);
    compile_after(expanded).expect("fixture must compile after")
}

// -----------------------------------------------------------------------------
// Snapshot settings
// -----------------------------------------------------------------------------

/// Return an [`insta::Settings`] pre-configured for compiler snapshots.
///
/// Every snapshot in the v2 suite wants `sort_maps = true` so that output is
/// stable regardless of `HashMap` iteration order on the producer side. This
/// helper centralizes that default; call
/// `snapshot_settings().bind(|| insta::assert_json_snapshot!(value))` instead
/// of duplicating a `with_settings!` block in every test.
pub fn snapshot_settings() -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.set_sort_maps(true);
    s
}
