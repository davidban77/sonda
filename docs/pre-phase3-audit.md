# Pre-Phase 3 Audit — Action Plan

**Date:** 2026-03-22
**Status:** In progress

Full project health check before starting Phase 3 (sonda-server REST API).

---

## Batch 1 — Blockers & High Priority

Must be resolved before Phase 3 begins.

### 1. Create LICENSE file
- **Issue:** `Cargo.toml` declares `license = "MIT"`, README links to `LICENSE`, but no file exists.
- **Action:** Create `LICENSE` with the standard MIT license text.
- **Files:** `LICENSE` (new)

### 2. Commit Cargo.lock
- **Issue:** `.gitignore` excludes `Cargo.lock`, but this is a binary crate project. Builds aren't reproducible, and CI cache keys based on `Cargo.lock` always miss.
- **Action:** Remove `Cargo.lock` from `.gitignore`, run `cargo generate-lockfile`, and `git add Cargo.lock`.
- **Files:** `.gitignore`, `Cargo.lock`

### 3. Feature-gate Kafka tests
- **Issue:** Tests in `sonda-core/src/sink/mod.rs` reference `SinkConfig::Kafka` without `#[cfg(feature = "kafka")]`. Running `cargo test -p sonda-core --no-default-features` fails with 5 compile errors.
- **Action:** Wrap all Kafka-specific tests in `#[cfg(feature = "kafka")]`.
- **Files:** `sonda-core/src/sink/mod.rs`

### 4. Add burst support to log runner
- **Issue:** `LogScenarioConfig` accepts `bursts:` in YAML and the CLI accepts `--burst-*` flags for logs, but `run_logs_with_sink()` ignores the field entirely. Metrics runner supports bursts; log runner does not.
- **Action:** Port the burst window logic from `schedule/runner.rs` into `schedule/log_runner.rs`. Check `is_in_burst()` each tick and adjust `effective_interval` like the metrics runner does.
- **Files:** `sonda-core/src/schedule/log_runner.rs`

### 5. Fix README `sonda logs` CLI reference
- **Issue:** README documents 4 flags that don't exist: `--message`, `--severity-weights`, `--seed`, `--replay-file`.
- **Action:** Either implement the flags in `sonda/src/cli.rs` (LogsArgs) or remove them from README. Recommendation: implement them since they map to useful LogTemplateGenerator config fields.
- **Files:** `README.md`, potentially `sonda/src/cli.rs`, `sonda/src/config.rs`

### 6. Fix README `--encoder` accepted values
- **Issue:** README says `Accepted values: prometheus_text` for `sonda metrics`, but the CLI accepts `prometheus_text`, `influx_lp`, and `json_lines`.
- **Action:** Update the CLI reference section to list all three encoders.
- **Files:** `README.md`

### 7. Add `--output` flag to README metrics reference
- **Issue:** The `--output <PATH>` flag exists in the CLI but is not documented in README's `sonda metrics` section.
- **Action:** Add `--output` to the metrics CLI flags table.
- **Files:** `README.md`

### 8. Add Loki to README services table
- **Issue:** Docker-compose includes Loki (port 3100) and Taskfile mentions it, but the README services table omits it.
- **Action:** Add Loki row to the services table.
- **Files:** `README.md`

### 9. Fix CLAUDE.md generator module layout
- **Issue:** Lists `counter.rs`, `gauge.rs`, `microburst.rs` that don't exist on disk.
- **Action:** Remove non-existent files from the layout. Optionally add a "Planned" section listing them.
- **Files:** `sonda-core/CLAUDE.md`

### 10. Remove unused `rand` dependency
- **Issue:** `rand = { version = "0.8", features = ["small_rng"] }` is declared in `sonda-core/Cargo.toml` but never imported anywhere. The `UniformRandom` generator uses SplitMix64 hash directly.
- **Action:** Remove `rand` from `[dependencies]` in `sonda-core/Cargo.toml`.
- **Files:** `sonda-core/Cargo.toml`

---

## Batch 2 — CI, Repo Quality, Polish

Improves project maturity but not blocking.

