# Tutorial

This tutorial walks you through Sonda's features step by step, starting with a single CLI
command and building up to multi-scenario runs, log generation, and pushing to backends.

**What you need:**

- Sonda installed ([Getting Started](../getting-started.md) covers installation)
- Docker and Docker Compose (for backend sections only)

---

## Your First Metric

Generate a constant metric with a single command:

```bash
sonda metrics --name my_metric --rate 1 --duration 5s
```

You will see output like this:

```
▶ my_metric  signal_type: metrics | rate: 1/s | encoder: prometheus_text | sink: stdout | duration: 5s
my_metric 0 1711900000000
my_metric 0 1711900001000
my_metric 0 1711900002000
```

Each line has three parts: **metric name**, **value**, and **timestamp** (Unix milliseconds).

Set a specific value with `--offset`:

```bash
sonda metrics --name cpu_idle --rate 1 --duration 5s --offset 99.5
```

Add labels to match real Prometheus metrics:

```bash
sonda metrics --name http_requests_total --rate 1 --duration 5s \
  --offset 1 --label env=prod --label region=us-east
```

```
http_requests_total{env="prod",region="us-east"} 1 1711900000000
```

!!! tip "Status lines"
    Lines starting with `▶` and `■` are status output printed to stderr. Pipe stdout freely --
    status messages will not interfere. Use `--quiet` to suppress them entirely.

---

## Signal Shapes -- Generators

Generators control the **value** of each emitted data point. Sonda ships six generators:

| Generator | Description | Best for |
|-----------|-------------|----------|
| `constant` | Fixed value every tick | Up/down indicators, baselines |
| `sine` | Smooth sinusoidal wave | CPU, latency, cyclical load |
| `sawtooth` | Linear ramp, resets at period | Queue depth, buffer fill |
| `uniform` | Random value in [min, max] | Jitter, noisy signals |
| `sequence` | Cycles through an explicit list | Alert threshold testing |
| `csv_replay` | Replays values from a CSV file | Reproducing real incidents |

### constant

The default generator. Set the value with `--offset`:

```bash
sonda metrics --name up --rate 1 --duration 3s --offset 1
```

### sine

Produces a smooth wave defined by amplitude, offset (midpoint), and period:

```bash
sonda metrics --name cpu_usage --rate 2 --duration 10s \
  --value-mode sine --amplitude 40 --offset 50 --period-secs 30
```

This oscillates between 10 and 90, centered on 50, completing one cycle every 30 seconds.

??? info "Sine wave math"
    The formula is: `value = offset + amplitude * sin(2 * pi * elapsed / period)`.
    At t=0 the value equals offset. It peaks at offset + amplitude after one quarter period.

### sawtooth

A linear ramp from 0 to 1 that resets every period:

```bash
sonda metrics --name queue_depth --rate 2 --duration 10s \
  --value-mode sawtooth --period-secs 5
```

### uniform

Random values drawn uniformly between `--min` and `--max`:

```bash
sonda metrics --name jitter_ms --rate 2 --duration 5s \
  --value-mode uniform --min 1 --max 100
```

!!! tip "Deterministic replay"
    Pass `--seed 42` to get the same random sequence every run. Useful for reproducible tests.

### sequence

Cycles through an explicit list of values. Only available via YAML:

```bash
sonda metrics --scenario examples/sequence-alert-test.yaml --duration 10s
```

```yaml title="examples/sequence-alert-test.yaml"
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

### csv_replay

Replays recorded values from a CSV file. Only available via YAML:

```bash
sonda metrics --scenario examples/csv-replay-metrics.yaml
```

```yaml title="examples/csv-replay-metrics.yaml"
name: cpu_replay
rate: 1
duration: 60s
generator:
  type: csv_replay
  file: examples/sample-cpu-values.csv
  column: 1
  has_header: true
  repeat: true
labels:
  instance: prod-server-42
  job: node
encoder:
  type: prometheus_text
sink:
  type: stdout
