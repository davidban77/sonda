# Guides

Task-shaped pages: pick the one that matches what you are trying to do.

The [**Tutorial**](tutorial.md) is the guided tour through every part of Sonda.
The rest of this section is organized by what you are testing: alert rules,
ingest pipelines, network telemetry, real-data replay, or operational scale.

## Tutorial

A seven-page walk through generators, encoders, sinks, log generation, scheduling,
multi-scenario runs, and the Server API. Start here if you have just installed
Sonda and want to see what every knob does.

- [**Tutorial overview**](tutorial.md) -- the table of contents and what each page covers.

## Catalog and packs

Pre-built scenarios you can run instantly, and the building blocks that compose them.

- [**Built-in Scenarios**](scenarios.md) -- the curated catalog. `sonda catalog list`
  to browse, `sonda catalog run <name>` to launch.
- [**Dynamic Labels**](dynamic-labels.md) -- per-event label values via `${...}`
  placeholders driven by RNG, sequences, or pools.
- [**Example Scenarios**](examples.md) -- the YAML files under `examples/` and what
  each one demonstrates.
- [**Metric Packs**](metric-packs.md) -- inline pack expansion inside `scenarios:`
  for fan-out across instances, services, or paths.

## Alert testing

Triggering, resolving, and validating alert rules with the right metric shape.

- [**Alert testing overview**](alert-testing.md) -- the entry point. Maps each alert
  pattern to the right generator and sub-page.
- [**Threshold and `for:` duration**](alert-testing-thresholds.md) -- sine, sequence,
  and constant for crossing thresholds with predictable timing.
- [**Resolution and recovery**](alert-testing-resolution.md) -- gap windows that
  drop the metric so resolution fires.
- [**Compound and correlated alerts**](alert-testing-correlation.md) -- `phase_offset`
  and `clock_group` for `A AND B` rules.
- [**Cardinality explosion alerts**](alert-testing-cardinality.md) -- `cardinality_spikes`
  for series-count guardrails.
- [**Replaying recorded incidents**](alert-testing-replay.md) -- `sequence` for short
  patterns, `csv_replay` for production exports.
- [**Histogram and summary alerts**](histogram-alerts.md) -- bucket-based and
  quantile-based latency alerts.
- [**Recording rules**](recording-rules.md) -- pushing known constants to verify
  computed outputs.
- [**Alerting pipeline**](alerting-pipeline.md) -- end-to-end with vmalert,
  Alertmanager, and a webhook receiver.
- [**CI alert validation**](ci-alert-validation.md) -- catching broken rules in CI
  before they reach production.

## Pipelines and scale

Validating ingest changes, capacity, and end-to-end backend behavior.

- [**Pipeline validation**](pipeline-validation.md) -- smoke-testing relabel rules,
  encoder switches, and routing changes.
- [**Synthetic monitoring**](synthetic-monitoring.md) -- always-on `sonda-server` in
  Kubernetes for blackbox-style probes.
- [**Capacity planning**](capacity-planning.md) -- sizing a backend before you cut
  over real traffic.
- [**E2E testing**](e2e-testing.md) -- the canonical start-stack, push, query loop
  with VictoriaMetrics, Loki, Kafka, and OTLP.

## Network telemetry

Modeling network devices and validating automation responses.

- [**Network device telemetry**](network-device-telemetry.md) -- routers, switches,
  interface counters, link failover cascades.
- [**Network automation testing**](network-automation-testing.md) -- exercising
  remediation flows that react to network alerts.

## Importing real data

Turning recorded series into reusable scenarios.

- [**CSV import**](csv-import.md) -- `sonda import` for pattern detection and
  scenario generation from CSV.
- [**Grafana CSV replay**](grafana-csv-replay.md) -- the `csv_replay` generator for
  bit-for-bit reproduction.

## Troubleshooting

- [**Troubleshooting**](troubleshooting.md) -- diagnostics for connection refused,
  empty backends, schema mismatches, and the `localhost` trap.
