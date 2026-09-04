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

use std::path::PathBuf;

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

/// The metric name a compiled entry carries. Packs only ever expand to
/// metric signals, so any other variant is a bug rather than a case.
fn entry_name(entry: &ScenarioEntry) -> &str {
    match entry {
        ScenarioEntry::Metrics(config) => config.name.as_str(),
        other => panic!("a pack expanded to a non-metric entry: {other:?}"),
    }
}

fn entry_labels(entry: &ScenarioEntry) -> Option<&std::collections::HashMap<String, String>> {
    match entry {
        ScenarioEntry::Metrics(config) => config.labels.as_ref(),
        other => panic!("a pack expanded to a non-metric entry: {other:?}"),
    }
}

/// A `CompileError`'s Display names the phase that failed; the reason is
/// down the source chain. Flatten it so an assertion can look for the reason.
fn error_chain(err: &sonda_core::CompileError) -> String {
    let mut out = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(e) = source {
        out.push_str(&format!(": {e}"));
        source = e.source();
    }
    out
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

// ---------------------------------------------------------------------------
// Extension chains (W4 phase 1c)
// ---------------------------------------------------------------------------

/// An extension over a real builtin, resolved through the real resolver
/// chain and compiled by the real pipeline.
///
/// The corpus cannot cover this on its own: no builtin declares `extends:`
/// yet, so the graph path would be exercised by unit tests only — and a code
/// path no corpus entry drives is fake coverage. This fixture stands in
/// until the first builtin extension ships, at which point
/// `the_corpus_records_how_many_builtins_extend` starts reporting it.
const IOSXE_FIXTURE: &str = "\
version: 2
kind: composable
name: fixture_iosxe_interface
description: an extension over the SNMP interface base, for the graph gate
category: network
extends: telegraf_snmp_interface

shared_labels:
  platform: iosxe

metrics:
  - name: ifInDiscards
    generator: { type: step, start: 0.0, step_size: 0.05 }

deviations:
  - metric: ifOperStatus
    replace:
      generator: { type: constant, value: 2.0 }
  - metric: ifHCOutOctets
    not_supported: true
";

fn scenario_referencing_name(name: &str) -> String {
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
  - id: dev
    signal_type: metrics
    pack: {name}
"
    )
}

/// The graph path, end to end: resolve an extension by name through the
/// chained resolver, materialize it over the embedded base, and compile.
#[test]
fn an_extension_over_a_builtin_compiles_through_the_real_pipeline() {
    let base = builtin::find("telegraf_snmp_interface")
        .expect("the base this fixture extends must be embedded");
    let base_pack = builtin::parse_pack(base).expect("the base must parse");
    let base_selectors: Vec<String> = base_pack.metrics.iter().map(|m| m.selector()).collect();
    // Guard: the fixture's deviations name real selectors. If the base is
    // edited so they do not, this says so instead of the gate quietly
    // covering a shape the base no longer has.
    for needed in ["ifOperStatus", "ifHCOutOctets"] {
        assert!(
            base_selectors.iter().any(|s| s == needed),
            "fixture deviates on '{needed}', which base '{}' no longer declares: {base_selectors:?}",
            base_pack.name
        );
    }

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fixture-iosxe.yaml"), IOSXE_FIXTURE).expect("write");
    let resolver = sonda_core::catalog::CatalogPackResolver::new(Some(dir.path()));

    let entries = compile_scenario_file(
        &scenario_referencing_name("fixture_iosxe_interface"),
        &resolver,
    )
    .expect("the extension must compile through the resolved chain");

    let names: Vec<&str> = entries.iter().map(entry_name).collect();

    // Additive metric present, deviated one still present, removed one gone.
    assert!(
        names.contains(&"ifInDiscards"),
        "addition missing: {names:?}"
    );
    assert!(
        names.contains(&"ifOperStatus"),
        "deviated metric missing: {names:?}"
    );
    assert!(
        !names.contains(&"ifHCOutOctets"),
        "`not_supported` did not remove it: {names:?}"
    );
    assert_eq!(
        entries.len(),
        base_pack.metrics.len(),
        "one removed, one added: {names:?}"
    );

    // The extension's shared label reaches every signal, base-derived ones
    // included — the materialized pack's shared_labels, not the extension's
    // own metrics' alone.
    for entry in &entries {
        let labels = entry_labels(entry);
        assert_eq!(
            labels.and_then(|l| l.get("platform")).map(String::as_str),
            Some("iosxe"),
            "{} lost the extension's shared label",
            entry_name(entry)
        );
    }
}

