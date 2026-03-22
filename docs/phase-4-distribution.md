# Phase 4 — Distribution & Publishing Implementation Plan

**Goal:** Users can install Sonda via pre-built binaries, `cargo install`, or Docker. The core
library is available on crates.io for programmatic use.

**Prerequisite:** Phase 3 complete — sonda-server works with full lifecycle API, Docker image builds,
multi-arch support, and Helm chart are in place.

**Final exit criteria:** `curl | sh` installs a working binary, `cargo install sonda` works,
`sonda-core` is published on crates.io, and GitHub Releases contain checksummed tarballs for all
supported platforms.

**Design principle — zero friction:** Every installation method should work in a single command.
Users should not need to install Rust, Docker, or any other toolchain to get a working `sonda`
binary.

---

## Slice 4.0 — Release Binaries & Install Script

### Motivation

Users need a way to install Sonda without building from source. This slice creates a CI pipeline
that produces static binaries for four platforms and an install script that detects the user's
platform and downloads the correct binary.

### Input state
- Phase 3 passes all gates.
- `Dockerfile` exists with multi-stage musl build (Slice 3.7).
- `.github/workflows/release.yml` exists with Docker image publishing (Slice 3.8).
- Both `sonda` and `sonda-server` binaries compile for musl targets.

### Specification

**Files to create:**

- `install.sh`:
  - Detect OS (`uname -s` → `linux` or `darwin`) and architecture (`uname -m` → `x86_64` or
    `aarch64`/`arm64`, normalizing `arm64` to `aarch64`).
  - Map to the correct tarball name: `sonda-{version}-{target}.tar.gz` where target is one of:
    - `x86_64-unknown-linux-musl`
    - `aarch64-unknown-linux-musl`
    - `x86_64-apple-darwin`
    - `aarch64-apple-darwin`
  - Download from GitHub Releases: `https://github.com/davidban77/sonda/releases/download/{version}/sonda-{version}-{target}.tar.gz`.
  - When `SONDA_VERSION` env var is set, use that version (e.g., `v0.1.0`). Otherwise, fetch the
    latest release tag via the GitHub API (`/repos/davidban77/sonda/releases/latest`).
  - Download the `SHA256SUMS` file from the same release.
  - Verify the tarball's SHA256 checksum against the checksums file using `sha256sum` (Linux) or
    `shasum -a 256` (macOS).
  - Extract to `$SONDA_INSTALL_DIR` if set, otherwise `/usr/local/bin`. If the target directory
    requires root access, prompt the user or use `sudo`.
  - Print success message with installed version and binary location.
  - Exit with non-zero status on any failure (download, checksum mismatch, extraction).
  - Usage: `curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh`
  - Support `SONDA_VERSION=v0.1.0` env var for version pinning.
  - The script must be POSIX-compliant (`#!/bin/sh`) for maximum portability.

**Files to modify:**

- `.github/workflows/release.yml` — extend the existing workflow:
  - Trigger: on `v*` tag push (in addition to existing Docker image steps).
  - Build matrix with four targets:
    - `x86_64-unknown-linux-musl` (runner: `ubuntu-latest`)
    - `aarch64-unknown-linux-musl` (runner: `ubuntu-latest`, using `cross` or `cargo-zigbuild`)
    - `x86_64-apple-darwin` (runner: `macos-latest`)
    - `aarch64-apple-darwin` (runner: `macos-latest`)
  - For each target:
    - Install the appropriate Rust target via `rustup target add`.
    - Build both `sonda` and `sonda-server` in release mode with `--target`.
    - For Linux musl targets, install `musl-tools` or use `cross`.
    - Create tarball: `sonda-{version}-{target}.tar.gz` containing both `sonda` and `sonda-server`
      binaries.
  - After all matrix builds complete:
    - Collect all tarballs.
    - Generate `SHA256SUMS` file containing checksums for all tarballs.
    - Upload tarballs and `SHA256SUMS` as GitHub Release assets using `softprops/action-gh-release`.
    - The release body should include installation instructions pointing to the install script.

- `README.md` — add an Installation section near the top with the following methods (in order):
  - **Install script** (recommended): `curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh`
  - **GitHub Releases**: manual download link to `https://github.com/davidban77/sonda/releases/latest`
  - **Cargo install**: `cargo install sonda` (note: coming in Slice 4.1)
  - **Docker**: `docker pull ghcr.io/davidban77/sonda:latest` (already documented, consolidate here)
  - **Helm**: `helm install sonda ./helm/sonda` (already documented, consolidate here)

### Output files
| File | Status |
|------|--------|
| `install.sh` | new |
| `.github/workflows/release.yml` | modified |
| `README.md` | modified |

