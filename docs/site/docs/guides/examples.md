# Example Scenarios

The `examples/` directory contains ready-to-run YAML scenario files covering every
generator, encoder, and sink combination. Each file works as-is with the `sonda` CLI.

```bash
sonda metrics --scenario examples/basic-metrics.yaml
```

## Basic Metrics

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `basic-metrics.yaml` | sine | prometheus_text | stdout | 1000 evt/s sine wave with labels and a recurring gap |
| `simple-constant.yaml` | constant | prometheus_text | stdout | Minimal `up=1` metric at 10 evt/s for 10 seconds |
| `step-counter.yaml` | step | prometheus_text | stdout | Monotonic counter with wrap-around at 1000 |
| `jitter-sine.yaml` | sine + jitter | prometheus_text | stdout | Sine wave with deterministic jitter noise |

## Sink Types

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `tcp-sink.yaml` | sine | prometheus_text | tcp | Stream to a TCP listener (`nc -l 9999`) |
| `udp-sink.yaml` | constant | json_lines | udp | Send UDP datagrams to port 9998 |
| `file-sink.yaml` | sawtooth | influx_lp | file | Write to `/tmp/sonda-output.txt` |
| `http-push-sink.yaml` | sine | prometheus_text | http_push | POST batches to an HTTP endpoint |
| `http-push-retry.yaml` | sine | prometheus_text | http_push | HTTP push with exponential-backoff retry on 5xx/429/connect failures |
| `kafka-sink.yaml` | constant | prometheus_text | kafka | Publish to a local Kafka broker |
| `kafka-tls.yaml` | constant | prometheus_text | kafka | Publish to a TLS-secured Kafka broker with SASL PLAIN auth |

See [Sinks](../configuration/sinks.md) for configuration details on each sink type.

## OTLP / OpenTelemetry

| File | Signal | Encoder | Sink | Description |
|------|--------|---------|------|-------------|
| `otlp-metrics.yaml` | metrics | otlp | otlp_grpc | Push sine wave metrics to an OTEL Collector via gRPC* |
| `otlp-logs.yaml` | logs | otlp | otlp_grpc | Push template logs to an OTEL Collector via gRPC* |

*Requires building from source with `--features otlp`. Pre-built binaries do not include OTLP support. Run with: `cargo run --features otlp -- metrics --scenario examples/otlp-metrics.yaml`

## Encoding Formats

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `influx-file.yaml` | sawtooth | influx_lp | file | InfluxDB line protocol to a file |
| `json-tcp.yaml` | sine | json_lines | tcp | JSON Lines over TCP |
| `prometheus-http-push.yaml` | sine | prometheus_text | http_push | Prometheus text POSTed in batches |
| `precision-formatting.yaml` | sine | prom/json/influx | stdout | Demonstrates `precision` field with 3 encoders |
| `remote-write-vm.yaml` | sine | remote_write | remote_write | Protobuf remote write to VictoriaMetrics* |
| `multi-format-test.yaml` | constant | influx_lp | file | InfluxDB line protocol for pipeline validation |

*Pre-built binaries include remote-write support. When building from source, add `--features remote-write`. OTLP support requires `--features otlp` (not included in pre-built binaries). See [Encoders](../configuration/encoders.md) for details.

## Scheduling

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `burst-metrics.yaml` | sine | prometheus_text | stdout | Bursts to 5x rate for 2s every 10s |
| `cardinality-spike.yaml` | sine | prometheus_text | stdout | Injects 100 unique `pod_name` labels for 5s every 10s |
| `long-running-metrics.yaml` | sine | prometheus_text | stdout | No `duration` -- runs until stopped (pair with `sonda-server` POST/DELETE) |

## Histograms and Summaries

| File | Signal | Encoder | Sink | Description |
|------|--------|---------|------|-------------|
| `histogram.yaml` | histogram | prometheus_text | stdout | Exponential distribution, 100 observations/tick (latency-style histogram) |
| `summary.yaml` | summary | prometheus_text | stdout | Normal distribution (mean 0.1, stddev 0.02), 100 observations/tick |

Run these with the matching subcommand:

```bash
sonda histogram --scenario examples/histogram.yaml
sonda summary --scenario examples/summary.yaml
```

