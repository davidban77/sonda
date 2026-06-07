---
title: Example scenarios
description: Index of ready-to-run YAML scenario files in the examples/ directory, covering every generator, encoder, and sink.
---

# Example scenarios

The `examples/` directory contains ready-to-run YAML scenario files. Every generator, encoder, and sink combination is covered. Each file works as-is with the `sonda` CLI.

```bash
sonda run examples/basic-metrics.yaml
```

## Basic metrics

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `basic-metrics.yaml` | sine | prometheus_text | stdout | 1000 evt/s sine wave with labels and a recurring gap |
| `simple-constant.yaml` | constant | prometheus_text | stdout | Minimal `up=1` metric at 10 evt/s for 10 seconds |
| `step-counter.yaml` | step | prometheus_text | stdout | Monotonic counter that wraps around at 1000 |
| `jitter-sine.yaml` | sine + jitter | prometheus_text | stdout | Sine wave with deterministic jitter noise |

## Sink types

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `tcp-sink.yaml` | sine | prometheus_text | tcp | Stream to a TCP listener (`nc -l 9999`) |
| `udp-sink.yaml` | constant | json_lines | udp | Send UDP datagrams to port 9998 |
| `file-sink.yaml` | sawtooth | influx_lp | file | Write to `/tmp/sonda-output.txt` |
| `http-push-sink.yaml` | sine | prometheus_text | http_push | POST batches to an HTTP endpoint |
| `http-push-retry.yaml` | sine | prometheus_text | http_push | HTTP push with exponential-backoff retry on 5xx, 429, and connect failures |
| `kafka-sink.yaml` | constant | prometheus_text | kafka | Publish to a local Kafka broker |
| `kafka-tls.yaml` | constant | prometheus_text | kafka | Publish to a TLS-secured Kafka broker with SASL PLAIN auth |

See [Sinks](../build/sinks.md) for configuration details on each sink type.

## OTLP / OpenTelemetry

| File | Signal | Encoder | Sink | Description |
|------|--------|---------|------|-------------|
| `otlp-metrics.yaml` | metrics | otlp | otlp_grpc | Push sine wave metrics to an OTel Collector over gRPC* |
| `otlp-logs.yaml` | logs | otlp | otlp_grpc | Push template logs to an OTel Collector over gRPC* |

*Requires building from source with `--features otlp`. Pre-built binaries do not include OTLP support. Run with: `cargo run --features otlp -- run examples/otlp-metrics.yaml`.

## Encoding formats

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `influx-file.yaml` | sawtooth | influx_lp | file | InfluxDB line protocol written to a file |
| `json-tcp.yaml` | sine | json_lines | tcp | JSON Lines over TCP |
| `prometheus-http-push.yaml` | sine | prometheus_text | http_push | Prometheus text POSTed in batches |
| `precision-formatting.yaml` | sine | prom/json/influx | stdout | Demonstrates the `precision` field with 3 encoders |
| `remote-write-vm.yaml` | sine | remote_write | remote_write | Protobuf remote write to VictoriaMetrics* |
| `multi-format-test.yaml` | constant | influx_lp | file | InfluxDB line protocol for pipeline validation |

*Pre-built binaries include remote-write support. When building from source, add `--features remote-write`. OTLP support requires `--features otlp` (not included in pre-built binaries). See [Encoders](../build/encoders.md) for details.

## Scheduling

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `burst-metrics.yaml` | sine | prometheus_text | stdout | Bursts to 5x the rate for 2s every 10s |
| `cardinality-spike.yaml` | sine | prometheus_text | stdout | Injects 100 unique `pod_name` labels for 5s every 10s |
| `long-running-metrics.yaml` | sine | prometheus_text | stdout | No `duration` — runs until stopped (pair with `sonda-server` POST/DELETE) |

## Histograms and summaries

| File | Signal | Encoder | Sink | Description |
|------|--------|---------|------|-------------|
| `histogram.yaml` | histogram | prometheus_text | stdout | Exponential distribution, 100 observations per tick (latency-style histogram) |
| `summary.yaml` | summary | prometheus_text | stdout | Normal distribution (mean 0.1, stddev 0.02), 100 observations per tick |

