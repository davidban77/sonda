# Sonda v2 Refactor — Progress

## Current Status
- **Phase:** 1 — Compiler AST and parser (complete, pending merge)
- **Branch:** `refactor/unified-scenarios-v2`
- **Integration PR:** #197 (targets `main`, accumulates all v2 work)
- **Next PR:** PR 3 — Defaults resolution and normalization

## Milestone Checklist

| # | Milestone | Status | PR | Date |
|---|-----------|--------|----|------|
| 0 | Scaffolding & test foundation | Done | PR 1 | 2026-04-11 |
| 1 | Compiler AST and parser | In Review | PR 2 (#198) | 2026-04-11 |
| 2 | Defaults resolution | Not Started | PR 3 | |
| 3 | Pack expansion in scenarios | Not Started | PR 4 | |
| 4 | `after` compiler + dependency graph | Not Started | PR 5 | |
| 5 | Runtime wiring + parity tests | Not Started | PR 6 | |
| 6 | CLI unification | Not Started | PR 7 | |
| 7 | Built-ins migration + docs | Not Started | PR 8 | |
| 8 | Server API + final cleanup | Not Started | PR 9 | |

## PR Log

| PR | Title | Branch | Target | Status | Date |
|----|-------|--------|--------|--------|------|
| 1 | Compile snapshot harness + test foundation | (direct) | integration | Merged | 2026-04-11 |
| 2 | Compiler AST, parser, and version dispatch | `feat/v2-ast-parser` | integration (#198) | In Review | 2026-04-11 |

## Test Coverage

| Layer | Tests | Scope |
|-------|-------|-------|
| Compiler parser unit tests | 45 | AST parsing, validation, shorthand, edge cases |
| Compiler fixture integration tests | 11 | Valid/invalid YAML examples parsed from disk |
| Compile snapshot golden files | 12 | v1 parity baseline (6 fixtures x raw+prepared) |
| **New in refactor** | **68** | |
| Workspace total | 2,543 | All existing + new |

## Validation Matrix Status

See [v2-validation-status.md](v2-validation-status.md) for the full 162-row checklist.

**Every row is a mandatory merge blocker. No exceptions.**

**Summary:** 4 of 162 rows addressed so far (all in section 11 — new v2 features).

| Section | Rows | Addressed | Notes |
|---------|------|-----------|-------|
| 1-10. Feature parity | 97 | 0 | Needs compilation pipeline + runtime wiring |
| 11. New v2 features | 18 | 4 | 11.1, 11.4, 11.5, 11.10 done in PR 2 |
| 12-15. CLI/Server/UX/Deploy | 47 | 0 | Later PRs (7-9) |
| **16. Scenario parity bridge** | **12** | **0** | **v1→v2 compile + runtime for all built-ins + story** |
| **17. Pack parity bridge** | **3** | **0** | **v1→v2 compile + runtime for all built-in packs** |

## Completed Work

### PR 1 — Compile snapshot harness (2026-04-11)
- `sonda-core/src/config/snapshot.rs` — deterministic JSON snapshot serializer
- `Serialize` derives added to all config types (feature-gated)
- 6 semantic YAML fixtures + 12 golden-file integration tests
- `KafkaSaslConfig.password` skip_serializing for security
- Reviewer findings fixed: feature gates, trailing newlines, password masking

### PR 2 — Compiler AST and parser (2026-04-11, in review)
- `sonda-core/src/compiler/mod.rs` — AST types: `V2ScenarioFile`, `V2Defaults`, `V2Entry`, `AfterClause`, `AfterOp`
- `sonda-core/src/compiler/parse.rs` — parser with 9 validation rules, `detect_version()`
- Single-signal shorthand wrapping (inline + pack)
- Deterministic parse dispatch via `ShapeProbe` (no ambiguous fallback)
- Cross-generator mutual exclusion validation
- `MetricOverride.labels` aligned to `BTreeMap` for determinism
- 45 unit tests + 11 fixture integration tests (5 valid, 6 invalid YAML examples)
- Module named `compiler` (describes function, not version number)

## Active Risks
- Snapshot format stability — must be deterministic and survive refactor
- `deny_unknown_fields` on compiler types prevents forward-compatible parsing (deliberate)

## Process Notes
- All PRs target integration branch (`refactor/unified-scenarios-v2`), not `main`
- Integration branch merges to `main` only after full validation matrix passes (162/162)
- Progress file and validation status updated at end of every PR
- Every PR includes example YAML fixtures for reviewability
- Implementation plans get user approval before launching implementer
- Implementer gets requirements and constraints, not exact code blueprints