See [Generators](../configuration/generators.md) for distribution options.

## Metric Packs

| File | Pack | Encoder | Sink | Description |
|------|------|---------|------|-------------|
| `pack-scenario.yaml` | telegraf_snmp_interface | prometheus_text | stdout | Expand a pack into per-metric scenarios with user-supplied labels |
| `pack-with-overrides.yaml` | telegraf_snmp_interface | prometheus_text | stdout | Override one metric's generator (`ifOperStatus` -> `flap`) without editing the pack |

Packs must be on the search path. Run from the repo root (where `./packs/` exists), set
`SONDA_PACK_PATH`, or pass `--pack-path ./packs`. See [Metric Packs](metric-packs.md) for details.

## Dynamic Labels

Dynamic labels rotate a label's value on every tick -- unlike cardinality spikes, the label is
always present and cycles through a bounded set.

| File | Generator | Strategy | Description |
|------|-----------|----------|-------------|
| `dynamic-labels-fleet.yaml` | sine | counter (10) | 10-node fleet: `hostname=host-0..host-9` rotating on a CPU-usage metric |
| `dynamic-labels-regions.yaml` | uniform | values list | Cycle `region` through `us-east-1, us-west-2, eu-west-1` on an API-latency metric |
| `dynamic-labels-multi.yaml` | step | counter + values | Two rotating labels (`hostname` counter x `region` values) on a request counter |
| `dynamic-labels-logs.yaml` | template (logs) | counter (3) | Rotating `pod_name` label on structured log events |

