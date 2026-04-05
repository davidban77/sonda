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
| `kafka-sink.yaml` | constant | prometheus_text | kafka | Publish to a local Kafka broker |

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

## Log Scenarios

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `log-template.yaml` | template | json_lines | stdout | Structured logs with field pools and severity weights |
| `log-replay.yaml` | replay | json_lines | stdout | Replay lines from an existing log file |
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
| `cardinality-alert-test.yaml` | constant | prometheus_text | http_push | 500-value cardinality spike for cardinality alerts |

See the [Alert Testing](alert-testing.md) guide for end-to-end walkthrough.

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
| `network-link-failure.yaml` | metrics | Link failure cascade: interface down, traffic shift, error spike (6 scenarios) |

Run with `sonda run`:

```bash
sonda run --scenario examples/network-device-baseline.yaml
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