### Test criteria
- `install.sh` correctly detects OS and architecture on Linux x86_64.
- `install.sh` correctly detects OS and architecture on macOS arm64.
- `install.sh` fails with a clear error when run on an unsupported platform.
- `install.sh` fails with a clear error when checksum verification fails.
- `install.sh` respects `SONDA_VERSION` env var.
- `install.sh` respects `SONDA_INSTALL_DIR` env var.
- `install.sh` is valid POSIX shell (`shellcheck install.sh` passes).
- Release workflow produces tarballs for all four targets.
- Tarballs contain both `sonda` and `sonda-server` binaries.
- `SHA256SUMS` file contains correct checksums for all tarballs.
- Release assets are attached to the GitHub Release.

### Review criteria
- `install.sh` is POSIX-compliant (no bashisms).
- `install.sh` handles all error cases (network failure, missing tools, permission denied).
- `install.sh` does not silently overwrite existing binaries without indication.
- Release workflow uses a build matrix (not duplicated steps per target).
- Cross-compilation for `aarch64-unknown-linux-musl` uses a proven approach (`cross` or `cargo-zigbuild`).
- Checksums are generated in a separate job after all builds complete (not per-build).
- No secrets are leaked in workflow logs.

### UAT criteria
- Push a `v0.1.0-rc1` tag → release workflow runs → GitHub Release has four tarballs + checksums.
- Download tarball for current platform → extract → `./sonda --version` prints version.
- Run `install.sh` on a clean Linux VM → `sonda --version` works.
- Run `install.sh` with `SONDA_VERSION=v0.1.0-rc1` → installs that specific version.
- Run `install.sh` with `SONDA_INSTALL_DIR=/tmp/sonda-test` → binaries appear there.
- Tamper with a tarball → `install.sh` fails with checksum error.

---

## Slice 4.1 — Crate Publishing (crates.io)

### Motivation

Rust developers expect to install CLI tools via `cargo install` and use libraries via `Cargo.toml`
dependencies. This slice prepares all three crates for publishing on crates.io and creates an
automated publish workflow.

### Input state
- Slice 4.0 passes all gates.
- All three crates build and pass tests.
- `install.sh` and release binaries are working.

### Specification

**Files to modify:**

1. Root `Cargo.toml` — add workspace-level metadata for shared fields:
   ```toml
   [workspace.package]
   version = "0.1.0"
   edition = "2021"
   rust-version = "1.75"
   authors = ["David Flores <davidflores77@gmail.com>"]
   license = "MIT OR Apache-2.0"
   repository = "https://github.com/davidban77/sonda"
   homepage = "https://github.com/davidban77/sonda"
   description = "Synthetic telemetry generator for testing observability pipelines"
   keywords = ["telemetry", "metrics", "observability", "testing", "synthetic"]
   categories = ["command-line-utilities", "development-tools::testing"]
   ```
   Update the workspace dependency for `sonda-core` to include a version specifier:
   ```toml
   [workspace.dependencies]
   sonda-core = { path = "sonda-core", version = "0.1.0" }
   ```

2. `sonda-core/Cargo.toml` — add crate-specific metadata:
   ```toml
   [package]
   name = "sonda-core"
   version.workspace = true
   edition.workspace = true
   authors.workspace = true
   license.workspace = true
   repository.workspace = true
   homepage.workspace = true
   keywords.workspace = true
   categories = ["development-tools::testing"]
   description = "Core engine for Sonda — synthetic telemetry generation library"
   readme = "../README.md"
   ```

3. `sonda/Cargo.toml` — add crate-specific metadata:
   ```toml
   [package]
   name = "sonda"
   version.workspace = true
   edition.workspace = true
   authors.workspace = true
   license.workspace = true
   repository.workspace = true
   homepage.workspace = true
   keywords.workspace = true
   categories = ["command-line-utilities", "development-tools::testing"]
   description = "CLI for Sonda — synthetic telemetry generator for testing observability pipelines"
   readme = "../README.md"
   ```

4. `sonda-server/Cargo.toml` — add crate-specific metadata:
   ```toml
   [package]
   name = "sonda-server"
   version.workspace = true
   edition.workspace = true
   authors.workspace = true
   license.workspace = true
   repository.workspace = true
   homepage.workspace = true
   keywords.workspace = true
   categories = ["web-programming::http-server", "development-tools::testing"]
   description = "HTTP control plane for Sonda — synthetic telemetry generator"
   readme = "../README.md"
   ```

5. Ensure workspace dependencies use both `path` and `version` for sonda-core:
   ```toml
   # In sonda/Cargo.toml and sonda-server/Cargo.toml (via workspace dependency)
   sonda-core = { path = "../sonda-core", version = "0.1.0" }
   ```
   This allows local development (path) while crates.io resolves via version.

**Files to create:**

- `LICENSE-MIT` — MIT license text with copyright assigned to "David Flores and Sonda contributors".
- `LICENSE-APACHE` — Apache License 2.0 text with copyright assigned to "David Flores and Sonda
  contributors".
