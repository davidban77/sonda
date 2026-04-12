# Sonda v2 Validation Matrix — Status

Tracks all 162 validation checks for the v2 refactor.
**Every row must pass before the integration branch merges to `main`. No exceptions.**

Sections 1-15 come from the original v2 feature parity matrix (148 rows).
Sections 16-17 are parity bridge tests added to guarantee that every built-in
scenario and pack produces identical output in v2 format (14 rows).

**Legend:** Pass | Fail | Not Tested | N/A

---

## 1. Signal Types (6 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 1.1 | Metric signals (gauges/counters) | Not Tested | PR 6 | |
| 1.2 | Log signals (template) | Not Tested | PR 6 | |
| 1.3 | Log signals (replay) | Not Tested | PR 6 | |
| 1.4 | Histogram signals | Not Tested | PR 6 | |
| 1.5 | Summary signals | Not Tested | PR 6 | |
| 1.6 | Mixed signal types in one file | Not Tested | PR 6 | |

## 2. Metric Generators — Core (10 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 2.1 | constant generator | Not Tested | PR 6 | |
| 2.2 | sine generator | Not Tested | PR 6 | |
| 2.3 | sawtooth generator | Not Tested | PR 6 | |
| 2.4 | uniform generator | Not Tested | PR 6 | |
| 2.5 | sequence generator | Not Tested | PR 6 | |
| 2.6 | step generator | Not Tested | PR 6 | |
| 2.7 | spike generator | Not Tested | PR 6 | |
| 2.8 | csv_replay generator | Not Tested | PR 6 | |
| 2.9 | CSV auto-discovery (Grafana headers) | Not Tested | PR 6 | |
| 2.10 | CSV per-column labels | Not Tested | PR 6 | |

## 3. Operational Aliases (7 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 3.1 | steady alias | Not Tested | PR 6 | |
| 3.2 | flap alias | Not Tested | PR 6 | |
| 3.3 | saturation alias | Not Tested | PR 6 | |
| 3.4 | leak alias | Not Tested | PR 6 | |
| 3.5 | degradation alias | Not Tested | PR 6 | |
| 3.6 | spike_event alias | Not Tested | PR 6 | |
| 3.7 | Custom up/down values for flap | Not Tested | PR 6 | |

## 4. Histogram & Summary Generators (8 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 4.1 | Exponential distribution | Not Tested | PR 6 | |
| 4.2 | Normal distribution | Not Tested | PR 6 | |
| 4.3 | Uniform distribution | Not Tested | PR 6 | |
| 4.4 | Custom buckets | Not Tested | PR 6 | |
| 4.5 | Custom quantiles | Not Tested | PR 6 | |
| 4.6 | observations_per_tick | Not Tested | PR 6 | |
| 4.7 | mean_shift_per_sec | Not Tested | PR 6 | |
| 4.8 | Cumulative bucket counters | Not Tested | PR 6 | |

## 5. Encoders (8 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 5.1 | prometheus_text | Not Tested | PR 6 | |
| 5.2 | influx_lp with custom field_key | Not Tested | PR 6 | |
| 5.3 | json_lines | Not Tested | PR 6 | |
| 5.4 | syslog (logs only) | Not Tested | PR 6 | |
| 5.5 | remote_write | Not Tested | PR 6 | |
| 5.6 | otlp | Not Tested | PR 6 | |
| 5.7 | precision field | Not Tested | PR 6 | |
| 5.8 | Default encoder per signal type | Not Tested | PR 3 | Defaults resolution |

## 6. Sinks (12 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 6.1 | stdout | Not Tested | PR 6 | |
| 6.2 | file | Not Tested | PR 6 | |
| 6.3 | tcp | Not Tested | PR 6 | |
| 6.4 | udp | Not Tested | PR 6 | |
| 6.5 | http_push with batch_size | Not Tested | PR 6 | |
| 6.6 | http_push custom headers | Not Tested | PR 6 | |
| 6.7 | remote_write with batch_size | Not Tested | PR 6 | |
| 6.8 | kafka with TLS + SASL | Not Tested | PR 6 | |
| 6.9 | loki with labels + batch_size | Not Tested | PR 6 | |
| 6.10 | otlp_grpc | Not Tested | PR 6 | |
| 6.11 | --output CLI shorthand | Not Tested | PR 7 | |
| 6.12 | Retry with backoff | Not Tested | PR 6 | |

