# Phase 6 — Product Polish Implementation Plan

**Goal:** Fix documentation drift, add a sequence value generator, promote the VictoriaMetrics
integration, add a scrape endpoint to sonda-server, and write the alert testing guide that makes
SREs adopt Sonda.

**Prerequisite:** Phase 5 complete — governance automation, release-please, and workflow
documentation are all in place.

**Final exit criteria:** README accurately reflects implemented features, documentation has no
phantom files or stale references, the sequence generator enables explicit value patterns, the
VictoriaMetrics stack is discoverable, sonda-server exposes a Prometheus-scrapeable metrics
endpoint, and the alert testing guide walks users through real-world alert validation workflows.

**Design principle — accuracy over aspiration:** Every claim in documentation must be verifiable
today. Features that are planned but not yet implemented are clearly marked as roadmap items.
Users should never encounter a documented feature that does not work.

---

## Slice 6.0 — Fix README & Documentation Drift

### Motivation

The README is the front door of the project. Currently it claims Sonda produces "traces and flows"
which are not implemented. The `sonda-core/CLAUDE.md` lists phantom files (counter.rs, gauge.rs,
microburst.rs) that do not exist. The Helm chart has incorrect GitHub URLs. These inaccuracies
erode trust and confuse new users. This slice fixes all documentation drift.

### Input state
- Phase 5 passes all gates.
- README.md exists with Features, Installation, CLI Reference, and Example Scenarios sections.
- `sonda-core/CLAUDE.md` exists with Module Layout section.
- `helm/sonda/Chart.yaml` exists with home and sources URLs.
- All sonda-server routes use `{id}` path syntax (axum 0.8).

### Specification

**Files to modify:**

1. `README.md`:
   - **Introduction paragraph**: Replace "metrics, logs, traces, and flows" with "metrics and
     logs" and add a note that traces and flows are on the roadmap.
   - **Features section**: Expand to list all implemented capabilities:
     - 4 metric value generators: constant, uniform, sine, sawtooth
     - 2 log generators: template, replay
     - 4 encoders: prometheus_text, influx_lp, json_lines, syslog
     - 9 sinks: stdout, file, TCP, UDP, http_push, loki, kafka, channel, memory
     - Gap windows and burst windows for failure modeling
     - Multi-scenario concurrent execution
     - sonda-server HTTP control plane
   - **Add "Supported Signal Types" section** (between Features and Installation) clearly
     showing the signal matrix:
     - Metrics: 4 generators, 3 metric encoders (prometheus_text, influx_lp, json_lines),
       7 delivery sinks (stdout, file, TCP, UDP, http_push, kafka, channel)
     - Logs: 2 generators, 2 log encoders (json_lines, syslog), 8 delivery sinks
       (stdout, file, TCP, UDP, http_push, loki, kafka, channel)
   - **Restructure top-level sections** for new-user flow:
     1. Introduction (what is Sonda)
     2. Features
     3. Supported Signal Types
     4. Installation
     5. Quick Start
     6. CLI Reference
     7. YAML Scenario Files
     8. Log Scenario Files
     9. Example Scenarios
     10. sonda-server
     11. Docker Deployment
     12. Kubernetes Deployment
     13. End-to-End Integration Tests
     14. Development
     15. Contributing
     16. License

2. `sonda-core/CLAUDE.md`:
   - **Remove phantom files** from the Module Layout section: `counter.rs`, `gauge.rs`, and
     `microburst.rs` do not exist and must not be listed.
   - **Verify every listed file** actually exists in the source tree. Add `memory.rs` to the
     sink listing if missing.
   - **Update any out-of-date descriptions** for files that have evolved since the layout was
     written.

3. `helm/sonda/Chart.yaml`:
   - Change `home:` from `https://github.com/davidflores77/sonda` to
     `https://github.com/davidban77/sonda`.
   - Change `sources:` entry from `https://github.com/davidflores77/sonda` to
     `https://github.com/davidban77/sonda`.

4. **Audit docs for `:id` vs `{id}` consistency**: All route references in README.md,
   architecture.md, and server CLAUDE.md should use `{id}` (axum 0.8 syntax). Fix any
   occurrences of `:id`.

