# Sonda v2 Refactor — Progress

## Current Status
- **Phase:** 1 — v2 AST and parser
- **Branch:** `refactor/unified-scenarios-v2`
- **Integration PR:** #197 (targets `main`, accumulates all v2 work)
- **Next PR:** PR 3 — Defaults resolution and normalization

## Milestone Checklist

| # | Milestone | Status | PR | Date |
|---|-----------|--------|----|------|
| 0 | Scaffolding & test foundation | Done | PR 1 | 2026-04-11 |
| 1 | v2 AST and parser | In Review | PR 2 (#198) | 2026-04-11 |
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
| 2 | v2 AST, parser, and version dispatch | `feat/v2-ast-parser` | integration (#198) | In Review | 2026-04-11 |

## Test Coverage

| Layer | Tests | Scope |
|-------|-------|-------|
| v2 parser unit tests | 45 | AST parsing, validation, shorthand, edge cases |
| Compile snapshot golden files | 12 | v1 parity baseline (6 fixtures x raw+prepared) |
| **New in refactor** | **57** | |
| Workspace total | 2,532 | All existing + new |

## Validation Matrix Status

See [v2-validation-status.md](v2-validation-status.md) for the full 162-row checklist.

**Every row is a mandatory merge blocker. No exceptions.**

**Summary:** 4 of 162 rows addressed so far (all in section 11 — new v2 features).

| Section | Rows | Addressed | Notes |
|---------|------|-----------|-------|
| 1. Signal types | 6 | 0 | Parity — needs runtime wiring (PR 6) |
| 2. Metric generators | 10 | 0 | Parity — needs runtime wiring (PR 6) |
| 3. Operational aliases | 7 | 0 | Parity — needs runtime wiring (PR 6) |
| 4. Histogram & summary | 8 | 0 | Parity — needs runtime wiring (PR 6) |
| 5. Encoders | 8 | 0 | Parity — needs runtime wiring (PR 6) |
| 6. Sinks | 12 | 0 | Parity — needs runtime wiring (PR 6) |
| 7. Scheduling & windows | 11 | 0 | Parity — needs runtime wiring (PR 6) |
| 8. Multi-scenario features | 6 | 0 | Parity — needs runtime wiring (PR 6) |
| 9. Pack features | 15 | 0 | Pack expansion (PR 4) + runtime (PR 6) |
| 10. Story features | 15 | 0 | `after` compiler (PR 5) + runtime (PR 6) |
| 11. New v2 features | 18 | 4 | 11.1, 11.4, 11.5, 11.10 done in PR 2 |
| 12. CLI commands | 22 | 0 | CLI unification (PR 7) |
| 13. Server API | 9 | 0 | Server (PR 9) |
| 14. Status output & UX | 9 | 0 | CLI (PR 7) + runtime (PR 6) |
| 15. Deployment | 7 | 0 | Final cleanup (PR 9) |
| **16. Scenario parity bridge** | **12** | **0** | **v1→v2 compile + runtime parity for all 11 built-ins + 1 story** |
| **17. Pack parity bridge** | **3** | **0** | **v1→v2 compile + runtime parity for all 3 built-in packs** |

## Completed Work

### PR 1 — Compile snapshot harness (2026-04-11)
- `sonda-core/src/config/snapshot.rs` — deterministic JSON snapshot serializer
- `Serialize` derives added to all config types (feature-gated)
- 6 semantic YAML fixtures + 12 golden-file integration tests
- `KafkaSaslConfig.password` skip_serializing for security
- Reviewer findings fixed: feature gates, trailing newlines, password masking

### PR 2 — v2 AST and parser (2026-04-11, in review)
- `sonda-core/src/v2/mod.rs` — AST types: `V2ScenarioFile`, `V2Defaults`, `V2Entry`, `AfterClause`, `AfterOp`
- `sonda-core/src/v2/parse.rs` — parser with 9 validation rules, `detect_version()`
- Single-signal shorthand wrapping (inline + pack)
- Deterministic parse dispatch via `ShapeProbe` (no ambiguous fallback)
- `MetricOverride.labels` aligned to `BTreeMap` for consistency
- 45 unit tests covering valid, invalid, and edge cases

## Active Risks
- Snapshot format stability — must be deterministic and survive refactor
- `deny_unknown_fields` on v2 types prevents forward-compatible parsing (deliberate)

## Process Notes
- All PRs target integration branch (`refactor/unified-scenarios-v2`), not `main`
- Integration branch merges to `main` only after full validation matrix passes
- Progress file and validation status updated at end of every PR