## 7. Scheduling & Windows (11 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 7.1 | Gap windows | Not Tested | PR 6 | |
| 7.2 | Burst windows | Not Tested | PR 6 | |
| 7.3 | Gap overrides burst | Not Tested | PR 6 | |
| 7.4 | Cardinality spikes (counter) | Not Tested | PR 6 | |
| 7.5 | Cardinality spikes (random) | Not Tested | PR 6 | |
| 7.6 | Multiple cardinality spikes | Not Tested | PR 6 | |
| 7.7 | Gap suppresses cardinality spikes | Not Tested | PR 6 | |
| 7.8 | Jitter | Not Tested | PR 6 | |
| 7.9 | Dynamic labels (counter strategy) | Not Tested | PR 6 | |
| 7.10 | Dynamic labels (values list) | Not Tested | PR 6 | |
| 7.11 | Multiple dynamic labels | Not Tested | PR 6 | |

## 8. Multi-Scenario Features (6 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 8.1 | phase_offset | Not Tested | PR 6 | |
| 8.2 | clock_group | Not Tested | PR 5/6 | |
| 8.3 | Concurrent execution | Not Tested | PR 6 | |
| 8.4 | Independent completion | Not Tested | PR 6 | |
| 8.5 | --dry-run on multi-scenario | Not Tested | PR 7 | Enhanced for v2 |
| 8.6 | Aggregate summary at end | Not Tested | PR 6 | |

## 9. Pack Features (15 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 9.1 | Run pack by name | Not Tested | PR 4/7 | |
| 9.2 | Run pack from YAML | Not Tested | PR 4/6 | |
| 9.3 | Pack search path | Not Tested | PR 4 | |
| 9.4 | Pack by file path | Not Tested | PR 4 | |
| 9.5 | Per-metric overrides (generator) | Not Tested | PR 4 | |
| 9.6 | Per-metric overrides (labels) | Not Tested | PR 4 | |
| 9.7 | Unknown override key → error | Not Tested | PR 4 | |
| 9.8 | Label merge order | Not Tested | PR 4 | |
| 9.9 | Pack --dry-run | Not Tested | PR 7 | Enhanced |
| 9.10 | List packs | Not Tested | PR 7 | |
| 9.11 | Show pack YAML | Not Tested | PR 7 | |
| 9.12 | Custom pack definitions | Not Tested | PR 4 | |
| 9.13 | Built-in: telegraf_snmp_interface | Not Tested | PR 8 | |
| 9.14 | Built-in: node_exporter_cpu | Not Tested | PR 8 | |
| 9.15 | Built-in: node_exporter_memory | Not Tested | PR 8 | |

## 10. Story Features — Absorbed into v2 (15 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 10.1 | after: flap < threshold | Not Tested | PR 5 | |
| 10.2 | after: saturation > threshold | Not Tested | PR 5 | |
| 10.3 | after: degradation > threshold | Not Tested | PR 5 | |
| 10.4 | after: spike < threshold | Not Tested | PR 5 | |
| 10.5 | Transitive chains (A → B → C) | Not Tested | PR 5 | |
| 10.6 | Circular dependency detection | Not Tested | PR 5 | |
| 10.7 | Unknown ref → error | Not Tested | PR 5 | |
| 10.8 | Out-of-range threshold → error | Not Tested | PR 5 | |
| 10.9 | Threshold true at t=0 → error | Not Tested | PR 5 | |
| 10.10 | sine/steady in after → error | Not Tested | PR 5 | |
| 10.11 | Shared clock_group | Not Tested | PR 5 | |
| 10.12 | Shared labels across signals | Not Tested | PR 3 | via defaults |
| 10.13 | Per-signal label overrides | Not Tested | PR 3 | |
| 10.14 | Per-signal rate/duration override | Not Tested | PR 3 | |
| 10.15 | Per-signal encoder/sink override | Not Tested | PR 3 | |

