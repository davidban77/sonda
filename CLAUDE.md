# Sonda — Synthetic Telemetry Generator

Sonda generates realistic synthetic observability signals — metrics, logs, traces, and flows — for
testing pipelines, validating ingest paths, and simulating failure scenarios.

The **core library is the product**. The CLI and HTTP server are delivery mechanisms built on top of it.

## Workspace Structure

This is a Cargo workspace with three crates:

- **sonda-core** — library crate: all domain logic (generators, encoders, sinks, schedules).
- **sonda** — binary crate: CLI (thin layer over core, clap + YAML config).
- **sonda-server** — binary crate: HTTP API control plane (axum, post-MVP).
- **sonda-wasm** — cdylib crate: WebAssembly facade over the pure engine (core without the
  `runtime` feature) that powers the docs-site playground. Rebuild the bundle with `task site:wasm`.

No business logic lives outside sonda-core. If the CLI or server needs new behavior, it goes in core.

## The builtin pack catalog

`packs/` at the repo root holds the curated metric packs. It is the single copy: `sonda-core`
embeds those files with `include_str!` (`src/catalog/builtin.rs`) so the static binary needs no
runtime file discovery, `docker-compose.yml` mounts the same directory at `/packs`, and the
schema corpus validates it. The directory is flat — packs group by their `category:` field, not
by subdirectory. Adding one means adding its `include_str!` entry *and* moving `PACK_COUNT`;
the count gate fails if you do only one.

A pack must be addressable: within it a metric `name` is either unique, or *every* spec sharing
it declares a unique `id:` (`node_exporter_cpu` uses the CPU modes). Names may not contain `.`,
which separates `name` from `id` in a selector. An unaddressable pack does not load — that is
what lets `overrides:` and `after.ref` refuse a bare ambiguous name instead of guessing.

A pack may `extends:` another. Resolution is a pre-pass — `packs::extend::materialize` folds the
chain into one `MetricPackDef` indistinguishable from a hand-written one — so expansion, label
composition and sub-signal registration never learn that packs can extend. `metrics:` is purely
additive there; `deviations:` is the only way to change a base metric, and each gives exactly
one of `replace:` or `not_supported:`. Addressability is re-checked after every fold, because
two links can each be fine and their merge not be.

Each crate has its own `CLAUDE.md` with module layout, patterns, and conventions.

## Agent Workflow

Agent definitions and workflow rules live in the user's personal `~/.claude/` directory
(agents, rules, skills). The orchestration rule defines the full pipeline.

**Quick reference:** all code changes follow: `rust-implementer` → `doc` + `reviewer` + `UAT`,
on a feature branch. The implementer writes code, tests, and doc updates.

## Coding Conventions

- **Error handling**: `thiserror` in sonda-core, `anyhow` in CLI and server. Never `unwrap()` in
  library code. `expect()` only with a clear message for truly unrecoverable cases.
- **Allocations**: minimize per-event allocations. Pre-build label prefixes, reuse buffers, write
  into caller-provided `Vec<u8>`.
- **Testing**: every generator, encoder, and schedule function gets a unit test. Deterministic seeds
  for RNG-based generators. Tests in `#[cfg(test)] mod tests` within the same file.
- **Naming**: snake_case for modules/functions, PascalCase for types/traits. No abbreviations
  except widely understood ones (`tcp`, `udp`, `rng`).
- **Formatting**: `cargo fmt` before every commit. `cargo clippy -- -D warnings` must pass.
- **Docs**: public items in sonda-core must have `///` doc comments.

## Quality Gates

Every commit must pass:

```bash
cargo build --workspace
cargo test --workspace --no-fail-fast
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

`--no-fail-fast` is not optional. `cargo test --workspace` stops at the first failing *binary*,
so a failure in `sonda-core` means `sonda-server` and the CLI integration tests never run at all.
A run that aborted early has been reported as "everything passes" more than once. Name the
failing tests individually; a count is not a result.

## Writing a Check

Most defects this repo has shipped were in its *checks*, not its code. Before adding one, answer
these four. Each is here because it already went wrong.

1. **Can it pass vacuously?** An empty corpus, a renamed directory, a generator that writes
   nothing, a parse that returns nothing — all produce "no differences found". Assert the input
   exists *before* checking it. `cli_subcommand_parity.rs` does this twice; the SVG gate in
   `ci.yml` deletes its corpus first so silence surfaces as deletions rather than success.

2. **Does the red-verification depend on an accident?** Confirm the mutation is present in the
   file before trusting its result — a sabotage that silently fails to apply is indistinguishable
   from a passing gate. Then ask *why* it went red: a check once passed only because the corpus
   happened to contain the needle exactly once, on the line the mutation deleted. The same defect
   elsewhere was invisible.

3. **Is the exemption something a human types?** An opt-out that appears in a diff
   (`<!-- verbs:historical -->`) is honest. Silently skipping anything the check does not
   recognise is not — it reads as coverage.

4. **Does the comment describe what the code does?** A doc comment claiming a majority rule sat
   above `!named.is_empty()`; a comment saying "two consecutive blanks" sat above a counter that
   never reset. If a comment states a rule, the rule belongs in one function both the prose and
   the callers point at.

Two further rules of thumb, earned the expensive way:

- **Prefer exact checks to heuristic ones.** Comparing a value to its source of truth converges.
  Pattern-matching over prose or layout has an unbounded tail of shapes: every round finds
  another, and each fix is genuinely correct. If a check cannot be made exact, ship it with its
  limitations written down rather than iterating toward completeness.
- **A test that copies the code it tests will diverge on exactly the line the bug is on.** Drive
  the real thing — `scripts/tests/extract_run_bodies.py` exists so the action's contract tests
  execute `action.yml`'s own shell rather than a transcription of it.

## How to Build

```bash
cargo build --workspace                                              # debug build
cargo build --release --target x86_64-unknown-linux-musl -p sonda    # static musl binary
```

## Architecture & Design

Full design rationale is in `docs/architecture.md`. Key decisions:

- Cargo workspace for parallel compilation and clean dep isolation.
- Trait objects (`Box<dyn Trait>`) for generators, encoders, sinks — extensible without dispatch changes.
- YAML for all scenario config; CLI flags and `SONDA_*` env vars override.
- Tokio-first: scenarios run as `tokio::task::spawn` tasks on a shared multi-thread runtime.
- Static binary (musl). Pure-Rust deps only (rustls, not openssl).

## Extension Points

To add a generator, encoder, or sink: use the matching skill in `.claude/skills/` (add-generator,
add-encoder, add-sink). Each crate's `CLAUDE.md` also has step-by-step guidance.

## The published GitHub Action

`action.yml` at the repo root is a composite action wrapping `sonda test` for CI. It resolves the
ref it was pinned at to a concrete release (`scripts/resolve_release.py`), installs that release
with the repo's own `install.sh` — reused, not reimplemented, so the checksum verification has one
definition — and runs `sonda test`.

- **Inputs never reach a shell.** Every input is passed through `env:`, never interpolated into a
  `run:` body, and argv is built as a quoted array. `scripts/tests/action_argv_test.sh` drives that
  construction with hostile values and fails if `action.yml` stops containing it.
- **`release.yml` moves a `vN` tag** onto each release so `uses: davidban77/sonda@v1` tracks the
  newest `1.x`. Guarded so a pre-release or a `2.0.0` never moves `v1`.
- **Two workflows:** `action-contract` in `ci.yml` (fast, always) and `action-selftest.yml`
  (live stack, path-filtered) which runs `uses: ./` against real vmalert + Alertmanager.

## Phase Plans

Completed development phases are documented in `docs/phase-{0..9}-*.md` for historical reference.
