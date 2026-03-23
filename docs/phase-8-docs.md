# Phase 8 — Documentation (MkDocs Material)

**Goal:** A professional, user-facing documentation site hosted on GitHub Pages via MkDocs Material.
The docs target SREs, platform engineers, and developers who want to adopt Sonda — not contributors
to the codebase. Every page must be grounded in what the code actually does today.

**Prerequisite:** The project has a working CLI, server, multiple generators/encoders/sinks, Docker
support, Helm chart, CI/CD with release-please, and an e2e test suite.

**Agent:** `@doc` — documentation-focused agent with discovery-first mandate.

**Final exit criteria:** `mkdocs build --strict` passes, GitHub Pages deploys automatically via CI,
and the site covers: getting started, all use cases, configuration reference, deployment guide, and
CLI/API reference — with zero references to features that don't exist.

---

## Slice 8.0 — MkDocs Scaffold & GitHub Pages CI

### Input state
- Project repo exists with working CI in `.github/workflows/`.
- No existing MkDocs setup.

### Specification

**Files to create:**
- `docs/site/mkdocs.yml`:
  ```yaml
  site_name: Sonda
  site_description: Synthetic telemetry generator for testing observability pipelines
  site_url: https://davidban77.github.io/sonda/
  repo_url: https://github.com/davidban77/sonda
  repo_name: davidban77/sonda

  theme:
    name: material
    palette:
      - media: "(prefers-color-scheme: light)"
        scheme: default
        primary: deep purple
        accent: amber
        toggle:
          icon: material/brightness-7
          name: Switch to dark mode
      - media: "(prefers-color-scheme: dark)"
        scheme: slate
        primary: deep purple
        accent: amber
        toggle:
          icon: material/brightness-4
          name: Switch to light mode
    features:
      - navigation.sections
      - navigation.expand
      - navigation.top
      - search.highlight
      - content.code.copy
      - content.tabs.link

  markdown_extensions:
    - admonition
    - pymdownx.details
    - pymdownx.superfences
    - pymdownx.highlight:
        anchor_linenums: true
    - pymdownx.inlinehilite
    - pymdownx.tabbed:
        alternate_style: true
    - pymdownx.snippets
    - attr_list
    - md_in_html
    - toc:
        permalink: true

  nav:
    - Home: index.md
  ```

- `docs/site/docs/index.md`:
  - Landing page. What Sonda is (2-3 sentences), what it's for (bullet list of use cases),
    quick install, and a single working example.
  - **Discovery required**: run `cargo run -p sonda -- --help` and inspect the actual binary
    output to write an honest feature summary.
  - **Must NOT** mention traces or flows unless they are implemented.

- `docs/site/requirements.txt`:
  ```
  mkdocs-material>=9.5
  ```

- `.github/workflows/docs.yml`:
  - Trigger: push to main (paths: `docs/site/**`), manual dispatch.
  - Steps: checkout → setup Python → install mkdocs-material → `mkdocs build --strict` → deploy
    to GitHub Pages via `mkdocs gh-deploy --force`.
  - Use `actions/setup-python@v5` and `pip install -r docs/site/requirements.txt`.

**Verification:**
```bash
task site:build    # installs deps in venv automatically, then builds with --strict
task site:serve    # preview at http://localhost:8000
```

### Output files
| File | Status |
|------|--------|
| `docs/site/mkdocs.yml` | new |
| `docs/site/docs/index.md` | new |
| `docs/site/requirements.txt` | new |
| `.github/workflows/docs.yml` | new |

### Quality criteria
- `mkdocs build --strict` produces zero warnings.
- `mkdocs serve` renders correctly at localhost:8000.
- Landing page has a working CLI example that was verified against the actual binary.
- Landing page does NOT mention unimplemented features (traces, flows, etc.).
- GitHub Actions workflow is valid YAML with correct trigger paths.

---

## Slice 8.1 — Getting Started Guide

### Input state
- Slice 8.0 passes all quality criteria.
- MkDocs scaffold builds.

### Specification