/// A deviation naming nothing must fail the compile, which is what stops
/// selector coverage above from passing vacuously: if a no-op deviation were
/// tolerated, the fixture could deviate on nothing and still look green.
#[test]
fn a_deviation_naming_no_metric_in_the_base_fails_the_compile() {
    let sabotaged = IOSXE_FIXTURE.replace("metric: ifOperStatus", "metric: ifNotAThing");
    assert!(
        sabotaged.contains("ifNotAThing"),
        "the mutation must actually be present in the fixture text"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fixture-iosxe.yaml"), &sabotaged).expect("write");
    let resolver = sonda_core::catalog::CatalogPackResolver::new(Some(dir.path()));

    let err = compile_scenario_file(
        &scenario_referencing_name("fixture_iosxe_interface"),
        &resolver,
    )
    .expect_err("a deviation matching nothing must not compile");
    let chain = error_chain(&err);
    assert!(
        chain.contains("ifNotAThing") && chain.contains("matches no metric"),
        "must name the selector that matched nothing: {chain}"
    );
}

/// How many builtins declare `extends:`. Zero today, and the message says
/// so rather than the gate implying the corpus covers the graph path — the
/// fixture above is what covers it. When the first builtin extension ships
/// this starts asserting the corpus itself walks one.
#[test]
fn the_corpus_records_how_many_builtins_extend() {
    assert_eq!(builtin::PACKS.len(), PACK_COUNT, "vacuous-pass guard");

    let extending: Vec<&str> = builtin::PACKS
        .iter()
        .filter(|p| {
            builtin::parse_pack(p)
                .map(|d| d.extends.is_some())
                .unwrap_or(false)
        })
        .map(|p| p.name)
        .collect();

    if extending.is_empty() {
        // Not a failure: no builtin extension has shipped. The assertion
        // that matters is that the fixture test above exists and drives the
        // graph path through the real pipeline.
        return;
    }
    for name in extending {
        let pack = builtin::find(name).expect("just enumerated");
        let resolver = sonda_core::catalog::CatalogPackResolver::new(None);
        let entries = compile_scenario_file(&scenario_referencing_name(name), &resolver)
            .unwrap_or_else(|e| panic!("builtin extension {} must compile: {e}", pack.file));
        assert!(!entries.is_empty(), "{}", pack.file);
    }
}

/// The listing must mark the two ways an extension becomes unreferenceable.
///
/// `sonda list` gained `(unusable)` marking for a pack that fails
/// `validate_pack`; `extends:` adds two more causes, and neither is visible
/// from one file. `merged` is where the whole name space exists, so that is
/// where the chain is walked — and the verdict must match what a run gives.
#[test]
fn a_broken_extends_chain_is_marked_in_the_listing() {
    fn pack_yaml(name: &str, extends: Option<&str>) -> String {
        let line = extends
            .map(|b| format!("extends: {b}\n"))
            .unwrap_or_default();
        format!(
            "kind: composable\nname: {name}\ndescription: d\ncategory: test\n{line}\
             metrics:\n  - name: {name}_m\n    generator: {{ type: constant, value: 1.0 }}\n"
        )
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let write = |file: &str, body: String| {
        std::fs::write(dir.path().join(file), body).expect("write");
    };
    write("orphan.yaml", pack_yaml("orphan", Some("no_such_base")));
    write("x.yaml", pack_yaml("x", Some("y")));
    write("y.yaml", pack_yaml("y", Some("x")));
    write("fine.yaml", pack_yaml("fine", None));
    // An extension over an embedded base: resolvable only because `merged`
    // sees the builtins too, which is the case a user-dir walk would miss.
    write(
        "over-builtin.yaml",
        pack_yaml("over_builtin", Some("telegraf_snmp_interface")),
    );

    let listing = sonda_core::catalog::merged(Some(dir.path())).expect("merged");
    let verdict = |name: &str| -> Option<String> {
        listing
            .entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} must be listed, not dropped"))
            .pack_error
            .clone()
    };

    let orphan = verdict("orphan").expect("a missing base must be marked");
    assert!(orphan.contains("no_such_base"), "{orphan}");

    for name in ["x", "y"] {
        let cycle = verdict(name).unwrap_or_else(|| panic!("{name} is in a cycle, must be marked"));
        assert!(cycle.contains("cycle"), "{name}: {cycle}");
    }

    assert_eq!(verdict("fine"), None, "a plain pack must stay clean");
    assert_eq!(
        verdict("over_builtin"),
        None,
        "an extension over an embedded base resolves and must stay clean"
    );

    // The listing's verdict and a run's must agree, in both directions.
    let resolver = sonda_core::catalog::CatalogPackResolver::new(Some(dir.path()));
    let chain = error_chain(
        &compile_scenario_file(&scenario_referencing_name("orphan"), &resolver)
            .expect_err("the pack the listing marks must actually fail"),
    );
    assert!(chain.contains("no_such_base"), "{chain}");
    compile_scenario_file(&scenario_referencing_name("over_builtin"), &resolver)
        .expect("the pack the listing calls clean must actually compile");
}

/// A pack whose deviations can never be applied is marked too — the same
/// rule `resolve_pack_chain` enforces, asked at listing time.
#[test]
fn deviations_without_extends_are_marked_in_the_listing() {
    const LONE: &str = "\
kind: composable
name: lone
description: deviates with nothing to deviate from
category: test
metrics:
  - name: a
    generator: { type: constant, value: 1.0 }
deviations:
  - metric: a
    not_supported: true
";
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lone.yaml"), LONE).expect("write");

    let listing = sonda_core::catalog::merged(Some(dir.path())).expect("merged");
    let entry = listing
        .entries
        .iter()
        .find(|e| e.name == "lone")
        .expect("must be listed");
    let why = entry.pack_error.as_deref().expect("must be marked");
    assert!(why.contains("no `extends:`"), "{why}");

    let resolver = sonda_core::catalog::CatalogPackResolver::new(Some(dir.path()));
    let chain = error_chain(
        &compile_scenario_file(&scenario_referencing_name("lone"), &resolver)
            .expect_err("must also fail at run"),
    );
    assert!(chain.contains("no `extends:`"), "{chain}");
}

// ---------------------------------------------------------------------------
// Pack defaults must be plausible values, not just valid config
// ---------------------------------------------------------------------------

/// Specs whose default may legitimately go below zero, with the reason.
///
/// Empty today: every metric the builtin packs model is a count, a duration,
/// a byte total or a 0/1 enum. A pack that ships a genuinely signed metric —
/// a temperature, a clock delta — belongs here with a reason, so the
/// exemption appears in the diff rather than in a silent skip.
const MAY_BE_NEGATIVE: &[(&str, &str)] = &[];

/// No builtin pack's default generator may emit a negative value.
///
/// This exists because it happened. `http_request_duration_seconds.p50` used
/// `steady` at a centre of 0.032s; the alias defaults jitter to ±1.0
/// *absolute*, which is sensible for percentages and ruinous for seconds, and
/// the pack emitted -0.44s. It compiled, expanded, and passed every gate —
/// the value was only wrong, and nothing was looking at values.
///
/// The sampling drives the real pipeline: `desugar_entry` (aliases hide the
/// jitter), `create_generator`, then `wrap_with_jitter` — the same three
/// public calls the runner makes, in that order, so jitter is included rather
/// than assumed away. It is jitter that produced the original defect.
#[test]
fn no_builtin_pack_default_emits_a_negative_value() {
    // Two full periods of the longest cycle any builtin declares (300s at
    // rate 1), so a generator that only dips late is still sampled there.
    const TICKS: u64 = 600;
    assert_eq!(builtin::PACKS.len(), PACK_COUNT, "vacuous-pass guard");

    let resolver = builtin::BuiltinPackResolver::new();
    let mut sampled = 0usize;
    let mut offences: Vec<String> = Vec::new();

    for pack in builtin::PACKS {
        let entries = compile_scenario_file(&scenario_referencing(pack), &resolver)
            .unwrap_or_else(|e| panic!("builtin pack {} must compile: {e}", pack.file));

        for entry in entries {
            let name = entry_name(&entry).to_string();
            if MAY_BE_NEGATIVE
                .iter()
                .any(|(pack_name, metric)| *pack_name == pack.name && *metric == name)
            {
                continue;
            }

            let desugared = sonda_core::desugar_entry(entry)
                .unwrap_or_else(|e| panic!("{}: {name} must desugar: {e}", pack.file));
            let config = match &desugared {
                ScenarioEntry::Metrics(c) => c,
                other => panic!("{}: unexpected entry {other:?}", pack.file),
            };
            let generator =
                sonda_core::generator::create_generator(&config.generator, config.base.rate)
                    .unwrap_or_else(|e| panic!("{}: {name} must build: {e}", pack.file));
            let generator = sonda_core::generator::wrap_with_jitter(
                generator,
                config.base.jitter,
                config.base.jitter_seed,
            );

            for tick in 0..TICKS {
                let value = generator.value(tick);
                if value < 0.0 {
                    offences.push(format!("{}: {name} = {value} at tick {tick}", pack.file));
                    break;
                }
            }
            sampled += 1;
        }
    }

    // Exact, not a floor: `>= PACK_COUNT` would be satisfied by a walk that
    // reached one spec per pack and skipped the rest, which is most of them.
    let declared: usize = builtin::PACKS
        .iter()
        .map(|pack| {
            builtin::parse_pack(pack)
                .unwrap_or_else(|e| panic!("builtin pack {} must parse: {e}", pack.file))
                .metrics
                .len()
        })
        .sum();
    assert_eq!(
        sampled, declared,
        "sampled {sampled} specs but the packs declare {declared}; the walk did not reach every one"
    );
    assert!(
        offences.is_empty(),
        "a pack default emitted a negative value. Counts, durations and 0/1 \
         enums cannot be negative — check the generator's `noise:`, which the \
         `steady` alias defaults to ±1.0 absolute:\n  {}",
        offences.join("\n  ")
    );
}

/// `sonda-core/packs` is a symlink, and the packs are the root copy.
///
/// `cargo publish` packages only what is under the crate root, so the embedding
/// `include_str!` has to reach its files without leaving `sonda-core/` —
/// the symlink is what lets the single copy stay at the repo root. Replaced by
/// a real directory it would still compile, and the two copies would drift.
#[test]
fn the_crate_reaches_the_root_packs_through_a_symlink() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let link = crate_dir.join("packs");

    let meta = std::fs::symlink_metadata(&link)
        .unwrap_or_else(|e| panic!("{} must exist: {e}", link.display()));
    assert!(
        meta.file_type().is_symlink(),
        "{} must be a symlink, not a copy of the packs",
        link.display()
    );
    assert_eq!(
        std::fs::read_link(&link).expect("the symlink resolves"),
        PathBuf::from("../packs"),
        "the symlink must point at the repo-root packs/"
    );

    let root_pack = crate_dir
        .parent()
        .expect("the crate has a parent")
        .join("packs")
        .join(builtin::PACKS[0].file);
    assert_eq!(
        std::fs::read_to_string(link.join(builtin::PACKS[0].file))
            .expect("readable through the symlink"),
        std::fs::read_to_string(&root_pack).expect("readable at the root"),
        "the symlink must resolve to the same bytes as the root copy"
    );
}
