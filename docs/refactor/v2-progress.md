# Sonda v2 Refactor — Progress

## Current Status
- **Phase:** 6 complete (PR 7 — CLI unification)
- **Branch:** `refactor/unified-scenarios-v2`
- **Integration PR:** #197 (targets `main`, accumulates all v2 work)
- **Next PR:** PR 8 — Built-ins migration (convert `scenarios/*.yaml` and `packs/*.yaml` to v2 format, dedup overlapping examples, canonical failover example, absorb `stories/` into v2 docs/examples)

## Milestone Checklist

| # | Milestone | Status | PR | Date |
|---|-----------|--------|----|------|
| 0 | Scaffolding & test foundation | Done | PR 1 | 2026-04-11 |
| 1 | Compiler AST and parser | Done | PR 2 (#198) | 2026-04-11 |
| 2 | Defaults resolution | Done | PR 3 (#199) | 2026-04-12 |
| 3 | Pack expansion in scenarios | Done | PR 4 | 2026-04-12 |
| 4 | `after` compiler + dependency graph | Done | PR 5 | 2026-04-12 |
| 5 | Runtime wiring + parity tests | Done | PR 6 | 2026-04-13 |
| 6 | CLI unification | Done | PR 7 | 2026-04-13 |
| 7 | Built-ins migration + docs | Not Started | PR 8 | |
| 8 | Server API + final cleanup | Not Started | PR 9 | |

## PR Log

| PR | Title | Branch | Target | Status | Date |
|----|-------|--------|--------|--------|------|
| 1 | Compile snapshot harness + test foundation | (direct) | integration | Merged | 2026-04-11 |
| 2 | Compiler AST, parser, and version dispatch | `feat/v2-ast-parser` | integration (#198) | Merged | 2026-04-11 |
| 3 | Defaults resolution + `parse_v2 → parse` rename | `feat/defaults-resolution` | integration (#199) | Merged | 2026-04-12 |
| 4 | Pack expansion inside `scenarios:` | `feat/pack-expansion` | integration | Merged | 2026-04-12 |
| 5 | `after` compiler + dependency graph + timing port | `feat/after-compilation` | integration (#203) | Merged | 2026-04-12 |
| — | Test-infra consolidation: insta + rstest + fixture dedup | `chore/test-infra-consolidation` | integration (#204) | Merged | 2026-04-12 |
| 6 | Runtime wiring + parity tests | `feat/runtime-wiring` | integration (#205) | Merged | 2026-04-13 |
| 7 | CLI unification | `feat/cli-unification` | integration | In Review | 2026-04-13 |

## Test Coverage

| Layer | Tests | Scope |
|-------|-------|-------|
| Compiler parser unit tests | 58 | AST parsing, validation, shorthand, edge cases (rstest tables for invalid-YAML families) |
| Compiler normalize unit tests | 37 | Defaults inheritance, label merge (inline eager / pack deferred), built-in fallbacks, missing-rate error, defaults-labels surfacing |
| Compiler expand unit tests | 35 | Pack expansion, label precedence, auto-IDs (including duplicate-name disambiguation), post-expansion id uniqueness, override validation, after propagation, resolver trait |
| Compiler timing unit tests | 44 | Crossing math for every supported generator (sawtooth/step/sequence/spike/flap/saturation/leak/degradation/spike_event/constant) and blanket rejections (sine/steady/uniform/csv_replay); inactive-max wrap-around regression — rstest tables per generator family |
| Compiler compile_after unit tests | 38 | Reference resolution, self-ref, cycles, transitive chains, delay + phase_offset additivity, step/sequence crossings, cross-signal-type, alias desugaring, clock group auto-assignment + conflicts (including whitespace/empty-string handling), dotted/ambiguous pack refs, `InvalidDuration` coverage for after.delay/phase_offset/alias-param code paths, format_duration_secs round-trip |
| Compiler fixture integration tests | 15 | Valid/invalid YAML examples parsed + normalized from disk (insta file snapshots) |
| Compiler expand fixture integration tests | 4 | Pack expansion fixtures with insta snapshots + invalid-override rejection |
| Compiler compile_after fixture integration tests | 15 | 6 valid fixtures with CompiledFile insta snapshots + 9 invalid fixtures asserting specific CompileAfterError variants |
| Pack parity bridge integration tests | 5 | 3 pack compile parity (17.1–17.3) + 2 compile_after resolution tests (11.12 override, 11.13 entry-level propagation) |
| Story parity bridge integration test | 1 | 16.12 compile parity: `stories/link-failover.yaml` v1 math vs v2 compile agree on phase_offset to the millisecond |
| Compile snapshot fixtures (insta) | 6 | v1 parity baseline; prepared-entry snapshots (non-prepared variants deleted as byte-redundant) |
| Normalize snapshot fixtures (insta) | 3 | Resolved defaults snapshots (label merge, logs default encoder, pack entry) |
| Expand snapshot fixtures (insta) | 3 | Phase 3 snapshots (overrides, multi-pack, anonymous pack; pack-file-path deleted — dual-registered in resolver) |
| Compile_after snapshot fixtures (insta) | 6 | CompiledFile snapshots covering transitive chain, step/sequence targets, cross-signal-type, phase_offset + delay sum, dotted pack ref (simple-chain deleted — subset of transitive) |
| Compiler prepare unit tests | 28 | Translator per-variant happy paths, every `PrepareError` variant, label BTreeMap→HashMap conversion, `observations_per_tick` u32→u64 widening, `clock_group`/`phase_offset` pass-through, non-v2 version rejection, rstest variant-matched missing-field coverage |
| Compile one-shot unit tests | 6 | `compile_scenario_file` end-to-end composition; `CompileError` `From` variants fire per phase |
| Runtime parity integration tests (rstest, rows 16.1–16.11) | 11 | Byte-equal for single-signal, line-multiset for multi-signal (`interface-flap`, `network-link-failure`); seeds pinned symmetrically on v1 and v2 sides |
| Link-failover runtime parity (row 16.12) | 1 | Staggered test-window `phase_offset` override on both sides; compile-side offsets already covered by the compile-parity sibling test |
| Pack runtime parity (rows 17.1–17.3) | 3 | All three built-in packs compared byte-for-byte via the same runtime path |
| Translator-semantic direct tests | 10 | Rows 1.6, 2.9–2.10, 4.1–4.8 (Exponential/Normal/Uniform distributions + histogram custom buckets), 5.2, 5.7, 6.12, 7.1–7.3 |
| Runtime parity fixtures (v2-parity, with headers) | 21 | 11 v1-built-in mirrors + 10 hand-written translator probes; every fixture has a leading comment identifying matrix rows and any deliberate divergence |
| Workspace total | 2,798 | All existing + new (+80 from PR 6; +13 from PR 7 — CLI subprocess suite, catalog/scenario_loader/sink_format unit tests, clock_group_is_auto provenance tests) |

## Validation Matrix Status

See [v2-validation-status.md](v2-validation-status.md) for the full 178-row checklist.

**Every row is a mandatory merge blocker. No exceptions.**

**Summary:** 146 of 178 rows Pass (36 flipped by PR 7).

| Section | Rows | Pass | Notes |
|---------|------|------|-------|
| 1-10. Feature parity | 98 | 84 | PR 3: 5.8, 10.12–10.15; PR 4: 9.1–9.8, 9.12; PR 5: 10.1–10.11; PR 6: 1.1–1.6, 2.1–2.10, 3.1–3.7, 4.1–4.8, 5.1/5.2/5.3/5.7, 6.1/6.12, 7.1–7.11, 8.1/8.2/8.3/8.4; PR 7: 6.11, 8.5/8.6, 9.9/9.10/9.11. Deferred: 5.4–5.6 (syslog/remote_write/otlp encoders, PR 8 smoke), 6.2–6.10 (non-stdout sinks, PR 8 smoke), 9.13/9.14/9.15 (built-in migration, PR 8). |
| 11. New v2 features | 18 | 18 | Complete. |
| 12-15. CLI/Server/UX/Deploy | 47 | 29 | PR 7: 12.1–12.20, 12.22 (21 rows); 14.1–14.9 (9 rows). Deferred: 12.21 (story removal, PR 9), 13.1–13.9 (server API, PR 9), 15.1–15.7 (deployment, PR 9). |
| **16. Scenario parity bridge** | **12** | **12** | **All 12 rows Pass both compile and runtime (PR 5 + PR 6).** |
| **17. Pack parity bridge** | **3** | **3** | **All three built-in packs Pass both compile and runtime (PR 4 + PR 6).** |

## Completed Work

### PR 7 — CLI unification (2026-04-13, in review)

- **`sonda/src/scenario_loader.rs`** — new `load_scenario_entries(path, scenario_catalog, pack_catalog) -> anyhow::Result<LoadedScenario>`. Resolves `@name` shorthand / path via existing `resolve_scenario_source`, calls `sonda_core::compiler::parse::detect_version()`, and branches: v2 → `compile_scenario_file` via a `FilesystemPackResolver` shim; v1 → explicit probe chain `is_pack_config` → `is_flat_single_scenario` (new) → `MultiScenarioConfig` fallback. Returns `Vec<ScenarioEntry>` + the detected version for downstream formatter routing. All v2 compile errors surface through `anyhow::Context` with the source path; no `From<CompileError> for SondaError` added.
- **`sonda/src/catalog.rs`** — new unified row iterator. `CatalogRow` shape is `{name, type, category, signal, description, runnable}`; `Source<'a>` enum holds borrowed `&BuiltinScenario` or `&PackEntry` (extensible to stories or other kinds). `catalog_rows`, `find_row`, `find_closest_name` (edit-distance suggestions) all work without modifying `ScenarioCatalog` / `PackCatalog`.
- **`sonda/src/dry_run.rs`** — spec §5 pretty formatter + JSON DTO behind `--format=json`. Pretty output lands on stderr (matches v1 `--dry-run` convention); JSON goes to stdout. DTO carries `clock_group_is_auto` as a boolean so consumers can distinguish auto-assigned from explicit chains.
- **`sonda/src/sink_format.rs`** — shared `sink_display` helper. Exhaustive match over every `SinkConfig` variant under every feature combination, including feature-gated `HttpPushDisabled {}` / `LokiDisabled {}` / `RemoteWriteDisabled {}` / `KafkaDisabled {}` / `OtlpGrpcDisabled {}`. Unified the sink banner format across `status.rs` start banner and `dry_run.rs` resolved-config block — both now use spec §5 `name (detail)` parens form (the prior `status.rs` used colon-separated `name: detail`, a format drift). Closed the `cargo build -p sonda --no-default-features` regression that existed at PR 7 implementer-commit time.
- **`sonda/src/init/yaml_gen.rs`** — full rewrite. Every `InitScenarioType` variant (SingleMetric, Pack, Logs, Histogram, Summary) now emits `version: 2` + `defaults:` + `scenarios:`. Round-trip tests compile the emitted YAML via `compile_scenario_file` to enforce the "init output is always runnable" invariant. No v1 fallback, no `--legacy` flag.
- **`sonda/src/cli.rs`** — `Commands::Catalog(CatalogArgs)` variant with `list` / `show` / `run` actions. `PacksRunArgs` gained `output: Option<PathBuf>` (BLOCKER 2 fix — catalog-run-pack with `-o` now writes to file). `#[command(hide = true)]` on `Scenarios`, `Packs`, `Story` — still callable (row-16.12 oracle), hidden from `--help`. Global `--format` on the CLI root (orthogonal to `--dry-run`).
- **`sonda/src/main.rs`** — `Run` arm routes through `scenario_loader::load_scenario_entries`; `Commands::Catalog` dispatch with `CatalogRunArgs → (RunArgs | PacksRunArgs)` projection so v1/v2 dispatch and CLI overrides apply identically. `apply_run_overrides` (in `config.rs`) is the single override application point for both surfaces, preserving YAML < env < CLI precedence unchanged.
- **`sonda/src/status.rs`** — `print_start` gains an optional `clock_group: <name> (auto)` line when the entry carries a clock group. `print_summary_by_clock_group` fires when ≥2 distinct groups present (including "ungrouped" as a distinct value); flat `print_summary` otherwise. `ClockGroupStats::group_is_auto: Option<bool>` threads provenance through to the aggregate.
- **sonda-core surface change (minimum):** `BaseScheduleConfig::clock_group_is_auto: Option<bool>` + `ScenarioEntry::clock_group_is_auto()` getter — authorized scope expansion. `#[serde(skip)]` so the field never leaks into YAML (compiler output, not user input). `CompiledEntry::clock_group_is_auto` + private `ClockGroupAssignment` enum in `compile_after.rs` populate it exactly at the four assignment sites (auto-derived chain → `true`, user-explicit → `false`, conflicting → error, single-node → `false`). `prepare::build_base` carries it through verbatim. v1 entries that never go through the v2 compiler read `None`, so the `(auto)` renderer correctly suppresses the suffix.
- **Subprocess test suite** under `sonda/tests/`: `cli_run_dispatch.rs` (5 tests), `cli_catalog.rs` (14 tests incl. the `-o` on pack regression), `dry_run_format.rs` (3 tests), `init_v2_output.rs` (4 tests — one per signal type), plus new fixtures under `sonda/tests/fixtures/cli/` (flat v1, multi-v1, inline v2, multi-after-chain v2, pack-backed v2, broken self-ref v2). `sonda/tests/common/mod.rs` consolidates subprocess helpers. All tests use `Command::new(env!("CARGO_BIN_EXE_sonda"))`; no new dev-deps.
- **Nine commits** on `feat/cli-unification` (3 feat + 1 doc + 5 fix-pass). Every commit passes ALL FIVE gates individually: `cargo build --workspace`, `cargo build -p sonda --no-default-features`, `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check`.
- **Reviewer fix-pass resolved 5 BLOCKERs**: (1) non-exhaustive `sink_display` match breaking `--no-default-features` build; (2) `catalog run <pack> -o <path>` silently dropping the flag; (3) `(auto)` suffix misfiring when user wrote `clock_group: chain_*` explicitly — fixed by threading `is_auto` provenance through compile_after → prepare → BaseScheduleConfig; (4) `v2` in production symbol names (`print_v2_dry_run` → `print_dry_run`, `compile_v2` inlined); (5) flat v1 single-scenario YAMLs rejected by `sonda run --scenario` (spec §6.1 violation).
- **Validation matrix rows closed (36)**: 6.11, 8.2 (fully — prior PRs landed end-to-end carry; PR 7 added the CLI observability surface), 8.5, 8.6, 9.9–9.11, 12.1–12.20 + 12.22, 14.1–14.9. Not closed: 12.21 (story removal — PR 9 scope).
- Discipline: no `v2` prefix/suffix on any Rust symbol (production or test). Existing `sonda-core/tests/v2_*.rs` parity-bridge files unchanged (PR 9 renames them alongside v1 CLI removal). Built-in YAMLs in `scenarios/` and `packs/` untouched (PR 8).

### Test infra consolidation (2026-04-12, in review)
- Adopted `insta` (JSON + YAML features) for golden snapshots and `rstest` for parametrized unit tests; both hoisted into `[workspace.dependencies]` and pulled into `sonda-core` dev-deps.
- Deleted hand-rolled `sonda-core/src/config/snapshot.rs` (390 LOC) and its `pub mod snapshot;` declaration — compile_snapshot tests now use `insta::assert_json_snapshot!` directly on the existing `Serialize` derives.
- Consolidated duplicated fixture/resolver helpers (`fixture()`, `load_repo_pack`, `builtin_pack_resolver`, snapshot helpers) into `sonda-core/tests/common/mod.rs`; shared surface for every future v2 integration test.
- Migrated 26 JSON goldens to 24 insta `.snap` files under `sonda-core/tests/snapshots/`. Deleted 2 redundant fixtures: `valid-compile-simple-chain` (strict subset of `valid-compile-transitive-chain` — clock-group assertion merged into the surviving test) and `valid-expand-pack-file-path` (covered by the dual-registered `InMemoryPackResolver`). Deleted 6 byte-redundant semantic `.json` non-prepared variants (content identical to `.prepared.json` minus `start_delay_ms`).
- Parametrized timing / parse / normalize / expand / compile_after embedded unit tests with `rstest` tables where the case families were cookie-cutter; left semantically-unique tests as standalone `#[test]` fns rather than force-fit them into tables. rstest reports each `#[case]` as a distinct test, so per-case failure location is preserved.
- Net source LOC: **−1,189** across 45 files (excluding `Cargo.lock`). Workspace test count: 2,704 → 2,705 (+1 net: rstest case expansion offset by the 12 deleted snapshot-harness self-tests and 2 redundant fixture tests — no integration coverage lost).
- Zero production code changes outside the `snapshot.rs` deletion. All four quality gates green at every commit (`cargo build`, `cargo test`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`).
- Branch: `chore/test-infra-consolidation`, 16 commits (14 code + 2 docs), based on `refactor/unified-scenarios-v2` post-PR-5. Implementer-authored discipline: no `v2` prefix on any new symbol, `#[rustfmt::skip]` applied to rstest tables to keep `#[case(...)]` rows column-aligned.

### PR 5 — `after` compiler + dependency graph (2026-04-12, merged #203)
- `sonda-core/src/compiler/timing.rs` — pure threshold-crossing math per §3.3 table. Ported verbatim from the v1 `sonda/src/story/timing.rs` (flap / sawtooth / spike / steady) and extended to cover every supported generator: `step_crossing_secs`, `sequence_crossing_secs`, `constant_crossing_secs`, plus blanket `sine_crossing_secs`, `uniform_crossing_secs`, `csv_replay_crossing_secs`. Module compiles without the `config` feature so `no-config` builds remain intact.
- `sonda-core/src/compiler/compile_after.rs` — spec §4.4 (`after`-clause compilation) + §4.5 (clock-group assignment) compile pass. Exports `compile_after()`, `CompiledFile`, `CompiledEntry`, `CompileAfterError` (typed variants: `UnknownRef`, `AmbiguousSubSignalRef`, `SelfReference`, `CircularDependency`, `UnsupportedGenerator`, `OutOfRangeThreshold`, `AmbiguousAtT0`, `ConflictingClockGroup`, `NonMetricsTarget`, `InvalidDuration`). The type itself witnesses "after is resolved" — same discipline as `ExpandedFile`/`NormalizedFile`. Note: the milestone table below reserves execution-plan "Phase 5" for runtime wiring (PR 6); this file closes the *spec* §4.4 and §4.5 compile-time passes only.
- **Reference resolution** keyed on `ExpandedEntry.id` (inline ids + pack sub-signal ids like `{entry}.{metric}#{n}`). Bare `{entry}.{metric}` against duplicate-name packs surfaces `AmbiguousSubSignalRef` with the concrete `#N` candidates listed.
- **Topological sort + cycle detection.** Kahn's algorithm produces the resolution order; when `sorted.len() < n`, a DFS with gray/black coloring reconstructs the full cycle path (`[A, B, C, A]`) for `CircularDependency.cycle`.
- **Offset formula** (matrix row 11.14): `total = user_phase_offset + Σ crossing_time + Σ delay`. `after.delay` and explicit `phase_offset` are parsed via `sonda_core::config::validate::parse_duration`. Output is formatted back to a parseable duration string (`"1m"`, `"85s"`, `"152.308s"`) by `format_duration_secs` — millisecond precision, round-trips cleanly through `parse_duration`.
- **Alias desugaring** happens inside `crossing_secs` before dispatch: `flap` → flap math, `saturation`/`leak`/`degradation` → sawtooth math, `spike_event` → spike math, `steady` → sine (rejected). Keeps the compiler pure — no `ScenarioConfig` fabrication needed.
- **Clock-group derivation** (spec §4.5): connected components of the undirected `after` graph receive a shared group. If any member has an explicit `clock_group` that value is adopted for the whole component; if two distinct values appear, `ConflictingClockGroup` fires with both values and entries named. Auto-name is `chain_{lowest_lex_id}` among component members with `Some(id)`.
- **Cross-signal-type after** (§3.5, matrix 11.11): the dependent can be metrics/logs/histogram/summary; the target must be metrics — `NonMetricsTarget` rejects logs/histogram/summary targets with a clear diagnostic.
- **v1 story path preserved.** `sonda/src/story/timing.rs` was deleted; `sonda/src/story/after_resolve.rs` now imports the shared math from `sonda_core::compiler::timing`. All 67 v1 story tests still pass.
- `sonda-core/tests/v2_compile_after_fixtures.rs` — 16 integration tests: 7 valid fixtures with `CompiledFile` golden JSON snapshots (simple chain, transitive chain, step target, sequence target, cross-signal-type, phase_offset + delay sum, pack dotted ref) + 9 invalid fixtures asserting each distinct `CompileAfterError` variant (unknown ref, cycle, self-reference, unsupported sine, out-of-range, ambiguous-at-t=0, conflicting clock_group, ambiguous pack ref, non-metrics target).
- `sonda-core/tests/v2_story_parity.rs` — compile-parity bridge for matrix row 16.12: `stories/link-failover.yaml` equivalent compiles to `phase_offset` values matching v1 story math (same `timing::*_crossing_secs` calls) to millisecond precision.
- `sonda-core/tests/v2_pack_parity.rs` — extended with two `compile_after`-level tests closing matrix 11.12 and 11.13: override-level `after` lands on the specific pack sub-signal; entry-level `after` propagates to every expanded sub-signal and they all receive the same resolved offset and shared clock group.
- Validation matrix rows closed: 10.1–10.11 (`after` semantics), 11.7 (dotted ref), 11.9 (delay), 11.11 (cross-signal-type), 11.12–11.13 (full Pass with resolved offsets), 11.14 (offset sum), 11.15–11.16 (clock groups), 11.17–11.18 (step/sequence targets). 16.12 compile parity Pass; runtime parity deferred to PR 6.
- No `v2` prefix on any symbol inside `sonda-core::compiler` (matches PR 3/4 discipline).

### PR 1 — Compile snapshot harness (2026-04-11)
- `sonda-core/src/config/snapshot.rs` — deterministic JSON snapshot serializer
- `Serialize` derives added to all config types (feature-gated)
- 6 semantic YAML fixtures + 12 golden-file integration tests
- `KafkaSaslConfig.password` skip_serializing for security
- Reviewer findings fixed: feature gates, trailing newlines, password masking

### PR 2 — Compiler AST and parser (2026-04-11, merged)
- `sonda-core/src/compiler/mod.rs` — AST types: `ScenarioFile`, `Defaults`, `Entry`, `AfterClause`, `AfterOp`
- `sonda-core/src/compiler/parse.rs` — parser with 9 validation rules, `detect_version()`
- Single-signal shorthand wrapping (inline + pack)
- Deterministic parse dispatch via `ShapeProbe` (no ambiguous fallback)
- Cross-generator mutual exclusion validation
- `MetricOverride.labels` aligned to `BTreeMap` for determinism
- 45 unit tests + 11 fixture integration tests (5 valid, 6 invalid YAML examples)
- Module named `compiler` (describes function, not version number)

### PR 4 — Pack expansion inside `scenarios:` (2026-04-12, pending review)
- `sonda-core/src/compiler/expand.rs` — `expand()`, `ExpandedFile`, `ExpandedEntry`, `ExpandError`
- `PackResolver` trait with classification helper (`classify_pack_reference`) and `PackResolveOrigin` (Name | FilePath); `InMemoryPackResolver` test/embedded impl
- Five-level label precedence chain applied per spec §2.2 (defaults → pack shared → pack per-metric → entry → override) using `BTreeMap<String, String>` for determinism
- Entry-level `after` propagated to every expanded metric; override-level `after` replaces entry-level for that specific metric (resolution deferred to PR 5)
- Auto-ID scheme: anonymous pack entries receive `"{pack_def_name}_{entry_index}"`; sub-signal IDs are `"{effective_entry_id}.{metric_name}"` for unique-by-name packs and `"{effective_entry_id}.{metric_name}#{spec_index}"` for packs shipping multiple `MetricSpec`s under the same name (e.g. `node_exporter_cpu`)
- Post-expansion id uniqueness check: `ExpandError::DuplicateEntryId` fires when a user-authored inline id collides with an auto-synthesized pack-entry id (the parser's id check only sees user-provided ids, so this pass closes the gap)
- Override key validation — unknown override keys produce `ExpandError::UnknownOverrideKey` with pack name and valid metric list, matching v1 `expand_pack` diagnostic shape
- `MetricOverride` gained an optional `after: Option<AfterClause>` field (backward-compatible `#[serde(default)]`); v1 `expand_pack` ignores it
- 33 expand unit tests + 5 new fixture integration tests (4 valid with golden snapshots, 1 invalid) + 3 pack parity bridge tests (matrix rows 17.1–17.3 compile parity, plus v2-only sub-signal id uniqueness assertion)
- Addresses validation matrix rows 9.1–9.8, 9.12, 11.6, 11.8, 17.1, 17.2, 17.3 (compile-parity only); 11.12 and 11.13 Pass for the carry-through portion — actual `after` resolution is PR 5
- `parse_v2 → parse` alignment from PR 3 reused; no v1/v2 prefix on any symbol inside `sonda-core::compiler`
- Snapshot golden `valid-defaults-pack-entry.json` updated because `MetricOverride` now serializes with `after: null`

### PR 3 — Defaults resolution and normalization (2026-04-12, pending review)
- `sonda-core/src/compiler/normalize.rs` — `normalize()`, `NormalizedFile`, `NormalizedEntry`, `NormalizeError`
- Precedence for `rate`/`duration`/`encoder`/`sink`: entry-level > `defaults:` > built-in fallback (eager, both inline and pack entries)
- Built-in encoder per signal type: `prometheus_text` for metrics/histogram/summary, `json_lines` for logs
- Built-in sink: `stdout`
- **Label composition is asymmetric** (rationale in `normalize.rs` module docs under "Labels merge"):
  - Inline entries: eager merge — `defaults.labels ∪ entry.labels`, entry wins on key conflict
  - Pack entries: no merge — `NormalizedEntry.labels` = entry's own labels; `NormalizedFile.defaults_labels` surfaces the source map so pack expansion can layer it correctly against pack `shared_labels` / per-metric / override labels
- Pack entries' `pack:` and `overrides:` fields carried through untouched (pack expansion is PR 4)
- Required-field validation: missing `rate` identifies the offending entry by index + name/id/pack
- Rename `parse_v2 → parse` workspace-wide (module prefix carries the version)
- 34 normalize unit tests + 4 new fixtures (3 valid with golden snapshots, 1 invalid)
- Addresses validation matrix rows 5.8, 10.12, 10.13, 10.14, 10.15, 11.2, 11.3
- Reviewer NOTE (pack-label precedence collision) resolved inline via Option 2 — documented in `normalize.rs` module docs (see "Labels merge")
- Reviewer NITs addressed: stale `V2 AST types` comment renamed; no-op `serde(deny_unknown_fields)` dropped from `NormalizedFile`/`NormalizedEntry`; snapshot-harness `expect()` calls converted to `unwrap_or_else(panic!)` with OS error detail

### PR 6 — Runtime wiring + parity tests (2026-04-13, in review)

- **`sonda-core/src/compiler/prepare.rs`** — new Phase 6 translator (`CompiledFile → Vec<ScenarioEntry>`). `prepare()` consumes a `CompiledFile`, fast-fails on non-v2 version via `PrepareError::UnsupportedVersion`, then dispatches on `signal_type` to variant-specific helpers. Field-for-field mapping: `labels: BTreeMap→HashMap` lossless (keys are String, no duplicates), `observations_per_tick: u32→u64` via `u64::from` (no `as` cast), `phase_offset` / `clock_group` pass through verbatim, `CompiledEntry::id` intentionally dropped (its job ended in `compile_after`'s dependency resolution — `ScenarioEntry` has no id slot). `PrepareError` variants: `UnknownSignalType`, `MissingGenerator` (metrics-only), `MissingLogGenerator` (logs-only), `MissingDistribution` (histogram/summary), `UnsupportedVersion`.
- **`sonda-core/src/compile.rs`** — new one-shot `compile_scenario_file(yaml, &dyn PackResolver) -> Result<Vec<ScenarioEntry>, CompileError>` composing `parse → normalize → expand → compile_after → prepare`. Unified `CompileError` with `#[from]` on each phase's error. Each variant doc is phase-anchored (`**Phase N** (name): ...`). Uses a private `DynPackResolver<'a>` newtype to bridge `&dyn PackResolver` (requested public API) against `expand<R: PackResolver>`'s generic bound — keeps `expand.rs` frozen. Re-exports on `sonda_core::*`: `compile_scenario_file`, `CompileError`, `PrepareError`.
- **`sonda-core/tests/common/mod.rs`** — extended with `run_and_capture_stdout(entries: Vec<ScenarioEntry>) -> Vec<u8>`: mirrors a trimmed `launch_scenario` in test-only code, spawning runners with in-memory capturing sinks instead of stdout. **Does not honor shutdown during `start_delay`** (unlike production `launch_scenario` which polls every 50ms) — fine for current call sites, caveat documented for future cancellation wiring. Sibling helpers: `assert_line_multisets_equal` for multi-signal thread-interleaved output, `normalize_timestamps` that strips Prometheus `<value> <11–19 digits>\n` ms-epoch trailers and JSON `"timestamp":"...Z"` fields (regression-anchored by two tests in the same module). No public `SinkConfig::Channel` variant was added — test surface stays out of the production enum.
- **Runtime parity suite (`sonda-core/tests/v2_runtime_parity.rs`)** — one `#[rstest]` with 11 `#[case::<scenario_name>(...)]` rows closing matrix rows 16.1–16.11. `Comparison::ByteEqual` for single-signal scenarios; `Comparison::LineMultiset` for `interface-flap` and `network-link-failure` (multi-signal → thread-interleaved writes). Seeds pinned symmetrically on v1 and v2 sides. Test durations are short (500ms–1s) to keep the suite fast.
- **Link-failover runtime parity (`sonda-core/tests/v2_story_parity.rs::link_failover_runtime_parity`)** — closes row 16.12 runtime half. Staggered `[1ms, 10ms, 20ms]` `phase_offset` override applied symmetrically on both sides so the test completes in ~1s (actual compiled offsets are 1m / ~152s, which is covered by the sibling `link_failover_compile_parity` test). The v1 oracle is a hand-built `v1_link_failover_entries` helper — `sonda-core` tests cannot dev-dep the `sonda` binary crate that owns `compile_story`, so the helper is explicitly framed as a **hand-built v2-equivalent reference** in its docstring, not a mirror. Drift risk is low because the compile-parity sibling test pins the v2 compile offsets and the `sonda story` CLI smoke path still exercises the v1 code until PR 9 removes it.
- **Pack runtime parity (`sonda-core/tests/v2_pack_runtime_parity.rs`)** — 3 tests closing rows 17.1–17.3. v1 side: `expand_pack` → hand-built `PackScenarioConfig`; v2 side: new one-shot. Byte-equal per-sub-signal after timestamp normalization.
- **Translator semantic tests (`sonda-core/tests/v2_translator_semantics.rs`)** — 10 direct tests covering matrix rows not naturally exercised by the built-in parity suite: 1.6 (mixed signal types), 2.9–2.10 (csv_replay auto-discovery + per-column labels), 4.1–4.8 (summary distribution variants — `#[rstest]` with Exponential/Normal/Uniform cases + histogram custom-buckets), 5.2 (`influx_lp` with custom `field_key`), 5.7 (encoder `precision`), 6.12 (TCP retry config), 7.1–7.3 (gaps / bursts / gap-overrides-burst). All assertions are translator-shape checks against hand-built reference `ScenarioEntry`s — no scheduler runs needed.
- **21 runtime-parity fixtures** under `sonda-core/tests/fixtures/v2-parity/`: 11 mirrors of `scenarios/*.yaml` built-ins plus 10 hand-written translator probes. Every fixture has a leading comment header naming the matrix rows it closes and (for probes) stating explicitly that it is not a v1 mirror. The pack fixtures (`node-exporter-*.yaml`, `telegraf-snmp-interface.yaml`) already had headers from PR 4.
- **Validation matrix rows Pass**: 16.1–16.11 runtime, 16.12 runtime, 17.1–17.3 runtime, plus 1.1–1.6 / 2.1–2.10 / 3.1–3.7 / 4.1–4.8 / 5.1/5.3/5.7/5.8 / 6.1/6.12 / 7.1–7.11 / 8.1/8.2/8.3/8.4. The 8.2 claim is **end-to-end carry only** — `clock_group` threads through the translator into `ScenarioEntry.clock_group`; any per-entry observability log line or scheduler coordination is deferred to PR 7 (which owns CLI status output).
- **Fix-pass commits** (post-review): doc fixes on `PrepareError` variants + `CompileError` phase anchors + broken intra-doc links in `prepare()`; YAML-comment headers on 21 fixtures; Exponential/Uniform distribution rstest in `v2_translator_semantics`; `v1_link_failover_entries` docstring correction; `normalize_timestamps` regression anchor; `v2_pack_runtime_parity` match-dispatch cleanup; strengthened `missing_required_field_fails_per_signal_type` with variant-specific `matches!`.
- **Workspace test count**: 2,705 → 2,785 (**+80**). All four quality gates green on every one of the 17 commits. Branch: `feat/runtime-wiring`.

### What PR 6 deliberately did not do

- **`clock_group` runtime observability.** PR 7 scope. The value threads through the translator; any log line or scheduler coordination is UX / CLI work.
- **`From<CompileError> for SondaError`.** PR 7 scope — lands naturally when the CLI starts routing through `compile_scenario_file`.
- **Built-in scenario migration to v2 format.** PR 8.
- **v1 story CLI removal.** PR 9.
- **`SinkConfig::Channel` public variant.** Kept test sink substitution out of the production enum.
- **Relax `expand<R: PackResolver>` to `+ ?Sized`.** `expand.rs` is frozen; the `DynPackResolver<'a>` newtype is the bridge. Unfreeze and remove the newtype in a future PR that naturally reopens `expand.rs`.

## PR 8 Preparation Notes

PR 8 is **built-ins migration**. A future session starting cold on PR 8 should read this section first; everything below is the handoff context from PR 7.

### Prerequisite — Stage 1 test-infra chore (lands before PR 8a)

A separate `chore/test-infra-encoder-sink` PR is planned to land on the integration branch **before** PR 8a starts. It parametrizes `sonda-core/tests/encoder_sink_matrix.rs` (1,304 LOC / 54 non-parametrized tests → target ≤400 LOC with `#[rstest]` tables), adds a null-field redaction to `common::snapshot_settings()` to shrink the 24 insta snapshots (~350-400 LOC of noise), deletes the confirmed orphan fixture `tests/fixtures/v2-parity/summary-latency.yaml`, and sweeps for pair-wise fixture duplication under `v2-examples/`. Target: -20% integration-test LOC (4,621 → ~3,700).

This is Stage 1 of the three-stage test consolidation plan scoped 2026-04-14. Stages 2 (fixture cleanup in PR 8a) and 3 (parity bridge collapse + rename in PR 9) follow naturally.

### What PR 7 already hands off

- **`sonda run --scenario <file>`** — auto-dispatches v1 (flat single-scenario / multi-scenario / pack-scenario) or v2 transparently per spec §6.1. Once PR 8 converts `scenarios/*.yaml` and `packs/*.yaml` to v2, the same CLI surface takes them end-to-end with no caller changes.
- **`sonda catalog list/show/run`** — the unified browsing/running surface. After PR 8 migrates built-ins, the catalog list will show all of them as v2. `catalog run <name>` works the same regardless.
- **`compile_scenario_file` is the canonical library entry point** for v2 compilation. PR 8's built-in migrations should use it (directly or via `sonda run`) in their parity tests.
- **v1 loaders still exist** (`is_flat_single_scenario`, `is_pack_config`, `MultiScenarioConfig`). They will be removed in PR 9 when the last built-in YAMLs are v2 and the v1 story CLI is gone. PR 8 should not remove them.
- **The runtime parity suite in `sonda-core/tests/v2_runtime_parity.rs`** already compares v1 vs v2 output byte-for-byte (rows 16.1–16.12) and pack compile+runtime parity (rows 17.1–17.3). Use these as the regression harness when swapping YAML contents.

### What PR 8 must build

0. **Decide scope for `examples/*.yaml`** — surfaced 2026-04-14 during Stage 1 test-infra chore. The repo root `examples/` directory holds 62 user-facing example YAMLs, all currently v1 (no `version: 2` field). Options: (a) migrate all 62 to v2 in PR 8a alongside `scenarios/`, (b) migrate only canonical examples and let auto-dispatch keep the rest working, or (c) defer entirely (auto-dispatch via `sonda run --scenario` keeps v1 examples working, so PR 8 is not blocked). Recommended: (b) — migrate the examples that are cross-linked from `docs/site/` to demonstrate v2 shape, let the long tail auto-dispatch. Confirm this decision when PR 8a kicks off.

1. **Migrate `scenarios/*.yaml` to v2 format.** Every built-in scenario (cpu-spike, memory-leak, disk-fill, latency-degradation, error-rate-spike, interface-flap, network-link-failure, steady-state, log-storm, cardinality-explosion, histogram-latency) gets `version: 2` + `defaults:` + `scenarios:` shape. Compile-parity test (v2 YAML via `compile_scenario_file` == reference `Vec<ScenarioEntry>`) must pass for each. Runtime parity (byte-equal or line-multiset stdout) must still pass against the pinned v1 baselines the parity suite already covers.
2. **Migrate `stories/link-failover.yaml`** into the v2 scenarios (absorbed, not a separate directory). Close the canonical "network link failover" example on the v2 surface. Matrix rows 9.13–9.15 flip to Pass here.
3. **Migrate `packs/*.yaml`** if any changes are needed — pack definition format is nominally unchanged per spec §7, but verify each of the three built-ins (`telegraf_snmp_interface`, `node_exporter_cpu`, `node_exporter_memory`) still resolves via `classify_pack_reference`.
4. **Dedup overlapping examples.** `network-link-failure.yaml` (scenario) and `link-failover.yaml` (story) both express the same situation — pick the canonical one (probably `link-failover.v2.yaml` with the full `after:` chain) and remove the other. Spec §5.3 calls this out.
5. **Close feature-parity rows that smoke-tests land on**: 5.4–5.6 (syslog/remote_write/otlp encoders), 6.2–6.10 (non-stdout sinks). These are docker-compose smoke tests per the SRE review memory — `@smoke` agent runs here.
6. **Update `docs/site/` examples** that reference v1 built-ins to show the v2 shape. Fold v2-scenarios.md callouts into the main docs flow where duplication arises. The doc-level dedup follows the YAML migration.

### Target matrix rows for PR 8

- **5.4, 5.5, 5.6** encoder smoke (syslog, remote_write, otlp)
- **6.2–6.10** sink smoke (file, tcp, udp, http_push, remote_write, kafka, loki, otlp_grpc)
- **9.13, 9.14, 9.15** built-in packs end-to-end
- **15.1–15.7** deployment rows may land here or PR 9 depending on docker-compose scope

### Scope discipline for PR 8

Do NOT also:
- Remove v1 CLI subcommands or v1 story module (PR 9).
- Remove v1 loaders in `scenario_loader.rs` (PR 9).
- Delete `v1_link_failover_entries` helper in `sonda-core/tests/v2_story_parity.rs` (PR 9).
- Rename `sonda-core/tests/v2_*.rs` parity bridge files (PR 9).
- Touch the sonda-core compile pipeline (frozen after PR 6 + the PR 7 `clock_group_is_auto` addition).
- Add `From<CompileError> for SondaError` or other sonda-core surface changes.

### Testing conventions for PR 8

- `@smoke` agent runs alongside `@uat` when the changeset touches Docker Compose / Helm / sink-integration surfaces (smoke tests row 5.4–5.6 + 6.2–6.10).
- v2-migration PRs MUST regenerate the v1 parity snapshots where the v1 built-in changes, but if the v2 migration preserves semantic parity (same compiled entries, same runtime output), no snapshot changes are needed.

### Stage 2 test cleanup folded into PR 8a

PR 8a is the natural home for fixture-level cleanup that the built-in migration makes obvious. This is an opportunistic pass, not a major work item:

- **Prune redundant `v2-examples/` fixtures** when a migrated built-in supersedes them. Current inventory: 34 YAMLs under `tests/fixtures/v2-examples/`. The `link-failover` / `network-link-failure` dedup called out in "What PR 8 must build" item 4 is the obvious one; additional dupes should be identified during migration and removed in the same commit that lands the v2 built-in.
- **Do NOT touch** `v2_story_parity.rs` (299 LOC, 2 tests). It still has a v1 side to compare against — the v1 `compile_story` function stays until PR 9. The story YAML moves, the test stays.
- **Do NOT rename any `v2_*.rs`** file here (still PR 9 scope).

Target for Stage 2: ~100-200 LOC deletion from `v2-examples/` YAMLs plus the associated fixture-example test functions that exercise them. No parity-test changes.

## PR 9 Forward Pointer — final cleanup

Consolidated checklist for the final cleanup PR. Surface this list in the PR 9 plan.

### v1 story parity oracle cleanup

When PR 9 removes the v1 story CLI (`sonda story --file`), the hand-built `v1_link_failover_entries` helper in `sonda-core/tests/v2_story_parity.rs` becomes a relic. It exists only because `sonda-core` tests cannot dev-dep the binary crate that owns `compile_story`. Once the v1 story module is gone, there is no "v1 side" to mirror, so PR 9 should either:

- **Delete** the `link_failover_runtime_parity` test and the `v1_link_failover_entries` helper entirely (the `link_failover_compile_parity` test remains and is sufficient — it pins the v2 compile offsets byte-for-byte against the `timing::*_crossing_secs` math), **or**
- **Refactor** to compare v2-compile stdout against a pinned byte-snapshot (via `insta::assert_snapshot!` on one canonical v2 run) if the runtime execution remains valuable to protect.

The first option is cleaner and aligns with PR 9's remit ("remove transitional oracle code").

### Stage 3 test consolidation — parity bridge collapse + rename

This is the **Stage 3** leg of the 2026-04-14 test consolidation plan. Stage 1 lands pre-PR 8 (encoder/sink parametrization + snapshot redaction); Stage 2 is the opportunistic fixture cleanup folded into PR 8a; Stage 3 is everything in this section. Target: ~20% additional integration-test LOC reduction on top of Stage 1.

Per the `feedback_no_v2_prefix` discipline, test files should not carry `v2_` prefixes. The existing parity-bridge files were tolerated during the v1→v2 transition and persist today purely as v1-equivalence gates. When PR 9 removes v1, most of them either collapse or rename:

**Collapse / delete (v1 side is gone, parity assertion is moot):**

- **`v2_story_parity.rs`** (299 LOC, 2 tests) — delete outright. See the "v1 story parity oracle cleanup" subsection above for the options; `link_failover_compile_parity` is already subsumed by the timing-math unit tests in `compiler::timing::tests` once the v1 story module stops existing.
- **`v2_pack_parity.rs` (451 LOC) + `v2_pack_runtime_parity.rs` (229 LOC)** — collapse into a single `pack_parity.rs` (~250 LOC target). With v1's `packs::expand_pack` still callable (it's part of `sonda-core`, not the binary), the choice is either (a) keep a thin sanity gate that asserts v2-compile output matches the shape v1's `expand_pack` produces, or (b) drop the v1 side and snapshot current v2 behavior. Recommended: option (b) — byte-anchor snapshots are cheaper than hand-rolled structural comparators.

**Rename (drop the `v2_` prefix, content stays):**

- `v2_runtime_parity.rs` → `runtime_parity.rs` (or absorb into per-scenario test files)
- `v2_translator_semantics.rs` → `translator_semantics.rs` — **but first re-evaluate each of the 15 tests**. Several cases were written to guarantee v1-shape compile output from the translator; post-v1 they may be redundant with unit tests in `compiler::normalize::tests` and `compiler::expand::tests`. Delete the redundant cases; keep the ones that cover v2-only semantics (e.g., dynamic labels, cardinality spike, pack overrides).
- `v2_compile_after_fixtures.rs` → `compile_after_fixtures.rs`
- `v2_fixture_examples.rs` → `fixture_examples.rs`
- `v2_expand_fixtures.rs` → `expand_fixtures.rs`

**Net Stage 3 target:**

- Delete ~500 LOC via story-parity removal + pack-parity collapse.
- Delete ~100-200 LOC via translator-semantics pruning (depends on the per-case audit).
- File-rename churn does not move the LOC needle but closes the `no_v2_prefix` discipline.

Memory records: `project_pr9_test_rename.md` tracks the rename decision; `rollout_agent_workflow.md` / `feedback_pr_pipeline_speedups.md` track the staging choice (Stage 3 lives in PR 9 because the parity bridges are only removable once v1 is gone — rewriting them pre-PR 9 doubles the work).

### NITs carried forward from PR 7 reviewer passes

- **NIT 7 — dry-run `[config]` header missing `(id: ...)` annotation** per spec §5 (`[config] [1/3] interface_oper_state (id: primary_link_state)`). Requires threading `CompiledEntry.id` through `compile_scenario_file`'s output. Forward work: enrich the library's return shape or pair `Vec<ScenarioEntry>` with a side `Vec<Option<String>>` of ids.
- **Pack-expansion dry-run missing `[override]` markers** per spec §5.2. Pre-existing gap from PR 7 commit 1 (not a regression). Render `[override]` tag on generator/label fields that came from the entry's `overrides:` block rather than the pack default. Requires carrying override provenance through `expand::ExpandedEntry`.
- **`_scenario_type: InitScenarioType` parameter in `run_init_scenario`** is now dead because every init path uses `compile_scenario_file`. Either delete the parameter + enum or document why it's retained. Small cleanup.
- **`#[allow(dead_code)] load_multi_config`** in `sonda/src/config.rs` — BLOCKER 5's flat-v1 fix routed around it, so the helper is now genuinely unused in the runtime path. Delete it (and its six unit tests) once v1 removal opens that area.

### Pre-existing `--no-default-features` test hygiene

PR 7 BLOCKER 1 fix made `cargo build -p sonda --no-default-features` pass for the first time. That revealed 4 pre-existing tests in `sonda/src/config.rs` that call `.expect(...)` without `#[cfg(feature = "http")]` gates and fail under `cargo test --workspace --no-default-features`: `parse_sink_override_http_push_with_endpoint`, `parse_sink_override_loki_with_endpoint`, `all_three_retry_flags_together_succeeds`, `logs_all_three_retry_flags_together_succeeds`. They originate from commit `82a690f` (PR #167, 2026-04-07) — **not a PR 7 regression** — but they should be fixed in PR 9 (or a dedicated hygiene PR) alongside the v1 cleanup. Gate `cargo build -p sonda --no-default-features` passes; test gate does not.

### Server API v2

- Accept v2 YAML bodies in `sonda-server` endpoints. Rows 13.1–13.9 close here.
- Matrix row 13.9 is explicitly "v2 multi-scenario response" — new response shape for the unified model.

### UX observations from PR 7 UAT

- **Exit code 0 on TCP sink connection failure** — UAT flagged. The process prints the error to stderr but exits successfully, which could mask failures in CI. Pre-existing behavior; audit and decide if exit code should propagate the sink failure.
- **File-sink concurrency** — when `catalog run <multi-pack> -o <file>` writes concurrent scenarios to a single file path, only the last scenario's output survives (file-sink concurrency limitation acknowledged in PR 7's `cli_catalog::catalog_run_pack_honors_output_flag` test).

## Active Risks
- Snapshot format stability — must be deterministic and survive refactor
- `deny_unknown_fields` on parse-time AST prevents forward-compatible parsing (deliberate); `NormalizedFile`/`NormalizedEntry` are Serialize-only projections and intentionally do not carry that attribute

## Process Notes
- All PRs target integration branch (`refactor/unified-scenarios-v2`), not `main`
- Integration branch merges to `main` only after full validation matrix passes (178/178)
- Progress file and validation status updated at end of every PR
- Every PR includes example YAML fixtures for reviewability
- Implementation plans get user approval before launching implementer
- Implementer gets requirements and constraints, not exact code blueprints
