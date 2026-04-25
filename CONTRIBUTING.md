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

For the Docker Compose-based end-to-end harness (real backends — VictoriaMetrics, Loki,
Kafka), see [`docs/e2e-tests.md`](docs/e2e-tests.md).

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

## Pull Request Process

All changes to `main` go through pull requests. Here is the expected workflow:

1. **Create a feature branch** off `main`:

   ```bash
   git checkout main && git pull
   git checkout -b feat/my-new-feature
   ```

   Use a descriptive branch name prefixed with the change type: `feat/`, `fix/`, `docs/`, etc.

2. **Open a pull request** against `main`. The PR template will pre-fill sections for Summary,
   Changes, Test plan, and a Checklist. Fill in all sections.

3. **Use a conventional commit as the PR title.** Since PRs are squash-merged, the PR title
   becomes the commit message on `main`. Examples:

   ```
   feat(core): add histogram value generator
   fix: resolve panic in gap scheduler
   docs: update CLI reference for new flag
   ```

4. **Wait for CI.** The following checks must pass:
   - Build, test, clippy, and fmt (from `ci.yml`).
   - PR title validation (from `commitlint.yml`).

5. **Get a review.** At least one approving review is required.

6. **Squash merge.** Use the "Squash and merge" option in GitHub. The PR title is used as the
   commit message.

For the full release process (how commits become versions and releases), see
[docs/release-workflow.md](docs/release-workflow.md).

## Dependency Updates

Dependencies are kept current by [Dependabot](https://docs.github.com/en/code-security/dependabot).
It opens PRs weekly for Cargo dependencies and GitHub Actions version updates. If CI passes on a
dependabot PR, review the dependency changelog and merge. See
[docs/release-workflow.md](docs/release-workflow.md) for details on handling dependency updates.

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