- `.github/workflows/publish.yml`:
  - Trigger: `workflow_dispatch` with an optional `dry_run` boolean input (default `true`).
  - Steps:
    1. Checkout repository.
    2. Install stable Rust toolchain.
    3. Run `cargo publish --dry-run -p sonda-core` to verify readiness.
    4. Run `cargo publish --dry-run -p sonda` to verify readiness.
    5. Run `cargo publish --dry-run -p sonda-server` to verify readiness.
    6. If `dry_run` is `false`:
       - `cargo publish -p sonda-core` (must publish first — other crates depend on it).
       - Wait 30 seconds for crates.io index to update.
       - `cargo publish -p sonda`.
       - `cargo publish -p sonda-server`.
    7. Uses `CARGO_REGISTRY_TOKEN` secret for authentication.
  - Publishing order matters: `sonda-core` must be published before `sonda` and `sonda-server`
    because they depend on it. A sleep or retry loop between publishes ensures the index has
    updated.

**Files to modify:**

- `README.md` — update the Installation section (added in Slice 4.0):
  - Add crates.io badges at the top of the README:
    ```markdown
    [![crates.io](https://img.shields.io/crates/v/sonda.svg)](https://crates.io/crates/sonda)
    [![crates.io](https://img.shields.io/crates/v/sonda-core.svg)](https://crates.io/crates/sonda-core)
    ```
  - Update the `cargo install` entry to remove the "coming in 4.1" note:
    ```
    cargo install sonda
    ```
  - Add a "Library usage" subsection showing how to depend on `sonda-core`:
    ```toml
    [dependencies]
    sonda-core = "0.1"
    ```
    With a brief Rust code example showing programmatic use of the core library (e.g., creating a
    generator and encoding a metric).

### Output files
| File | Status |
|------|--------|
| `Cargo.toml` (root) | modified |
| `sonda-core/Cargo.toml` | modified |
| `sonda/Cargo.toml` | modified |
| `sonda-server/Cargo.toml` | modified |
| `LICENSE-MIT` | new |
| `LICENSE-APACHE` | new |
| `.github/workflows/publish.yml` | new |
| `README.md` | modified |

### Test criteria
- `cargo publish --dry-run -p sonda-core` succeeds.
- `cargo publish --dry-run -p sonda` succeeds.
- `cargo publish --dry-run -p sonda-server` succeeds.
- `cargo package -p sonda-core` produces a valid `.crate` file.
- `cargo package -p sonda` produces a valid `.crate` file.
- `LICENSE-MIT` and `LICENSE-APACHE` are included in packaged crates.
- Workspace dependency resolution works with both `path` and `version` specified.
- All existing tests continue to pass (`cargo test --workspace`).
- Publish workflow dry-run completes successfully in CI.

### Review criteria
- License is `MIT OR Apache-2.0` (dual license, standard for Rust ecosystem).
- All three crates have complete metadata (description, repository, homepage, keywords, categories).
- `readme` field points to `../README.md` so crates.io shows the project README.
- Workspace dependency uses both `path` (for local dev) and `version` (for crates.io resolution).
- Publish workflow requires manual trigger — no accidental publishes on push.
- Publish order is enforced: `sonda-core` before dependents.
- `CARGO_REGISTRY_TOKEN` is used as a secret, not hardcoded.
- No path-only dependencies remain (would cause crates.io rejection).
- License files are present and correctly formatted.

### UAT criteria
- `cargo publish --dry-run -p sonda-core` → success, no warnings.
- `cargo publish --dry-run -p sonda` → success, no warnings.
- `cargo publish --dry-run -p sonda-server` → success, no warnings.
- Trigger publish workflow with `dry_run: true` → all three dry-run steps pass.
- After actual publish: `cargo install sonda` on a clean machine → installs and runs.
- After actual publish: add `sonda-core = "0.1"` to a test project → compiles and links.

---

## Dependency Graph

```
Slice 4.0 (release binaries + install script)
  |
Slice 4.1 (crate publishing on crates.io)
```

Slice 4.0 establishes the binary distribution pipeline. Slice 4.1 builds on it by adding Rust
ecosystem distribution. Both slices are independent in code changes but ordered so that binary
distribution (the more common installation path) is validated first.

---

## Post-Phase 4

With Phase 4 complete, Sonda is installable via five methods: install script, GitHub Releases,
`cargo install`, Docker, and Helm. Future distribution channels (not designed here):

- **Homebrew tap** — `brew install davidban77/tap/sonda` via a custom tap repository.
- **APT/RPM packages** — `.deb` and `.rpm` packages for native Linux package managers.
- **Nix flake** — for NixOS and Nix users.
- **Windows binaries** — `x86_64-pc-windows-msvc` target in the release matrix.
- **GitHub Action** — `uses: davidban77/sonda-action@v1` for CI pipeline testing.
- **Pre-built scenario library** — published alongside the tool as downloadable YAML bundles.