**Discovery required:**
```bash
# Verify actual install methods
cat Cargo.toml | head -20               # version, name
cat Dockerfile                           # Docker image name/tag
cargo run -p sonda -- metrics --help     # actual flags
cargo run -p sonda -- logs --help        # actual flags
ls examples/*.yaml                       # available example scenarios
```

**Files to create:**
- `docs/site/docs/getting-started.md`:
  Sections (each with a working, tested example):
  1. **Installation** — cargo install, pre-built binary (if releases exist), Docker pull.
     Check actual release artifacts on GitHub.
  2. **Your first metric** — simplest possible `sonda metrics` command. Show the output.
  3. **Using a scenario file** — one of the existing example YAMLs. Show the YAML and the output.
  4. **Generating logs** — simplest `sonda logs` command. Show the output.
  5. **What next** — links to use case guides and configuration reference.

**Files to modify:**
- `docs/site/mkdocs.yml` — add to nav:
  ```yaml
  nav:
    - Home: index.md
    - Getting Started: getting-started.md
  ```

### Output files
| File | Status |
|------|--------|
| `docs/site/docs/getting-started.md` | new |
| `docs/site/mkdocs.yml` | modified |

### Quality criteria
- Every command and YAML example tested against the actual binary.
- Installation instructions match actual release/build process.
- A reader with Rust installed can follow the page top-to-bottom and have metrics on stdout
  within 5 minutes.
- Page is under 600 words (excluding code blocks).

---

## Slice 8.2 — Configuration Reference

### Input state
- Slice 8.1 passes all quality criteria.

### Specification

**Discovery required:**
```bash
# Discover all config fields from source
grep -r "Deserialize" sonda-core/src/config/ --include="*.rs" -A 20
# Discover all generator variants
grep -r "GeneratorConfig" sonda-core/src/generator/ --include="*.rs"
# Discover all encoder variants
grep -r "EncoderConfig" sonda-core/src/encoder/ --include="*.rs"
# Discover all sink variants
grep -r "SinkConfig" sonda-core/src/sink/ --include="*.rs"
# Discover all CLI flags
cargo run -p sonda -- metrics --help
cargo run -p sonda -- logs --help
# Discover env vars
grep -r "SONDA_" sonda/src/ --include="*.rs"
# Discover multi-scenario config
grep -r "MultiScenarioConfig\|ScenarioEntry" sonda-core/src/config/ --include="*.rs"
```

**Files to create:**
- `docs/site/docs/configuration/scenario-file.md`:
  - Full YAML scenario file reference.
  - Every field documented with type, default, and a working example.
  - Organized by section: name/rate/duration, generator, gaps/bursts, labels, encoder, sink.
  - Include a "complete example" at the top that touches every field.

- `docs/site/docs/configuration/generators.md`:
  - One subsection per generator with: description, parameters, YAML example, value plot sketch
    (describe the shape in words — e.g., "oscillates between offset-amplitude and offset+amplitude").
  - **Discovery**: list actual generator .rs files and their fields.

- `docs/site/docs/configuration/encoders.md`:
  - One subsection per encoder: format description, wire format example, YAML config.
  - **Discovery**: list actual encoder .rs files.

- `docs/site/docs/configuration/sinks.md`:
  - One subsection per sink: what it does, config fields, YAML example.
  - For network sinks: include the target address format.
  - **Discovery**: list actual sink .rs files.

- `docs/site/docs/configuration/cli-reference.md`:
  - Auto-discovered from `--help` output.
  - Every subcommand, every flag, with a one-line example.
  - Precedence rules: YAML < env vars < CLI flags.
  - Environment variables reference.

**Files to modify:**
- `docs/site/mkdocs.yml` — add to nav:
  ```yaml
  - Configuration:
    - Scenario Files: configuration/scenario-file.md
    - Generators: configuration/generators.md
    - Encoders: configuration/encoders.md
    - Sinks: configuration/sinks.md
    - CLI Reference: configuration/cli-reference.md
  ```

### Output files
| File | Status |
|------|--------|
| `docs/site/docs/configuration/scenario-file.md` | new |
| `docs/site/docs/configuration/generators.md` | new |
| `docs/site/docs/configuration/encoders.md` | new |
| `docs/site/docs/configuration/sinks.md` | new |
| `docs/site/docs/configuration/cli-reference.md` | new |
| `docs/site/mkdocs.yml` | modified |