### Output files
| File | Status |
|------|--------|
| `README.md` | modified |
| `sonda-core/CLAUDE.md` | modified |
| `helm/sonda/Chart.yaml` | modified |

### Test criteria
- `sonda-core/CLAUDE.md` lists no files that do not exist in `sonda-core/src/`.
- `helm/sonda/Chart.yaml` home URL points to `davidban77`, not `davidflores77`.
- README does not mention "traces and flows" as an implemented feature.
- README Features section mentions all four encoders and all sink types.
- No occurrences of `:id` in route paths across any documentation file.
- All existing tests continue to pass (`cargo test --workspace`).

### Review criteria
- README introduction is accurate and not aspirational about unimplemented features.
- Supported Signal Types matrix is correct (no encoder/sink listed that does not support that
  signal type).
- sonda-core CLAUDE.md module layout matches the actual `src/` directory tree exactly.
- Helm chart URLs are correct and consistent.
- Section ordering in README follows a logical new-user flow.
- No unrelated code changes are included.

### UAT criteria
- A new user reading the README can understand what Sonda does in under 30 seconds.
- Every sink type mentioned in the README has a corresponding example YAML in `examples/`.
- Every file listed in `sonda-core/CLAUDE.md` exists when checked with `ls`.
- `helm/sonda/Chart.yaml` home URL is clickable and resolves to the correct GitHub repo.

---

## Slice 6.1 — Step/Sequence Value Generator

### Motivation

Real incident patterns are not sine waves or random noise. They follow explicit sequences: a
metric sits at 0 for several ticks, spikes to 95 for several ticks, then drops back. The
sequence generator lets users define exact value patterns that model real incidents, making it
the key building block for alert testing scenarios.

### Input state
- Slice 6.0 passes all gates.
- `sonda-core/src/generator/mod.rs` exists with `ValueGenerator` trait, `GeneratorConfig` enum,
  and `create_generator` factory function.
- Existing generators: constant, uniform, sine, sawtooth.

### Specification

**Files to create:**

- `sonda-core/src/generator/sequence.rs`:
  ```rust
  /// A value generator that steps through an explicit sequence of values.
  ///
  /// When `repeat` is true (default), the sequence cycles: `values[tick % len]`.
  /// When `repeat` is false, returns the last value for all ticks beyond the
  /// sequence length. This enables modeling real incident patterns like
  /// `[0, 0, 0, 95, 95, 95, 0, 0]` for a CPU spike.
  pub struct SequenceGenerator {
      values: Vec<f64>,
      repeat: bool,
  }

  impl SequenceGenerator {
      /// Create a new sequence generator.
      ///
      /// # Errors
      /// Returns `SondaError::Config` if `values` is empty.
      pub fn new(values: Vec<f64>, repeat: bool) -> Result<Self, SondaError> { ... }
  }

  impl ValueGenerator for SequenceGenerator {
      fn value(&self, tick: u64) -> f64 {
          // When repeat: values[tick % values.len()]
          // When !repeat: values[min(tick, values.len() - 1)]
      }
  }
  ```

**Files to modify:**

- `sonda-core/src/generator/mod.rs`:
  - Add `pub mod sequence;`
  - Add `GeneratorConfig::Sequence { values: Vec<f64>, repeat: Option<bool> }` variant.
  - Add match arm in `create_generator()` that calls `SequenceGenerator::new(values, repeat.unwrap_or(true))`.
  - Re-export `SequenceGenerator` from the module.

- `sonda-core/CLAUDE.md`:
  - Add `sequence.rs` to the generator section of the module layout.

- `README.md`:
  - Add `sequence` to the "Metric generator types" table with parameters `values: Vec<f64>`,
    `repeat: bool` (optional, default true) and description.
  - Reference the new example file in the Example Scenarios section.

**Files to create (example):**

- `examples/sequence-alert-test.yaml`:
  ```yaml
  name: cpu_spike_test
  rate: 1
  duration: 80s

  generator:
    type: sequence
    values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
    repeat: true

  labels:
    instance: server-01
    job: node

  encoder:
    type: prometheus_text
  sink:
    type: stdout
  ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/generator/sequence.rs` | new |
| `sonda-core/src/generator/mod.rs` | modified |
| `sonda-core/CLAUDE.md` | modified |
| `README.md` | modified |
| `examples/sequence-alert-test.yaml` | new |

