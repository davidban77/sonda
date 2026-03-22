# Release Workflow

This document is the single source of truth for how development, releases, and dependency management
work in the Sonda project. It covers the full lifecycle from opening a PR to publishing a release.

---

## Development Flow

All changes follow this sequence:

1. **Branch off `main`.**
   Create a short-lived feature branch with a descriptive name:

   ```bash
   git checkout main && git pull
   git checkout -b feat/add-histogram-generator
   ```

   Branch naming convention: `<type>/<short-description>` (e.g., `feat/kafka-sink`,
   `fix/gap-scheduler-panic`, `docs/update-readme`).

2. **Write code using conventional commits.**
   Each commit message follows the [Conventional Commits](#conventional-commits) format. This is
   enforced by CI on the PR title (since PRs are squash-merged, the PR title becomes the final
   commit message on `main`).

3. **Open a pull request.**
   Push your branch and open a PR against `main`. The PR template (`.github/pull_request_template.md`)
   will pre-fill sections for Summary, Changes, Test plan, and a Checklist.

   The PR title must be a valid conventional commit message:

   ```
   feat(core): add histogram value generator
   fix: resolve panic in gap scheduler when duration is zero
   docs: update CLI reference for new --format flag
   ```

4. **CI validates the PR.**
   The following checks run automatically:
   - **CI** (`ci.yml`) -- build, test, clippy, fmt, cargo audit, Docker build verification.
   - **Commitlint** (`commitlint.yml`) -- validates the PR title is a conventional commit.
   - **CODEOWNERS** -- requests review from `@davidban77` for all changes.

5. **Review and approve.**
   At least one approving review is required (enforced by branch protection). The reviewer checks
   code quality, test coverage, and documentation.

6. **Squash merge.**
   All PRs are squash-merged into `main`. The PR title becomes the commit message. This keeps
   `main` linear and ensures every commit on `main` is a valid conventional commit.

---

## Conventional Commits

Sonda uses [Conventional Commits](https://www.conventionalcommits.org/) to automate versioning and
changelog generation. The PR title (which becomes the squash-merge commit message) determines the
version bump.

### Commit format

```
<type>(<optional scope>): <description>

[optional body]

[optional footer(s)]
```

### Type-to-version-bump mapping

| Type | Version bump | Example |
|------|-------------|---------|
| `fix` | Patch (0.1.0 -> 0.1.1) | `fix: resolve encoder panic on empty labels` |
| `feat` | Minor (0.1.0 -> 0.2.0) | `feat: add histogram value generator` |
| `feat!` or `BREAKING CHANGE` in footer | Major (0.1.0 -> 1.0.0) | `feat!: redesign encoder trait` |

### Types that appear in the changelog but do not bump the version

| Type | Changelog section |
|------|------------------|
| `docs` | Documentation |
| `chore` | Miscellaneous |
| `ci` | Miscellaneous |
| `test` | Miscellaneous |
| `refactor` | Miscellaneous |
| `perf` | Miscellaneous |
| `build` | Miscellaneous |

### Breaking changes

A breaking change triggers a major version bump regardless of the commit type. There are two ways
to signal a breaking change:

1. Add `!` after the type: `feat!: redesign encoder trait`
2. Add a `BREAKING CHANGE` footer:

   ```
   feat: redesign encoder trait

   The Encoder trait now requires a `flush` method.

   BREAKING CHANGE: All Encoder implementations must add a `flush()` method.
   ```

### Accepted types

The commitlint CI check accepts these types: `feat`, `fix`, `test`, `docs`, `chore`, `refactor`,
`ci`, `perf`, `build`. Scope is optional.

---

## Release Process

Releases are automated by [release-please](https://github.com/googleapis/release-please). No manual
version bumps or changelog edits are needed.

### How it works

1. **Commits land on `main`** via squash-merged PRs with conventional commit messages.

2. **Release-please opens a Release PR.** After each push to `main`, the `release-please.yml`
   workflow runs and either creates or updates a Release PR. This PR contains:
   - Version bump in `Cargo.toml` (workspace root).
   - Updated `CHANGELOG.md` with categorized entries from commits since the last release.
   - Updated `helm/sonda/Chart.yaml` with the new version and appVersion.

3. **Admin reviews the Release PR.** Check that:
   - The version bump is correct (patch, minor, or major).
   - The changelog entries are accurate and well-categorized.
   - If a major version bump is needed but release-please chose minor, manually edit the PR.

4. **Merge the Release PR.** When merged, release-please creates a git tag (`v0.x.y`) on `main`.

5. **Pipelines trigger on the tag.** See [Pipeline Chain](#pipeline-chain) below.

---

## Pipeline Chain

When a version tag is pushed, the following pipelines execute in sequence:

```
merge to main
    |
    v
release-please.yml runs
    |
    v
Release PR opened/updated (version bump + changelog)
    |
    v
Admin merges Release PR
    |
    v
release-please creates git tag (v0.x.y)
    |
    v
release.yml triggers on v* tag
    |
    +---> binaries job: build 4 platform tarballs (linux-amd64, linux-arm64, macos-amd64, macos-arm64)
    +---> release job: create GitHub Release with tarballs + SHA256SUMS
    +---> docker job: build & push multi-arch image to ghcr.io
    |
    v
Admin manually triggers publish.yml (workflow_dispatch)
    |
    +---> dry-run publish of sonda-core, sonda, sonda-server
    +---> if dry_run=false: publish to crates.io in order (sonda-core first, then sonda, then sonda-server)
```

### Workflow files

| Workflow | Trigger | What it does |
|----------|---------|-------------|
| `ci.yml` | Push to any branch, PR to any branch | Build, test, clippy, fmt, audit, Docker build check |
| `commitlint.yml` | PR opened/edited/synced/reopened | Validate PR title is a conventional commit |
| `release-please.yml` | Push to `main` | Create/update a Release PR with version bump and changelog |
| `release.yml` | Tag push (`v*`) | Build platform binaries, create GitHub Release, push Docker image |
| `publish.yml` | Manual (`workflow_dispatch`) | Publish crates to crates.io (with dry-run option) |

---

## Dependency Updates

[Dependabot](https://docs.github.com/en/code-security/dependabot) keeps dependencies current by
automatically opening PRs.

### Configuration (`.github/dependabot.yml`)

Two ecosystems are monitored:

| Ecosystem | Schedule | PR prefix | Max open PRs |
|-----------|----------|-----------|-------------|
| Cargo (Rust dependencies) | Weekly | `deps` | 5 |
| GitHub Actions | Weekly | `ci` | Unlimited |

### Handling dependabot PRs

1. Dependabot opens a PR with the dependency update.
2. CI runs automatically (build, test, clippy, fmt, audit).
3. If CI passes, an admin reviews and merges the PR.
4. If CI fails, investigate whether the failure is a real incompatibility or a flaky test.

For Cargo dependency updates, check the dependency's changelog for breaking changes even if CI
passes -- behavioral changes may not be caught by the test suite.

### Pinning a dependency version

If a dependency update causes issues, you can temporarily pin the version in `Cargo.toml`:

```toml
# Pin to 1.2.x until upstream fixes the regression
some-dep = "=1.2.3"
```

Add a comment explaining why the version is pinned and link to the upstream issue.

---

## Admin Responsibilities

These tasks require repository admin access and cannot be automated:

### Review and merge Release PRs

When release-please opens a Release PR, review the version bump and changelog. If the automatically
determined version is incorrect (e.g., a breaking change was not flagged with `!` or
`BREAKING CHANGE`), manually edit the version in the PR before merging.

### Trigger crates.io publish

After a GitHub Release is created, manually trigger the `publish.yml` workflow:

1. Go to **Actions** > **Publish to crates.io** > **Run workflow**.
2. First run with `dry_run: true` (default) to verify everything packages correctly.
3. Run again with `dry_run: false` to publish for real.

The publish workflow requires a `CARGO_REGISTRY_TOKEN` secret configured in the repository settings.

### Major version decisions

Release-please determines the version bump from commit messages. For a major version bump, at least
one commit since the last release must contain either:
- A `!` after the type (e.g., `feat!: ...`)
- A `BREAKING CHANGE` footer

If commits were merged without proper breaking change annotation, manually edit the Release PR to
set the correct major version before merging.

### Configure branch protection

Branch protection for `main` must be configured in the GitHub UI under **Settings** > **Branches** >
**Branch protection rules**. See [Branch Protection](#branch-protection) below.

---

## Branch Protection

The following settings should be configured for the `main` branch in the GitHub UI:

### Required settings

| Setting | Value | Why |
|---------|-------|-----|
| Require a pull request before merging | Enabled | All changes go through review |
| Required approving reviews | 1 | At least one reviewer must approve |
| Require status checks to pass before merging | Enabled | CI must pass |
| Required status checks | `Build, Test, Clippy, Fmt`, `Conventional commit check` | Both CI and commitlint must pass |
| Require branches to be up to date before merging | Enabled | Prevents merge conflicts on `main` |
| Do not allow bypassing the above settings | Enabled | Even admins follow the process |

### Recommended settings

| Setting | Value | Why |
|---------|-------|-----|
| Restrict who can push to matching branches | Enabled (admins only) | Prevent direct pushes |
| Do not allow force pushes | Enabled | Protect commit history |
| Do not allow deletions | Enabled | Prevent accidental branch deletion |
| Require linear history | Enabled | Enforces squash or rebase merges |

---

## Quick Reference

Common operations at a glance.

### Cut a new release

Nothing to do manually for the version bump -- just merge PRs with conventional commit messages.
When you are ready to release:

1. Check the open Release PR from release-please (it accumulates changes automatically).
2. Review the version bump and changelog entries.
3. Merge the Release PR.
4. Wait for `release.yml` to create the GitHub Release with binaries and Docker image.
5. Trigger `publish.yml` with `dry_run: false` to publish to crates.io.

### Hotfix workflow

For an urgent fix that needs to go out immediately:

1. Branch off `main`: `git checkout -b fix/critical-encoder-bug`
2. Make the fix with a `fix:` commit.
3. Open a PR with title: `fix: resolve critical encoder panic on nil values`
4. Get a fast review, merge.
5. Merge the Release PR that release-please creates/updates.
6. Trigger `publish.yml` if the fix affects published crates.

### Handle a dependency update

1. Review the dependabot PR -- check CI results and the dependency changelog.
2. If CI is green and the update looks safe, approve and merge.
3. If CI fails, investigate. Either fix the incompatibility or close the PR and pin the version.

### Pin a dependency version

```toml
# In Cargo.toml -- pin until upstream issue #123 is resolved
problematic-crate = "=1.2.3"
```

Add the pin, open a PR, and add a `FIXME` comment with the upstream issue link so the pin is
eventually removed.

### Override a version bump

If release-please chose the wrong version (e.g., minor instead of major):

1. Open the Release PR.
2. Edit `Cargo.toml` and `CHANGELOG.md` to set the correct version.
3. Push the edit to the Release PR branch.
4. Merge when ready.

### Publish to crates.io

1. Go to **Actions** > **Publish to crates.io**.
2. Click **Run workflow**.
3. Set `dry_run` to `true` for a test run, or `false` to publish.
4. The workflow publishes in order: `sonda-core` first (other crates depend on it), then `sonda`,
   then `sonda-server`.
