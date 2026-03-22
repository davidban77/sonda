# Phase 5 — Governance & Release Automation Implementation Plan

**Goal:** Establish project governance conventions, automated versioning with changelog generation,
and comprehensive workflow documentation so that contributors and administrators have a clear,
repeatable process for development and releases.

**Prerequisite:** Phase 4 complete — release binaries, install script, and crate publishing are all
working. CI/CD pipelines produce artifacts on tag push.

**Final exit criteria:** Dependabot keeps dependencies current, PR templates enforce consistency,
CODEOWNERS ensures review coverage, conventional commits are enforced in CI, release-please
automates versioning and changelog, and all workflows are documented for contributors and admins.

**Design principle — automation over convention:** Every governance rule that can be enforced by CI
should be. Human discipline is unreliable; automated checks are not.

---

## Slice 5.0 — GitOps Foundation

### Motivation

Before automating releases, the project needs foundational governance: automated dependency updates,
consistent PR structure, code ownership rules, and commit message enforcement. These are the
building blocks that make automated versioning reliable in Slice 5.1.

### Input state
- Phase 4 passes all gates.
- `.github/workflows/ci.yml` exists with build, test, clippy, and fmt checks.
- `.github/workflows/release.yml` exists with binary builds and Docker publishing.
- `.github/workflows/publish.yml` exists with crates.io publishing.
- `CONTRIBUTING.md` exists with build, test, and commit message guidance.
- `CLAUDE.md` documents phases 0 through 4.

### Specification

**Files to create:**

1. `.github/dependabot.yml`:
   - Cargo ecosystem: weekly updates targeting `main`, commit prefix `deps`, max 5 open PRs.
   - GitHub Actions ecosystem: weekly updates targeting `main`, commit prefix `ci`.

2. `.github/pull_request_template.md`:
   - Sections: Summary, Changes, Test plan, Checklist.
   - Checklist items: tests pass, clippy clean, fmt clean, docs updated if needed.

3. `.github/CODEOWNERS`:
   - `* @davidban77` — owner reviews all PRs.
   - `sonda-core/ @davidban77` — core library requires owner review.

4. `.github/workflows/commitlint.yml`:
   - Triggers on `pull_request` events (opened, edited, synchronize, reopened).
   - Uses `amannn/action-semantic-pull-request@v5` to validate PR title.
   - Since PRs are squash-merged, the PR title becomes the commit message — so validating
     the PR title is sufficient for conventional commit enforcement.
   - Accepted types: `feat`, `fix`, `test`, `docs`, `chore`, `refactor`, `ci`, `perf`, `build`.
   - Scope is optional.

**Files to modify:**

5. `CLAUDE.md`:
   - Add Phase 5 to the Phase Overview list.
   - Add `docs/phase-5-governance.md` to the Reference Documents list.
   - Add `phase-5-governance.md` to the workspace structure tree under `docs/`.

### Output files
| File | Status |
|------|--------|
| `.github/dependabot.yml` | new |
| `.github/pull_request_template.md` | new |
| `.github/CODEOWNERS` | new |
| `.github/workflows/commitlint.yml` | new |
| `CLAUDE.md` | modified |

### Test criteria
- `dependabot.yml` is valid YAML with correct ecosystem identifiers.
- `pull_request_template.md` contains all four sections (Summary, Changes, Test plan, Checklist).
- `CODEOWNERS` uses valid GitHub syntax with correct paths and usernames.
- `commitlint.yml` is a valid GitHub Actions workflow that triggers on pull_request events.
- `commitlint.yml` accepts all specified commit types (feat, fix, test, docs, chore, refactor, ci, perf, build).
- All existing tests continue to pass (`cargo test --workspace`).
- All existing lints continue to pass (`cargo clippy --workspace -- -D warnings`).

### Review criteria
- `dependabot.yml` limits open PRs to avoid flooding the repo.
- `dependabot.yml` uses appropriate prefixes for commit messages.
- PR template checklist aligns with the project's quality gates in `CLAUDE.md`.
- `CODEOWNERS` correctly protects `sonda-core/` with owner review.
- `commitlint.yml` validates PR titles (not individual commit messages) since squash-merge is used.
- Accepted commit types match the project's existing conventions in `CONTRIBUTING.md`.
- `CLAUDE.md` updates are consistent with the existing documentation style.
- No unrelated files are modified.