## 11. New v2 Features (18 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 11.1 | version: 2 field | Pass | PR 2 | parse_v2 validates version |
| 11.2 | defaults: block | Not Tested | PR 3 | Parsed in PR 2, resolution in PR 3 |
| 11.3 | Entry-level overrides defaults | Not Tested | PR 3 | |
| 11.4 | id field on entries | Pass | PR 2 | Uniqueness + format validated |
| 11.5 | Single-signal shorthand | Pass | PR 2 | Flat files wrapped automatically |
| 11.6 | Pack inside scenarios: list | Not Tested | PR 4 | Parsed in PR 2, expansion in PR 4 |
| 11.7 | Dotted after ref into pack | Not Tested | PR 5 | |
| 11.8 | Auto-generated pack IDs | Not Tested | PR 4 | |
| 11.9 | delay in after clause | Not Tested | PR 5 | Parsed in PR 2 |
| 11.10 | Structured after validation | Pass | PR 2 | AfterOp enum, serde validation |
| 11.11 | Cross-signal-type after | Not Tested | PR 5 | |
| 11.12 | after on pack override | Not Tested | PR 4/5 | |
| 11.13 | Pack entry-level after propagation | Not Tested | PR 4/5 | |
| 11.14 | after + phase_offset sum | Not Tested | PR 5 | |
| 11.15 | Clock group auto-assignment | Not Tested | PR 5 | |
| 11.16 | Conflicting clock_group → error | Not Tested | PR 5 | |
| 11.17 | after with step generator | Not Tested | PR 5 | |
| 11.18 | after with sequence generator | Not Tested | PR 5 | |

## 12. CLI Commands (22 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 12.1 | Run scenario file | Not Tested | PR 6/7 | |
| 12.2 | One-off metric | Not Tested | PR 7 | Unchanged |
| 12.3 | One-off logs | Not Tested | PR 7 | Unchanged |
| 12.4 | One-off histogram | Not Tested | PR 7 | Unchanged |
| 12.5 | One-off summary | Not Tested | PR 7 | Unchanged |
| 12.6 | @name shorthand | Not Tested | PR 7 | Unchanged |
| 12.7 | --dry-run | Not Tested | PR 7 | Enhanced for v2 |
| 12.8 | --quiet / -q | Not Tested | PR 7 | Unchanged |
| 12.9 | --verbose / -v | Not Tested | PR 7 | Unchanged |
| 12.10 | --scenario-path | Not Tested | PR 7 | |
| 12.11 | --pack-path | Not Tested | PR 7 | |
| 12.12 | List built-in scenarios | Not Tested | PR 7 | catalog list |
| 12.13 | List packs | Not Tested | PR 7 | catalog list --type pack |
| 12.14 | Show catalog item | Not Tested | PR 7 | catalog show |
| 12.15 | Run catalog item | Not Tested | PR 7 | catalog run |
| 12.16 | Filter by category | Not Tested | PR 7 | |
| 12.17 | JSON output | Not Tested | PR 7 | |
| 12.18 | sonda import (CSV) | Not Tested | PR 7 | Unchanged |
| 12.19 | sonda init | Not Tested | PR 7 | Enhanced for v2 |
| 12.20 | CLI overrides on scenario | Not Tested | PR 7 | |
| 12.21 | sonda story --file | Not Tested | PR 9 | Removed/aliased |
| 12.22 | sonda packs run with --label | Not Tested | PR 7 | |

## 13. Server API (9 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 13.1 | Health check | Not Tested | PR 9 | Unchanged |
| 13.2 | Start scenario (YAML body) | Not Tested | PR 9 | |
| 13.3 | Start scenario (JSON body) | Not Tested | PR 9 | |
| 13.4 | List running | Not Tested | PR 9 | Unchanged |
| 13.5 | Inspect scenario | Not Tested | PR 9 | Unchanged |
| 13.6 | Stop scenario | Not Tested | PR 9 | Unchanged |
| 13.7 | Live stats | Not Tested | PR 9 | Unchanged |
| 13.8 | Scrape endpoint | Not Tested | PR 9 | Unchanged |
| 13.9 | v2 multi-scenario response | Not Tested | PR 9 | New |

