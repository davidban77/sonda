# Sonda v2 Validation Matrix — Status

Tracks all 178 validation checks for the v2 refactor.
**Every row must pass before the integration branch merges to `main`. No exceptions.**

Sections 1-15 come from the original v2 feature parity matrix (163 rows).
Sections 16-17 are parity bridge tests added to guarantee that every built-in
scenario and pack produces identical output in v2 format (15 rows).

**Legend:** Pass | Fail | Not Tested | N/A

---

## 1. Signal Types (6 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 1.1 | Metric signals (gauges/counters) | Pass | PR 6 | |
| 1.2 | Log signals (template) | Pass | PR 6 | |
| 1.3 | Log signals (replay) | Pass | PR 6 | |
| 1.4 | Histogram signals | Pass | PR 6 | |
| 1.5 | Summary signals | Pass | PR 6 | |
| 1.6 | Mixed signal types in one file | Pass | PR 6 | |

## 2. Metric Generators — Core (10 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 2.1 | constant generator | Pass | PR 6 | |
| 2.2 | sine generator | Pass | PR 6 | |
| 2.3 | sawtooth generator | Pass | PR 6 | |
| 2.4 | uniform generator | Pass | PR 6 | |
| 2.5 | sequence generator | Pass | PR 6 | |
| 2.6 | step generator | Pass | PR 6 | |
| 2.7 | spike generator | Pass | PR 6 | |
| 2.8 | csv_replay generator | Pass | PR 6 | |
| 2.9 | CSV auto-discovery (Grafana headers) | Pass | PR 6 | |
| 2.10 | CSV per-column labels | Pass | PR 6 | |

## 3. Operational Aliases (7 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 3.1 | steady alias | Pass | PR 6 | |
| 3.2 | flap alias | Pass | PR 6 | |
| 3.3 | saturation alias | Pass | PR 6 | |
| 3.4 | leak alias | Pass | PR 6 | |
| 3.5 | degradation alias | Pass | PR 6 | |
| 3.6 | spike_event alias | Pass | PR 6 | |
| 3.7 | Custom up/down values for flap | Pass | PR 6 | |

## 4. Histogram & Summary Generators (8 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 4.1 | Exponential distribution | Pass | PR 6 | |
| 4.2 | Normal distribution | Pass | PR 6 | |
| 4.3 | Uniform distribution | Pass | PR 6 | |
| 4.4 | Custom buckets | Pass | PR 6 | |
| 4.5 | Custom quantiles | Pass | PR 6 | |
| 4.6 | observations_per_tick | Pass | PR 6 | |
| 4.7 | mean_shift_per_sec | Pass | PR 6 | |
| 4.8 | Cumulative bucket counters | Pass | PR 6 | |

## 5. Encoders (8 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 5.1 | prometheus_text | Pass | PR 6 | |
| 5.2 | influx_lp with custom field_key | Pass | PR 6 | |
| 5.3 | json_lines | Pass | PR 6 | |
| 5.4 | syslog (logs only) | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 5.5 | remote_write | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 5.6 | otlp | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 5.7 | precision field | Pass | PR 6 | |
| 5.8 | Default encoder per signal type | Pass | PR 3 | Defaults resolution: metrics/histogram/summary → prometheus_text, logs → json_lines |

## 6. Sinks (12 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 6.1 | stdout | Pass | PR 6 | |
| 6.2 | file | Not Tested | PR 8 | Smoke-test scope |
| 6.3 | tcp | Not Tested | PR 8 | Smoke-test scope |
| 6.4 | udp | Not Tested | PR 8 | Smoke-test scope |
| 6.5 | http_push with batch_size | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 6.6 | http_push custom headers | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 6.7 | remote_write with batch_size | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 6.8 | kafka with TLS + SASL | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 6.9 | loki with labels + batch_size | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 6.10 | otlp_grpc | Not Tested | PR 8 | Feature-gated; smoke-test scope |
| 6.11 | --output CLI shorthand | Pass | PR 7 | Available on `sonda metrics`, `sonda logs` (existing); added to `sonda run` and `sonda catalog run` (new) |
| 6.12 | Retry with backoff | Pass | PR 6 | Translator-semantic: `RetryConfig` threads through for `tcp` sink (`v2_translator_semantics::row_6_12_retry_config`) |

