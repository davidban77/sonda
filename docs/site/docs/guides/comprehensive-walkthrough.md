# Comprehensive Walkthrough

A hands-on tour of every Sonda capability. Work through each section in order, or jump to the
part you need. Every command and YAML snippet has been tested against Sonda v0.3.0.

---

## Part 1: Setup

### Prerequisites

- **Rust toolchain** (1.70+) -- install via [rustup](https://rustup.rs/)
- **Docker** and **Docker Compose v2** -- for the observability stack
- **Task** (optional) -- [taskfile.dev](https://taskfile.dev/) for convenience commands
- **netcat** (`nc`) -- for TCP/UDP sink testing
- **curl** -- for HTTP API testing

### Build from source

```bash
git clone https://github.com/davidban77/sonda.git
cd sonda
cargo build --workspace
```

Verify the build:

```bash
cargo run -p sonda -- --version
```

```text title="Output"
sonda 0.3.0
```

### Start the observability stack

The VictoriaMetrics compose stack includes VictoriaMetrics, vmagent, Grafana, and the sonda-server:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

Verify all services:

| Service | URL | Purpose |
|---------|-----|---------|
| sonda-server | `http://localhost:8080` | Sonda HTTP API |
| VictoriaMetrics | `http://localhost:8428` | Time series database |
| vmagent | `http://localhost:8429` | Metrics relay agent |
| Grafana | `http://localhost:3000` | Dashboards (no login required) |

Additional services are available via [Docker Compose profiles](https://docs.docker.com/compose/how-tos/profiles/):

| Service | URL | Profile |
|---------|-----|---------|
| Loki | `http://localhost:3100` | `--profile loki` |
| Kafka (external) | `localhost:9094` | `--profile kafka` |
| Kafka UI | `http://localhost:8081` | `--profile kafka` |

```bash
# Start with Loki:
docker compose -f examples/docker-compose-victoriametrics.yml --profile loki up -d

# Start with Kafka:
docker compose -f examples/docker-compose-victoriametrics.yml --profile kafka up -d

# Start everything:
docker compose -f examples/docker-compose-victoriametrics.yml --profile loki --profile kafka up -d
```

```bash
curl -s http://localhost:8080/health
```

```json title="Output"
{"status":"ok"}
```

---

## Part 2: CLI Basics -- Every Generator

Sonda ships six value generators. Each produces a different signal shape.

### Constant

Emits the same value every tick. The default value is `0` when no offset is specified.

```bash
sonda metrics --name up --rate 5 --duration 3s
```

```text title="Output"
up 0 1774298633558
up 0 1774298633764
up 0 1774298633964
up 0 1774298634160
up 0 1774298634364
...
```

Set a specific constant value with `--offset`:

```bash
sonda metrics --name up --rate 5 --duration 3s --offset 1
```

### Sine

Produces a smooth oscillation: `value = offset + amplitude * sin(2 * pi * tick / period_ticks)`.

```bash
sonda metrics \
  --name cpu_temp \
  --value-mode sine \
  --amplitude 25 \
  --offset 50 \
  --period-secs 10 \
  --rate 2 \
  --duration 5s
```

```text title="Output"
cpu_temp 50 1774298637531
cpu_temp 57.72542485937368 1774298638036
cpu_temp 64.69463130731182 1774298638535
cpu_temp 70.22542485937369 1774298639031
cpu_temp 73.77641290737884 1774298639534
cpu_temp 75 1774298640036
cpu_temp 73.77641290737884 1774298640534
cpu_temp 70.22542485937369 1774298641036
cpu_temp 64.69463130731182 1774298641536
cpu_temp 57.72542485937369 1774298642036
cpu_temp 50 1774298642536
```

The wave oscillates between 25 (`offset - amplitude`) and 75 (`offset + amplitude`) with a 10-second period.

### Sawtooth

A linear ramp from `min` to `max`, resetting at each period boundary.

```bash
sonda metrics \
  --name saw_wave \
  --value-mode sawtooth \
  --min 0 \
  --max 100 \
  --period-secs 5 \
  --rate 2 \
  --duration 5s
```

```text title="Output"
saw_wave 0 1774298648021
saw_wave 10 1774298648526
saw_wave 20 1774298649021
saw_wave 30 1774298649526
saw_wave 40 1774298650022
saw_wave 50 1774298650526
saw_wave 60 1774298651026
saw_wave 70 1774298651526
saw_wave 80 1774298652026
saw_wave 90 1774298652526
saw_wave 0 1774298653026
```

### Uniform Random

Generates random values between `min` and `max`. Use `--seed` for deterministic replay.

```bash
sonda metrics \
  --name random_val \
  --value-mode uniform \
  --min 10 \
  --max 90 \
  --seed 42 \
  --rate 3 \
  --duration 3s
```

```text title="Output"
random_val 69.32519030174588 1774298653967
random_val 68.2543018631486 1774298654305
random_val 27.068700996215277 1774298654639
random_val 15.486471330021853 1774298654972
random_val 68.41594195329043 1774298655305
random_val 48.6569421803497 1774298655639
random_val 88.521924357163 1774298655970
random_val 87.4730871720204 1774298656302
random_val 52.856660743260775 1774298656639
random_val 34.1756827733237 1774298656970
```

Re-running with `--seed 42` produces identical output -- useful for reproducible tests.

### Sequence

Steps through a predefined list of values. Requires a YAML scenario file.

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

```bash
sonda metrics --scenario examples/sequence-alert-test.yaml --duration 8s
```

```text title="Output"
cpu_spike_test{instance="server-01",job="node"} 10 1774298657916
cpu_spike_test{instance="server-01",job="node"} 10 1774298658921
cpu_spike_test{instance="server-01",job="node"} 10 1774298659921
cpu_spike_test{instance="server-01",job="node"} 10 1774298660919
cpu_spike_test{instance="server-01",job="node"} 10 1774298661921
cpu_spike_test{instance="server-01",job="node"} 95 1774298662921
cpu_spike_test{instance="server-01",job="node"} 95 1774298663919
cpu_spike_test{instance="server-01",job="node"} 95 1774298664921
cpu_spike_test{instance="server-01",job="node"} 95 1774298665921
```

Model interface flapping with binary values:

```yaml title="interface-flapping.yaml"
name: interface_oper_state
rate: 1
duration: 30s
generator:
  type: sequence
  values: [1, 1, 1, 0, 0, 0, 1, 1]
  repeat: true
labels:
  device: eth0
  hostname: router-01
encoder:
  type: prometheus_text
sink:
  type: stdout
```

### CSV Replay

Replays real values from a CSV file -- useful for reproducing production incidents.

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

The sample CSV contains a real incident pattern: baseline (~14%), spike to ~95%, recovery.
Run it with the scenario's default `duration: 60s` to see the full cycle:

```bash
sonda metrics --scenario examples/csv-replay-metrics.yaml
```

```text title="Output (truncated)"
cpu_replay{instance="prod-server-42",job="node"} 12.3 1774298673121
cpu_replay{instance="prod-server-42",job="node"} 14.1 1774298674126
cpu_replay{instance="prod-server-42",job="node"} 13.8 1774298675123
cpu_replay{instance="prod-server-42",job="node"} 15.2 1774298676123
...
cpu_replay{instance="prod-server-42",job="node"} 56.2 1774298685123
cpu_replay{instance="prod-server-42",job="node"} 71.8 1774298686123
cpu_replay{instance="prod-server-42",job="node"} 83.4 1774298687123
cpu_replay{instance="prod-server-42",job="node"} 89.1 1774298688123
cpu_replay{instance="prod-server-42",job="node"} 92.7 1774298689123
cpu_replay{instance="prod-server-42",job="node"} 95.3 1774298690123
...
cpu_replay{instance="prod-server-42",job="node"} 15.8 1774298709123
cpu_replay{instance="prod-server-42",job="node"} 14.6 1774298710123
cpu_replay{instance="prod-server-42",job="node"} 13.9 1774298711123
cpu_replay{instance="prod-server-42",job="node"} 14.2 1774298712123
```

!!! tip "How CSV replay maps ticks to rows"

    Each event tick reads the next CSV row in order. At `rate: 1` (one event per second), each
    row takes one second to emit. At `rate: 10`, the same CSV replays 10x faster.

    **Duration vs CSV size**: The sample file has 50 data rows. At `rate: 1`, you need at least
    `duration: 50s` to emit every row once. The scenario uses `duration: 60s` with `repeat: true`,
    so it wraps around and replays the first 10 rows again after the initial pass.

    **Replay real incidents**: Export real data from VictoriaMetrics (`/api/v1/export`) or
    Prometheus, save it as CSV, and replay it through your pipeline. This lets you validate
    alerting rules, dashboard thresholds, or ingest capacity without waiting for a real incident.

    **Tuning replay speed**: Increase `rate` to compress long incidents into short runs. A
    1440-row CSV (one row per minute over 24 hours) at `rate: 100` replays in ~14 seconds.

---

## Part 3: CLI Basics -- Every Encoder

Encoders determine the wire format of emitted telemetry.

### Prometheus Text (default)

The default format. Each line is a Prometheus exposition sample: `name{labels} value timestamp`.

```bash
sonda metrics --name test_metric --rate 2 --duration 2s --label host=web01
```

```text title="Output"
test_metric{host="web01"} 0 1774298694852
test_metric{host="web01"} 0 1774298695353
test_metric{host="web01"} 0 1774298695856
test_metric{host="web01"} 0 1774298696356
test_metric{host="web01"} 0 1774298696857
```

### InfluxDB Line Protocol

Compatible with InfluxDB, VictoriaMetrics `/write` endpoint, and Telegraf.

```bash
sonda metrics \
  --name test_metric \
  --rate 2 \
  --duration 2s \
  --encoder influx_lp \
  --label host=web01
```

```text title="Output"
test_metric,host=web01 value=0 1774298688849350000
test_metric,host=web01 value=0 1774298689351096000
test_metric,host=web01 value=0 1774298689854404000
test_metric,host=web01 value=0 1774298690354413000
test_metric,host=web01 value=0 1774298690853610000
```

Timestamps are in nanoseconds (InfluxDB convention).

### JSON Lines

Structured JSON, one object per line. Compatible with Elasticsearch, Loki, and NDJSON pipelines.

```bash
sonda metrics \
  --name test_metric \
  --rate 2 \
  --duration 2s \
  --encoder json_lines \
  --label host=web01
```

```json title="Output"
{"name":"test_metric","value":0.0,"labels":{"host":"web01"},"timestamp":"2026-03-23T20:44:51.835Z"}
{"name":"test_metric","value":0.0,"labels":{"host":"web01"},"timestamp":"2026-03-23T20:44:52.338Z"}
{"name":"test_metric","value":0.0,"labels":{"host":"web01"},"timestamp":"2026-03-23T20:44:52.840Z"}
```

### Syslog (RFC 5424)

For log events. Produces syslog-formatted output.

```bash
sonda logs \
  --mode template \
  --message "Request completed" \
  --rate 3 \
  --duration 2s \
  --encoder syslog \
  --label app=myservice
```

```text title="Output"
<14>1 2026-03-23T20:45:04.970Z sonda sonda - - - Request completed
<14>1 2026-03-23T20:45:05.306Z sonda sonda - - - Request completed
<14>1 2026-03-23T20:45:05.641Z sonda sonda - - - Request completed
```

### Remote Write (Prometheus protobuf)

!!! note
    Pre-built binaries and Docker images include remote-write support. The `--features remote-write` flag is only needed when building from source: `cargo build --features remote-write -p sonda`.

Used with the `remote_write` sink for pushing to Prometheus, VictoriaMetrics, Thanos Receive, Cortex, or Mimir.

```yaml title="examples/remote-write-vm.yaml"
name: cpu_usage_rw
rate: 10
duration: 60s
generator:
  type: sine
  amplitude: 50
  period_secs: 60
  offset: 50
labels:
  instance: server-01
  job: sonda
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
  batch_size: 100
```

Run the scenario (assumes VictoriaMetrics is listening on `localhost:8428`):

```bash
sonda metrics --scenario examples/remote-write-vm.yaml
```

Verify the metric arrived:

```bash
curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage_rw' | python3 -m json.tool
```

---

## Part 4: CLI Basics -- Every Sink

Sinks determine where encoded telemetry is sent.

### Stdout (default)

Prints to standard output. Useful for piping to other tools or debugging.

```bash
sonda metrics --name test --rate 2 --duration 2s
```

### File

Writes to a file on disk. Parent directories are created automatically.

```bash
sonda metrics \
  --name test_metric \
  --rate 2 \
  --duration 2s \
  --label host=web01 \
  --output /tmp/sonda-output.txt
```

```bash
cat /tmp/sonda-output.txt
```

```text title="Output"
test_metric{host="web01"} 0 1774298694852
test_metric{host="web01"} 0 1774298695353
test_metric{host="web01"} 0 1774298695856
test_metric{host="web01"} 0 1774298696356
test_metric{host="web01"} 0 1774298696857
```

In a YAML scenario, use the `file` sink type:

```yaml
sink:
  type: file
  path: /tmp/sonda-output.txt
```

### TCP

Streams metrics over a persistent TCP connection.

Start a listener, then send:

```bash
# Terminal 1: start listener
nc -l 9999 > /tmp/tcp-received.txt &

# Terminal 2: send metrics
sonda metrics --scenario examples/tcp-sink.yaml
```

```yaml title="TCP scenario (examples/tcp-sink.yaml)"
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

```text title="Received output"
tcp_test{host="web01"} 42 1774298981426
tcp_test{host="web01"} 42 1774298981764
tcp_test{host="web01"} 42 1774298982097
```

### UDP

Same pattern as TCP, but connectionless. Good for high-volume, loss-tolerant scenarios.

```bash
# Terminal 1: listen for UDP
nc -u -l 9998 > /tmp/udp-received.txt &

# Terminal 2: send metrics
sonda metrics --scenario examples/udp-sink.yaml
```

```yaml title="UDP scenario (examples/udp-sink.yaml)"
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

```json title="Received output"
{"name":"udp_test","value":99.0,"labels":{"host":"router01"},"timestamp":"2026-03-23T20:49:57.487Z"}
{"name":"udp_test","value":99.0,"labels":{"host":"router01"},"timestamp":"2026-03-23T20:49:57.825Z"}
```

### HTTP Push

POSTs batches to an HTTP endpoint. Compatible with VictoriaMetrics, Prometheus, and vmagent.

```yaml title="examples/http-push-sink.yaml"
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
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain; version=0.0.4"
  batch_size: 65536
```

### Loki

Pushes log entries to a Grafana Loki instance via the push API.

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

Start the Loki service if you haven't already:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml --profile loki up -d
```

Run the scenario:

```bash
sonda logs --scenario examples/loki-json-lines.yaml
```

Verify logs arrived in Loki:

```bash
curl -s 'http://localhost:3100/loki/api/v1/query?query={job="sonda"}' | python3 -m json.tool
```

You can also explore logs in Grafana at `http://localhost:3000` by selecting the **Loki** datasource in Explore.

### Kafka

Publishes to a Kafka topic.

!!! note
    Pre-built binaries and Docker images include Kafka support. The `--features kafka` flag is only needed when building from source: `cargo build --features kafka -p sonda`.

Start the Kafka service:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml --profile kafka up -d
```

```yaml title="examples/kafka-sink.yaml"
name: kafka_constant
rate: 100.0
duration: 30s
generator:
  type: constant
  value: 1.0
labels:
  hostname: localhost
  env: dev
encoder:
  type: prometheus_text
sink:
  type: kafka
  brokers: "localhost:9094"
  topic: sonda-metrics
```

```bash
sonda metrics --scenario examples/kafka-sink.yaml
```

For log events over Kafka:

```yaml title="examples/kafka-json-logs.yaml"
name: app_logs_kafka
rate: 10
duration: 60s
generator:
  type: template
  templates:
    - message: "Event from {service} severity {level}"
      field_pools:
        service: ["auth", "api", "worker"]
        level: ["INFO", "WARN", "ERROR"]
  severity_weights:
    info: 0.7
    warn: 0.2
    error: 0.1
encoder:
  type: json_lines
sink:
  type: kafka
  brokers: "localhost:9094"
  topic: sonda-logs
```

```bash
sonda logs --scenario examples/kafka-json-logs.yaml
```

Inspect messages in Kafka UI at `http://localhost:8081`.

---

## Part 5: Log Generation

Sonda generates structured log events in two modes.

### Template Mode

Define message templates with `{placeholder}` tokens and field pools to randomize values.

```yaml title="examples/log-template.yaml"
name: app_logs_template
rate: 10
duration: 60s
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
  seed: 42
labels:
  device: wlan0
  hostname: router-01
encoder:
  type: json_lines
sink:
  type: stdout
```

You can attach static `labels` to every log event in a scenario. Labels appear in both JSON Lines
and syslog output (as RFC 5424 structured data). This is useful for tagging log streams by source,
environment, or device.

```bash
sonda logs --scenario examples/log-template.yaml --duration 3s
```

```json title="Output (each line is a separate JSON object)"
{"timestamp":"2026-03-23T20:45:12.020Z","severity":"info","message":"Request from 10.0.0.3 to /api/v1/users returned 404","labels":{"device":"wlan0","hostname":"router-01"},"fields":{"endpoint":"/api/v1/users","ip":"10.0.0.3","status":"404"}}
{"timestamp":"2026-03-23T20:45:12.122Z","severity":"error","message":"Service transform processed 100 events in 47ms","labels":{"device":"wlan0","hostname":"router-01"},"fields":{"count":"100","duration_ms":"47","service":"transform"}}
{"timestamp":"2026-03-23T20:45:12.225Z","severity":"error","message":"Request from 10.0.0.3 to /api/v1/metrics returned 500","labels":{"device":"wlan0","hostname":"router-01"},"fields":{"endpoint":"/api/v1/metrics","ip":"10.0.0.3","status":"500"}}
```

Quick CLI-only log generation:

```bash
sonda logs \
  --mode template \
  --message "User {user} logged in from {ip}" \
  --rate 3 \
  --duration 2s \
  --encoder json_lines
```

!!! note
    Field pools are only available via YAML scenarios. CLI-only templates emit placeholder tokens as literal text.

### Replay Mode

Replays lines from an existing log file, cycling back to the start when exhausted.

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

### Syslog Encoder for Logs

Combine the syslog encoder with log generation for RFC 5424 output:

```bash
sonda logs \
  --mode template \
  --message "Request completed" \
  --rate 3 \
  --duration 2s \
  --encoder syslog
```

```text title="Output"
<14>1 2026-03-23T20:45:04.970Z sonda sonda - - - Request completed
```

---

## Part 6: Gap Windows and Burst Windows

Gap and burst windows let you simulate real-world failure and load patterns.

### Gap Windows

Gaps create recurring silent periods where no events are emitted. This simulates scrape failures,
network partitions, or agent restarts.

```bash
sonda metrics \
  --name gap_test \
  --rate 5 \
  --duration 10s \
  --gap-every 4s \
  --gap-for 2s
```

```text title="Output (timestamps show ~2s gap between 717993 and 719996)"
gap_test 0 1774298715992
gap_test 0 1774298716197
gap_test 0 1774298716797
gap_test 0 1774298717193
gap_test 0 1774298717393
gap_test 0 1774298717795
gap_test 0 1774298717993
gap_test 0 1774298719996   <-- ~2 second gap
gap_test 0 1774298720198
gap_test 0 1774298720401
...
```

The gap cycle: emit for 2s, go silent for 2s, repeat. Out of every 4-second window, 2 seconds have no data.

### Burst Windows

Bursts create high-rate periods. The rate multiplier increases throughput during the burst window.

```bash
sonda metrics \
  --name burst_test \
  --rate 2 \
  --duration 6s \
  --burst-every 3s \
  --burst-for 1s \
  --burst-multiplier 5
```

During burst: rate = `2 * 5 = 10` events/sec (100ms intervals).
Outside burst: rate = 2 events/sec (500ms intervals).

```text title="Output (first 1s has tight ~100ms spacing, then ~500ms)"
burst_test 0 1774298731273
burst_test 0 1774298731378   <-- ~100ms burst
burst_test 0 1774298731478
burst_test 0 1774298731578
...
burst_test 0 1774298732278
burst_test 0 1774298732378
burst_test 0 1774298732877   <-- ~500ms normal
burst_test 0 1774298733378
burst_test 0 1774298733878
```

### Gap Windows in YAML

```yaml
name: interface_oper_state
rate: 1000
duration: 30s
generator:
  type: sine
  amplitude: 5.0
  period_secs: 30
  offset: 10.0
gaps:
  every: 2m
  for: 20s
labels:
  hostname: t0-a1
  zone: eu1
encoder:
  type: prometheus_text
sink:
  type: stdout
```

### Burst Windows in YAML

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

### Mapping to Alert Testing

| Pattern | Simulates | Alert Testing Use |
|---------|-----------|-------------------|
| **Gap window** | Scrape failure, agent restart | Test alert resolution during data absence |
| **Burst window** | Traffic spike, DDoS | Test rate-based alerts, buffer overflow |
| **Gap + sequence** | Interface flapping | Test flap detection rules |

---

## Part 7: Multi-Scenario with Correlation

Use `sonda run` to execute multiple scenarios concurrently from a single YAML file.

### Basic Multi-Scenario

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
            endpoint: ["/api/v1/health", "/api/v1/metrics"]
      severity_weights:
        info: 0.7
        warn: 0.2
        error: 0.1
      seed: 42
    labels:
      service: api-gateway
      env: staging
    encoder:
      type: json_lines
    sink:
      type: file
      path: /tmp/sonda-logs.json
```

```bash
sonda run --scenario examples/multi-scenario.yaml
```

Metrics go to stdout, logs go to `/tmp/sonda-logs.json`. Both run on separate threads.

### Phase Offset for Correlation

The `phase_offset` field delays a scenario's start relative to the group. Combined with
`clock_group`, this creates controlled timing relationships between metrics.

```yaml title="examples/multi-metric-correlation.yaml"
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
    encoder:
      type: prometheus_text
    sink:
      type: stdout

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
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

```bash
sonda run --scenario examples/multi-metric-correlation.yaml
```

CPU spikes to 95% at t=0s. Memory follows at t=3s with 88%. This tests a compound alert:

```yaml title="Alert rule under test"
- alert: HighCpuAndMemory
  expr: cpu_usage > 90 AND memory_usage_percent > 85
  for: 5m
```

### Network Device Monitoring Example

Model a network device with correlated metrics:

```yaml title="network-monitoring.yaml"
scenarios:
  - signal_type: metrics
    name: interface_oper_state
    rate: 1
    duration: 120s
    phase_offset: "0s"
    clock_group: network-test
    generator:
      type: sequence
      values: [1, 1, 1, 0, 0, 0, 1, 1]
      repeat: true
    labels:
      device: eth0
      hostname: router-01
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  - signal_type: metrics
    name: hardware_cpu_percent
    rate: 1
    duration: 120s
    phase_offset: "0s"
    clock_group: network-test
    generator:
      type: sine
      amplitude: 30.0
      period_secs: 60
      offset: 50.0
    labels:
      hostname: router-01
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  - signal_type: metrics
    name: hardware_memory_percent
    rate: 1
    duration: 120s
    phase_offset: "10s"
    clock_group: network-test
    generator:
      type: constant
      value: 65.0
    labels:
      hostname: router-01
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

---

## Part 8: sonda-server REST API

> For the complete reference, see [Server API](../deployment/sonda-server.md).

The sonda-server provides an HTTP control plane for managing scenarios programmatically.

### Start the Server

```bash
cargo run -p sonda-server -- --port 8080 --bind 0.0.0.0
```

Or use the Docker Compose stack (already running if you started it in Part 1).

### POST /scenarios -- Create a Scenario

Submit a YAML scenario body:

```bash
curl -s -X POST \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/simple-constant.yaml \
  http://localhost:8080/scenarios | python3 -m json.tool
```

```json title="Response (201 Created)"
{
    "id": "656e067f-bab8-487b-ad24-ab37cae053a8",
    "name": "up",
    "status": "running"
}
```

### POST a Log Scenario

```bash
curl -s -X POST \
  -H "Content-Type: text/yaml" \
  -d '
signal_type: logs
name: app_logs_server
rate: 5
duration: 30s
generator:
  type: template
  templates:
    - message: "Connection from {ip}"
      field_pools:
        ip: ["10.0.0.1", "10.0.0.2"]
  severity_weights:
    info: 0.8
    error: 0.2
encoder:
  type: json_lines
sink:
  type: stdout
' http://localhost:8080/scenarios | python3 -m json.tool
```

```json title="Response"
{
    "id": "d4b0c49f-eef9-436e-abf8-dd033e5503de",
    "name": "app_logs_server",
    "status": "running"
}
```

### GET /scenarios -- List All

```bash
curl -s http://localhost:8080/scenarios | python3 -m json.tool
```

```json title="Response"
{
    "scenarios": [
        {
            "id": "656e067f-bab8-487b-ad24-ab37cae053a8",
            "name": "up",
            "status": "running",
            "elapsed_secs": 2.11264175
        }
    ]
}
```

### GET /scenarios/{id} -- Inspect Detail

```bash
curl -s http://localhost:8080/scenarios/656e067f-... | python3 -m json.tool
```

```json title="Response"
{
    "id": "656e067f-bab8-487b-ad24-ab37cae053a8",
    "name": "up",
    "status": "running",
    "elapsed_secs": 2.170663833,
    "stats": {
        "total_events": 22,
        "current_rate": 9.987737964211696,
        "bytes_emitted": 418,
        "errors": 0
    }
}
```

### GET /scenarios/{id}/stats -- Live Stats

```bash
curl -s http://localhost:8080/scenarios/656e067f-.../stats | python3 -m json.tool
```

```json title="Response"
{
    "total_events": 23,
    "current_rate": 9.987737964211696,
    "target_rate": 10.0,
    "bytes_emitted": 437,
    "errors": 0,
    "uptime_secs": 2.207126333,
    "state": "running",
    "in_gap": false,
    "in_burst": false
}
```

### GET /scenarios/{id}/metrics -- Prometheus Scrape

Returns recent metrics in Prometheus text format, suitable for scraping:

```bash
curl -s http://localhost:8080/scenarios/656e067f-.../metrics
```

```text title="Response"
up 1 1774298869889
up 1 1774298869994
up 1 1774298870094
up 1 1774298870194
up 1 1774298870293
```

Configure Prometheus or vmagent to scrape this endpoint:

```yaml title="prometheus.yml snippet"
scrape_configs:
  - job_name: sonda
    metrics_path: /scenarios/<scenario-id>/metrics
    static_configs:
      - targets: ["sonda-server:8080"]
```

### DELETE /scenarios/{id} -- Stop

```bash
curl -s -X DELETE http://localhost:8080/scenarios/656e067f-... | python3 -m json.tool
```

```json title="Response"
{
    "id": "656e067f-bab8-487b-ad24-ab37cae053a8",
    "status": "stopped",
    "total_events": 24
}
```

### Long-Running Scenarios (Start / Stop Pattern)

Every example above includes a `duration` field, so scenarios auto-terminate after a few
seconds. For continuous synthetic load — alert-rule testing, dashboard demos, pipeline
soak tests — **omit `duration`** and the scenario runs indefinitely until you stop it.

**1. Start** — POST a scenario without `duration`:

```bash
curl -s -X POST \
  -H "Content-Type: text/yaml" \
  -d '
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
' http://localhost:8080/scenarios | python3 -m json.tool
```

```json title="Response (201 Created)"
{
    "id": "a1b2c3d4-...",
    "name": "continuous_cpu",
    "status": "running"
}
```

**2. Monitor** — check live stats while it runs:

```bash
curl -s http://localhost:8080/scenarios/a1b2c3d4-.../stats | python3 -m json.tool
```

```json title="Response"
{
    "total_events": 1200,
    "current_rate": 10.01,
    "target_rate": 10.0,
    "bytes_emitted": 62400,
    "errors": 0,
    "uptime_secs": 120.03,
    "state": "running",
    "in_gap": false,
    "in_burst": false
}
```

**3. Stop** — DELETE when you are done:

```bash
curl -s -X DELETE http://localhost:8080/scenarios/a1b2c3d4-... | python3 -m json.tool
```

```json title="Response"
{
    "id": "a1b2c3d4-...",
    "status": "stopped",
    "total_events": 1200
}
```

!!! tip
    The same pattern works for log scenarios — just add `signal_type: logs` and use a
    log generator/encoder. See [Scenario File reference](../configuration/scenario-file.md)
    for the full schema.

### Multiple Concurrent Scenarios

Post several scenarios, each gets its own thread:

```bash
# Start three scenarios
curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/simple-constant.yaml \
  http://localhost:8080/scenarios

curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/docker-metrics.yaml \
  http://localhost:8080/scenarios

curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/burst-metrics.yaml \
  http://localhost:8080/scenarios

# List all running
curl -s http://localhost:8080/scenarios | python3 -m json.tool
```

---

## Part 9: Pushing to VictoriaMetrics

> For Docker Compose stacks with VictoriaMetrics, see [Docker deployment](../deployment/docker.md#victoriametrics-stack).

Three ways to get Sonda data into VictoriaMetrics.

### Direct Push via HTTP

Use the `http_push` sink to POST Prometheus text format directly:

```yaml title="examples/victoriametrics-metrics.yaml"
name: sonda_http_request_duration_ms
rate: 10
duration: 120s
generator:
  type: sine
  amplitude: 40.0
  period_secs: 30
  offset: 60.0
labels:
  instance: api-server-01
  job: sonda
  method: GET
encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
  batch_size: 65536
```

!!! note
    When running inside the Docker Compose stack, use `http://victoriametrics:8428` as the URL. From the host, use `http://localhost:8428`.

### Remote Write to vmagent

!!! note
    Pre-built binaries and Docker images include remote-write support. The `--features remote-write` flag is only needed when building from source: `cargo build --features remote-write -p sonda`.

```yaml title="examples/remote-write-vm.yaml"
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8429/api/v1/write"
  batch_size: 100
```

Compatible targets: VictoriaMetrics, vmagent, Prometheus, Thanos Receive, Cortex, Mimir, Grafana Cloud.

Run the scenario:

```bash
sonda metrics --scenario examples/remote-write-vm.yaml
```

### Scrape-Based via sonda-server

Configure Prometheus or vmagent to scrape the sonda-server metrics endpoint:

1. Start a scenario via the API (see Part 8).
2. Use the `GET /scenarios/{id}/metrics` endpoint as the scrape target.
3. Add to your Prometheus config.

### Verify Data Arrived

```bash
# Check if the metric series exist
curl -s 'http://localhost:8428/api/v1/series?match[]={__name__=~"sonda.*"}' | python3 -m json.tool

# Query the latest value
curl -s 'http://localhost:8428/api/v1/query?query=sonda_http_request_duration_ms' | python3 -m json.tool

# Query a range
curl -s 'http://localhost:8428/api/v1/query_range?query=sonda_http_request_duration_ms&start=2026-03-23T00:00:00Z&end=2026-03-23T23:59:59Z&step=60s' | python3 -m json.tool
```

### View in Grafana

1. Open `http://localhost:3000` (no login required -- anonymous admin is enabled).
2. Go to **Dashboards > Sonda Overview**.
3. The dashboard shows four panels:
    - **Generated Metric Values** -- time series of all Sonda metrics
    - **Event Rate** -- events/sec for each scenario
    - **Active Scenarios** -- count of running scenarios
    - **Gap/Burst Indicators** -- visual indicators of gap and burst windows

---

## Part 10: Alert Testing Scenarios

### Threshold Alert: Sine Wave Crossing 90%

```yaml title="threshold-test.yaml"
name: cpu_usage
rate: 1
duration: 180s
generator:
  type: sine
  amplitude: 50.0
  period_secs: 60
  offset: 50.0
labels:
  instance: server-01
  job: node
encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

The wave oscillates 0-100. It exceeds 90 for about 12.3 seconds per 60-second cycle.

Test against this alert rule:

```yaml title="Alert rule"
- alert: HighCPU
  expr: cpu_usage > 90
  for: 10s
```

The alert fires because the sine spends ~12.3s above 90, exceeding the 10s `for:` duration.

### Duration-Based Alert: Sequence at 95% for Exactly N Seconds

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

5 seconds at baseline (10%), then 5 seconds at spike (95%), then 6 seconds recovery. This tests whether an alert with `for: 5s` fires exactly at the right moment.

### Alert Resolution via Gap Window

```yaml title="gap-resolution-test.yaml"
name: error_rate
rate: 1
duration: 120s
generator:
  type: constant
  value: 95.0
gaps:
  every: 30s
  for: 10s
labels:
  instance: server-01
  job: node
encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

The metric holds at 95% but disappears for 10 seconds every 30 seconds. This tests whether your alert resolves during the gap (stale marker behavior).

### Multi-Condition Alert: CPU AND Memory

Use the multi-metric correlation scenario from Part 7:

```bash
sonda run --scenario examples/multi-metric-correlation.yaml
```

The compound condition `cpu_usage > 90 AND memory_usage_percent > 85` is satisfied starting at t=3s when memory enters its spike phase.

### Network-Specific: Interface Down Alert

```yaml title="interface-down-alert-test.yaml"
scenarios:
  - signal_type: metrics
    name: interface_oper_state
    rate: 1
    duration: 60s
    phase_offset: "0s"
    clock_group: nre-test
    generator:
      type: sequence
      values: [1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1]
      repeat: true
    labels:
      device: ge-0/0/0
      hostname: core-rtr-01
    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
```

Test against: `alert: InterfaceDown / expr: interface_oper_state == 0 / for: 3s`

---

## Part 11: Recording Rule Validation

Push known values, then verify computed outputs match expectations.

### Constant Value Test

```yaml title="examples/recording-rule-test.yaml"
name: http_requests_total
rate: 1
duration: 120s
generator:
  type: constant
  value: 100.0
labels:
  method: GET
  status: "200"
  job: api
encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

Pair with a recording rule:

```yaml title="examples/recording-rule-prometheus.yml"
groups:
  - name: sonda-test-rules
    rules:
      - record: job:http_requests_total:rate5m
        expr: sum(rate(http_requests_total[5m])) by (job)
```

### Workflow

1. Start the VictoriaMetrics stack.
2. Push known values: `sonda metrics --scenario examples/recording-rule-test.yaml`
3. Load the recording rule into Prometheus or vmalert.
4. Wait at least two evaluation intervals (default 1 minute each).
5. Query the output:

```bash
curl -s 'http://localhost:8428/api/v1/query?query=job:http_requests_total:rate5m' | python3 -m json.tool
```

For a constant gauge pushed at 1 event/sec, `rate()` measures per-second change -- expect near-zero for a constant value.

---

## Part 12: Long-Running Scenarios

### Start a Long Scenario

```bash
sonda metrics \
  --scenario examples/victoriametrics-metrics.yaml \
  --duration 600s
```

Or via the API for background execution:

```bash
curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/victoriametrics-metrics.yaml \
  http://localhost:8080/scenarios
```

### Monitor via Stats API

Poll the stats endpoint periodically:

```bash
SCENARIO_ID="<id-from-create-response>"

# Check every 30 seconds
curl -s "http://localhost:8080/scenarios/$SCENARIO_ID/stats" | python3 -m json.tool
```

Watch for:

- `total_events` increasing steadily
- `current_rate` matching `target_rate`
- `errors` remaining at 0
- `in_gap` / `in_burst` toggling as configured

### View in Grafana

Open `http://localhost:3000` and navigate to **Dashboards > Sonda Overview**. The dashboard auto-refreshes and shows:

- Real-time metric values as a time series
- Event rate tracking
- Active scenario count
- Gap and burst state indicators

### Graceful Shutdown

Press `Ctrl+C` to stop the CLI. The runner thread finishes its current event and exits cleanly.

For the server, `DELETE /scenarios/{id}` stops a specific scenario. Server shutdown (`Ctrl+C` on the server process) stops all running scenarios.

---

## Part 13: Docker and Kubernetes Deployment

> For the complete references, see [Docker](../deployment/docker.md) and [Kubernetes](../deployment/kubernetes.md).

### Docker Run (Single Metric)

```bash
docker run --rm --entrypoint /sonda ghcr.io/davidban77/sonda:latest \
  metrics --name cpu_test --rate 5 --duration 10s --value-mode sine \
  --amplitude 50 --offset 50 --period-secs 10
```

### Docker Compose (Full Stack)

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

This starts:

- **sonda-server** on port 8080
- **VictoriaMetrics** on port 8428
- **vmagent** on port 8429
- **Grafana** on port 3000 (with pre-provisioned VictoriaMetrics datasource and Sonda Overview dashboard)

Loki and Kafka are available via profiles:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml --profile loki --profile kafka up -d
```

Submit scenarios via the API:

```bash
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/victoriametrics-metrics.yaml \
  http://localhost:8080/scenarios
```

Tear down:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

### Helm Chart Deployment

The Helm chart deploys sonda-server to Kubernetes:

```bash
helm install sonda helm/sonda \
  --set image.tag=0.3.0 \
  --set server.port=8080
```

Render templates without deploying:

```bash
helm template sonda helm/sonda
```

```yaml title="Rendered Deployment (partial)"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: sonda
spec:
  replicas: 1
  template:
    spec:
      containers:
        - name: sonda
          image: ghcr.io/davidban77/sonda:0.3.0
          args: ["--port", "8080", "--bind", "0.0.0.0"]
          ports:
            - name: http
              containerPort: 8080
          livenessProbe:
            httpGet:
              path: /health
              port: http
```

Mount scenario files via ConfigMap:

```yaml title="values.yaml"
scenarios:
  basic-metrics.yaml: |
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
```

---

## Part 14: CI/CD Integration

> For running Sonda in containers, see [Docker deployment](../deployment/docker.md).

### GitHub Actions Example

```yaml title=".github/workflows/alert-test.yml"
name: Alert Rule Validation
on: [push]

jobs:
  test-alerts:
    runs-on: ubuntu-latest
    services:
      victoriametrics:
        image: victoriametrics/victoria-metrics:v1.108.1
        ports:
          - 8428:8428

    steps:
      - uses: actions/checkout@v4

      - name: Install Sonda
        run: |
          curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh

      - name: Push test metrics
        run: |
          sonda metrics \
            --scenario examples/recording-rule-test.yaml \
            --duration 30s

      - name: Verify data arrived
        run: |
          SERIES=$(curl -s 'http://localhost:8428/api/v1/series?match[]={__name__="http_requests_total"}')
          echo "$SERIES"
          COUNT=$(echo "$SERIES" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('data',[])))")
          test "$COUNT" -gt 0 || exit 1
```

### Exit Code Checking

Sonda exits with code 0 on success, non-zero on errors. Use this for CI assertions:

```bash
sonda metrics --name test --rate 1 --duration 5s \
  --output /tmp/test-output.txt \
  && echo "PASS" || echo "FAIL"
```

### Duration-Bounded Runs

Always use `--duration` in CI to prevent runaway scenarios:

```bash
sonda metrics --scenario my-scenario.yaml --duration 30s
```

---

## Part 15: Improvement Recommendations

The following table captures areas where Sonda could improve for SRE and NRE adoption.

| Category | Improvement | Why It Matters | Priority |
|----------|------------|----------------|----------|
| **CLI UX** | Add `--dry-run` flag to preview config without emitting | Helps debug YAML scenarios before running long tests | High |
| **CLI UX** | Support `--value` flag directly for constant mode (instead of `--offset`) | `--offset 1` for a constant value is confusing; `--value 1` is intuitive | High |
| **CLI UX** | Add `--verbose` / `--quiet` flags for controlling output verbosity | No way to see stats summary after a run without the server | Medium |
| **CLI UX** | Print a summary line on completion (total events, duration, errors) | Currently the CLI exits silently; you have to count lines yourself | High |
| **Generators** | Add `step` generator (monotonically increasing counter) | Essential for testing `rate()` and `increase()` PromQL functions | High |
| **Generators** | Add `spike` generator (baseline with configurable spike events) | More intuitive than sequence for simple threshold tests | Medium |
| **Generators** | Add `jitter` option to generators (add random noise to any signal) | Real metrics are never perfectly smooth; jitter improves realism | Medium |
| **Generators** | Support multiple CSV columns in csv_replay (multi-metric from one file) | Production exports have many columns; one metric per run is limiting | Low |
| **Encoders** | Add OpenTelemetry Protocol (OTLP) encoder | Industry standard; needed for OTEL Collector pipelines | High |
| **Sinks** | Add webhook/callback sink (POST to arbitrary URL on scenario events) | Enables workflow triggering -- Prefect, Ansible EDA, PagerDuty | High |
| **Sinks** | Add OTLP/gRPC sink | Direct push to OTEL Collector | High |
| **Labels** | Label cardinality simulation (rotating hostnames, pod names) | Testing cardinality explosion and high-cardinality queries | High |
| **Labels** | Label templates with counters (`host-{n}` where n increments) | Simulating dynamic infrastructure (auto-scaling, container churn) | Medium |
| **Scenario** | Scenario templates/presets for common NRE patterns | Reduce YAML boilerplate for common use cases (interface flapping, BGP) | Medium |
| **Scenario** | Conditional scenario chaining (if metric A > threshold, start scenario B) | Model cascading failures and cause-effect relationships | Low |
| **Server** | `POST /scenarios` accept multi-scenario YAML (like `sonda run`) | Currently only single scenarios via API; `sonda run` has multi-scenario | High |
| **Server** | Scenario scheduling (start at specific time, repeat on cron) | Long-running synthetic monitoring without external orchestration | Medium |
| **Server** | Server-Sent Events (SSE) or WebSocket for live stats streaming | Polling stats is clunky; streaming enables real-time dashboards | Low |
| **Server** | API authentication (API key or mTLS) | Required for production deployment | Medium |
| **Deployment** | Prometheus ServiceMonitor in Helm chart | Auto-discovery in Prometheus Operator deployments | Medium |
| **Deployment** | Alertmanager integration example (fire test alert, verify AM receives) | Complete the alert testing loop | High |
| **Deployment** | Pre-built scenario library (YAML files for common patterns) | Downloadable patterns for CPU spike, memory leak, interface flap, etc. | Medium |
| **Documentation** | Interactive scenario builder (web UI or TUI) | Lower the barrier for YAML authoring | Low |
| **Traces** | Add trace/span generation (signal_type: traces) | Completes the three pillars of observability | Medium |
| **Testing** | E2E test for Loki sink (currently only VM and Kafka tested) | Loki sink is documented but not CI-verified | Medium |
| **NRE-Specific** | SNMP trap generator (UDP sink + SNMP-formatted payload) | Network monitoring validation | Low |
| **NRE-Specific** | gNMI/gRPC streaming telemetry simulation | Modern network telemetry testing | Low |
| **Automation** | EvidenceBundle output format (structured snapshot of metrics+logs) | Direct integration with incident response automation | Medium |

---

## Part 16: Additional Guides and Use Cases

### Guide 1: Testing Network Automation Workflows with Sonda

**Audience**: Network Reliability Engineers using Ansible EDA, Prefect, or StackStorm.

**Outline**:

- Generate `interface_oper_state` and `bgp_session_state` metrics using the sequence generator
- Push to VictoriaMetrics, configure alert rules
- Wire Alertmanager webhook to Ansible EDA event source
- Verify the automation runbook triggers on the synthetic alert
- Test flap detection by varying sequence timing
- Validate that remediation workflows run end-to-end

### Guide 2: Simulating Network Device Telemetry

**Audience**: NREs building monitoring dashboards for network infrastructure.

**Outline**:

- Model a network device with 10+ interfaces using multi-scenario YAML
- Generate correlated metrics: `interface_oper_state`, `interface_in_octets`, `interface_out_octets`, `interface_errors`
- Use sawtooth for traffic counters, sequence for state, sine for CPU/memory
- Push to VictoriaMetrics and build Grafana dashboards
- Simulate a link failure cascade (one interface goes down, traffic shifts)
- Validate SNMP-style metrics work with Prometheus relabeling rules

### Guide 3: Validating EvidenceBundle Collection Pipelines

**Audience**: SREs building automated incident response workflows.

**Outline**:

- Define what an EvidenceBundle is (metrics snapshot + log excerpt + timeline)
- Generate synthetic metrics and logs that represent a known incident
- Push metrics to VM, logs to Loki, both with correlated labels
- Trigger an alert that kicks off evidence collection
- Verify the collected bundle contains the expected data
- Test edge cases: gap windows during collection, high-cardinality labels

### Guide 4: Long-Running Synthetic Monitoring Setup

**Audience**: Platform engineers who want persistent telemetry for dashboard validation.

**Outline**:

- Deploy sonda-server via Helm chart in a Kubernetes cluster
- Submit long-duration scenarios (hours/days) via the API
- Configure Prometheus ServiceMonitor to scrape sonda-server
- Set up Grafana dashboards for monitoring the synthetic data
- Monitor sonda-server health via the stats API
- Handle scenario rotation (stop old, start new) for changing test patterns
- Alert on sonda itself (detect if synthetic data stops flowing)

### Guide 5: CI/CD Alert Rule Validation Pipeline

**Audience**: SREs who want to test alert rules in CI before deploying to production.

**Outline**:

- Set up a GitHub Actions workflow with VictoriaMetrics service container
- Push synthetic metrics matching each alert rule's conditions
- Wait for evaluation intervals
- Query VictoriaMetrics to verify the alert would fire
- Assert on expected metric values and alert states
- Integrate with PR review process (alert rule changes require passing tests)

### Guide 6: Capacity Planning with Synthetic Load

**Audience**: Platform engineers sizing observability infrastructure.

**Outline**:

- Generate high-volume metrics (1000+ events/sec) to test pipeline throughput
- Vary label cardinality to find cardinality limits
- Use burst windows to simulate traffic spikes
- Measure VictoriaMetrics/Prometheus ingestion rate and storage growth
- Calculate infrastructure sizing based on synthetic load tests