```

For full generator configuration details, see [Generators](../configuration/generators.md).

---

## Output Formats -- Encoders

Encoders control **how** each data point is serialized. The same metric looks different in each format:

=== "prometheus_text (default)"

    ```bash
    sonda metrics --name http_rps --rate 1 --duration 3s \
      --offset 42 --label env=prod
    ```

    ```
    http_rps{env="prod"} 42 1711900000000
    ```

=== "influx_lp"

    ```bash
    sonda metrics --name http_rps --rate 1 --duration 3s \
      --offset 42 --label env=prod --encoder influx_lp
    ```

    ```
    http_rps,env=prod value=42 1711900000000000000
    ```

=== "json_lines"

    ```bash
    sonda metrics --name http_rps --rate 1 --duration 3s \
      --offset 42 --label env=prod --encoder json_lines
    ```

    ```json
    {"name":"http_rps","value":42.0,"labels":{"env":"prod"},"timestamp":"2026-03-31T20:00:00.000Z"}
    ```

=== "syslog (logs only)"

    ```bash
    sonda logs --mode template --rate 1 --duration 3s \
      --encoder syslog --label app=myservice
    ```

    ```
    <14>1 2026-03-31T20:00:00.000Z sonda sonda - - [sonda app="myservice"] synthetic log event
    ```

!!! info "remote_write encoder"
    The `remote_write` encoder produces Prometheus remote write protobuf format. It requires
    the `remote-write` feature flag when building from source. Pre-built binaries and Docker
    images include it by default. See [Encoders](../configuration/encoders.md) for details.

---

## Destinations -- Sinks

Sinks control **where** data is sent. Sonda supports eight sinks:

| Sink | Description | CLI flag |
|------|-------------|----------|
| `stdout` | Print to standard output | _(default)_ |
| `file` | Write to a file | `--output path` |
| `tcp` | Stream to a TCP listener | YAML only |
| `udp` | Send to a UDP endpoint | YAML only |
| `http_push` | POST batches to an HTTP endpoint | YAML only |
| `loki` | Push logs to Grafana Loki | YAML only |
| `kafka` | Publish to a Kafka topic | YAML only |
| `remote_write` | Prometheus remote write protocol | YAML only |

### stdout (default)

No flags needed -- stdout is the default sink. Pipe output to any tool:

```bash
sonda metrics --name up --rate 10 --duration 5s | wc -l
```

### file

Write to a file with `--output`:

```bash
sonda metrics --name up --rate 10 --duration 5s --output /tmp/metrics.txt
```

### tcp

Stream metrics over TCP. Start a listener first, then run the scenario:

```bash
sonda metrics --scenario examples/tcp-sink.yaml
```

??? example "TCP sink setup"
    Start a listener in another terminal:

    ```bash
    nc -lk 9999
    ```

    ```yaml title="examples/tcp-sink.yaml"
    name: cpu_usage
    rate: 10
    duration: 5s
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 10
      offset: 50.0
    labels:
      host: server-01
      region: us-east
    encoder:
      type: prometheus_text
    sink:
      type: tcp
      address: "127.0.0.1:9999"
    ```

### udp

Send metrics over UDP:

```bash
sonda metrics --scenario examples/udp-sink.yaml
```

??? example "UDP sink setup"
    Start a listener in another terminal:

    ```bash
    nc -lu 9998
    ```

    ```yaml title="examples/udp-sink.yaml"
    name: cpu_usage
    rate: 10
    duration: 5s
    generator:
      type: constant
      value: 1.0
    labels:
      host: server-01
    encoder:
      type: json_lines
    sink:
      type: udp
      address: "127.0.0.1:9998"
    ```

### http_push

POST batched data to any HTTP endpoint:

```bash
sonda metrics --scenario examples/http-push-sink.yaml
```

The key sink fields are `url`, `content_type`, and `batch_size` (bytes buffered before each POST).

### loki

Push JSON logs to Grafana Loki:

```bash
sonda logs --scenario examples/loki-json-lines.yaml
```

```yaml title="examples/loki-json-lines.yaml"
name: app_logs_loki
rate: 10
duration: 60s
generator:
  type: template
  templates:
    - message: "Request from {ip} to {endpoint}"
      field_pools:
        ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3"]
        endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs"]
  severity_weights:
    info: 0.7
    warn: 0.2
    error: 0.1
labels:
  job: sonda
  env: dev
encoder:
  type: json_lines
sink:
  type: loki
  url: http://localhost:3100
  batch_size: 50
```

### kafka

Publish to a Kafka topic:

```bash
sonda metrics --scenario examples/kafka-sink.yaml
```

The key sink fields are `brokers` and `topic`. See `examples/kafka-sink.yaml` for a complete example.

For full sink configuration details, see [Sinks](../configuration/sinks.md).

---

## Generating Logs

Sonda generates structured log events with two modes: **template** and **replay**.

### Template mode

From the CLI, generate simple logs:

```bash
sonda logs --mode template --rate 5 --duration 5s \
  --message "User logged in from {ip}"