## 14. Status Output & UX (9 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 14.1 | Start banner | Not Tested | PR 6 | |
| 14.2 | Stop banner | Not Tested | PR 6 | |
| 14.3 | Live progress (TTY) | Not Tested | PR 6 | |
| 14.4 | Live progress (non-TTY) | Not Tested | PR 6 | |
| 14.5 | Multi-scenario numbering | Not Tested | PR 6 | |
| 14.6 | Color behavior | Not Tested | PR 6 | |
| 14.7 | Gap/burst/spike tags | Not Tested | PR 6 | |
| 14.8 | Aggregate summary | Not Tested | PR 6 | |
| 14.9 | Ctrl+C graceful shutdown | Not Tested | PR 6 | |

## 15. Deployment (7 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 15.1 | Docker image | Not Tested | PR 9 | |
| 15.2 | Docker Compose stack | Not Tested | PR 9 | |
| 15.3 | VictoriaMetrics compose stack | Not Tested | PR 9 | |
| 15.4 | Helm chart | Not Tested | PR 9 | |
| 15.5 | Scenario ConfigMap injection | Not Tested | PR 8/9 | |
| 15.6 | Static musl binary | Not Tested | PR 9 | |
| 15.7 | E2E test suite | Not Tested | PR 9 | |

---

## PARITY BRIDGE TESTS

These sections are **mandatory merge blockers**. They verify that every existing built-in
scenario, pack, and story produces identical output when converted to v2 format.

Testing has two levels per file:
- **Compile parity**: v1 and v2 files compile to identical `Vec<ScenarioEntry>` JSON snapshots
- **Runtime parity**: v1 and v2 files produce identical stdout output (deterministic, seeded, limited ticks)

Both levels must pass. A compile-only pass is not sufficient — runtime execution must match.

## 16. Built-in Scenario Parity (12 rows)

For each built-in scenario, a hand-written v2 equivalent is created. Both are compiled
and executed. Output must be byte-identical (for deterministic generators) or
structurally identical (for non-deterministic generators like uniform).

| # | Scenario File | Compile Parity | Runtime Parity | PR | Notes |
|---|--------------|----------------|----------------|-----|-------|
| 16.1 | cpu-spike.yaml | Not Tested | Not Tested | PR 6 | Single metric, sine-based |
| 16.2 | memory-leak.yaml | Not Tested | Not Tested | PR 6 | Single metric, leak alias |
| 16.3 | disk-fill.yaml | Not Tested | Not Tested | PR 6 | Single metric, saturation alias |
| 16.4 | latency-degradation.yaml | Not Tested | Not Tested | PR 6 | Single metric, degradation alias |
| 16.5 | error-rate-spike.yaml | Not Tested | Not Tested | PR 6 | Single metric, spike_event alias |
| 16.6 | interface-flap.yaml | Not Tested | Not Tested | PR 6 | Single metric, flap alias |
| 16.7 | network-link-failure.yaml | Not Tested | Not Tested | PR 6 | Multi-signal, phase_offset, clock_group |
| 16.8 | steady-state.yaml | Not Tested | Not Tested | PR 6 | Single metric, steady alias |
| 16.9 | log-storm.yaml | Not Tested | Not Tested | PR 6 | Log signal, template generator |
| 16.10 | cardinality-explosion.yaml | Not Tested | Not Tested | PR 6 | Cardinality spikes, dynamic labels |
| 16.11 | histogram-latency.yaml | Not Tested | Not Tested | PR 6 | Histogram signal |
| 16.12 | link-failover.yaml (story) | Not Tested | Not Tested | PR 6 | Story → v2 with after: clauses |

## 17. Built-in Pack Parity (3 rows)

For each built-in pack, a v2 scenario file is created that uses the pack inside
`scenarios:`. The expanded output must match the current `sonda packs run` output.

| # | Pack | Compile Parity | Runtime Parity | PR | Notes |
|---|------|----------------|----------------|-----|-------|
| 17.1 | telegraf-snmp-interface.yaml | Not Tested | Not Tested | PR 4/6 | 5 metrics, network category |
| 17.2 | node-exporter-cpu.yaml | Not Tested | Not Tested | PR 4/6 | 8 metrics, step sizes sum to 1.0 |
| 17.3 | node-exporter-memory.yaml | Not Tested | Not Tested | PR 4/6 | 5 metrics, 16 GiB defaults |
