# Alert Testing

Test your Prometheus and VictoriaMetrics alerting rules with synthetic metrics before deploying
them to production. Sonda generates the exact metric shapes to trigger alerts, so you can verify
they fire (and resolve) exactly when you expect.

## The Problem

You write alert rules, deploy them, and hope they work. But common issues slip through:

- An alert with `for: 5m` that never fires because the metric only breaches the threshold for 3 minutes.
- A gap-fill rule that triggers a false alert during a 30-second scrape outage.
- A compound alert (`A > 90 AND B > 85`) that never fires because the two metrics never overlap.

Sonda solves this by generating metrics with **exact values**, **precise timing**, and
**configurable failure patterns** -- all driven from a YAML file you can check into your repo.

## Threshold Alerts

**Goal**: verify a `HighCPU` alert fires when `cpu_usage > 90`.

### The Sine Generator Math

The sine generator produces: `value = offset + amplitude * sin(2 * pi * tick / period_ticks)`

With `amplitude=50` and `offset=50`, the wave oscillates between 0 and 100.
A threshold at 90 is crossed when `sin(x) > 0.8`:

- `sin(x) = 0.8` at `x = arcsin(0.8) = 0.927 radians` (53.1 degrees)
- The sine exceeds 0.8 from `x = 0.927` to `x = pi - 0.927 = 2.214`
- That is `1.287 / 6.283 = 20.5%` of each cycle
- With a 60-second period: **~12.3 seconds above 90 per cycle**

### Value at Each Tick

| Tick (sec) | sin(2*pi*t/60) | Value | Above 90? |
|------------|----------------|-------|-----------|
| 0  | 0.000  | 50.0  | No  |
| 5  | 0.500  | 75.0  | No  |
| 10 | 0.866  | 93.3  | Yes |
| 15 | 1.000  | 100.0 | Yes |
| 20 | 0.866  | 93.3  | Yes |
| 25 | 0.500  | 75.0  | No  |

### Working Example

```yaml title="sine-threshold-test.yaml"
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
  type: stdout
```

```bash
sonda metrics --scenario sine-threshold-test.yaml
```

Output (one line per second):

```
cpu_usage{instance="server-01",job="node"} 50 1774287245640
cpu_usage{instance="server-01",job="node"} 55.226 1774287246641
cpu_usage{instance="server-01",job="node"} 60.395 1774287247645
...
cpu_usage{instance="server-01",job="node"} 93.301 1774287255641
cpu_usage{instance="server-01",job="node"} 100.0 1774287260641
```

The metric crosses 90 around tick 9, stays above until tick 21, then drops -- giving you
~12 seconds above threshold per 60-second cycle.

## Testing `for:` Duration Behavior

Prometheus alerts with a `for:` clause require the condition to be true for a **continuous**
duration before firing. Use the sequence generator for exact control.

### Sequence Generator Approach

The sequence generator steps through an explicit list of values, one per tick:

```yaml title="for-duration-test.yaml"
name: cpu_usage
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
sonda metrics --scenario for-duration-test.yaml
```

```
cpu_spike_test{instance="server-01",job="node"} 10 1774287178070
cpu_spike_test{instance="server-01",job="node"} 10 1774287179075
...
cpu_spike_test{instance="server-01",job="node"} 95 1774287183075
```

Each value lasts exactly one second at `rate: 1`. Ticks 5-9 are above 90 (5 seconds), then the
pattern repeats. Adjust the number of above-threshold values to match your `for:` duration.

### Constant Generator Shortcut

For simple sustained-threshold tests, use a constant generator:

```yaml title="constant-threshold-test.yaml"
name: cpu_usage
rate: 1
duration: 360s

generator:
  type: constant
  value: 95.0

labels:
  instance: server-01
  job: node

encoder:
  type: prometheus_text
sink:
  type: stdout
```

Run this for 6 minutes to test a `for: 5m` alert. The value stays at 95 for the entire
duration -- the alert fires after 5 minutes of continuous threshold breach.

## Testing Alert Resolution

Use gap windows to control when metrics disappear. When a metric goes silent during a gap,
Prometheus treats it as stale, causing the alert to resolve.