### Quality criteria
- Every generator, encoder, and sink that exists in source code is documented.
- No generators, encoders, or sinks that don't exist are mentioned.
- Every YAML example is valid and tested with `sonda metrics --scenario`.
- CLI reference matches actual `--help` output exactly.
- Each page is scannable — tables for parameter reference, prose for explanations.

---

## Slice 8.3 — Use Case: Alert Testing Guide

### Input state
- Slice 8.2 passes all quality criteria.
- Configuration reference pages exist (for cross-linking).

### Specification

This is the **highest-value documentation page** per the SRE review. It must answer: "How do I
use Sonda to validate my alert rules before promoting them to production?"

**Discovery required:**
```bash
# Find alert-related examples
find . -name "*.yaml" | xargs grep -l -i "alert\|threshold" 2>/dev/null
# Check what docker-compose setups exist
ls docker-compose*.yml tests/e2e/docker-compose* 2>/dev/null
# Verify VictoriaMetrics e2e setup
cat tests/e2e/docker-compose.yml 2>/dev/null | head -50
# Check if Prometheus/Alertmanager are in any compose
grep -r "alertmanager\|prometheus" docker-compose*.yml tests/e2e/ --include="*.yml" -l
```

**Migrate existing content**: `docs/guide-alert-testing.md` (850+ lines) already covers this
topic comprehensively. Adapt and restructure this content for MkDocs rather than writing from
scratch.

**Files to create:**
- `docs/site/docs/guides/alert-testing.md`:
  Sections:
  1. **The problem** — 3 sentences: you write alert rules, you need to know they fire correctly,
     Sonda lets you generate the exact metric shapes to trigger them.
  2. **Threshold alerts** — step-by-step:
     - Goal: verify `HighCPU` fires when CPU > 90%.
     - YAML: sine generator with amplitude=50, offset=50 → crosses 90 twice per period.
     - **Show the math**: sin(x) > 0.8 when... → alert fires for X seconds per period.
     - Full working YAML + `sonda metrics` command.
  3. **Testing `for:` duration behavior** — how to use gap/burst timing to control how long the
     metric stays above threshold, so you can verify the `for: 5m` clause.
  4. **Testing alert resolution** — use gap windows: metric is above threshold, gap drops it to
     zero, alert resolves, metric resumes, alert re-fires.
  5. **Pushing to VictoriaMetrics** — working example with `http_push` sink pointing at
     `/api/v1/import/prometheus`. Include vmagent relay via remote_write encoder+sink.
  6. **Scrape-based integration** — document the scrape endpoint (`GET /scenarios/{id}/metrics`)
     for Prometheus pull-based workflows. Show sonda-server + Prometheus scrape config.
  7. **Multi-metric correlation** — demonstrate `phase_offset` to generate correlated metrics
     (e.g., CPU and memory rising together with a time lag). Show working YAML.
  8. **Sequence and csv_replay generators** — show how to use the sequence generator for
     deterministic threshold crossing and csv_replay for replaying real production patterns.
  9. **Full example** — complete docker-compose + YAML + commands to run end-to-end.

**Files to modify:**
- `docs/site/mkdocs.yml` — add to nav:
  ```yaml
  - Guides:
    - Alert Testing: guides/alert-testing.md
  ```

### Output files
| File | Status |
|------|--------|
| `docs/site/docs/guides/alert-testing.md` | new |
| `docs/site/mkdocs.yml` | modified |

### Quality criteria
- The threshold alert example is tested end-to-end: YAML → sonda → output verified.
- The math for sine wave threshold crossing is correct and explained clearly.
- The VictoriaMetrics push example uses the correct endpoint URL.
- Scrape endpoint, remote write, sequence generator, csv_replay, and multi-metric correlation
  are all documented with working examples.
- Page is scannable: a reader can skip to their exact scenario.

---

## Slice 8.4 — Use Case: Pipeline Validation & CI Integration

### Input state
- Slice 8.3 passes all quality criteria.

### Specification