Run them with `sonda run`. Histograms and summaries are `signal_type:` variants in the scenario YAML:

```bash
sonda run examples/histogram.yaml
sonda run examples/summary.yaml
```

See [Generators](../build/generators.md) for distribution options.

## Metric packs

| File | Pack | Encoder | Sink | Description |
|------|------|---------|------|-------------|
| `pack-scenario.yaml` | telegraf_snmp_interface | prometheus_text | stdout | Expand a pack into per-metric scenarios with user-supplied labels |
| `pack-with-overrides.yaml` | telegraf_snmp_interface | prometheus_text | stdout | Override one metric's generator (`ifOperStatus` -> `flap`) without editing the pack |

Packs are `kind: composable` YAML files in a catalog directory you control. Point `sonda run` at that directory with `--catalog <dir>` so `pack: <name>` references resolve. See [Metric Packs](../build/catalogs-and-packs.md) for the catalog layout.

## Dynamic labels

Dynamic labels rotate a label's value on every tick. Unlike cardinality spikes, the label is always present and cycles through a bounded set.

| File | Generator | Strategy | Description |
|------|-----------|----------|-------------|
| `dynamic-labels-fleet.yaml` | sine | counter (10) | 10-node set: `hostname=host-0..host-9` rotating on a CPU-usage metric |
| `dynamic-labels-regions.yaml` | uniform | values list | Cycle `region` through `us-east-1, us-west-2, eu-west-1` on an API-latency metric |
| `dynamic-labels-multi.yaml` | step | counter + values | Two rotating labels (`hostname` counter x `region` values) on a request counter |
| `dynamic-labels-logs.yaml` | template (logs) | counter (3) | Rotating `pod_name` label on structured log events |