```

For rich logs with field pools and severity weights, use a YAML scenario:

```bash
sonda logs --scenario examples/log-template.yaml --duration 10s
```

```yaml title="examples/log-template.yaml (excerpt)"
generator:
  type: template
  templates:
    - message: "Request from {ip} to {endpoint} returned {status}"
      field_pools:
        ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3", "192.168.1.10"]
        endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs"]
        status: ["200", "201", "400", "404", "500"]
    - message: "Service {service} processed {count} events in {duration_ms}ms"
      field_pools:
        service: ["ingest", "transform", "export"]
        count: ["1", "10", "100", "1000"]
        duration_ms: ["5", "12", "47", "200"]
  severity_weights:
    info: 0.7
    warn: 0.2
    error: 0.1
```

!!! note "Field pools require YAML"
    The `--message` CLI flag accepts a single template, but `{placeholder}` tokens are not
    resolved without field pools. Use a YAML scenario for realistic log generation.

### Replay mode

Replay lines from an existing log file:

```bash
sonda logs --scenario examples/log-replay.yaml
```

```yaml title="examples/log-replay.yaml"
name: app_logs_replay
rate: 5
duration: 30s
generator:
  type: replay
  file: /var/log/app.log
encoder:
  type: json_lines
sink:
  type: stdout
```

Lines are replayed in order and cycle back to the start when the file is exhausted.

### Syslog output

Combine template logs with the syslog encoder for RFC 5424 output:

```bash
sonda logs --mode template --rate 2 --duration 5s --encoder syslog
```

---

## Scheduling -- Gaps and Bursts

Gaps and bursts simulate real-world irregularities in telemetry streams.

### Gaps

Drop all events for a window within a recurring cycle. Useful for simulating network
partitions or scrape failures:

```bash
sonda metrics --name net_bytes --rate 2 --duration 20s \
  --gap-every 10s --gap-for 3s
```

This emits at 2/s, but goes silent for 3 seconds out of every 10-second cycle.

### Bursts

Temporarily multiply the emission rate. Useful for load spikes or log storms:

```bash
sonda metrics --name req_rate --rate 10 --duration 20s \
  --burst-every 10s --burst-for 2s --burst-multiplier 5
```

This emits at 10/s normally, but spikes to 50/s for 2 seconds every 10-second cycle.

YAML example with bursts:

```bash
sonda metrics --scenario examples/burst-metrics.yaml --duration 20s
```

```yaml title="examples/burst-metrics.yaml"
name: cpu_burst
rate: 100
duration: 60s
generator:
  type: sine
  amplitude: 20.0
  period_secs: 60
  offset: 50.0
bursts:
  every: 10s
  for: 2s
  multiplier: 5.0
labels:
  host: web-01
  zone: us-east-1
encoder:
  type: prometheus_text
sink:
  type: stdout
```

| Pattern | Real-world scenario |
|---------|---------------------|
| Gap 3s every 60s | Scrape target restarts |
| Gap 30s every 5m | Network partition |
| Burst 5x for 2s every 30s | Traffic spike |
| Burst 10x for 1s every 10s | Log storm during deploy |

!!! tip "Combine with generators"
    Gaps and bursts work with any generator. A sine wave with periodic gaps creates realistic
    "flapping service" patterns for alert testing.

---

## Multi-Scenario Runs

Run metrics and logs concurrently from a single file using `sonda run`:

```bash
sonda run --scenario examples/multi-scenario.yaml
```

```yaml title="examples/multi-scenario.yaml"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 100
    duration: 30s
    generator:
      type: sine
      amplitude: 50
      period_secs: 60
      offset: 50
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  - signal_type: logs
    name: app_logs
    rate: 10
    duration: 30s
    generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint}"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3"]
            endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs"]
      severity_weights:
        info: 0.7
        warn: 0.2
        error: 0.1
      seed: 42
    encoder:
      type: json_lines
    sink:
      type: file
      path: /tmp/sonda-logs.json
