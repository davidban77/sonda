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

## Sink Types

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `tcp-sink.yaml` | sine | prometheus_text | tcp | Stream to a TCP listener (`nc -l 9999`) |
| `udp-sink.yaml` | constant | json_lines | udp | Send UDP datagrams to port 9998 |
| `file-sink.yaml` | sawtooth | influx_lp | file | Write to `/tmp/sonda-output.txt` |
| `http-push-sink.yaml` | sine | prometheus_text | http_push | POST batches to an HTTP endpoint |
| `kafka-sink.yaml` | constant | prometheus_text | kafka | Publish to a local Kafka broker |

See [Sinks](../configuration/sinks.md) for configuration details on each sink type.

## Encoding Formats

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `influx-file.yaml` | sawtooth | influx_lp | file | InfluxDB line protocol to a file |
| `json-tcp.yaml` | sine | json_lines | tcp | JSON Lines over TCP |
| `prometheus-http-push.yaml` | sine | prometheus_text | http_push | Prometheus text POSTed in batches |
| `precision-formatting.yaml` | sine | prom/json/influx | stdout | Demonstrates `precision` field with 3 encoders |
| `remote-write-vm.yaml` | sine | remote_write | remote_write | Protobuf remote write to VictoriaMetrics* |

*Requires `--features remote-write`. See [Encoders](../configuration/encoders.md) for details.

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

See [Docker deployment](../deployment/docker.md) for the full stack setup.

## Alert Testing

| File | Generator | Encoder | Sink | Description |
|------|-----------|---------|------|-------------|
| `sequence-alert-test.yaml` | sequence | prometheus_text | stdout | CPU spike pattern crossing a 90% threshold |
| `csv-replay-metrics.yaml` | csv_replay | prometheus_text | stdout | Replay a real production incident from CSV |
| `recording-rule-test.yaml` | constant | prometheus_text | http_push | Known value for recording rule validation |

See the [Alert Testing](alert-testing.md) guide for end-to-end walkthrough.

## Multi-Scenario

| File | Signal | Description |
|------|--------|-------------|
| `multi-scenario.yaml` | metrics + logs | Run both signal types concurrently |
| `multi-metric-correlation.yaml` | metrics | Correlated CPU + memory with `phase_offset` for compound alerts |

Run multi-scenario files with `sonda run`:

```bash
sonda run --scenario examples/multi-scenario.yaml
```

!!! tip
    Override any scenario field from the CLI. For example, change the duration:
    `sonda metrics --scenario examples/basic-metrics.yaml --duration 10s`