**Discovery required:**
```bash
# Check exit code behavior
cargo run -p sonda -- metrics --name up --rate 10 --duration 1s > /dev/null 2>&1; echo $?
cargo run -p sonda -- metrics 2>&1; echo $?
# Check if there's a run/multi-scenario subcommand
cargo run -p sonda -- --help
cargo run -p sonda -- run --help 2>/dev/null
# Check CI examples
ls .github/workflows/
# Check e2e test scripts
ls tests/e2e/*.sh 2>/dev/null
```

**Migrate existing content**: `docs/guide-alert-testing.md` Section 5 covers recording rules.
`examples/recording-rule-test.yaml` and `examples/recording-rule-prometheus.yml` are ready-to-use
examples.

**Files to create:**
- `docs/site/docs/guides/pipeline-validation.md`:
  Sections:
  1. **The problem** — you changed your ingest pipeline, encoder config, or recording rules.
     How do you know nothing broke?
  2. **Smoke testing with CLI** — run sonda with a known scenario, check exit code, pipe to wc -l.
  3. **Multi-format validation** — run same metrics through different encoders, verify each format
     arrives at its destination.
  4. **CI integration** — example GitHub Actions step that runs sonda as a test, verifying output.
     Show exit code handling, duration flags for bounded CI time.
  5. **E2E testing** — reference the existing e2e test setup if it exists. Show how to use
     docker-compose to spin up sonda + backend + verification.

- `docs/site/docs/guides/recording-rules.md`:
  Sections:
  1. **The problem** — you have Prometheus recording rules, you need to verify they compute
     correctly against known input.
  2. **Approach** — push known values via sonda → wait for evaluation → query the recording rule.
  3. **Working example** — constant generator (known value) → push to VM → query recording rule
     output → verify.

**Files to modify:**
- `docs/site/mkdocs.yml` — add to nav:
  ```yaml
  - Guides:
    - Alert Testing: guides/alert-testing.md
    - Pipeline Validation: guides/pipeline-validation.md
    - Recording Rules: guides/recording-rules.md
  ```

### Output files
| File | Status |
|------|--------|
| `docs/site/docs/guides/pipeline-validation.md` | new |
| `docs/site/docs/guides/recording-rules.md` | new |
| `docs/site/mkdocs.yml` | modified |

### Quality criteria
- CI example is a valid GitHub Actions step (tested syntax).
- Exit code documentation matches actual binary behavior.
- Recording rules guide includes a complete, working example.
- All commands are copy-paste ready.

---

## Slice 8.5 — Deployment Guide (Docker, Kubernetes, Bare Metal)

### Input state
- Slice 8.4 passes all quality criteria.

### Specification

**Discovery required:**
```bash
# Docker
cat Dockerfile
ls docker-compose*.yml
# Check image name/tags in releases
grep -r "image:" docker-compose*.yml helm/ --include="*.yml" --include="*.yaml" 2>/dev/null
# Helm
ls helm/ helm/sonda/ 2>/dev/null
cat helm/sonda/Chart.yaml 2>/dev/null
cat helm/sonda/values.yaml 2>/dev/null
# Check Helm chart URL correctness (SRE review flagged davidflores77 vs davidban77)
grep -r "home\|url\|repository" helm/sonda/Chart.yaml 2>/dev/null
# Check taskfile
cat Taskfile.yml 2>/dev/null | head -40
ls Taskfile*.yml 2>/dev/null
# Server
cargo run -p sonda-server -- --help 2>/dev/null
```

**Migrate existing content**: `README.md` has Docker, Kubernetes, and VictoriaMetrics sections.
`docs/release-workflow.md` covers the release process.

**Files to create:**
- `docs/site/docs/deployment/docker.md`:
  Sections:
  1. **Running with Docker** — `docker run` with example scenario via volume mount or stdin.
  2. **Docker Compose examples** — link/copy the best compose files. If there's a
     VictoriaMetrics compose in tests/e2e, promote it here.
  3. **Building from source** — multi-stage build, musl target.

- `docs/site/docs/deployment/kubernetes.md`:
  Sections:
  1. **Helm chart** — install command, values reference.
  2. **Scenario injection** — how ConfigMaps drive scenarios.
  3. **Example values.yaml** — minimal working deployment.
  4. **Known issues** — flag the Chart.yaml URL issue if still present.

