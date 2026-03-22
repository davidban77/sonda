# Contributing to Sonda

## Building

```bash
cargo build --workspace
```

For a static musl binary (requires the musl target to be installed):

```bash
cargo build --release --target x86_64-unknown-linux-musl -p sonda
```

## Testing

```bash
cargo test --workspace
```

## Linting and Formatting

Both must pass before committing:

```bash
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

To apply formatting automatically:

```bash
cargo fmt --all
```

## Commit Message Format

This project uses a conventional commit style:

```
<type>(<scope>): <short description>
```

Types:

- `feat` — new capability or behavior
- `fix` — bug fix
- `test` — adding or updating tests
- `docs` — documentation only
- `chore` — tooling, config, or housekeeping

Scope examples: `slice-0.1`, `slice-2.5`, `audit-12`, `core`, `cli`.

The first line must be 72 characters or fewer. Use the body for context
when the change is non-obvious.

## Project Structure

The project is a Cargo workspace with three crates:

- `sonda-core` — library crate with all domain logic (generators, encoders, sinks)
- `sonda` — CLI binary (thin layer over core)
- `sonda-server` — HTTP API server (post-MVP)

All business logic belongs in `sonda-core`. The CLI and server are delivery
mechanisms only.

## Error Handling

- Use `thiserror` in `sonda-core` for typed library errors.
- Use `anyhow` in `sonda` and `sonda-server` for application-level errors.
- Never call `unwrap()` in library code.

## Adding Extension Points

Consult the skill guides in `.claude/skills/` before adding a new generator,
encoder, or sink — they document the exact steps and quality checklist for
each extension type.