### UAT criteria
- Open a PR with title `feat: test PR` — commitlint check passes.
- Open a PR with title `invalid title` — commitlint check fails.
- Open a PR with title `fix(core): resolve edge case` — commitlint check passes (scoped).
- Dependabot begins creating dependency update PRs within a week.
- PR template appears automatically when opening a new PR via the GitHub UI.
- CODEOWNERS triggers review requests when a PR is opened.

---

## Slice 5.1 — Automated Versioning & Changelog

### Motivation

Manual version bumps and changelog maintenance are error-prone and easily forgotten. Release-please
reads conventional commit messages, determines the appropriate version bump (major/minor/patch),
updates the changelog, and creates a release PR — all automatically. When that PR is merged, a git
tag is created that triggers the existing release pipeline.

### Input state
- Slice 5.0 passes all gates.
- Conventional commit enforcement is active via `commitlint.yml`.
- `.github/workflows/release.yml` triggers on `v*` tag push.
- `CHANGELOG.md` exists with an `[Unreleased]` section.

### Specification

**Files to create:**

1. `.github/workflows/release-please.yml`:
   - Triggers on push to `main` branch.
   - Uses `googleapis/release-please-action@v4`.
   - Configured with `release-type: rust`.
   - Updates workspace `Cargo.toml` version.
   - Creates a "Release PR" containing version bump and `CHANGELOG.md` updates.
   - When the Release PR is merged, creates a git tag (`v0.x.y`).
   - The tag triggers the existing `release.yml` workflow (binaries + Docker).

2. `release-please-config.json`:
   - Workspace root package (path: `.`).
   - Extra files to update: `helm/sonda/Chart.yaml` (version and appVersion fields).
   - Changelog sections mapping:
     - `feat` maps to "Features"
     - `fix` maps to "Bug Fixes"
     - `docs` maps to "Documentation"
     - `chore` maps to "Miscellaneous"

3. `.release-please-manifest.json`:
   - Initial version: `{".": "0.1.0"}`.

**Files to modify:**

4. `CHANGELOG.md`:
   - Restructure existing `[Unreleased]` content under a `## [0.1.0]` heading.
   - Add a new empty `## [Unreleased]` section at the top for future changes.
   - This allows release-please to manage the changelog going forward.

### Output files
| File | Status |
|------|--------|
| `.github/workflows/release-please.yml` | new |
| `release-please-config.json` | new |
| `.release-please-manifest.json` | new |
| `CHANGELOG.md` | modified |

### Test criteria
- `release-please.yml` is a valid GitHub Actions workflow that triggers on push to main.
- `release-please-config.json` is valid JSON with correct release-type and changelog sections.
- `.release-please-manifest.json` is valid JSON with initial version `0.1.0`.
- `CHANGELOG.md` has both `[Unreleased]` and `[0.1.0]` sections.
- `CHANGELOG.md` retains all existing content under the `[0.1.0]` heading.
- All existing tests continue to pass (`cargo test --workspace`).

### Review criteria
- Release-please configuration matches the workspace structure (single root package).
- The `release-type: rust` setting is correct for a Cargo workspace.
- Extra files list includes `helm/sonda/Chart.yaml` for Helm chart version sync.
- Changelog sections mapping covers the most common commit types.
- The existing changelog content is preserved, not lost during restructuring.
- The release-please workflow does not conflict with the existing `release.yml` workflow.
- Tag format (`v0.x.y`) matches the existing `release.yml` trigger pattern.

### UAT criteria
- Push a `feat` commit to main → release-please opens a Release PR with minor version bump.
- Push a `fix` commit to main → release-please updates the Release PR with patch version bump.
- Merge the Release PR → a `v0.x.y` tag is created → `release.yml` triggers.
- The generated `CHANGELOG.md` in the Release PR correctly categorizes commits.
- `helm/sonda/Chart.yaml` version is updated in the Release PR.

---

## Slice 5.2 — Workflow Documentation

### Motivation