### Test criteria
- `SequenceGenerator::new(vec![], true)` returns a config error.
- `SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).value(0)` returns `1.0`.
- `SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).value(3)` returns `1.0` (wraps).
- `SequenceGenerator::new(vec![1.0, 2.0, 3.0], true).value(5)` returns `3.0` (wraps).
- `SequenceGenerator::new(vec![1.0, 2.0], false).value(0)` returns `1.0`.
- `SequenceGenerator::new(vec![1.0, 2.0], false).value(5)` returns `2.0` (clamped).
- `GeneratorConfig::Sequence` deserializes correctly from YAML.
- `create_generator` with sequence config returns a working generator.
- The example YAML file loads and runs: `sonda metrics --scenario examples/sequence-alert-test.yaml --duration 5s`.
- All existing tests continue to pass.

### Review criteria
- `SequenceGenerator` is `Send + Sync`.
- Empty values vector is rejected at construction time, not at runtime.
- The `value()` method has no allocations (pure index arithmetic).
- `repeat` defaults to `true` via `Option<bool>` in the config variant.
- Doc comments on the struct and all public methods.
- No changes to files outside the specification scope.

### UAT criteria
- `sonda metrics --name cpu --rate 1 --duration 16s --value-mode sequence --values 0,0,0,0,0,95,95,95,95,95,0,0,0,0,0,0` produces values matching the input pattern.
  (Note: CLI flag support for `--values` may require adding the flag to the metrics subcommand.
  If not feasible in this slice, UAT validates via YAML scenario only.)
- The example YAML runs for its full duration and produces output with the expected value pattern.
- Output values cycle as expected when the sequence repeats.

---

## Slice 6.2 — Promote E2E Compose & VictoriaMetrics Docs

### Motivation

The E2E test stack (`tests/e2e/docker-compose.yml`) includes VictoriaMetrics, vmagent, Kafka,
Loki, and Grafana. This is exactly what users want for evaluating Sonda, but it is buried in the
test directory and not documented for end-user consumption. This slice creates a user-facing
docker-compose setup and makes the VictoriaMetrics integration prominent in the README.

### Input state
- Slice 6.1 passes all gates.
- `tests/e2e/docker-compose.yml` exists with VictoriaMetrics, Prometheus, vmagent, Kafka,
  Grafana, and Loki services.
- `examples/prometheus-http-push.yaml` exists showing HTTP push to an endpoint.

### Specification

**Files to create:**

1. `examples/docker-compose-victoriametrics.yml`:
   - Simplified version of `tests/e2e/docker-compose.yml` for end-user consumption.
   - Services:
     - `sonda-server`: built from the Dockerfile, port 8080, depends on VictoriaMetrics.
     - `victoriametrics`: VictoriaMetrics single-node, port 8428, with HTTP import and vmui
       enabled. Data volume for persistence.
     - `vmagent`: vmagent relay agent, port 8429, configured to scrape sonda-server (future)
       and forward to VictoriaMetrics. Include a comment noting that protobuf remote write
       relay requires Phase 7.
     - `grafana`: Grafana with anonymous access enabled, port 3000, with VictoriaMetrics
       datasource pre-provisioned.
   - Well-commented YAML explaining each service's role and how to customize.
   - Remove test-specific configuration (test timeouts, test scenario injection, etc.).

2. `examples/victoriametrics-metrics.yaml`:
   - Scenario that pushes Prometheus text metrics to VictoriaMetrics directly.
   - Uses `http_push` sink targeting `http://victoriametrics:8428/api/v1/import/prometheus`.
   - Sine wave generator with labels for realistic data.
   - Content-Type set to `text/plain`.
   - Duration: 120 seconds at 10 events/sec.

**Files to modify:**

3. `README.md`:
   - Add a "VictoriaMetrics Setup" subsection inside the Docker Deployment section:
     - How to start the VM stack: `docker compose -f examples/docker-compose-victoriametrics.yml up -d`
     - How to push metrics via sonda-server or CLI.
     - How to query VictoriaMetrics to verify data: `curl "http://localhost:8428/api/v1/series?match[]={__name__=~'sonda.*'}"`.
     - Known limitation: vmagent relay via protobuf remote write requires Phase 7.
   - Link to `examples/docker-compose-victoriametrics.yml` and `examples/victoriametrics-metrics.yaml`.