```
Time:  0s          30s         60s         90s        120s
       |-----------|xxxxxxxxxxx|-----------|xxxxxxxxxxx|
       emit events   gap (30s)  emit events   gap (30s)
```

Gaps occupy the **tail** of each cycle. With `every: 60s` and `for: 30s`, the gap runs from
second 30 to second 60 of each cycle.

```yaml title="gap-alert-test.yaml"
name: cpu_usage
rate: 1
duration: 300s

generator:
  type: constant
  value: 95.0

gaps:
  every: 60s
  for: 20s

labels:
  instance: server-01
  job: node

encoder:
  type: prometheus_text
sink:
  type: stdout
```

This keeps the value at 95 (above threshold) but introduces a 20-second gap every 60 seconds.
The alert enters pending state during the 40-second emit window but may not reach the `for:`
duration before the gap resets it.

## Pushing to VictoriaMetrics

The fastest path to alert testing: push metrics into a TSDB and verify alerts fire.

### HTTP Push (Prometheus Text Format)

Change the sink to push directly to VictoriaMetrics:

```yaml title="vm-push-scenario.yaml"
name: cpu_usage
rate: 1
duration: 120s

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

### Prometheus Remote Write (Protobuf)

For vmagent relay or any remote-write-compatible receiver (Prometheus, Thanos, Cortex, Mimir,
Grafana Cloud), use the `remote_write` encoder and sink pair:

!!! note
    The remote write feature requires the `remote-write` feature flag:
    `cargo build --features remote-write -p sonda`

```yaml title="remote-write-scenario.yaml"
name: cpu_usage
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

The `remote_write` sink automatically batches TimeSeries into a single WriteRequest,
snappy-compresses, and POSTs with the correct protocol headers. Compatible targets:

| Target | URL |
|--------|-----|
| VictoriaMetrics | `http://localhost:8428/api/v1/write` |
| vmagent | `http://localhost:8429/api/v1/write` |
| Prometheus | `http://localhost:9090/api/v1/write` |
| Thanos Receive | `http://localhost:19291/api/v1/receive` |
| Cortex/Mimir | `http://localhost:9009/api/v1/push` |

### Verify Data Arrived

```bash
# Check that the series exists
curl "http://localhost:8428/api/v1/series?match[]={__name__='cpu_usage'}"

# Query the latest value
curl "http://localhost:8428/api/v1/query?query=cpu_usage"
```

## Scrape-Based Integration

If you prefer the Prometheus pull model, sonda-server exposes a scrape endpoint for each
running scenario.

### Start sonda-server

```bash
cargo run -p sonda-server -- --port 8080
```

Submit a scenario:

```bash
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @sine-threshold-test.yaml \
  http://localhost:8080/scenarios
```

The response includes a scenario ID:

```json
{"id": "a1b2c3d4-..."}
```

### Scrape Endpoint

Each running scenario exposes its latest metrics at:

```
GET /scenarios/{id}/metrics
```

Configure Prometheus to scrape this endpoint:

```yaml title="prometheus.yml"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    static_configs:
      - targets: ['localhost:8080']
    metrics_path: /scenarios/<scenario-id>/metrics
```

Replace `<scenario-id>` with the ID returned from the POST request.

## Multi-Metric Correlation

Many production alerts depend on more than one metric. Compound alert rules like
`cpu_usage > 90 AND memory_usage_percent > 85` require both conditions to be true
simultaneously.

Sonda supports this with `phase_offset` and `clock_group` in multi-scenario YAML files.

| Field | Default | Description |
|-------|---------|-------------|
| `phase_offset` | `"0s"` | Delay before this scenario starts emitting, relative to the group launch time |
| `clock_group` | (none) | Groups scenarios under a shared timing reference |

### Working Example

```yaml title="multi-metric-correlation.yaml"
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
sonda run --scenario multi-metric-correlation.yaml
```

### Understanding the Timing

```
Wall time  cpu_usage (offset=0s)   memory_usage (offset=3s)
--------   ---------------------   ------------------------
t=0s       starts: 20             sleeping
t=1s       20                     sleeping
t=2s       20                     sleeping
t=3s       95 (above threshold)   starts: 40
t=4s       95                     40
t=5s       95                     40
t=6s       95                     88 (above threshold)
t=7s       95                     88
t=8s       20 (drops)             88
```