## 7. Scheduling & Windows (11 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 7.1 | Gap windows | Pass | PR 6 | |
| 7.2 | Burst windows | Pass | PR 6 | |
| 7.3 | Gap overrides burst | Pass | PR 6 | |
| 7.4 | Cardinality spikes (counter) | Pass | PR 6 | |
| 7.5 | Cardinality spikes (random) | Pass | PR 6 | |
| 7.6 | Multiple cardinality spikes | Pass | PR 6 | |
| 7.7 | Gap suppresses cardinality spikes | Pass | PR 6 | |
| 7.8 | Jitter | Pass | PR 6 | |
| 7.9 | Dynamic labels (counter strategy) | Pass | PR 6 | |
| 7.10 | Dynamic labels (values list) | Pass | PR 6 | |
| 7.11 | Multiple dynamic labels | Pass | PR 6 | |

## 8. Multi-Scenario Features (6 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 8.1 | phase_offset | Pass | PR 6 | |
| 8.2 | clock_group | Pass | PR 5/6/7 | Compile-time assignment (PR 5); runtime carry-through (PR 6); CLI observability — start banner adds `clock_group: <name> (auto)` line via `status::format_clock_group` (PR 7) |
| 8.3 | Concurrent execution | Pass | PR 6 | |
| 8.4 | Independent completion | Pass | PR 6 | |
| 8.5 | --dry-run on multi-scenario | Pass | PR 7 | Spec §5 pretty output via `dry_run::write_text` for v2 files; v1 files unchanged |
| 8.6 | Aggregate summary at end | Pass | PR 7 | `status::print_summary_by_clock_group` fires when ≥2 distinct groups present; flat summary otherwise |

## 9. Pack Features (15 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 9.1 | Run pack by name | Pass | PR 4 | Compile-level: name lookup via PackResolver; CLI wiring lands in PR 7 |
| 9.2 | Run pack from YAML | Pass | PR 4 | Compile-level: v2 YAML with `scenarios: - pack:` parses and expands |
| 9.3 | Pack search path | Pass | PR 4 | PackResolver trait abstraction; filesystem search path wiring lands in PR 7 CLI |
| 9.4 | Pack by file path | Pass | PR 4 | classify_pack_reference routes `./x.yaml` or `/abs/x.yaml` to FilePath origin |
| 9.5 | Per-metric overrides (generator) | Pass | PR 4 | expand::select_pack_metric_generator picks override > spec > constant(0) |
| 9.6 | Per-metric overrides (labels) | Pass | PR 4 | Override labels sit at precedence level 7 in the merge |
| 9.7 | Unknown override key → error | Pass | PR 4 | ExpandError::UnknownOverrideKey lists key + pack + valid metrics |
| 9.8 | Label merge order | Pass | PR 4 | Five-level precedence chain validated by `label_precedence_chain_applied_in_order` |
| 9.9 | Pack --dry-run | Pass | PR 7 | v2 pack-backed scenarios expand into per-metric blocks in dry-run output; covered by `dry_run_format::dry_run_pack_backed_expands_sub_signals` |
| 9.10 | List packs | Pass | PR 7 | `sonda catalog list --type pack` |
| 9.11 | Show pack YAML | Pass | PR 7 | `sonda catalog show <pack-name>` |
| 9.12 | Custom pack definitions | Pass | PR 4 | InMemoryPackResolver accepts any MetricPackDef; classify_pack_reference supports file paths |
| 9.13 | Built-in: telegraf_snmp_interface | Not Tested | PR 8 | |
| 9.14 | Built-in: node_exporter_cpu | Not Tested | PR 8 | |
| 9.15 | Built-in: node_exporter_memory | Not Tested | PR 8 | |