### Output files
| File | Status |
|------|--------|
| `examples/docker-compose-victoriametrics.yml` | new |
| `examples/victoriametrics-metrics.yaml` | new |
| `README.md` | modified |

### Test criteria
- `docker compose -f examples/docker-compose-victoriametrics.yml config` validates successfully.
- `examples/victoriametrics-metrics.yaml` is valid YAML that deserializes into a `ScenarioConfig`.
- The compose file includes all four services: sonda-server, victoriametrics, vmagent, grafana.
- The compose file does not reference test-specific configuration or paths.
- All existing tests continue to pass.

### Review criteria
- Compose file is self-contained and does not depend on files in `tests/e2e/`.
- Service names are user-friendly, not test-prefixed.
- VictoriaMetrics and Grafana versions are pinned to specific tags, not `latest`.
- Comments explain the purpose of each service and non-obvious configuration.
- vmagent limitation regarding protobuf remote write is clearly documented.
- No test infrastructure leaks into the user-facing compose file.

### UAT criteria
- `docker compose -f examples/docker-compose-victoriametrics.yml up -d` starts all four services.
- `curl http://localhost:8080/health` returns `{"status":"ok"}` from sonda-server.
- POST `examples/victoriametrics-metrics.yaml` to sonda-server, wait 10 seconds, query
  VictoriaMetrics API to see the metric series.
- Grafana at http://localhost:3000 has VictoriaMetrics datasource configured.
- `docker compose -f examples/docker-compose-victoriametrics.yml down` cleans up.

---

## Slice 6.3 — Scrape Endpoint on sonda-server

### Motivation

Many observability stacks use a pull model where Prometheus or vmagent scrapes metrics from
endpoints. Currently sonda-server only supports push (POST a scenario and it pushes to a sink).
Adding a scrape endpoint (`GET /scenarios/{id}/metrics`) enables pull-based integration: start a
scenario, configure Prometheus to scrape sonda-server, and the generated metrics flow into the
TSDB without configuring a sink. This is the simplest possible integration for Prometheus users.

### Input state
- Slice 6.2 passes all gates.
- `sonda-server/src/routes/scenarios.rs` exists with POST, GET list, GET detail, DELETE, and
  GET stats endpoints.
- `sonda-core` has `PrometheusEncoder`, `MetricEvent`, and `ScenarioHandle` with stats.
- `sonda-core/src/schedule/runner.rs` exists with the main event loop.

### Specification

**Files to modify in sonda-core:**

1. `sonda-core/src/schedule/stats.rs`:
   - Add `recent_metrics: VecDeque<MetricEvent>` field to `ScenarioStats`.
   - Add `max_recent_metrics: usize` constant (default 100).
   - Add `pub fn push_metric(&mut self, event: MetricEvent)` method that pushes to the deque
     and evicts the oldest entry when capacity is exceeded.
   - Add `pub fn drain_recent_metrics(&mut self) -> Vec<MetricEvent>` method that drains and
     returns all buffered metrics.

2. `sonda-core/src/schedule/runner.rs`:
   - After encoding and writing each metric event, clone the `MetricEvent` and push it to the
     stats buffer via `stats.write().push_metric(event)`.
   - This adds one clone per event. The buffer size is bounded (default 100), so memory is
     capped.

3. `sonda-core/src/schedule/handle.rs`:
   - Add `pub fn recent_metrics(&self) -> Vec<MetricEvent>` method that drains the buffer
     from stats.

**Files to modify in sonda-server:**

4. `sonda-server/src/routes/scenarios.rs`:
   - Add `GET /scenarios/{id}/metrics` handler:
     - Looks up the scenario by ID in the shared state.
     - If the scenario is not found, returns 404.
     - If the scenario is a log scenario (no metric events), returns 404 with a message.
     - Drains recent metrics from the handle.
     - Encodes them using `PrometheusEncoder` into a `String`.
     - Returns with `Content-Type: text/plain; version=0.0.4; charset=utf-8`.
   - Accept an optional `limit` query parameter (default 100, max 1000) to control how many
     recent events to return.

5. `sonda-server/src/routes/mod.rs`:
   - Wire the new route: `GET /scenarios/{id}/metrics` -> handler.

