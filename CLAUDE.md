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
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

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
