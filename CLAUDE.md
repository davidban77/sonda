# Sonda — Synthetic Telemetry Generator

## What is Sonda?

Sonda generates realistic synthetic observability signals — metrics, logs, traces, and flows — for
testing pipelines, validating ingest paths, and simulating failure scenarios. It models the failure
patterns that actually break real systems: gaps, micro-bursts, cardinality spikes, and shaped value
sequences.

The **core library is the product**. The CLI and HTTP server are delivery mechanisms built on top of it.

## Workspace Structure

This is a Cargo workspace with three crates:

```
sonda/                       ← workspace root (you are here)
├── sonda-core/              ← library crate: the engine (all domain logic)
├── sonda/                   ← binary crate: the CLI (thin layer over core)
├── sonda-server/            ← binary crate: HTTP API control plane (post-MVP)
├── docs/                    ← architecture doc, phase plans
│   ├── architecture.md
│   ├── phase-0-mvp.md
│   ├── phase-1-encoders-sinks.md
│   ├── phase-2-logs-concurrency.md
│   ├── phase-3-server.md
│   └── phase-4-distribution.md
└── .claude/
    ├── agents/              ← subagent definitions (see Agent Workflow below)
    │   ├── implementer.md   ← writes production code in isolated worktree
    │   ├── tester.md        ← writes and runs tests
    │   ├── reviewer.md      ← audits code (read-only)
    │   └── uat.md           ← validates from a real user's perspective
    └── skills/              ← reusable workflow patterns
        ├── add-generator/   ← how to add a new ValueGenerator
        ├── add-encoder/     ← how to add a new Encoder
        └── add-sink/        ← how to add a new Sink
```

**sonda-core** owns: telemetry models, schedules, value generators, encoders, sinks.
**sonda** (CLI) owns: arg parsing (clap), config loading (YAML + env), invoking core.
**sonda-server** owns: REST API (axum), scenario lifecycle, stats endpoints.

No business logic lives outside sonda-core. If the CLI or server needs new behavior, it goes in core.

---

## Agent Workflow

This project is developed by a team of Claude Code agents, each with a specific role. Development
proceeds slice-by-slice, with a human approval gate between slices.

### Roles

| Role | Subagent | Model | Responsibility |
|------|----------|-------|---------------|
| **Implementer** | `@implementer` | sonnet | Reads the slice spec, writes production code in an isolated worktree. Does not write tests. |
| **Tester** | `@tester` | sonnet | Reads the slice spec and implemented code, writes unit + integration tests, runs them. |
| **Reviewer** | `@reviewer` | opus | Audits code against architecture doc and coding conventions. Read-only — reports issues. |
| **UAT** | `@uat` | opus | Builds the project, runs the binary as a real user would, validates observable behavior end-to-end. |

### Workflow per Slice

Each slice follows this sequence. The human orchestrator spawns each subagent with the slice ID:

```
1. @implementer 0.2   → reads slice spec, writes code in worktree, commits
2. @tester 0.2        → reads spec + code, writes tests, runs them, commits
3. @reviewer 0.2      → audits everything, reports PASS/FAIL/PASS WITH NOTES
4. @uat 0.2           → builds binary, runs user scenarios, validates output
5. Human reviews results and approves → move to next slice
```

If any role reports a BLOCKER, the implementer re-runs to fix it before retrying.

### Subagent Details

Subagent definitions live in `.claude/agents/`. Each has YAML frontmatter controlling:

- **tools**: which tools the agent can use (e.g., reviewer has no Write/Edit)
- **model**: sonnet for implementation speed, opus for deep analysis
- **permissionMode**: `acceptEdits` for code writers, `plan` (read-only) for reviewer
- **isolation**: `worktree` for implementer (isolated git worktree)

The slice ID is passed via `$ARGUMENTS` — e.g., `@implementer 0.2` sets `$ARGUMENTS=0.2`.

### Rules for All Agents

- **Read the slice spec first.** Every agent starts by reading the current slice from the phase plan
  in `docs/`. The slice ID is passed as `$ARGUMENTS` (e.g., `@implementer 0.2`).
- **Read architecture.md.** Every agent must check `docs/architecture.md` for design decisions before
  writing or reviewing code.
- **Read the crate CLAUDE.md.** Before modifying a crate, read its `CLAUDE.md` for crate-specific
  guidance.
- **One slice at a time.** Never work ahead. Each slice builds on the verified output of the previous
  slice.
- **Commit after each role.** The implementer commits code, the tester commits tests. Reviewer and UAT
  do not commit — they report.
- **Exit gates are hard.** A slice is not done until all four roles have passed. Failures get fixed
  by re-running the failing role.

### Skills

Reusable workflow patterns live in `.claude/skills/`. Agents reference these when performing common
tasks:

- **add-generator** — step-by-step guide to adding a new `ValueGenerator` implementation
- **add-encoder** — step-by-step guide to adding a new `Encoder` implementation
- **add-sink** — step-by-step guide to adding a new `Sink` implementation

---

## Key Design Decisions

1. **Cargo workspace over single crate** — parallel compilation, clean dep isolation, independent
   publishability of sonda-core. See `docs/architecture.md` Section 4 for rationale.

2. **Trait objects for extension points** — generators, encoders, and sinks are `Box<dyn Trait>`.
   Dynamic dispatch overhead is negligible relative to I/O cost. This keeps core extensible without
   modifying dispatch logic.

3. **YAML for scenario config** — all runtime behavior (signal shape, rate, labels, encoder, sink) is
   defined in YAML files. CLI flags and `SONDA_*` env vars override any value. No behavior requires a
   code change.

4. **Sync-first, async later** — the MVP is synchronous. Concurrency comes via std::thread + mpsc.
   Tokio is introduced only in sonda-server when HTTP I/O demands it. sonda-core stays async-agnostic.

5. **Static binary (musl)** — primary target is `x86_64-unknown-linux-musl`. No C dependencies in
   sonda-core. Pure-Rust alternatives only (rustls, not openssl; miniz_oxide, not libz).

## Core Extension Points

All three follow the same pattern: a trait in sonda-core, a factory that returns `Box<dyn Trait>`,
and config-driven selection via YAML.

- **Generators** — `pub trait ValueGenerator: Send + Sync { fn value(&self, tick: u64) -> f64; }`
- **Encoders** — `pub trait Encoder: Send + Sync { fn encode_metric(&self, ...) -> Result<()>; }`
- **Sinks** — `pub trait Sink: Send + Sync { fn write(&mut self, data: &[u8]) -> Result<()>; }`

To add a new implementation: create the struct in the appropriate module, implement the trait, register
it in the factory function, and add a variant to the YAML config enum. Each crate's CLAUDE.md has
step-by-step guidance.

## Coding Conventions

- **Error handling**: use `thiserror` for library errors in sonda-core, `anyhow` in the CLI and server.
  Never `unwrap()` in library code. `expect()` only with a clear message for truly unrecoverable cases.
- **Allocations**: minimize per-event allocations. Pre-build label prefixes, reuse buffers, write into
  caller-provided `Vec<u8>`. See `docs/architecture.md` Section 5.4 on encoder pre-building.
- **Testing**: every generator, encoder, and schedule function gets a unit test. Use deterministic seeds
  for any RNG-based generator. Tests live in `#[cfg(test)] mod tests` within the same file.
- **Naming**: snake_case for modules and functions, PascalCase for types and traits. No abbreviations
  except widely understood ones (e.g., `tcp`, `udp`, `rng`).
- **Formatting**: `cargo fmt` before every commit. `cargo clippy -- -D warnings` must pass.
- **Documentation**: public items in sonda-core must have `///` doc comments. Internal items should have
  comments when the "why" is not obvious from the code.

## Quality Gates (enforced by all agents)

Every commit must pass:

```bash
cargo build --workspace                        # compiles
cargo test --workspace                         # all tests pass
cargo clippy --workspace -- -D warnings        # no lint warnings
cargo fmt --all -- --check                     # formatting clean
```

The UAT agent additionally runs the binary and validates real output.

## How to Build and Test

```bash
# build everything
cargo build --workspace

# run tests
cargo test --workspace

# build static musl binary (requires musl target installed)
cargo build --release --target x86_64-unknown-linux-musl -p sonda

# run clippy
cargo clippy --workspace -- -D warnings

# format check
cargo fmt --all -- --check
```

## Phase Overview

Development is split into five phases. Each has a dedicated plan doc in `docs/`:

- **Phase 0 — MVP**: workspace skeleton, sonda-core engine, Prometheus encoder, stdout sink, scheduler
  with gaps, value generators, CLI, tests, static binary.
- **Phase 1 — Encoders & Sinks**: Influx LP, JSON Lines, remote-write, file sink, TCP/UDP sink, Kafka sink.
- **Phase 2 — Logs, Bursts & Concurrency**: log events, burst windows, Loki sink, multi-scenario threading.
- **Phase 3 — sonda-server**: axum REST API, scenario lifecycle, stats endpoints.
- **Phase 4 — Distribution**: release binaries, install script, crate publishing on crates.io.

## Reference Documents

- `docs/architecture.md` — full architecture design document
- `docs/phase-0-mvp.md` — Phase 0 MVP implementation plan
- `docs/phase-1-encoders-sinks.md` — Phase 1 plan
- `docs/phase-2-logs-concurrency.md` — Phase 2 plan
- `docs/phase-3-server.md` — Phase 3 plan
- `docs/phase-4-distribution.md` — Phase 4 plan