Governance automation is only useful if contributors and administrators understand the workflows.
This slice creates comprehensive documentation covering the development flow, release process,
dependency management, and administrator responsibilities.

### Input state
- Slice 5.1 passes all gates.
- All governance automation is in place: dependabot, commitlint, release-please.
- `CONTRIBUTING.md` exists with basic build/test/lint guidance.
- `README.md` exists with installation and usage documentation.

### Specification

**Files to create:**

1. `docs/release-workflow.md`:
   - **Development Flow**: branch off main, use conventional commits, open PR, CI validates,
     review, squash merge.
   - **Conventional Commits**: how commit types map to version bumps (`feat` = minor, `fix` = patch,
     `BREAKING CHANGE` in footer = major).
   - **Release Process**: release-please opens Release PR, admin reviews, merge, tag created,
     pipelines trigger.
   - **Pipeline Chain**: tag push triggers `release.yml` (binaries + Docker + GitHub Release),
     manual `publish.yml` (crates.io).
   - **Dependency Updates**: dependabot opens PRs weekly, CI validates, admin reviews and merges.
   - **Admin Responsibilities**: major version decisions, crates.io publish approval, branch
     protection setup in GitHub UI.
   - **Branch Protection**: document the recommended GitHub UI settings (require PR reviews,
     require CI status checks, no force push to main, no direct pushes to main).
   - **Quick Reference**: cheat sheet of common operations (cut a release, hotfix workflow,
     dependency update, version pinning).

**Files to modify:**

2. `CONTRIBUTING.md`:
   - Add a "Pull Request Process" section describing PR workflow, template usage, and review
     expectations.
   - Add a reference to `docs/release-workflow.md` for the full release process.

3. `README.md`:
   - Add a "Contributing" section near the bottom linking to `CONTRIBUTING.md` and
     `docs/release-workflow.md`.

### Output files
| File | Status |
|------|--------|
| `docs/release-workflow.md` | new |
| `CONTRIBUTING.md` | modified |
| `README.md` | modified |

### Test criteria
- `docs/release-workflow.md` contains all eight sections specified above.
- `CONTRIBUTING.md` references `docs/release-workflow.md`.
- `README.md` contains a "Contributing" section with links.
- All markdown files render correctly (no broken links within the repo).
- All existing tests continue to pass (`cargo test --workspace`).

### Review criteria
- Documentation is accurate and matches the actual CI/CD configuration.
- Conventional commit type-to-version-bump mapping is correct.
- Pipeline chain description matches the actual workflow trigger relationships.
- Branch protection recommendations are practical and complete.
- Quick reference covers the most common admin operations.
- No sensitive information (tokens, secrets) is mentioned in documentation.
- Writing is clear, concise, and consistent with the project's documentation style.

### UAT criteria
- A new contributor can follow the development flow docs to open their first PR successfully.
- An admin can follow the release process docs to cut a release without prior knowledge.
- The quick reference cheat sheet covers at least: cutting a release, hotfix, dependency update.
- All internal links in the documentation resolve to existing files.

---

## Dependency Graph

```
Slice 5.0 (GitOps foundation: dependabot, PR template, CODEOWNERS, commitlint)
  |
Slice 5.1 (automated versioning: release-please, changelog management)
  |
Slice 5.2 (workflow documentation: release-workflow.md, CONTRIBUTING.md updates)
```

Slice 5.0 establishes the governance foundation that Slice 5.1 builds on — conventional commit
enforcement is required before release-please can reliably determine version bumps. Slice 5.2
documents the complete system once all automation is in place.

---

## Post-Phase 5

With Phase 5 complete, the project has a fully automated governance pipeline: dependency updates
flow in via dependabot, conventional commits are enforced, versions are bumped automatically,
changelogs are generated, and releases are triggered by merging a PR. Future governance
improvements (not designed here):

- **Security policy** — `SECURITY.md` with vulnerability reporting guidelines.
- **Issue templates** — bug report and feature request templates with structured fields.
- **Stale issue bot** — automatically close issues and PRs with no activity.
- **Release notes generation** — richer release notes with contributor acknowledgments.
- **Signed commits** — require GPG or SSH commit signatures for all contributions.
- **SBOM generation** — software bill of materials attached to each release.
