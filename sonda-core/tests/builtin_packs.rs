//! Every builtin pack must survive the real compile pipeline.
//!
//! The count gate in `catalog::builtin` proves the embedded set is the size
//! it claims. That says nothing about whether the packs *work*: an embedded
//! file with a generator type the compiler does not know parses as YAML,
//! lists in `sonda list`, and fails only when someone runs it.
//!
//! So this drives each pack through `compile_scenario_file` — parse,
//! normalize, expand, resolve `after`, prepare — the same path `sonda run`
//! takes, with a scenario that references the pack by name and a
//! `BuiltinPackResolver` answering. A pack that cannot expand fails here.

#![cfg(feature = "config")]

use sonda_core::catalog::builtin::{self, BuiltinPack, PACK_COUNT};
use sonda_core::compile_scenario_file;
use sonda_core::ScenarioEntry;

/// A runnable scenario whose only entry is the pack under test.
fn scenario_referencing(pack: &BuiltinPack) -> String {
    format!(
        "version: 2
kind: runnable

defaults:
  rate: 1
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    pack: {}
",
        pack.name
    )
}

/// Guards every per-pack assertion below against iterating over nothing.
///
/// An `include_str!` list that lost its entries, or a `PACKS` slice renamed
/// out from under this file, would otherwise make each loop body run zero
/// times and report success.
#[test]
fn the_embedded_corpus_is_the_size_it_declares() {
    assert_eq!(
        builtin::PACKS.len(),
        PACK_COUNT,
        "the embedded set and PACK_COUNT disagree; every check in this file \
         iterates that set and would pass vacuously"
    );
    const { assert!(PACK_COUNT > 0, "an empty builtin catalog is never correct") };
}

#[test]
fn every_builtin_pack_compiles_through_the_real_pipeline() {
    assert_eq!(
        builtin::PACKS.len(),
        PACK_COUNT,
        "vacuous-pass guard: iterated count must equal the declared constant"
    );

    let resolver = builtin::BuiltinPackResolver::new();
    let mut compiled_packs = 0;
    for pack in builtin::PACKS {
        let entries = compile_scenario_file(&scenario_referencing(pack), &resolver)
            .unwrap_or_else(|e| panic!("builtin pack {} must compile: {e}", pack.file));

        assert!(
            !entries.is_empty(),
            "{} expanded to no scenario entries",
            pack.file
        );
        for entry in &entries {
            assert!(
                matches!(entry, ScenarioEntry::Metrics(_)),
                "{}: a metric pack must expand to metric entries",
                pack.file
            );
        }
        compiled_packs += 1;
    }
    assert_eq!(
        compiled_packs, PACK_COUNT,
        "every declared pack must have been compiled"
    );
}

/// One entry per metric spec, so a pack that silently lost its metrics on
/// the way through expansion is caught by count rather than by shape.
#[test]
fn each_pack_expands_to_one_entry_per_metric_spec() {
    assert_eq!(builtin::PACKS.len(), PACK_COUNT, "vacuous-pass guard");

    let resolver = builtin::BuiltinPackResolver::new();
    for pack in builtin::PACKS {
        let def = builtin::parse_pack(pack)
            .unwrap_or_else(|e| panic!("builtin pack {} must parse: {e}", pack.file));
        let entries = compile_scenario_file(&scenario_referencing(pack), &resolver)
            .unwrap_or_else(|e| panic!("builtin pack {} must compile: {e}", pack.file));

        assert!(
            !def.metrics.is_empty(),
            "{} declares no metrics at all",
            pack.file
        );
        assert_eq!(
            entries.len(),
            def.metrics.len(),
            "{}: {} metric specs but {} compiled entries",
            pack.file,
            def.metrics.len(),
            entries.len()
        );
    }
}

