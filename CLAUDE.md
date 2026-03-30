# Sonda — Synthetic Telemetry Generator

Sonda generates realistic synthetic observability signals — metrics, logs, traces, and flows — for
testing pipelines, validating ingest paths, and simulating failure scenarios.

The **core library is the product**. The CLI and HTTP server are delivery mechanisms built on top of it.

## Workspace Structure

This is a Cargo workspace with three crates:

- **sonda-core** — library crate: all domain logic (generators, encoders, sinks, schedules).
- **sonda** — binary crate: CLI (thin layer over core, clap + YAML config).
- **sonda-server** — binary crate: HTTP API control plane (axum, post-MVP).

No business logic lives outside sonda-core. If the CLI or server needs new behavior, it goes in core.

Each crate has its own `CLAUDE.md` with module layout, patterns, and conventions.

## Agent Workflow

See `.claude/rules/agent-workflow.md` for the full agent pipeline, feature branch workflow, and
worktree cleanup rules.

**Quick reference:** all code changes follow: implementer → reviewer + UAT, on a feature
branch. The implementer writes both code and tests. Never merge worktree branches into `main`.

For parallel sessions, the human creates session worktrees under `.claude/sessions/` and launches
Claude Code from each one. See the rules file for details.

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
- Sync-first (std::thread + mpsc). Tokio only in sonda-server.
- Static binary (musl). Pure-Rust deps only (rustls, not openssl).

## Extension Points

To add a generator, encoder, or sink: use the matching skill in `.claude/skills/` (add-generator,
add-encoder, add-sink). Each crate's `CLAUDE.md` also has step-by-step guidance.

## Phase Plans

Development phases are documented in `docs/phase-{0..7}-*.md`. Read the relevant plan when working
on a slice.