**Files to modify (docs):**

6. `README.md`:
   - Add the new endpoint to the API endpoints table:
     `GET /scenarios/{id}/metrics` | 6.3 | Latest metrics in Prometheus text format (scrapeable)
   - Add a "Scrape Integration" subsection to the sonda-server section explaining how to
     configure Prometheus to scrape the endpoint.

7. `sonda-core/CLAUDE.md`:
   - Update `stats.rs` description to mention the recent_metrics buffer.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/schedule/stats.rs` | modified |
| `sonda-core/src/schedule/runner.rs` | modified |
| `sonda-core/src/schedule/handle.rs` | modified |
| `sonda-server/src/routes/scenarios.rs` | modified |
| `sonda-server/src/routes/mod.rs` | modified |
| `README.md` | modified |
| `sonda-core/CLAUDE.md` | modified |

### Test criteria
- `ScenarioStats::push_metric` adds events to the deque.
- When the deque exceeds `max_recent_metrics`, the oldest event is evicted.
- `drain_recent_metrics` returns all buffered events and empties the deque.
- `GET /scenarios/{id}/metrics` returns 404 for a non-existent scenario ID.
- `GET /scenarios/{id}/metrics` returns Prometheus text format for a running metrics scenario.
- `GET /scenarios/{id}/metrics` returns the correct Content-Type header.
- `GET /scenarios/{id}/metrics?limit=10` returns at most 10 events.
- The runner correctly pushes events to the stats buffer.
- All existing tests continue to pass.

### Review criteria
- The stats write lock is held only for the brief `push_metric` call, not during encoding or I/O.
- The deque has a hard capacity bound to prevent unbounded memory growth.
- `MetricEvent::clone()` is acceptable given the bounded buffer size (100 events max by default).
- The scrape endpoint returns valid Prometheus text exposition format.
- Error responses are consistent with existing API conventions (JSON error bodies).
- No changes to files outside the specification scope.

### UAT criteria
- Start sonda-server, POST a metrics scenario, wait 5 seconds.
- `curl http://localhost:8080/scenarios/{id}/metrics` returns Prometheus text with recent samples.
- Configure a `prometheus.yml` with a scrape target pointing to `localhost:8080/scenarios/{id}/metrics`.
- After a scrape interval, Prometheus has the metric series in its TSDB.
- Stop the scenario, scrape endpoint returns empty text (not an error).

---

## Slice 6.4 — Alert Testing Guide

### Motivation

The most compelling use case for Sonda is testing alerting rules. SREs need to know: "If I
push a metric above 90 for 5 minutes, does my alert fire?" Currently there is no guide
explaining how to do this. This slice writes the definitive alert testing guide, making Sonda
the go-to tool for alert validation in CI/CD pipelines.

### Input state
- Slice 6.3 passes all gates.
- Sequence generator exists (Slice 6.1).
- VictoriaMetrics compose stack exists (Slice 6.2).
- Scrape endpoint exists (Slice 6.3).
- All generators, encoders, and sinks are documented in README.

### Specification

**Files to create:**