The overlap window -- where **both** metrics are above threshold -- runs from t=6s to t=8s
(2 seconds per cycle). For a `for: 5m` alert, extend the above-threshold sequences or use
constant generators with a longer duration.

## Sequence and CSV Replay Generators

### Sequence Generator

The sequence generator steps through an explicit list of values. Use it for hand-crafted
threshold patterns:

```yaml title="sequence-alert-test.yaml"
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

With `repeat: true`, the pattern loops continuously. With `repeat: false`, the generator
holds the last value after the sequence ends.

### CSV Replay Generator

For replaying real production data, the `csv_replay` generator reads values from a CSV file:

```yaml title="csv-incident-replay.yaml"
name: cpu_usage
rate: 1
duration: 120s

generator:
  type: csv_replay
  file: examples/sample-cpu-values.csv
  column: 1
  has_header: true
  repeat: false

labels:
  instance: replay-01
  job: incident-test

encoder:
  type: prometheus_text
sink:
  type: stdout
```

```bash
sonda metrics --scenario csv-incident-replay.yaml
```

```
cpu_replay{instance="prod-server-42",job="node"} 12.3 1774287217908
cpu_replay{instance="prod-server-42",job="node"} 14.1 1774287218913
cpu_replay{instance="prod-server-42",job="node"} 13.8 1774287219913
...
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `file` | (required) | Path to the CSV file |
| `column` | `0` | Zero-based column index containing numeric values |
| `has_header` | `true` | Whether the first row is a header |
| `repeat` | `true` | Cycle back to the first value after reaching the end |

!!! tip
    Use `csv_replay` over `sequence` when you have more than ~20 values. It keeps the YAML
    clean and makes it easy to update the data by replacing the CSV file.

### Exporting Values From VictoriaMetrics

```bash
curl -s "http://your-vm:8428/api/v1/query_range?\
query=cpu_usage{instance='prod-01'}&\
start=$(date -d '1 hour ago' +%s)&\
end=$(date +%s)&\
step=10s" \
  | jq -r '["timestamp","cpu_percent"], (.data.result[0].values[] | [.[0], .[1]]) | @csv' \
  > incident-values.csv
```

## Full Example

A complete end-to-end setup using Docker Compose with VictoriaMetrics, Grafana, and
sonda-server.

### Start the Stack

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

This starts:

| Service | Port | Purpose |
|---------|------|---------|
| sonda-server | 8080 | REST API for scenario management |
| VictoriaMetrics | 8428 | Time series database |
| vmagent | 8429 | Metrics relay agent |
| Grafana | 3000 | Dashboards (auto-provisioned "Sonda Overview") |

### Submit a Test Scenario

```bash
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @- \
  http://localhost:8080/scenarios <<'EOF'
name: cpu_usage
rate: 1
duration: 120s

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
  url: "http://victoriametrics:8428/api/v1/import/prometheus"
  content_type: "text/plain"
EOF
```

### Verify

```bash
# Wait for some data points
sleep 15

# Check the metric exists
curl "http://localhost:8428/api/v1/query?query=cpu_usage"

# Open Grafana at http://localhost:3000
# Navigate to Dashboards > Sonda > Sonda Overview
```

### Tear Down

```bash
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

## Quick Reference

| Scenario | Generator | Key Config |
|----------|-----------|------------|
| Threshold crossing | `sine` | `amplitude=(threshold-min)/2`, `offset=(threshold+min)/2` |
| Sustained breach | `constant` | `value: <above threshold>` |
| Alert resolution via gap | `constant` + gaps | `gaps: {every: 60s, for: 20s}` |
| Precise `for:` duration | `sequence` | N values above threshold, then below |
| Incident replay (inline) | `sequence` | `repeat: false` for one-shot |
| Incident replay (file) | `csv_replay` | `file: values.csv`, `repeat: false` |
| Compound alert | multi-scenario | `phase_offset` + `clock_group` |
| Gradual degradation | `sawtooth` | `min: 50`, `max: 99`, `period_secs: 300` |