- `docs/site/docs/deployment/sonda-server.md`:
  Sections:
  1. **What sonda-server is** — REST API for managing scenario lifecycle.
  2. **Starting the server** — CLI flags (port, bind).
  3. **API reference** — every endpoint: method, path, request body, response, examples with curl.
     Explicitly document the scrape endpoint (`GET /scenarios/{id}/metrics`) for Prometheus
     pull-based integration.
  4. **Push integration** — document the `remote_write` sink for pushing metrics to
     Prometheus/VictoriaMetrics backends.
  5. **Running in production** — Docker, health checks, graceful shutdown.

**Files to modify:**
- `docs/site/mkdocs.yml` — add to nav:
  ```yaml
  - Deployment:
    - Docker: deployment/docker.md
    - Kubernetes: deployment/kubernetes.md
    - Server API: deployment/sonda-server.md
  ```

### Output files
| File | Status |
|------|--------|
| `docs/site/docs/deployment/docker.md` | new |
| `docs/site/docs/deployment/kubernetes.md` | new |
| `docs/site/docs/deployment/sonda-server.md` | new |
| `docs/site/mkdocs.yml` | modified |

### Quality criteria
- Docker run command tested and verified.
- Helm chart values match actual `values.yaml` in the repo.
- Every API endpoint in sonda-server is documented with a curl example.
- No broken links (helm chart URLs verified against actual Chart.yaml).
- Docker Compose examples reference the correct image name.

---

## Slice 8.6 — Development & Contributing Guide

### Input state
- Slice 8.5 passes all quality criteria.

### Specification

**Discovery required:**
```bash
# Taskfile
cat Taskfile.yml 2>/dev/null
task --list 2>/dev/null
# CI
cat .github/workflows/ci.yml
# Release
cat .github/workflows/release*.yml 2>/dev/null
ls .release-please*.json 2>/dev/null
cat release-please-config.json 2>/dev/null
# Dev setup
cat rust-toolchain.toml 2>/dev/null
grep "musl" Cargo.toml .cargo/config.toml 2>/dev/null
```

**Migrate existing content**: `CONTRIBUTING.md` and `docs/release-workflow.md` already cover the
development and release workflow.

**Files to create:**
- `docs/site/docs/contributing.md`:
  Sections:
  1. **Prerequisites** — Rust toolchain, Task (taskfile), Docker, musl target.
  2. **Dev environment setup** — task commands for setup, build, test.
  3. **Project structure** — brief workspace overview (link to architecture for depth).
  4. **Adding generators/encoders/sinks** — link to the relevant `.claude/skills/` patterns
     as step-by-step guides.
  5. **Running tests** — unit, integration, e2e.
  6. **CI/CD** — what CI checks, how releases work (release-please), how docs deploy.
  7. **Code conventions** — link to root CLAUDE.md for the full set, highlight the top 5.

**Files to modify:**
- `docs/site/mkdocs.yml` — add to nav:
  ```yaml
  - Contributing: contributing.md
  ```

### Output files
| File | Status |
|------|--------|
| `docs/site/docs/contributing.md` | new |
| `docs/site/mkdocs.yml` | modified |

### Quality criteria
- Taskfile commands match actual `task --list` output.
- Setup instructions tested from scratch (clone → build → test → all green).
- CI workflow description matches actual `.github/workflows/` files.
- Release-please process accurately described.
- Page doesn't duplicate `CLAUDE.md` — it summarizes and links.

---

## Slice 8.7 — Audit, Cleanup & Final Validation

### Input state
- Slices 8.0–8.6 pass all quality criteria.

### Specification

This is the verification slice. No new content — only auditing and fixing.

Note: The README intro was already fixed in Phase 6.0 — the audit should verify the fix is
accurate, not redo it.

**Procedure:**