```

Each scenario runs on its own thread. Use different sinks per scenario to keep outputs separate.

### Correlated metrics

Use `phase_offset` and `clock_group` to create time-shifted metrics that simulate compound
alert conditions:

```bash
sonda run --scenario examples/multi-metric-correlation.yaml
```

```yaml title="examples/multi-metric-correlation.yaml (excerpt)"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 1
    duration: 120s
    phase_offset: "0s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
      repeat: true
    labels:
      instance: server-01
      job: node
    # ...

  - signal_type: metrics
    name: memory_usage_percent
    rate: 1
    duration: 120s
    phase_offset: "3s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
      repeat: true
    labels:
      instance: server-01
      job: node
    # ...
```

CPU spikes at t=0, memory follows 3 seconds later -- testing compound alert rules like
`cpu > 90 AND memory > 85`.

For more on alert testing patterns, see [Alert Testing](alert-testing.md).

---

## The Server API

`sonda-server` provides an HTTP API for managing scenarios programmatically.

### Start the server

```bash
cargo run -p sonda-server
```

Or with Docker:

```bash
docker run -p 8080:8080 ghcr.io/davidban77/sonda-server:latest
```

### Submit a scenario

```bash
curl -X POST \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/simple-constant.yaml \
  http://localhost:8080/scenarios
```

```json
{"id":"<uuid>","name":"up","status":"running"}
```

### List scenarios

```bash
curl http://localhost:8080/scenarios
```

### Get scenario details

```bash
curl http://localhost:8080/scenarios/<id>
```

### Get live stats

```bash
curl http://localhost:8080/scenarios/<id>/stats
```

### Scrape metrics (Prometheus format)

```bash
curl http://localhost:8080/scenarios/<id>/metrics
```

### Stop a scenario

```bash
curl -X DELETE http://localhost:8080/scenarios/<id>
```

### Long-running scenarios

Omit `duration` to run indefinitely. Stop with DELETE when done:

```yaml title="examples/long-running-metrics.yaml"
name: continuous_cpu
rate: 10
generator:
  type: sine
  amplitude: 50.0
  period_secs: 60
  offset: 50.0
labels:
  instance: api-server-01
  job: sonda
encoder:
  type: prometheus_text
sink:
  type: stdout
```

```bash
# Start
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/long-running-metrics.yaml \
  http://localhost:8080/scenarios

# Stop later
curl -X DELETE http://localhost:8080/scenarios/<id>
```

For the full API reference, see [Server API](../deployment/sonda-server.md).

---

## Pushing to a Backend

Three ways to get data into a monitoring backend:

### 1. HTTP Push (import API)

POST metrics directly to a backend's import endpoint:

```bash
sonda metrics --scenario examples/victoriametrics-metrics.yaml
```

```yaml title="examples/victoriametrics-metrics.yaml (excerpt)"
sink:
  type: http_push
  url: "http://victoriametrics:8428/api/v1/import/prometheus"
  content_type: "text/plain"
  batch_size: 65536
```

### 2. Remote Write

Use the Prometheus remote write protocol for native compatibility:

```bash
sonda metrics --scenario examples/remote-write-vm.yaml
```

```yaml title="examples/remote-write-vm.yaml (excerpt)"
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
  batch_size: 100
```

Compatible targets include VictoriaMetrics, Prometheus, Thanos Receive, and Cortex/Mimir.

### 3. Scrape via sonda-server

Point Prometheus at sonda-server's `/scenarios/<id>/metrics` endpoint:

```yaml title="prometheus.yml (scrape config)"
scrape_configs:
  - job_name: sonda
    static_configs:
      - targets: ["sonda-server:8080"]
    metrics_path: /scenarios/<id>/metrics
```

### Verify data arrived

```bash
# VictoriaMetrics
curl "http://localhost:8428/api/v1/query?query=sonda_http_request_duration_ms"

# Prometheus
curl "http://localhost:9090/api/v1/query?query=sonda_http_request_duration_ms"
```

For Docker Compose setups with VictoriaMetrics and Grafana, see [Docker Deployment](../deployment/docker.md).

---

## Next Steps

- [Alert Testing](alert-testing.md) -- validate Prometheus alert rules with controlled threshold crossings
- [Pipeline Validation](pipeline-validation.md) -- test ingest pipelines with known data patterns
- [Recording Rules](recording-rules.md) -- verify recording rule correctness with synthetic inputs
- [Example Scenarios](examples.md) -- browse the full collection of ready-to-use YAML scenarios
- [Docker Deployment](../deployment/docker.md) -- run Sonda with VictoriaMetrics and Grafana
- [Server API](../deployment/sonda-server.md) -- full HTTP API reference for sonda-server