## 10. Story Features — Absorbed into v2 (15 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 10.1 | after: flap < threshold | Pass | PR 5 | compile_after: flap_crossing returns up_duration; covered by simple-chain fixture + unit tests |
| 10.2 | after: saturation > threshold | Pass | PR 5 | linear-interp crossing; saturation target fixture + saturation_greater_than_sets_offset unit test |
| 10.3 | after: degradation > threshold | Pass | PR 5 | degradation desugars to sawtooth; transitive-chain fixture asserts the 60+92.307s sum |
| 10.4 | after: spike < threshold | Pass | PR 5 | spike_event crossing at spike_duration; spike_less_than_sets_spike_duration unit test |
| 10.5 | Transitive chains (A → B → C) | Pass | PR 5 | Kahn topo sort; transitive-chain fixture validates A→B→C accumulation |
| 10.6 | Circular dependency detection | Pass | PR 5 | DFS gray/black; invalid-compile-cycle fixture returns CircularDependency variant |
| 10.7 | Unknown ref → error | Pass | PR 5 | UnknownRef with available-id list; invalid-compile-unknown-ref fixture |
| 10.8 | Out-of-range threshold → error | Pass | PR 5 | OutOfRangeThreshold variant; invalid-compile-out-of-range fixture |
| 10.9 | Threshold true at t=0 → error | Pass | PR 5 | AmbiguousAtT0 variant; invalid-compile-ambiguous-at-t0 fixture (spike_event > 50) |
| 10.10 | sine/steady in after → error | Pass | PR 5 | UnsupportedGenerator variant; unit tests cover sine/steady/uniform/csv_replay |
| 10.11 | Shared clock_group | Pass | PR 5 | auto-assigned `chain_{lowest_lex_id}` for dependency chain members |
| 10.12 | Shared labels across signals | Pass | PR 3 | defaults.labels flows into every entry |
| 10.13 | Per-signal label overrides | Pass | PR 3 | entry labels win on conflict, union otherwise |
| 10.14 | Per-signal rate/duration override | Pass | PR 3 | entry rate/duration win over defaults |
| 10.15 | Per-signal encoder/sink override | Pass | PR 3 | entry encoder/sink win over defaults |

## 11. New v2 Features (18 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 11.1 | version: 2 field | Pass | PR 2 | parse() validates version |
| 11.2 | defaults: block | Pass | PR 3 | resolved into every entry by normalize() |
| 11.3 | Entry-level overrides defaults | Pass | PR 3 | entry values win over defaults across all precedence-eligible fields |
| 11.4 | id field on entries | Pass | PR 2 | Uniqueness + format validated |
| 11.5 | Single-signal shorthand | Pass | PR 2 | Flat files wrapped automatically |
| 11.6 | Pack inside scenarios: list | Pass | PR 4 | Parsed in PR 2, expansion in PR 4 with one ExpandedEntry per pack metric |
| 11.7 | Dotted after ref into pack | Pass | PR 5 | `{entry}.{metric}` resolves; bare form against duplicate-name packs surfaces AmbiguousSubSignalRef with candidate list |
| 11.8 | Auto-generated pack IDs | Pass | PR 4 | Deterministic `{pack_def_name}_{entry_index}` when `id` absent; duplicate metric names receive `"#{spec_index}"` suffix on the sub-signal id; post-expansion uniqueness check guards against user/auto id collisions |
| 11.9 | delay in after clause | Pass | PR 5 | delay parsed via parse_duration and added to crossing time; phase-offset-and-delay fixture asserts 10s + 60s + 15s = 85s |
| 11.10 | Structured after validation | Pass | PR 2 | AfterOp enum, serde validation |
| 11.11 | Cross-signal-type after | Pass | PR 5 | dependent can be metrics/logs/histogram/summary; target must be metrics (NonMetricsTarget error otherwise) |
| 11.12 | after on pack override | Pass | PR 4/5 | PR 4 propagation + PR 5 resolution; compile_after_on_pack_override_applies_per_metric asserts override `after` lands on the specific sub-signal |
| 11.13 | Pack entry-level after propagation | Pass | PR 4/5 | PR 4 propagation + PR 5 resolution; compile_after_pack_entry_level_propagates_to_all_sub_signals asserts every sub-signal inherits the same resolved offset |
| 11.14 | after + phase_offset sum | Pass | PR 5 | total = user_phase_offset + Σ crossing_time + Σ delay; phase-offset-and-delay fixture |
| 11.15 | Clock group auto-assignment | Pass | PR 5 | connected components receive `chain_{lowest_lex_id}` when no explicit value set |
| 11.16 | Conflicting clock_group → error | Pass | PR 5 | ConflictingClockGroup variant; invalid-compile-conflicting-clock-group fixture |
| 11.17 | after with step generator | Pass | PR 5 | step_crossing_secs uses ceil((threshold-start)/step_size) × tick interval; step-target fixture + unit tests |
| 11.18 | after with sequence generator | Pass | PR 5 | sequence_crossing_secs finds first tick where value crosses threshold; sequence-target fixture + unit tests |