1. **Cross-reference audit**: For every feature mentioned in docs, verify it exists in code.
   Verify that all Phase 6-7 features are documented: sequence generator, csv_replay generator,
   scrape endpoint, remote write encoder+sink, multi-metric correlation (phase_offset), and
   Grafana dashboards.
   For every major feature in code, verify it's mentioned in docs.
   ```bash
   # Generators in code vs docs
   ls sonda-core/src/generator/*.rs | xargs -I {} basename {} .rs
   grep -r "type:" docs/site/docs/configuration/generators.md | grep -v "^#"

   # Encoders in code vs docs
   ls sonda-core/src/encoder/*.rs | xargs -I {} basename {} .rs
   grep -r "encoder" docs/site/docs/configuration/encoders.md | head -20

   # Sinks in code vs docs
   ls sonda-core/src/sink/*.rs | xargs -I {} basename {} .rs
   grep -r "sink" docs/site/docs/configuration/sinks.md | head -20
   ```

2. **Link check**: `mkdocs build --strict` — all cross-references resolve.

3. **Consistency check**:
   - Project name spelled consistently (Sonda, not SONDA or sonda in prose).
   - GitHub URLs all point to `davidban77/sonda` (not davidflores77).
   - Docker image references are consistent.

4. **Size check**: No page exceeds ~800 words (guides can be longer). Run word counts:
   ```bash
   wc -w docs/site/docs/**/*.md docs/site/docs/*.md
   ```

5. **Navigation check**: Every .md file in `docs/site/docs/` is in the `nav:` of `mkdocs.yml`.

6. **README update**: Update the root `README.md` to:
   - Fix the intro (remove traces/flows if not implemented, list all actual features).
   - Add a "Documentation" section linking to the GitHub Pages site.
   - Trim the README to overview + quick start + link to full docs. The README should NOT
     try to be the docs — it should point to them.

7. **Build final site**:
   ```bash
   cd docs/site && mkdocs build --strict
   ```

### Output files
| File | Status |
|------|--------|
| Various `docs/site/docs/**/*.md` | modified (fixes) |
| `docs/site/mkdocs.yml` | modified (if nav fixes needed) |
| `README.md` | modified |

### Quality criteria
- `mkdocs build --strict` passes with zero warnings.
- Every generator, encoder, and sink in source code appears in docs.
- No docs reference features that don't exist.
- All GitHub/Docker URLs are correct and consistent.
- README is concise and links to the docs site.
- Word count per page is reasonable (no walls of text).

---

## Dependency Graph

```
Slice 8.0 (MkDocs scaffold + CI)
  ↓
Slice 8.1 (getting started)
  ↓
Slice 8.2 (configuration reference)
  ↓
  ├── Slice 8.3 (alert testing guide)       ← parallel with 8.4
  └── Slice 8.4 (pipeline validation guide)  ← parallel with 8.3
       ↓
Slice 8.5 (deployment guide)
  ↓
Slice 8.6 (contributing guide)
  ↓
Slice 8.7 (audit, cleanup & final validation)
```

Slices 8.3 and 8.4 can run in parallel after 8.2 since they both depend on the configuration
reference pages for cross-linking but don't depend on each other.

---

## Workflow per Slice

Phase 8 uses a modified agent workflow:

1. `@doc 8.X`      → discovers code state, writes/migrates docs, builds site, creates PR
2. `@reviewer 8.X`  → audits accuracy against source code, validates examples
3. `@uat 8.X`       → builds site, follows guides as a real user, validates end-to-end
4. Human reviews    → approves and merges PR

The tester agent is not used for docs slices — accuracy is covered by the doc agent's
`mkdocs build --strict` and the reviewer's cross-reference audit.

---

## Notes for the @doc Agent

- **Discovery first.** Every slice starts with discovery commands. Do NOT write docs from the
  phase plan descriptions alone — verify against actual source code.
- **All SRE review features are implemented.** All features from the SRE review (scrape endpoint,
  remote write protobuf, sequence generator, csv replay generator, multi-metric correlation) are
  now implemented. Document them accurately.
- **Examples are contractual.** Every YAML and CLI example in the docs must produce the
  described output when actually run. If it doesn't, the docs are wrong, not the code.
- **The SRE review is your north star.** The feedback in the review should guide tone, priorities,
  and what gets prominent placement. The alert testing guide (Slice 8.3) is the most important
  page after getting started.