1. `docs/guide-alert-testing.md`:

   **Section 1: "Generating metrics that cross thresholds"**
   - Explain sine generator math: `offset + amplitude * sin(2 * pi * tick / period_ticks)`.
   - With `amplitude=50, offset=50`, the range is 0-100.
   - A threshold at 90 is crossed when `sin(x) > 0.8`, which happens for a known fraction
     of each period.
   - Show the complete YAML config and expected timeline.

   **Section 2: "Controlling when alerts fire and resolve"**
   - Gap windows: `gap_every: 60s, gap_for: 30s` means the metric disappears for 30s every
     minute.
   - Show how this interacts with alert `for:` duration — the alert resolves during the gap
     if the gap exceeds the evaluation interval.
   - Provide timing diagrams (ASCII art).

   **Section 3: "Testing for: duration behavior"**
   - An alert with `for: 5m` needs the metric above threshold for 5 continuous minutes before
     firing.
   - Show how to use burst windows to sustain high values: burst with high multiplier keeps
     the rate steady while the generator value stays above threshold.
   - Show how the sequence generator (Slice 6.1) enables precise control:
     `values: [95, 95, 95, ..., 95, 10]` with known tick timing.

   **Section 4: "Testing with VictoriaMetrics"**
   - Push to VM directly via `http_push` sink to
     `http://victoriametrics:8428/api/v1/import/prometheus`.
   - Query VM to verify metric values: `curl "http://localhost:8428/api/v1/query?query=cpu_usage"`.
   - Show how to check alert state via VM's alert API if using vmalert.
   - Reference the compose stack from Slice 6.2.

   **Section 5: "Testing recording rules"**
   - Push known constant values (e.g., `value: 42`).
   - Wait for Prometheus/VM evaluation interval (default 1m).
   - Query the recording rule output and verify it matches the expected transformation.
   - Show a concrete example: recording rule `sum(rate(http_requests_total[5m]))` with
     Sonda pushing known values to validate the math.

   **Section 6: "Running in CI/CD"**
   - Docker compose up (stack from Slice 6.2).
   - POST scenarios via sonda-server API.
   - Wait for sufficient data (based on alert `for:` duration + evaluation interval).
   - Query VM/Prometheus API to assert alert state or metric values.
   - Docker compose down.
   - Provide a complete bash script example.

   **Section 7: "Replaying an incident pattern"** (depends on Slice 6.1 sequence generator)
   - Show how to record production metric values to a list.
   - Feed them into the sequence generator.
   - Replay through Sonda to reproduce the exact conditions that triggered (or should have
     triggered) an alert.

**Files to modify:**

2. `README.md`:
   - Add a link in the Features section or a new "Guides" section:
     "See the [Alert Testing Guide](docs/guide-alert-testing.md) for testing alerts and
     recording rules with Sonda."

### Output files
| File | Status |
|------|--------|
| `docs/guide-alert-testing.md` | new |
| `README.md` | modified |

### Test criteria
- `docs/guide-alert-testing.md` contains all seven sections specified above.
- All YAML examples in the guide are valid and parseable.
- All internal links in the guide resolve to existing files or sections.
- README contains a link to the guide.
- All existing tests continue to pass.

### Review criteria
- Mathematical explanations are correct (sine wave threshold crossing math).
- Timing diagrams are clear and accurate.
- CI/CD script example is complete and runnable.
- The guide does not reference features that do not exist.
- All YAML examples are consistent with the current ScenarioConfig schema.
- Writing is clear, practical, and targeted at SREs (not library developers).
- The sequence generator section correctly depends on Slice 6.1 being complete.

### UAT criteria
- An SRE with no prior Sonda experience can follow the guide to set up alert testing.
- The CI/CD script runs end-to-end on a clean machine with Docker.
- At least one YAML example from the guide produces the expected behavior when run with
  `sonda metrics --scenario`.
- The VictoriaMetrics integration steps work with the compose stack from Slice 6.2.

---

## Dependency Graph

```
Slice 6.0 (fix README & documentation drift)
  |
  +-- Slice 6.1 (sequence value generator)
  |     |
  |     +-- Slice 6.2 (VictoriaMetrics compose & docs)
  |           |
  |           +-- Slice 6.3 (scrape endpoint on sonda-server)
  |                 |
  |                 +-- Slice 6.4 (alert testing guide)
```

Slice 6.0 fixes the documentation foundation. Slice 6.1 adds the sequence generator that is
needed by both the VictoriaMetrics examples (6.2) and the alert testing guide (6.4). Slice 6.3
adds the scrape endpoint that the guide references. Slice 6.4 ties everything together into a
comprehensive alert testing workflow.

---

## Post-Phase 6

With Phase 6 complete, Sonda's documentation accurately reflects its capabilities, users can
model explicit incident patterns with the sequence generator, VictoriaMetrics integration is
discoverable and documented, and the alert testing guide provides a complete workflow for SREs.
Future product polish improvements (not designed here):

- **Interactive tutorial** — a `sonda tutorial` subcommand that walks through features step by step.
- **Scenario library** — a curated collection of pre-built scenarios for common use cases (CPU
  spike, memory leak, disk saturation, network partition).
- **Validation mode** — `sonda validate` that checks a scenario YAML for errors without running it.
- **Dry-run mode** — `sonda metrics --dry-run` that shows what would be generated without
  producing output.
- **Grafana dashboard templates** — pre-built dashboards that visualize Sonda-generated metrics
  alongside alert states.