See the [Dynamic Labels guide](../build/scheduling.md) for the walkthrough and [Dynamic labels](../reference/scenario-fields.md#dynamic-labels) for the field reference.

## Capacity planning

Test ingest pipelines by pushing high-volume, bursty, or high-cardinality workloads to VictoriaMetrics or another backend. Change `url:` to target your own stack.

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `capacity-throughput-test.yaml` | sine x3 | prometheus_text | http_push | Three concurrent streams at 1000 evt/s each (3000 total) to find saturation |
| `capacity-burst-test.yaml` | sine + bursts | prometheus_text | http_push | 500 evt/s baseline with 10x spikes for 5s every 30s to test backpressure |
| `capacity-cardinality-stress.yaml` | constant + spike | prometheus_text | http_push | 500-value `pod_name` and 200-value `endpoint` cardinality spikes to test the index |

See the [Capacity Planning](capacity-planning.md) guide for measurement methodology.

## Log scenarios

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `log-template.yaml` | template | json_lines | stdout | Structured logs with field pools and severity weights |
| `log-csv-replay.yaml` | csv_replay | json_lines | stdout | Replay structured log events from a CSV (timestamp, severity, message, fields) |
| `loki-json-lines.yaml` | template | json_lines | loki | Push log events to a Loki instance |
| `kafka-json-logs.yaml` | template | json_lines | kafka | Send log events to a Kafka topic |

Run log scenarios with `sonda run`. Log entries are `signal_type: logs` in the scenario YAML:

```bash
sonda run examples/log-template.yaml
```

## Docker and VictoriaMetrics

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `docker-metrics.yaml` | sine | prometheus_text | stdout | CPU wave (30 to 70%) for the Docker Compose stack |
| `docker-alerts.yaml` | sine | prometheus_text | stdout | Threshold-crossing sine for alert rule testing |
| `victoriametrics-metrics.yaml` | sine | prometheus_text | http_push | Push directly to VictoriaMetrics |
| `vm-push-scenario.yaml` | sine | prometheus_text | http_push | Push cpu_usage to VictoriaMetrics for alert testing |

See [Docker deployment](../deploy/docker.md) for the full stack setup.

## Alert testing

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `sine-threshold-test.yaml` | sine | prometheus_text | stdout | Sine wave crossing a 90% threshold |
| `for-duration-test.yaml` | sequence | prometheus_text | stdout | Sequence pattern for `for:` duration testing |
| `constant-threshold-test.yaml` | constant | prometheus_text | stdout | Sustained 95% breach for `for: 5m` alerts |
| `gap-alert-test.yaml` | constant | prometheus_text | stdout | Alert resolution via periodic gaps |
| `sequence-alert-test.yaml` | sequence | prometheus_text | stdout | CPU pattern crossing a 90% threshold |
| `spike-alert-test.yaml` | spike | prometheus_text | stdout | Periodic spike from baseline 50 to 250 for threshold alerts |
| `csv-replay-metrics.yaml` | csv_replay | prometheus_text | stdout | Replay a real production incident from CSV |
| `csv-replay-multi-column.yaml` | csv_replay | prometheus_text | stdout | Replay three columns from a single CSV at the same time as independent metrics |
| `csv-replay-grafana-auto.yaml` | csv_replay | prometheus_text | stdout | Replay a Grafana CSV export with auto-discovered columns and labels |
| `csv-replay-explicit-labels.yaml` | csv_replay | prometheus_text | stdout | Multi-column replay with per-column labels merged with scenario labels |
| `cardinality-alert-test.yaml` | constant | prometheus_text | http_push | 500-value cardinality spike for cardinality alerts |
| `ci-alert-validation.yaml` | constant | prometheus_text | http_push | Constant 95% CPU for 30s — short enough for CI, long enough for a `for: 5s` alert |

See the [Alert Testing](alert-testing.md) guide for the end-to-end walkthrough, and [CI Alert Validation](end-to-end-pipelines.md) for automated validation in GitHub Actions.

## Alerting pipeline

| File | Type | Description |
|------|------|-------------|
| `alertmanager/alerting-scenario.yaml` | Sonda scenario | Sine wave pushing to VictoriaMetrics for alert evaluation |
| `alertmanager/alert-rules.yml` | vmalert rules | `HighCpuUsage` (>90) and `ElevatedCpuUsage` (>70) alerts |
| `alertmanager/alertmanager.yml` | Alertmanager config | Routes all alerts to the webhook receiver |

These files are used with the `--profile alerting` Docker Compose profile. See the [Alerting Pipeline](end-to-end-pipelines.md) guide for the full walkthrough.

## Recording rules

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `recording-rule-test.yaml` | constant | prometheus_text | http_push | Known value for sum-based recording rule validation |
| `rate-rule-input.yaml` | sawtooth | prometheus_text | http_push | Sawtooth pattern for `rate()`-based recording rule testing |

See [Recording Rules](recording-rules.md) for the step-by-step guide.

## Pipeline validation

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `e2e-scenario.yaml` | constant | prometheus_text | http_push | Push a known value to VictoriaMetrics for end-to-end checks |
| `multi-format-test.yaml` | constant | influx_lp | file | InfluxDB line protocol to a file for format validation |
| `multi-pipeline-test.yaml` | constant + template | prom + json | stdout + file | Metrics and logs concurrently |

See [Pipeline Validation](end-to-end-pipelines.md) for usage patterns.

## Network device telemetry

| File | Signal | Description |
|------|--------|-------------|
| `network-device-baseline.yaml` | metrics | Router with 2 uplinks: traffic counters, state, CPU, memory (9 scenarios) |
| `network-link-failure.yaml` | metrics | 6-scenario link-failure cycle on `rtr-core-01`: Gi0/0/0 down 10s, Gi0/0/1 absorbs, errors and CPU rise, then recovers |

Run with `sonda run`:

```bash
sonda run examples/network-device-baseline.yaml
sonda run examples/network-link-failure.yaml
```

See [Network Device Telemetry](network-device-telemetry.md) for the full walkthrough, including a 3-signal cascade using `after:` that you can write from scratch.

## Multi-scenario

| File | Signal | Description |
|------|--------|-------------|
| `multi-scenario.yaml` | metrics + logs | Run both signal types at the same time |
| `multi-metric-correlation.yaml` | metrics | Correlated CPU and memory with `phase_offset` for compound alerts |
| `multi-pipeline-test.yaml` | metrics + logs | Pipeline validation with concurrent signal types |

Run multi-scenario files with `sonda run`:

```bash
sonda run examples/multi-scenario.yaml
```

!!! tip
    Override any scenario field from the CLI. For example, change the duration: `sonda run examples/basic-metrics.yaml --duration 10s`.