## 12. CLI Commands (22 rows)

| # | Capability | Status | PR | Notes |
|---|-----------|--------|-----|-------|
| 12.1 | Run scenario file | Pass | PR 7 | `scenario_loader::load_scenario_entries` dispatches v1 (flat single-scenario / multi-scenario / pack-scenario) and v2 from `version:` per spec §6.1 |
| 12.2 | One-off metric | Pass | PR 7 | Unchanged path; subprocess-tested via existing `quiet_flag.rs` etc. |
| 12.3 | One-off logs | Pass | PR 7 | Unchanged |
| 12.4 | One-off histogram | Pass | PR 7 | Unchanged |
| 12.5 | One-off summary | Pass | PR 7 | Unchanged |
| 12.6 | @name shorthand | Pass | PR 7 | `scenario_loader` reuses `resolve_scenario_source`; covered by unit tests |
| 12.7 | --dry-run | Pass | PR 7 | v2 pretty output (spec §5) + JSON DTO via `--format=json` |
| 12.8 | --quiet / -q | Pass | PR 7 | Unchanged; `sonda/tests/quiet_flag.rs` still green |
| 12.9 | --verbose / -v | Pass | PR 7 | Unchanged |
| 12.10 | --scenario-path | Pass | PR 7 | Honored by both v1 and v2 dispatch (covered in `cli_catalog`) |
| 12.11 | --pack-path | Pass | PR 7 | `FilesystemPackResolver` reads from CLI pack catalog |
| 12.12 | List built-in scenarios | Pass | PR 7 | `sonda catalog list` (also `--type scenario`) |
| 12.13 | List packs | Pass | PR 7 | `sonda catalog list --type pack` |
| 12.14 | Show catalog item | Pass | PR 7 | `sonda catalog show <name>` (scenario or pack) |
| 12.15 | Run catalog item | Pass | PR 7 | `sonda catalog run <name>` (scenario or pack) |
| 12.16 | Filter by category | Pass | PR 7 | `--category` (case-sensitive) on `catalog list` |
| 12.17 | JSON output | Pass | PR 7 | `--json` on `catalog list`; `--format=json` on `--dry-run` |
| 12.18 | sonda import (CSV) | Pass | PR 7 | Unchanged; `sonda/tests/csv_import.rs` still green |
| 12.19 | sonda init | Pass | PR 7 | All `--signal-type` variants emit v2 YAML; `init_v2_output.rs` round-trips through `compile_scenario_file` |
| 12.20 | CLI overrides on scenario | Pass | PR 7 | `apply_run_overrides` applies duration/rate/sink/encoder/labels to every entry uniformly |
| 12.21 | sonda story --file | Not Tested | PR 9 | Hidden via `#[command(hide = true)]`; still callable as the row-16.12 oracle |
| 12.22 | sonda packs run with --label | Pass | PR 7 | Equivalent surface via `sonda catalog run <pack> --label k=v`; legacy `sonda packs run` retained (hidden) |

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
| 14.1 | Start banner | Pass | PR 7 | `status::print_start` extended with optional `clock_group:` section |
| 14.2 | Stop banner | Pass | PR 7 | Unchanged from v1 — covered end-to-end by `cli_catalog::catalog_run_scenario_succeeds` |
| 14.3 | Live progress (TTY) | Pass | PR 7 | Unchanged |
| 14.4 | Live progress (non-TTY) | Pass | PR 7 | Unchanged |
| 14.5 | Multi-scenario numbering | Pass | PR 7 | `[i/N]` prefix on banners (existing) |
| 14.6 | Color behavior | Pass | PR 7 | Unchanged |
| 14.7 | Gap/burst/spike tags | Pass | PR 7 | Unchanged from v1 surface |
| 14.8 | Aggregate summary | Pass | PR 7 | Two paths: flat `print_summary` + `print_summary_by_clock_group` (≥2 distinct groups) |
| 14.9 | Ctrl+C graceful shutdown | Pass | PR 7 | Unchanged from v1 surface |

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
| 16.1 | cpu-spike.yaml | Pass | Pass | PR 6 | Single metric, sine-based; byte-equal |
| 16.2 | memory-leak.yaml | Pass | Pass | PR 6 | Single metric, leak alias; byte-equal |
| 16.3 | disk-fill.yaml | Pass | Pass | PR 6 | Single metric, saturation alias; byte-equal |
| 16.4 | latency-degradation.yaml | Pass | Pass | PR 6 | Single metric, degradation alias; byte-equal |
| 16.5 | error-rate-spike.yaml | Pass | Pass | PR 6 | Single metric, spike_event alias; byte-equal |
| 16.6 | interface-flap.yaml | Pass | Pass | PR 6 | Multi-signal (v1 YAML uses `signal_type: multi`); line-multiset |
| 16.7 | network-link-failure.yaml | Pass | Pass | PR 6 | Multi-signal, phase_offset, clock_group; line-multiset |
| 16.8 | steady-state.yaml | Pass | Pass | PR 6 | Single metric, steady alias; byte-equal |
| 16.9 | log-storm.yaml | Pass | Pass | PR 6 | Log signal, template generator; byte-equal |
| 16.10 | cardinality-explosion.yaml | Pass | Pass | PR 6 | Cardinality spikes, dynamic labels; byte-equal |
| 16.11 | histogram-latency.yaml | Pass | Pass | PR 6 | Histogram signal; byte-equal |
| 16.12 | link-failover.yaml (story) | Pass | Pass | PR 5/6 | Compile parity Pass (PR 5); runtime parity closed via `v2_story_parity::link_failover_runtime_parity` against hand-built v1-equivalent reference (PR 9 will clean the reference — see v2-progress.md PR 9 forward pointer) |

## 17. Built-in Pack Parity (3 rows)

For each built-in pack, a v2 scenario file is created that uses the pack inside
`scenarios:`. The expanded output must match the current `sonda packs run` output.

| # | Pack | Compile Parity | Runtime Parity | PR | Notes |
|---|------|----------------|----------------|-----|-------|
| 17.1 | telegraf-snmp-interface.yaml | Pass | Pass | PR 4/6 | v1 `expand_pack` and v2 pipeline yield byte-identical runtime output via `v2_pack_runtime_parity::row_17_1_telegraf_snmp_interface` |
| 17.2 | node-exporter-cpu.yaml | Pass | Pass | PR 4/6 | Eight per-mode metrics of `node_cpu_seconds_total` produce byte-identical runtime output via `v2_pack_runtime_parity::row_17_2_node_exporter_cpu` |
| 17.3 | node-exporter-memory.yaml | Pass | Pass | PR 4/6 | Five memory gauge metrics + override labels produce byte-identical runtime output via `v2_pack_runtime_parity::row_17_3_node_exporter_memory` |