See the [Dynamic Labels guide](dynamic-labels.md) for the walkthrough and
[Dynamic labels](../configuration/scenario-fields.md#dynamic-labels) for the field reference.

## Capacity Planning

Stress-test ingest pipelines by pushing high-volume, bursty, or high-cardinality workloads to
VictoriaMetrics (or another backend). Change `url:` to target your own stack.

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `capacity-throughput-test.yaml` | sine x3 | prometheus_text | http_push | Three concurrent streams at 1000 evt/s each (3000 total) to find saturation |
| `capacity-burst-test.yaml` | sine + bursts | prometheus_text | http_push | 500 evt/s baseline with 10x spikes for 5s every 30s to test backpressure |
| `capacity-cardinality-stress.yaml` | constant + spike | prometheus_text | http_push | 500-value `pod_name` + 200-value `endpoint` cardinality spikes to stress index |

See the [Capacity Planning](capacity-planning.md) guide for measurement methodology.

## Log Scenarios

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `log-template.yaml` | template | json_lines | stdout | Structured logs with field pools and severity weights |
| `log-csv-replay.yaml` | csv_replay | json_lines | stdout | Replay structured log events from a CSV (timestamp + severity + message + fields) |
| `loki-json-lines.yaml` | template | json_lines | loki | Push log events to a Loki instance |
| `kafka-json-logs.yaml` | template | json_lines | kafka | Send log events to a Kafka topic |

Run log scenarios with `sonda logs`:

```bash
sonda logs --scenario examples/log-template.yaml
```

## Docker and VictoriaMetrics

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `docker-metrics.yaml` | sine | prometheus_text | stdout | CPU wave (30--70%) for the Docker Compose stack |
| `docker-alerts.yaml` | sine | prometheus_text | stdout | Threshold-crossing sine for alert rule testing |
| `victoriametrics-metrics.yaml` | sine | prometheus_text | http_push | Push directly to VictoriaMetrics |
| `vm-push-scenario.yaml` | sine | prometheus_text | http_push | Push cpu_usage to VictoriaMetrics for alert testing |

See [Docker deployment](../deployment/docker.md) for the full stack setup.

## Alert Testing

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `sine-threshold-test.yaml` | sine | prometheus_text | stdout | Sine wave crossing a 90% threshold |
| `for-duration-test.yaml` | sequence | prometheus_text | stdout | Sequence pattern for `for:` duration testing |
| `constant-threshold-test.yaml` | constant | prometheus_text | stdout | Sustained 95% breach for `for: 5m` alerts |
| `gap-alert-test.yaml` | constant | prometheus_text | stdout | Alert resolution via periodic gaps |
| `sequence-alert-test.yaml` | sequence | prometheus_text | stdout | CPU spike pattern crossing a 90% threshold |
| `spike-alert-test.yaml` | spike | prometheus_text | stdout | Periodic spike from baseline 50 to 250 for threshold alerts |
| `csv-replay-metrics.yaml` | csv_replay | prometheus_text | stdout | Replay a real production incident from CSV |
| `csv-replay-multi-column.yaml` | csv_replay | prometheus_text | stdout | Replay three columns from a single CSV simultaneously as independent metrics |
| `csv-replay-grafana-auto.yaml` | csv_replay | prometheus_text | stdout | Replay a Grafana CSV export with auto-discovered columns and labels |
| `csv-replay-explicit-labels.yaml` | csv_replay | prometheus_text | stdout | Multi-column replay with per-column labels merged with scenario labels |
| `cardinality-alert-test.yaml` | constant | prometheus_text | http_push | 500-value cardinality spike for cardinality alerts |
| `ci-alert-validation.yaml` | constant | prometheus_text | http_push | Constant 95% CPU for 30s -- short enough for CI, long enough for a `for: 5s` alert |

See the [Alert Testing](alert-testing.md) guide for end-to-end walkthrough, and
[CI Alert Validation](ci-alert-validation.md) for automated validation in GitHub Actions.

## Alerting Pipeline

| File | Type | Description |
|------|------|-------------|
| `alertmanager/alerting-scenario.yaml` | Sonda scenario | Sine wave pushing to VictoriaMetrics for alert evaluation |
| `alertmanager/alert-rules.yml` | vmalert rules | HighCpuUsage (>90) and ElevatedCpuUsage (>70) alerts |
| `alertmanager/alertmanager.yml` | Alertmanager config | Routes all alerts to the webhook receiver |

These files are used with the `--profile alerting` Docker Compose profile. See the
[Alerting Pipeline](alerting-pipeline.md) guide for the full walkthrough.

## Recording Rules

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `recording-rule-test.yaml` | constant | prometheus_text | http_push | Known value for sum-based recording rule validation |
| `rate-rule-input.yaml` | sawtooth | prometheus_text | http_push | Sawtooth ramp for rate()-based recording rule testing |

See [Recording Rules](recording-rules.md) for the step-by-step guide.

## Pipeline Validation

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `e2e-scenario.yaml` | constant | prometheus_text | http_push | Push a known value to VictoriaMetrics for e2e checks |
| `multi-format-test.yaml` | constant | influx_lp | file | InfluxDB line protocol to file for format validation |
| `multi-pipeline-test.yaml` | constant + template | prom + json | stdout + file | Metrics and logs concurrently |

See [Pipeline Validation](pipeline-validation.md) for usage patterns.

## Network Device Telemetry

| File | Signal | Description |
|------|--------|-------------|
| `network-device-baseline.yaml` | metrics | Router with 2 uplinks: traffic counters, state, CPU, memory (9 scenarios) |
| `network-link-failure.yaml` | metrics | 6-scenario link-failure cycle on `rtr-core-01`: Gi0/0/0 down 10s, Gi0/0/1 absorbs, errors + CPU spike, then recovers |
| `scenarios/link-failover.yaml` | multi | Edge router link failover: primary flaps, backup saturates, latency degrades (3-signal `after:` chain) |

Run with `sonda run`:

```bash
sonda run --scenario examples/network-device-baseline.yaml
sonda run --scenario scenarios/link-failover.yaml
```

The `link-failover` scenario also ships in the built-in catalog:

```bash
sonda catalog run link-failover
```

See [Network Device Telemetry](network-device-telemetry.md) for the full walkthrough.

## Multi-Scenario

| File | Signal | Description |
|------|--------|-------------|
| `multi-scenario.yaml` | metrics + logs | Run both signal types concurrently |
| `multi-metric-correlation.yaml` | metrics | Correlated CPU + memory with `phase_offset` for compound alerts |
| `multi-pipeline-test.yaml` | metrics + logs | Pipeline validation with concurrent signal types |

Run multi-scenario files with `sonda run`:

```bash
sonda run --scenario examples/multi-scenario.yaml
```

!!! tip
    Override any scenario field from the CLI. For example, change the duration:
    `sonda metrics --scenario examples/basic-metrics.yaml --duration 10s`
