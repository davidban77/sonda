# Tutorial

This tutorial picks up where [Getting Started](../getting-started.md) left off. You have
Sonda installed and have run your first metric and log. Now let's explore the full range of
generators, encoders, sinks, and advanced features.

**What you need:**

- Sonda installed ([Getting Started](../getting-started.md) covers installation)
- Docker and Docker Compose (needed for [The Server API](#the-server-api) and [Pushing to a Backend](#pushing-to-a-backend) sections only)

Let's start with the different value shapes Sonda can produce.

---

## Generators

A metric that always outputs zero isn't very useful for testing. Generators let you shape
the values Sonda emits -- smooth waves for latency simulation, random noise for jitter,
or exact sequences to trigger alert thresholds.

Sonda ships eight generators:

| Generator | Description | Best for |
|-----------|-------------|----------|
| `constant` | Fixed value every tick | Up/down indicators, baselines |
| `sine` | Smooth sinusoidal wave | CPU, latency, cyclical load |
| `sawtooth` | Linear ramp, resets at period | Queue depth, buffer fill |
| `uniform` | Random value in [min, max] | Jitter, noisy signals |
| `sequence` | Cycles through an explicit list | Alert threshold testing |
| `step` | Monotonic counter with optional wrap | `rate()` and `increase()` testing |
| `spike` | Baseline with periodic spikes | Anomaly detection, alert thresholds |
| `csv_replay` | Replays values from a CSV file | Reproducing real incidents |

!!! note "YAML-only generators"
    Sequence, step, spike, and csv_replay require a scenario file -- they have no CLI flag equivalents.
    All other generators are available via `--value-mode`.

### constant

The default generator. Set the value with `--value`:

```bash
sonda metrics --name up --rate 1 --duration 3s --value 1
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

### sequence

For testing alert thresholds, you often need values that cross a specific boundary at a
specific time. Sequence gives you that exact control:

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

??? tip "More generators: sawtooth, uniform, step, csv_replay"
    **sawtooth** -- A linear ramp from 0 to 1 that resets every period. Useful for simulating
    queue fill and drain cycles:

    ```bash
    sonda metrics --name queue_depth --rate 2 --duration 10s \
      --value-mode sawtooth --period-secs 5
    ```

    **uniform** -- Random values drawn uniformly between `--min` and `--max`. Pass `--seed 42`
    for deterministic replay:

    ```bash
    sonda metrics --name jitter_ms --rate 2 --duration 5s \
      --value-mode uniform --min 1 --max 100
    ```

    **step** -- A monotonic counter that increments by `step_size` each tick. Set `max` to
    simulate counter resets, perfect for testing `rate()` and `increase()`:

    ```bash
    sonda metrics --scenario examples/step-counter.yaml --duration 5s
    ```

    **csv_replay** -- Replays recorded values from a CSV file. Point it at real incident data
    to reproduce production behavior:

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

    For multi-column CSV files, use `columns` instead of `column` to emit multiple metrics
    from a single scenario — see [Multi-column replay](../configuration/generators.md#csv_replay).

    For full generator configuration details, see [Generators](../configuration/generators.md).

!!! tip "Add realism with jitter"
    Real metrics are never perfectly smooth. Add `jitter` to any generator to introduce
    deterministic uniform noise:

    ```yaml
    generator:
      type: sine
      amplitude: 20
      period_secs: 120
      offset: 50
    jitter: 3.0
    jitter_seed: 42
    ```

    This adds noise in the range `[-3.0, +3.0]` to every value. Set `jitter_seed` for
    reproducible noise across runs. See [Generators - Jitter](../configuration/generators.md#jitter)
    for details.

You've seen what values Sonda can generate. Next, let's look at how those values get formatted on the wire.

---

## Encoders

Your monitoring backend expects data in a specific wire format. Sonda can speak all of them.

The same metric looks different in each format:

=== "prometheus_text (default)"

    ```bash
    sonda metrics --name http_rps --rate 1 --duration 3s \
      --value 42 --label env=prod
    ```

    ```text
    http_rps{env="prod"} 42 1711900000000
    ```

=== "influx_lp"

    ```bash
    sonda metrics --name http_rps --rate 1 --duration 3s \
      --value 42 --label env=prod --encoder influx_lp
    ```

    ```text
    http_rps,env=prod value=42 1711900000000000000
    ```

=== "json_lines"

    ```bash
    sonda metrics --name http_rps --rate 1 --duration 3s \
      --value 42 --label env=prod --encoder json_lines
    ```

    ```json
    {"name":"http_rps","value":42.0,"labels":{"env":"prod"},"timestamp":"2026-03-31T20:00:00.000Z"}
    ```

=== "syslog (logs only)"

    ```bash
    sonda logs --mode template --rate 1 --duration 3s \
      --encoder syslog --label app=myservice
    ```

    ```text
    <14>1 2026-03-31T21:40:38.941Z sonda sonda - - [sonda app="myservice"] synthetic log event
    ```

!!! warning "Feature-gated encoders"
    The `remote_write` encoder produces Prometheus remote write protobuf format. It requires
    the `remote-write` feature flag when building from source (`cargo build --features remote-write`).
    Pre-built binaries and Docker images include it by default.

    The `otlp` encoder produces OTLP protobuf format for metrics and logs. It requires
    the `otlp` feature flag (`cargo build --features otlp`). Pre-built binaries and Docker
    images do **not** include this feature -- you must build from source.

    See [Encoders](../configuration/encoders.md) for details.

With the right format chosen, the next question is: where should the data go?

---

## Sinks

So far everything has gone to stdout. In production testing, you need data flowing to
real backends -- over HTTP, TCP, or directly into Kafka or Loki.

Sonda supports nine sinks:

| Sink | Description | CLI flag |
|------|-------------|----------|
| `stdout` | Print to standard output | _(default)_ |
| `file` | Write to a file | `--output path` |
| `tcp` | Stream to a TCP listener | YAML only |
| `udp` | Send to a UDP endpoint | YAML only |
| `http_push` | POST batches to an HTTP endpoint | `--sink http_push --endpoint <url>` |
| `loki` | Push logs to Grafana Loki | `--sink loki --endpoint <url>` |
| `kafka` | Publish to a Kafka topic | `--sink kafka --brokers <addr> --topic <t>` |
| `remote_write` | Prometheus remote write protocol | `--sink remote_write --endpoint <url>` |
| `otlp_grpc` | OTLP/gRPC to an OpenTelemetry Collector | `--sink otlp_grpc --endpoint <url> --signal-type <s>` |

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

### http_push

POST batched data to any HTTP endpoint. This is the most universal network sink -- it
works with any backend that accepts HTTP imports. Use CLI flags for quick ad-hoc runs:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink http_push --endpoint http://localhost:9090/api/v1/push \
  --content-type "text/plain; version=0.0.4"
```

Or use a scenario file for full control (including custom headers):

```bash
sonda metrics --scenario examples/http-push-sink.yaml
```

```yaml title="examples/http-push-sink.yaml (key fields)"
sink:
  type: http_push
  url: "http://localhost:9090/api/v1/push"
  content_type: "text/plain; version=0.0.4"
  batch_size: 65536
```

The key sink fields are `url`, `content_type`, and `batch_size` (bytes buffered before each POST).

??? example "TCP sink setup"
    Stream metrics over TCP. Start a listener in another terminal:

    ```bash
    nc -lk 9999
    ```

    Then run:

    ```bash
    sonda metrics --scenario examples/tcp-sink.yaml
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

??? example "UDP sink setup"
    Send metrics over UDP. Start a listener in another terminal:

    ```bash
    nc -lu 9998
    ```

    Then run:

    ```bash
    sonda metrics --scenario examples/udp-sink.yaml
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

### loki

Push JSON logs to Grafana Loki. The fastest way is a single CLI command:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --sink loki --endpoint http://localhost:3100 \
  --label job=sonda --label env=dev
```

For richer logs with field pools and severity weights, use a scenario file:

```bash
sonda logs --scenario examples/loki-json-lines.yaml
```

??? example "Full Loki scenario file"

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

Publish metrics to a Kafka topic. Use CLI flags for a quick test:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink kafka --brokers 127.0.0.1:9092 --topic sonda-metrics
```

Or use a scenario file for full control:

```bash
sonda metrics --scenario examples/kafka-sink.yaml
```

??? example "Full Kafka scenario file"

    See `examples/kafka-sink.yaml` for the complete example with generator and encoder config.

    ```yaml title="examples/kafka-sink.yaml (key fields)"
    sink:
      type: kafka
      brokers: "localhost:9094"
      topic: sonda-metrics
    ```

### otlp_grpc

Push metrics or logs to an OpenTelemetry Collector via gRPC. Use CLI flags:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317 --signal-type metrics
```

For logs, `--signal-type` defaults to `logs` automatically:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317
```

Or use a scenario file:

```bash
sonda metrics --scenario examples/otlp-metrics.yaml
```

??? example "Full OTLP scenario file"

    ```yaml title="examples/otlp-metrics.yaml (key fields)"
    encoder:
      type: otlp
    sink:
      type: otlp_grpc
      endpoint: "http://localhost:4317"
      signal_type: metrics
      batch_size: 100
    ```

!!! warning "Feature flag required"
    OTLP requires the `otlp` Cargo feature. Pre-built binaries do **not** include it --
    build from source with `cargo build --features otlp -p sonda`.
    See [Sinks - otlp_grpc](../configuration/sinks.md#otlp_grpc) for the full configuration reference.

For full sink configuration details, see [Sinks](../configuration/sinks.md).

So far we've focused on metrics. Sonda also generates structured log events.

---

## Generating Logs

[Getting Started](../getting-started.md#generating-logs) showed basic log generation. Sonda
supports two log modes -- **template** for synthetic messages with randomized fields, and
**replay** for re-emitting lines from an existing log file. Let's explore what each can do
with YAML scenarios.

### Template mode with field pools

The CLI `--message` flag supports template syntax, but placeholder tokens like `{ip}` render
as literal text. For dynamic log messages with randomized fields, use a YAML scenario:

```bash
sonda logs --scenario examples/log-template.yaml --duration 5s
```

The `examples/log-template.yaml` file defines multiple message templates, each with its own
pool of randomized field values and severity weights. See
[Generators](../configuration/generators.md) for the full template configuration reference.

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
  file: examples/sample-app.log
encoder:
  type: json_lines
sink:
  type: stdout
```

Lines are replayed in order and cycle back to the start when the file is exhausted.

!!! tip "Bring your own log file"
    The example uses `examples/sample-app.log` which ships with Sonda. To replay your
    own logs, point `file:` at any text file -- one log line per line.

### Syslog output

Combine template logs with the syslog encoder for RFC 5424 output:

```bash
sonda logs --mode template --rate 2 --duration 5s --encoder syslog
```

Your metrics and logs are flowing, but real telemetry has irregularities. Let's add some.

---

## Scheduling -- Gaps and Bursts

Real telemetry is messy -- networks drop packets, services restart, traffic spikes hit.
Gaps and bursts let you inject those irregularities into your synthetic data, so you can
test how your pipeline and alerts behave under imperfect conditions.

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

Running one scenario at a time is great for exploration, but production systems emit multiple signals simultaneously.

---

## Multi-Scenario Runs

Production systems emit multiple signals simultaneously. `sonda run` lets you orchestrate
several scenarios concurrently from a single YAML file, each on its own thread.

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

Here is how the phase offset creates an overlapping window for compound alert testing:

```text
t=0s    cpu_usage starts        (values: 20 -> 95)
t=3s    memory_usage starts     (3s phase offset, values: 40 -> 88)
t=5s    Both above threshold    compound alert fires (cpu > 90 AND memory > 85)
```

CPU spikes at t=0, memory follows 3 seconds later -- testing compound alert rules like
`cpu > 90 AND memory > 85`.

For more on alert testing patterns, see [Alert Testing](alert-testing.md).

For long-running or programmatic use, Sonda includes an HTTP API.

---

## The Server API

For long-running or programmatic use, Sonda includes an HTTP API that lets you submit,
monitor, and stop scenarios without touching the CLI.

### Start the server

=== "Docker (recommended)"

    Already running if you started the stack in the prerequisites. Otherwise:

    ```bash
    docker run -p 8080:8080 ghcr.io/davidban77/sonda-server:latest
    ```

=== "From source"

    ```bash
    cargo run -p sonda-server
    ```

### Submit a scenario

```bash
curl -X POST \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/simple-constant.yaml \
  http://localhost:8080/scenarios
```

```json
{"id":"a1b2c3d4-...","name":"up","status":"running"}
```

!!! tip "Using the scenario ID"
    The `POST` response includes an `id` field (a UUID). Use this ID in all subsequent
    requests to check status, scrape metrics, or stop the scenario. The examples below
    use `<id>` as a placeholder -- replace it with the actual UUID from your response.
    You can also pipe through `jq` to extract it:

    ```bash
    ID=$(curl -s -X POST -H "Content-Type: text/yaml" \
      --data-binary @examples/simple-constant.yaml \
      http://localhost:8080/scenarios | jq -r '.id')
    ```

### List scenarios

```bash
curl http://localhost:8080/scenarios
```

### Get scenario details

```bash
curl http://localhost:8080/scenarios/$ID
```

### Get live stats

```bash
curl http://localhost:8080/scenarios/$ID/stats
```

### Scrape metrics (Prometheus format)

```bash
curl http://localhost:8080/scenarios/$ID/metrics
```

### Stop a scenario

```bash
curl -X DELETE http://localhost:8080/scenarios/$ID
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
curl -X DELETE http://localhost:8080/scenarios/$ID
```

For the full API reference, see [Server API](../deployment/sonda-server.md).

The final step is getting your synthetic data into a real monitoring backend.

---

## Pushing to a Backend

The final step is getting your synthetic data into a real monitoring backend for
end-to-end validation. Sonda supports three approaches.

!!! info "Complete backend examples"
    For complete Docker Compose setups with VictoriaMetrics, Prometheus, and Grafana,
    see [Alert Testing](alert-testing.md) and [Docker Deployment](../deployment/docker.md).
    This section covers the Sonda-specific configuration.

### 1. HTTP Push (import API)

POST metrics directly to a backend's import endpoint. You can use CLI flags for quick
ad-hoc runs without writing a scenario file:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --sink http_push --endpoint http://victoriametrics:8428/api/v1/import/prometheus \
  --content-type "text/plain"
```

Or use a scenario file for more control:

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

Use the Prometheus remote write protocol for native compatibility. CLI flags let you
set up a quick ad-hoc run:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder remote_write \
  --sink remote_write --endpoint http://localhost:8428/api/v1/write
```

Or use a scenario file:

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

### 3. Loki (logs)

Push logs directly to Grafana Loki:

```bash
sonda logs --mode template --rate 10 --duration 30s \
  --sink loki --endpoint http://localhost:3100 \
  --label app=myservice --label env=staging
```

Or use a scenario file for richer templates:

```bash
sonda logs --scenario examples/loki-json-lines.yaml
```

### 4. OTLP/gRPC (OpenTelemetry Collector)

Push directly to an OpenTelemetry Collector via gRPC. This requires building with `--features otlp`.
Use CLI flags for a quick test:

```bash
sonda metrics --name cpu --rate 10 --duration 30s \
  --encoder otlp \
  --sink otlp_grpc --endpoint http://localhost:4317 --signal-type metrics
```

Or use a scenario file:

```bash
sonda metrics --scenario examples/otlp-metrics.yaml
```

```yaml title="examples/otlp-metrics.yaml (excerpt)"
encoder:
  type: otlp
sink:
  type: otlp_grpc
  endpoint: "http://localhost:4317"
  signal_type: metrics
```

The Collector routes data to any configured exporter (Prometheus, Jaeger, Loki, etc.), making this
the most flexible backend integration option.

### 5. Scrape via sonda-server

Point Prometheus at sonda-server's metrics endpoint:

```yaml title="prometheus.yml (scrape config)"
scrape_configs:
  - job_name: sonda
    static_configs:
      - targets: ["sonda-server:8080"]
    metrics_path: /scenarios/<id>/metrics
```

!!! tip "Scrape path"
    Replace `<id>` with the scenario ID returned by `POST /scenarios`. Each running
    scenario exposes its own metrics endpoint.

### Verify data arrived

```bash
# VictoriaMetrics
curl "http://localhost:8428/api/v1/query?query=sonda_http_request_duration_ms"

# Prometheus
curl "http://localhost:9090/api/v1/query?query=sonda_http_request_duration_ms"
```

---

## Next Steps

**Testing alert rules?** Start with [Alert Testing](alert-testing.md).

**Validating a pipeline change?** See [Pipeline Validation](pipeline-validation.md).

**Verifying recording rules?** Check [Recording Rules](recording-rules.md).

**Running Sonda in production?** See [Docker Deployment](../deployment/docker.md) or [Server API](../deployment/sonda-server.md).

**Browsing ready-to-use scenarios?** See [Example Scenarios](examples.md).