### 11. Enhance CI pipeline
- **Issue:** CI only runs build/test/clippy/fmt. Missing: cargo audit, MSRV check, musl cross-compile, Kafka feature test, concurrency limits.
- **Action:** Update `.github/workflows/ci.yml`:
  - Add `cargo audit` step (or `actions-rs/audit-check`)
  - Add MSRV job (test against `rust-version` from Cargo.toml)
  - Add musl cross-compile step: `cargo build --release --target x86_64-unknown-linux-musl -p sonda`
  - Add `cargo test -p sonda-core --no-default-features` to verify without Kafka
  - Add `concurrency: { group: ${{ github.ref }}, cancel-in-progress: true }`
- **Files:** `.github/workflows/ci.yml`

### 12. Add repo quality files
- **Issue:** Missing `CONTRIBUTING.md`, `CHANGELOG.md`, `.editorconfig`.
- **Action:** Create each with appropriate content.
- **Files:** `CONTRIBUTING.md`, `CHANGELOG.md`, `.editorconfig` (all new)

### 13. Add MSRV to Cargo.toml
- **Issue:** No `rust-version` field. Users don't know the minimum Rust version.
- **Action:** Determine MSRV (test with older versions), add `rust-version = "1.XX"` to `[workspace.package]`.
- **Files:** `Cargo.toml`

### 14. Expand Taskfile
- **Issue:** Missing common tasks: `clean`, `audit`, `docs`, `musl`, `fmt`.
- **Action:** Add:
  - `clean` — `cargo clean`
  - `audit` — `cargo audit`
  - `docs` — `cargo doc --workspace --no-deps --open`
  - `musl` — `cargo build --release --target x86_64-unknown-linux-musl -p sonda`
  - `fmt` — `cargo fmt --all` (auto-fix, vs `lint` which checks)
- **Files:** `Taskfile.yml`

### 15. Add doc comments to Constant generator
- **Issue:** `Constant` struct and `Constant::new()` lack `///` doc comments. Only public type in sonda-core without them.
- **Action:** Add `///` doc comments.
- **Files:** `sonda-core/src/generator/constant.rs`

### 16. Consider SondaError::Sink variant improvement
- **Issue:** `#[from] std::io::Error` auto-converts ANY io::Error to SondaError::Sink, even non-sink errors. Several sinks construct artificial `io::Error` instances just to get the `#[from]` conversion.
- **Action:** Consider changing to `Sink(String)` matching other variants, or adding context. Low priority — current approach works but is imprecise.
- **Files:** `sonda-core/src/lib.rs`, all sink files

### 17. Document remaining example files
- **Issue:** `examples/loki-json-lines.yaml` and `examples/kafka-json-logs.yaml` exist but aren't referenced in README.
- **Action:** Add entries in the README "Example Scenarios" section.
- **Files:** `README.md`

### 18. Extract shared timestamp formatter
- **Issue:** `format_rfc3339_millis` (json.rs) and `format_syslog_timestamp` (syslog.rs) are identical 20-line functions.
- **Action:** Extract to a shared utility in `encoder/mod.rs` or a new `encoder/time.rs`.
- **Files:** `sonda-core/src/encoder/json.rs`, `sonda-core/src/encoder/syslog.rs`, new shared location

### 19. Per-event clone optimization (deferred)
- **Issue:** `name.clone()` and `labels.clone()` on every tick in the metrics runner. Code comments acknowledge this.
- **Action:** Deferred. Profile first, then consider changing `MetricEvent` to borrow `&str` + `&Labels` if this becomes a bottleneck. Not blocking for Phase 3.
- **Files:** `sonda-core/src/schedule/runner.rs`, `sonda-core/src/model/metric.rs`

### 20. Clean orphaned worktrees
- **Issue:** `.claude/worktrees/` contains ~24 orphaned agent worktrees consuming ~13GB.
- **Action:** Run `rm -rf .claude/worktrees/agent-*` to reclaim disk. These are gitignored and ephemeral.
- **Files:** `.claude/worktrees/`

---

## Verification

After completing both batches, verify:

```bash
# Full quality gate
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# Kafka feature gate
cargo test -p sonda-core --no-default-features

# Burst support for logs
sonda logs --mode template --rate 100 --duration 5s \
  --burst-every 2s --burst-for 500ms --burst-multiplier 5 | wc -l
# Should produce more than 500 lines (burst effect visible)

# README accuracy
sonda metrics --help   # compare against README
sonda logs --help      # compare against README
sonda run --help       # compare against README
```
