# Sonda v2 Refactor — Progress

## Current Status
- **Phase:** 5 complete (PR 6 — runtime wiring + parity tests)
- **Branch:** `refactor/unified-scenarios-v2`
- **Integration PR:** #197 (targets `main`, accumulates all v2 work)
- **Next PR:** PR 7 — CLI unification (`sonda run` v2 dispatch, `sonda catalog`, `sonda init` v2 output, deprecation/hiding of split commands)

## Milestone Checklist

| # | Milestone | Status | PR | Date |
|---|-----------|--------|----|------|
| 0 | Scaffolding & test foundation | Done | PR 1 | 2026-04-11 |
| 1 | Compiler AST and parser | Done | PR 2 (#198) | 2026-04-11 |
| 2 | Defaults resolution | Done | PR 3 (#199) | 2026-04-12 |
| 3 | Pack expansion in scenarios | Done | PR 4 | 2026-04-12 |
| 4 | `after` compiler + dependency graph | Done | PR 5 | 2026-04-12 |
| 5 | Runtime wiring + parity tests | Done | PR 6 | 2026-04-13 |
| 6 | CLI unification | Not Started | PR 7 | |
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
| 6 | Runtime wiring + parity tests | `feat/runtime-wiring` | integration | In Review | 2026-04-13 |

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
| Workspace total | 2,785 | All existing + new (+80 from PR 6; +57 tests across the 8 post-implementer-commit gate pass, plus +23 from the fix pass covering version gate, distribution rstest, rows 4.1–4.8, and regression anchors) |

## Validation Matrix Status

See [v2-validation-status.md](v2-validation-status.md) for the full 178-row checklist.

**Every row is a mandatory merge blocker. No exceptions.**

**Summary:** 110 of 178 rows Pass (62 flipped by PR 6).

| Section | Rows | Pass | Notes |
|---------|------|------|-------|
| 1-10. Feature parity | 98 | 77 | PR 3: 5.8, 10.12–10.15; PR 4: 9.1–9.8, 9.12; PR 5: 10.1–10.11; PR 6: 1.1–1.6, 2.1–2.10, 3.1–3.7, 4.1–4.8, 5.1/5.2/5.3/5.7, 6.1/6.12, 7.1–7.11, 8.1/8.2/8.3/8.4. Deferred: 5.4–5.6 (syslog/remote_write/otlp encoders, PR 8 smoke), 6.2–6.10 (non-stdout sinks, PR 8 smoke), 8.5/8.6 (CLI UX, PR 7), 9.9/9.10/9.11 (CLI, PR 7), 9.13/9.14/9.15 (built-in migration, PR 8). |
| 11. New v2 features | 18 | 18 | Complete. |
| 12-15. CLI/Server/UX/Deploy | 47 | 0 | Later PRs (7-9) |
| **16. Scenario parity bridge** | **12** | **12** | **All 12 rows Pass both compile and runtime (PR 5 + PR 6).** |
| **17. Pack parity bridge** | **3** | **3** | **All three built-in packs Pass both compile and runtime (PR 4 + PR 6).** |

## Completed Work

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

## PR 7 Preparation Notes

PR 7 is **CLI unification**. A future session starting cold on PR 7 should read this section first; everything below is the handoff context from PR 6.

### What PR 6 already hands off

- **`sonda_core::compile_scenario_file(yaml, &dyn PackResolver) -> Result<Vec<ScenarioEntry>, CompileError>`** — one-shot composition of the five v2 compile phases. This is the single library entry point the CLI should call when it detects a v2 file.
- **`sonda_core::compiler::prepare::prepare(CompiledFile) -> Result<Vec<ScenarioEntry>, PrepareError>`** — the phase-by-phase escape hatch, useful for `--dry-run` where the CLI may want to stop after `compile_after` and serialize the intermediate for inspection, or after `prepare` for the final pre-runtime view.
- **Every phase's error type remains publicly accessible** (`ParseError`, `NormalizeError`, `ExpandError`, `CompileAfterError`, `PrepareError`) so phase-by-phase callers get typed diagnostics. `CompileError` is the unified wrapper for the one-shot.
- **`ScenarioEntry.clock_group`** carries the auto-assigned or explicit clock-group string. No CLI surface reads it yet; PR 7 is the natural place to add a start-banner line or aggregate-summary grouping.
- **Runtime contract unchanged.** `prepare_entries` / `PreparedEntry` / `launch_scenario` / `run_multi` are untouched. PR 7 should not need to modify them — just route v2 output into them.

### What PR 7 must build

1. **`sonda run --scenario` v2 dispatch.** Detect `version: 2` at the top of the YAML (the existing parser has `detect_version()` — reuse it). Route v2 files through `compile_scenario_file`; route v1 files through the existing v1 loader. Both paths land in `prepare_entries` → `launch_scenario` / `run_multi`, so the branching is at the top of `run_command`.
2. **`sonda catalog list/show/run`** replacing `sonda scenarios` + `sonda packs` per spec §6.3. Catalog metadata fields: `name`, `type`, `category`, `signal`, `description`, `runnable`. Search path is the same as today (`--scenario-path`, `--pack-path`, env vars, `./scenarios/` + `./packs/`, `~/.sonda/`).
3. **`sonda init` v2 output.** The interactive wizard should emit `version: 2` files with `defaults:` and `scenarios:` blocks instead of the current v1 shape. Pre-fill paths (`--from @name`, `--from path.csv`) should map cleanly into the v2 schema.
4. **Deprecate or hide** `sonda scenarios`, `sonda packs`, `sonda story` subcommands. The story CLI must keep working — it is still the row-16.12 runtime oracle until PR 9.
5. **`--dry-run` enhancements (spec §5).** Print the compiled v2 view (resolved defaults, expanded packs, resolved `phase_offset`, assigned `clock_group`) in the format the spec shows. `compile_scenario_file` gives you the `Vec<ScenarioEntry>` directly; the formatting is the new work.
6. **CLI status output for `clock_group`.** Add a start-banner line for each scenario naming its clock_group (spec §5 format shows `clock_group: link_failover (auto)` as an example). Also consider grouping the aggregate summary by clock_group when more than one is present. This closes matrix row 8.2 "runtime observability" fully.
7. **`From<CompileError> for SondaError`.** PR 7's CLI will want to propagate compile errors through the existing `SondaError` chain.

### Target matrix rows for PR 7

- **Section 12** CLI: 12.1, 12.7 (enhanced `--dry-run`), 12.10, 12.11, 12.12–12.17 (catalog), 12.19 (`init` v2), 12.20, 12.22
- **Section 14** status output / UX: 14.1–14.9 (all)
- **Matrix row 8.2 runtime observability** — fully close (PR 6 closed end-to-end carry only)
- **Matrix row 8.6** aggregate summary grouping by clock_group
- **Matrix rows 5.2 + 6.11** `--output` shorthand on the CLI layer

### PR 9 forward pointer — v1 story parity oracle cleanup

When PR 9 removes the v1 story CLI (`sonda story --file`), the hand-built `v1_link_failover_entries` helper in `sonda-core/tests/v2_story_parity.rs` becomes a relic. It exists only because `sonda-core` tests cannot dev-dep the binary crate that owns `compile_story`. Once the v1 story module is gone there is no "v1 side" to mirror, so PR 9 should either:

- **Delete** the `link_failover_runtime_parity` test and the `v1_link_failover_entries` helper entirely (the `link_failover_compile_parity` test remains and is sufficient — it pins the v2 compile offsets byte-for-byte against the `timing::*_crossing_secs` math), **or**
- **Refactor** to compare v2-compile stdout against a pinned byte-snapshot (via `insta::assert_snapshot!` on one canonical v2 run) if the runtime execution remains valuable to protect.

The first option is cleaner and aligns with PR 9's remit ("remove transitional oracle code"). Surface this decision in PR 9's plan.

### Scope discipline for PR 7

Do NOT also:
- Migrate built-in scenario YAMLs to v2 format (PR 8).
- Remove v1 CLI subcommands (PR 9).
- Add v2 scenario server API (PR 9).
- Touch the sonda-core compile pipeline (all five phases are frozen after PR 6).
- Change `prepare_entries` / `PreparedEntry` / `launch_scenario` / `run_multi`.

### Testing conventions for PR 7

Same post-consolidation conventions as PR 6. The shared test surface is `sonda-core/tests/common/mod.rs`; any CLI-level test infrastructure goes in `sonda/tests/` (that directory already exists for CLI integration). For end-to-end CLI tests, spawn the binary as a subprocess — this is the natural setting for the subprocess tests that PR 6 deferred.

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