/// The metric names a user would see. A pack whose specs expanded under the
/// wrong names would still pass the count check above.
#[test]
fn every_metric_name_declared_by_a_pack_reaches_the_compiled_entries() {
    assert_eq!(builtin::PACKS.len(), PACK_COUNT, "vacuous-pass guard");

    let resolver = builtin::BuiltinPackResolver::new();
    for pack in builtin::PACKS {
        let def = builtin::parse_pack(pack)
            .unwrap_or_else(|e| panic!("builtin pack {} must parse: {e}", pack.file));
        let entries = compile_scenario_file(&scenario_referencing(pack), &resolver)
            .unwrap_or_else(|e| panic!("builtin pack {} must compile: {e}", pack.file));

        let compiled_names: Vec<&str> = entries
            .iter()
            .map(|entry| match entry {
                ScenarioEntry::Metrics(config) => config.name.as_str(),
                other => panic!("{}: unexpected entry {other:?}", pack.file),
            })
            .collect();

        for spec in &def.metrics {
            assert!(
                compiled_names.contains(&spec.name.as_str()),
                "{}: metric {:?} is declared but absent from the compiled entries {compiled_names:?}",
                pack.file,
                spec.name
            );
        }
    }
}

/// The chained resolver is what every surface actually builds, so assert the
/// packs compile through that too — not only through the builtin resolver in
/// isolation.
#[test]
fn every_builtin_pack_compiles_through_the_chained_catalog_resolver() {
    assert_eq!(builtin::PACKS.len(), PACK_COUNT, "vacuous-pass guard");

    let resolver = sonda_core::catalog::CatalogPackResolver::new(None);
    for pack in builtin::PACKS {
        let entries =
            compile_scenario_file(&scenario_referencing(pack), &resolver).unwrap_or_else(|e| {
                panic!(
                    "builtin pack {} must compile with no --catalog: {e}",
                    pack.file
                )
            });
        assert!(!entries.is_empty(), "{}", pack.file);
    }
}

/// The listing's verdict and a run's must agree.
///
/// `sonda list` marks an entry `(unusable)` from `CatalogEntry::pack_error`,
/// while `pack: <name>` fails at expansion. Two code paths, one question —
/// so assert they answer it the same way on the corpus that ships. Every
/// builtin compiles above, so every builtin must also list clean.
#[test]
fn no_builtin_lists_as_unusable() {
    let entries = builtin::entries();
    assert_eq!(entries.len(), PACK_COUNT, "vacuous-pass guard");

    let broken: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            e.pack_error
                .as_ref()
                .map(|why| format!("{}: {why}", e.name))
        })
        .collect();
    assert!(
        broken.is_empty(),
        "these builtins compile but `sonda list` calls them unusable:\n  {}",
        broken.join("\n  ")
    );
}

/// The other direction, which is the one that actually goes wrong: a pack
/// the resolver refuses must be *listed and marked*, not dropped — the
/// listing is how a user finds the file, and `sonda show` is how they read
/// it. Driven through the real on-disk walk, not a copy of its logic.
#[test]
fn an_unaddressable_pack_is_listed_and_marked() {
    const BROKEN: &str = "\
kind: composable
name: broken_pack
description: a pack whose repeated name declares no ids
category: test
metrics:
  - name: cpu
    generator: { type: constant, value: 1.0 }
  - name: cpu
    generator: { type: constant, value: 2.0 }
";
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("broken.yaml"), BROKEN).expect("write");

    let listing = sonda_core::catalog::enumerate_with_skips(dir.path()).expect("enumerate");
    assert!(
        listing.skipped.is_empty(),
        "an unaddressable pack must be listed, not skipped: {:?}",
        listing.skipped
    );
    let entry = listing
        .entries
        .iter()
        .find(|e| e.name == "broken_pack")
        .expect("a composable header must still produce an entry");
    let why = entry
        .pack_error
        .as_deref()
        .expect("an unaddressable pack must be marked, not listed as ordinary");
    assert!(
        why.contains("appears 2 times"),
        "the marker must carry the resolver's own reason, got: {why}"
    );

    // And the same pack really is unusable, so the marker is not decorative.
    let resolver = sonda_core::catalog::CatalogPackResolver::new(Some(dir.path()));
    let scenario = "version: 2\nkind: runnable\ndefaults: { rate: 1 }\n\
                    scenarios:\n  - signal_type: metrics\n    pack: broken_pack\n";
    let err = compile_scenario_file(scenario, &resolver)
        .expect_err("the pack the listing marks must actually fail to expand");
    // The top-level Display is a category; the reason is down the source
    // chain, which is where the two paths have to agree.
    let mut chain = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(e) = source {
        chain.push_str(&format!(": {e}"));
        source = e.source();
    }
    assert!(
        chain.contains("appears 2 times"),
        "listing and run must give the same reason, run said: {chain}"
    );
}
