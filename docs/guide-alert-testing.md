# Alert Testing Guide

Test your Prometheus and VictoriaMetrics alerting rules with synthetic metrics before deploying them
to production. This guide shows you how to generate predictable signals with Sonda, push them into a
time series database, and verify that alerts fire (and resolve) exactly when you expect.

## The Problem

Most teams test alerting rules in one of three ways:

1. **Not at all** -- the rule is written, deployed, and the team hopes it works.
2. **In production** -- the team waits for a real incident and checks if the alert fired.
3. **Manual injection** -- someone runs a `curl` command to push a few data points, but timing and
   duration are hard to control.

None of these approaches catch common issues:

- An alert with `for: 5m` that never fires because the metric crosses the threshold for only 3
  minutes.
- A gap-fill rule that triggers a false alert during a 30-second scrape outage.
- A recording rule that computes the wrong value because the input rate is different from what was
  assumed.

Sonda solves this by generating metrics with **exact values**, **precise timing**, and
**configurable failure patterns** -- all driven from a YAML file you can check into your repo and
run in CI.

---

## Section 1: Generating Metrics That Cross Thresholds

### The Sine Generator Math

The sine generator produces values using the formula:

```
value = offset + amplitude * sin(2 * pi * tick / period_ticks)
```

where `period_ticks = period_secs * rate`.

This means:

| Parameter | Effect |
|-----------|--------|
| `offset` | The midpoint of the wave -- values oscillate around this number |
| `amplitude` | Half the peak-to-peak swing -- the wave reaches `offset + amplitude` and `offset - amplitude` |
| `period_secs` | How many seconds one full cycle takes |
| `rate` | Events per second (determines tick spacing) |

### Example: Range 0 to 100

With `amplitude=50` and `offset=50`, the wave oscillates between 0 and 100:

- Minimum value: `50 - 50 = 0`
- Maximum value: `50 + 50 = 100`
- Midpoint: `50`

### When Does It Cross a Threshold?

A threshold at 90 is crossed when `offset + amplitude * sin(x) > 90`, which simplifies to
`sin(x) > 0.8`.

The sine function exceeds 0.8 for a known fraction of each cycle. Specifically:

- `sin(x) = 0.8` at `x = arcsin(0.8) = 0.9273 radians` (about 53.13 degrees)
- The sine exceeds 0.8 from `x = 0.9273` to `x = pi - 0.9273 = 2.2143` radians
- That is `2.2143 - 0.9273 = 1.287 radians` out of `2 * pi = 6.283 radians` per cycle
- Fraction of each cycle above threshold: `1.287 / 6.283 = 20.5%`

With a 60-second period, the value is above 90 for approximately **12.3 seconds per cycle**.

### Value at Each Tick

Here is what happens with `amplitude=50`, `offset=50`, `period_secs=60`, `rate=1` (1 tick/sec):

| Tick (seconds) | sin(2*pi*t/60) | Value | Above 90? |
|----------------|----------------|-------|-----------|
| 0 | 0.000 | 50.0 | No |
| 5 | 0.500 | 75.0 | No |
| 10 | 0.866 | 93.3 | Yes |
| 15 | 1.000 | 100.0 | Yes |
| 20 | 0.866 | 93.3 | Yes |
| 25 | 0.500 | 75.0 | No |
| 30 | 0.000 | 50.0 | No |
| 35 | -0.500 | 25.0 | No |
| 40 | -0.866 | 6.7 | No |
| 45 | -1.000 | 0.0 | No |
| 50 | -0.866 | 6.7 | No |
| 55 | -0.500 | 25.0 | No |

The threshold crossing starts around tick 9 and ends around tick 21 -- roughly 12 seconds above 90.

### YAML Scenario

```yaml
# sine-threshold-test.yaml
# Sine wave 0-100, crosses threshold 90 for ~12s per 60s cycle.
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

Run it:

```bash
sonda metrics --scenario sine-threshold-test.yaml
```

To push directly to VictoriaMetrics instead:

```yaml
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

---

## Section 2: Controlling When Alerts Fire and Resolve

### Gap Windows

A gap window creates a recurring silent period -- no metrics are emitted during the gap. This
models network outages, scrape failures, or process restarts.

```yaml
gaps:
  every: 60s
  for: 30s
```

This means: every 60 seconds, go silent for the last 30 seconds of the cycle. The timeline looks
like this:

```
Time:  0s          30s         60s         90s        120s
       |-----------|xxxxxxxxxxx|-----------|xxxxxxxxxxx|
       emit events   gap (30s)  emit events   gap (30s)
```

Note: gaps occupy the **tail** of each cycle. With `every: 60s` and `for: 30s`, the gap runs from
second 30 to second 60, then from second 90 to second 120, and so on.

### How Gaps Interact With Alerts

When a metric disappears during a gap, Prometheus treats it as stale after its
`staleness_delta` (default 5 minutes). Shorter gaps may not trigger staleness, but they will
cause the metric to be absent for the gap duration.

An alert with `for: 1m` that requires the metric to be above a threshold will **resolve** during
a 30-second gap if the evaluation interval catches the absence. This is exactly how real outages
cause alert flapping.

### Timing Diagram: Gaps vs. Alert State

```
Metric value:   |----90----|           |----90----|           |
                0s        30s   (gap)  60s       90s   (gap)  120s

Alert state:    |  pending |  inactive |  pending |  inactive |
                            ^           ^
                            |           |
                    alert resolves   alert re-enters pending
                    (metric absent)  (metric returns above threshold)
```

### YAML: Gap Window With Threshold Crossing

```yaml
# gap-alert-test.yaml
# Value stays at 95 (above threshold) but disappears during gaps.
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
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

This scenario keeps the value at 95 (above a 90 threshold) but introduces a 20-second gap every
60 seconds. The alert enters pending state during the 40-second emit window but may not reach
the `for:` duration before the next gap resets it.

---

## Section 3: Testing `for:` Duration Behavior

### The Problem

Prometheus alerts with a `for:` clause require the condition to be true for a continuous duration
before the alert fires. An alert with `for: 5m` needs the metric above threshold for 5 continuous
minutes.

The sequence generator gives you exact control over when and how long the value exceeds a threshold.

### Using the Sequence Generator

The sequence generator steps through an explicit list of values, one per tick. With `rate: 1`
(one event per second), each value in the list lasts exactly one second.

```yaml
generator:
  type: sequence
  values: [10, 10, 10, 95, 95, 95, 95, 95, 95, 10]
  repeat: true
```

This pattern:
- Ticks 0-2: value 10 (below threshold)
- Ticks 3-8: value 95 (above threshold for 6 seconds)
- Tick 9: value 10 (below threshold)
- Then repeats

### Scaling for Real `for:` Durations

For a `for: 5m` alert, you need the value above threshold for at least 300 continuous seconds.
At `rate: 1`, that means 300 entries of 95 in the sequence:

```yaml
# for-duration-test.yaml
# Hold value at 95 for 330 seconds (5m 30s), then drop to 10 for 30 seconds.
# The 30s margin ensures the alert fires even with evaluation interval jitter.
name: cpu_usage
rate: 1
duration: 720s

generator:
  type: sequence
  values: [
    # 330 seconds above threshold (5m 30s -- accounts for eval interval)
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 10
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 20
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 30
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 40
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 50
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 60
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 70
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 80
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 90
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 100
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 110
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 120
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 130
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 140
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 150
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 160
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 170
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 180
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 190
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 200
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 210
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 220
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 230
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 240
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 250
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 260
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 270
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 280
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 290
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 300
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 310
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 320
    95, 95, 95, 95, 95, 95, 95, 95, 95, 95,  # 330
    # 30 seconds below threshold (recovery)
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10
  ]
  repeat: true

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

### A More Practical Approach: Constant + Duration

For simple `for:` duration testing, you can use a constant generator and control the scenario
duration directly:

```yaml
# constant-threshold-test.yaml
# Hold value at 95 for the entire scenario duration.
# Run for 6 minutes to test a for: 5m alert.
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
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

Run this for 6 minutes. After 5 minutes, the alert with `for: 5m` should fire.

---

## Section 4: Testing With VictoriaMetrics

Sonda integrates directly with VictoriaMetrics (VM) via the HTTP push API. This is the fastest
path to alert testing: push metrics straight into the TSDB without configuring a scrape target.

### Start the Stack

Use the provided compose file to spin up VictoriaMetrics, Grafana, and sonda-server:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

This starts:

| Service | Port | Purpose |
|---------|------|---------|
| sonda-server | 8080 | REST API for scenario management |
| VictoriaMetrics | 8428 | Time series database with Prometheus-compatible API |
| vmagent | 8429 | Metrics relay agent |
| Grafana | 3000 | Dashboards and metric exploration |

The compose stack auto-provisions a **Sonda Overview** dashboard in Grafana. After starting the
stack, navigate to **Dashboards > Sonda > Sonda Overview** in Grafana at http://localhost:3000.
The dashboard shows generated metric values, event rate, active scenario count, and gap/burst
indicators -- all updated in real time as Sonda pushes data. See
[`docker/grafana/dashboards/sonda-overview.json`](../docker/grafana/dashboards/sonda-overview.json)
for the dashboard definition.

### Push Metrics via sonda-server

Submit a scenario to sonda-server and it runs in the background:

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

The response includes a scenario ID:

```json
{"id": "a1b2c3d4-..."}
```

### Push Metrics via the CLI (from the host)

If running Sonda locally (not via sonda-server), target the exposed VM port on localhost:

```yaml
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

### Push Metrics via Prometheus Remote Write

For vmagent relay or any remote-write-native receiver (Prometheus, Thanos, Cortex, Mimir, Grafana
Cloud), use the `remote_write` encoder and sink pair. This requires the `remote-write` feature flag:

```bash
cargo build --features remote-write -p sonda
```

```yaml
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
  batch_size: 100
```

The `remote_write` sink automatically batches TimeSeries into a single `WriteRequest`,
snappy-compresses, and POSTs with the correct protocol headers. See
[`examples/remote-write-vm.yaml`](../examples/remote-write-vm.yaml) for a complete example.

### Scrape Endpoint (Pull Model)

If you prefer the Prometheus pull model, sonda-server exposes a scrape endpoint for each running
scenario:

```
GET /scenarios/{id}/metrics
```

Configure Prometheus or vmagent to scrape this endpoint:

```yaml
# prometheus.yml or vmagent scrape config
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    static_configs:
      - targets: ['sonda-server:8080']
    metrics_path: /scenarios/<scenario-id>/metrics
```

### Verify Data Arrived

Query VictoriaMetrics to confirm the metric exists:

```bash
# Check that the series exists
curl "http://localhost:8428/api/v1/series?match[]={__name__='cpu_usage'}"

# Query the latest value
curl "http://localhost:8428/api/v1/query?query=cpu_usage"

# Query a range (last 5 minutes)
curl "http://localhost:8428/api/v1/query_range?query=cpu_usage&start=$(date -v-5M +%s)&end=$(date +%s)&step=15s"
```

### Verify Alert State With vmalert

If you are running vmalert (not included in the basic compose stack, but easy to add), query its
API to check whether the alert fired:

```bash
# List all active alerts
curl "http://localhost:8880/api/v1/alerts"
```

You can also check alert state in the VictoriaMetrics UI at `http://localhost:8428/vmui`.

---

## Section 5: Testing Recording Rules

Recording rules pre-compute expressions and store the result as a new time series. Testing them
requires pushing known input values and verifying the computed output.

A ready-to-run example is provided in the repository:

- [`examples/recording-rule-test.yaml`](../examples/recording-rule-test.yaml) -- Sonda scenario
  pushing a constant value of 100 for `http_requests_total`.
- [`examples/recording-rule-prometheus.yml`](../examples/recording-rule-prometheus.yml) -- Prometheus
  recording rule config computing `job:http_requests_total:rate5m`.

Run the scenario and load the rule file to see recording rules evaluate against known synthetic data.

### Push Known Values

Use the constant generator to push a known value:

```yaml
# recording-rule-input.yaml
# Push a constant value of 42 requests/sec for 5 minutes.
name: http_requests_total
rate: 1
duration: 300s

generator:
  type: constant
  value: 42.0

labels:
  instance: api-01
  job: web

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

### Wait for Evaluation

Recording rules are evaluated on a fixed interval (default 1 minute in Prometheus, configurable
in vmalert). After starting the scenario, wait at least **two evaluation intervals** to ensure the
rule has data to work with:

```bash
# Start the scenario
sonda metrics --scenario recording-rule-input.yaml &

# Wait for 2 evaluation intervals (2 minutes with default settings)
sleep 120
```

### Verify the Computed Value

Suppose your recording rule is:

```yaml
# recording rule in prometheus.yml or vmalert config
groups:
  - name: test_rules
    rules:
      - record: job:http_requests_total:sum
        expr: sum(http_requests_total) by (job)
```

With one instance pushing `42.0`, the sum should be `42.0`:

```bash
# Query the recording rule output
curl -s "http://localhost:8428/api/v1/query?query=job:http_requests_total:sum" | jq '.data.result'
```

Expected output:

```json
[
  {
    "metric": {"job": "web"},
    "value": [1711234567, "42"]
  }
]
```

### Testing Rate-Based Rules

For `rate()` or `irate()` rules, you need a metric whose value increases over time. Use the
sawtooth generator, which ramps linearly and resets:

```yaml
# rate-rule-input.yaml
# Sawtooth ramp from 0 to 1000 over 60 seconds, then reset.
# rate() over 1 minute should show ~16.67/sec (1000/60).
name: http_requests_total
rate: 1
duration: 300s

generator:
  type: sawtooth
  min: 0.0
  max: 1000.0
  period_secs: 60

labels:
  instance: api-01
  job: web

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

After sufficient data, `rate(http_requests_total[1m])` should return approximately `16.67`.

---

## Section 6: Running in CI/CD

The full alert testing workflow can be automated in a CI pipeline. Here is a complete script
that starts the stack, pushes a scenario, waits, verifies the alert fired, and tears down.

### Complete CI Script

```bash
#!/usr/bin/env bash
set -euo pipefail

# --- Configuration ---
COMPOSE_FILE="examples/docker-compose-victoriametrics.yml"
SONDA_URL="http://localhost:8080"
VM_URL="http://localhost:8428"
METRIC_NAME="ci_cpu_usage"
THRESHOLD=90
WAIT_SECONDS=30

# --- Start the stack ---
echo "Starting VictoriaMetrics + Sonda stack..."
docker compose -f "$COMPOSE_FILE" up -d --wait

# Wait for sonda-server to be healthy
echo "Waiting for sonda-server..."
for i in $(seq 1 30); do
  if curl -sf "$SONDA_URL/health" > /dev/null 2>&1; then
    echo "sonda-server is healthy."
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: sonda-server did not become healthy in 30 seconds."
    docker compose -f "$COMPOSE_FILE" logs sonda-server
    docker compose -f "$COMPOSE_FILE" down -v
    exit 1
  fi
  sleep 1
done

# --- Submit a scenario ---
echo "Submitting test scenario..."
SCENARIO_ID=$(curl -sf -X POST -H "Content-Type: text/yaml" \
  --data-binary @- \
  "$SONDA_URL/scenarios" <<EOF | jq -r '.id'
name: ${METRIC_NAME}
rate: 1
duration: ${WAIT_SECONDS}s

generator:
  type: constant
  value: 95.0

labels:
  instance: ci-test
  job: alert-validation

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://victoriametrics:8428/api/v1/import/prometheus"
  content_type: "text/plain"
EOF
)

echo "Scenario started: $SCENARIO_ID"

# --- Wait for data to accumulate ---
echo "Waiting ${WAIT_SECONDS}s for data..."
sleep "$WAIT_SECONDS"

# --- Verify the metric exists and is above threshold ---
echo "Querying VictoriaMetrics..."
VALUE=$(curl -sf "$VM_URL/api/v1/query?query=${METRIC_NAME}" \
  | jq -r '.data.result[0].value[1]')

if [ -z "$VALUE" ] || [ "$VALUE" = "null" ]; then
  echo "FAIL: Metric '${METRIC_NAME}' not found in VictoriaMetrics."
  docker compose -f "$COMPOSE_FILE" down -v
  exit 1
fi

echo "Metric value: $VALUE (threshold: $THRESHOLD)"

# Compare as integers (bash cannot do floating point comparison natively)
VALUE_INT=$(printf "%.0f" "$VALUE")
if [ "$VALUE_INT" -ge "$THRESHOLD" ]; then
  echo "PASS: Metric is above threshold."
else
  echo "FAIL: Metric $VALUE_INT is below threshold $THRESHOLD."
  docker compose -f "$COMPOSE_FILE" down -v
  exit 1
fi

# --- Tear down ---
echo "Stopping stack..."
docker compose -f "$COMPOSE_FILE" down -v

echo "Alert test completed successfully."
```

### GitHub Actions Example

```yaml
# .github/workflows/alert-test.yml
name: Alert Rule Validation
on:
  pull_request:
    paths:
      - 'alerting/**'

jobs:
  test-alerts:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Start test stack
        run: docker compose -f examples/docker-compose-victoriametrics.yml up -d --wait

      - name: Wait for services
        run: |
          for i in $(seq 1 30); do
            curl -sf http://localhost:8080/health && break
            sleep 1
          done

      - name: Run alert test
        run: ./scripts/test-alerts.sh

      - name: Tear down
        if: always()
        run: docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

### Key Considerations for CI

- **Timeouts**: Budget enough time for the alert's `for:` duration plus at least two evaluation
  intervals. A `for: 5m` alert needs at least 7 minutes of data before you can verify it fired.
- **Exit codes**: The script above exits with code 1 on failure. CI runners treat this as a
  failed step.
- **Cleanup**: Always run `docker compose down` in a `finally`/`if: always()` block so containers
  do not leak between CI runs.
- **Parallelism**: Each scenario gets a unique metric name. You can run multiple alert tests
  concurrently without collision.

---

## Section 7: Replaying an Incident Pattern

The sequence generator lets you replay exact production metric patterns. This is useful when an
incident occurred and you want to verify that your alerting rules would have caught it -- or when
you want to reproduce the conditions that triggered a false positive.

### Step 1: Extract the Pattern

Pull the metric values from your TSDB for the incident window. Here is an example using the
VictoriaMetrics API:

```bash
# Query 30-second resolution data for the last hour
curl -s "http://your-vm:8428/api/v1/query_range?\
query=cpu_usage{instance='prod-01'}&\
start=$(date -d '1 hour ago' +%s)&\
end=$(date +%s)&\
step=30s" \
  | jq -r '.data.result[0].values[][1]' \
  | tr '\n' ', '
```

This produces a comma-separated list of values like:

```
12.3, 14.1, 13.8, 15.2, 45.6, 78.9, 95.1, 97.3, 96.8, 94.2, 89.1, 45.3, 12.1, ...
```

### Step 2: Build the Sequence Scenario

Paste the values into a sequence generator:

```yaml
# incident-replay.yaml
# Replay the CPU spike from 2024-01-15T14:00:00Z
name: cpu_usage
rate: 1
duration: 600s

generator:
  type: sequence
  values: [12.3, 14.1, 13.8, 15.2, 45.6, 78.9, 95.1, 97.3, 96.8, 94.2, 89.1, 45.3, 12.1]
  repeat: false

labels:
  instance: replay-01
  job: incident-test

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

With `repeat: false`, the sequence plays once and then holds the last value for the remaining
duration. This matches how a real incident looks: the metric does not cycle.

### Step 3: Verify Alert Behavior

Push the replayed pattern into your test stack and check whether the alert fires at the right
time:

```bash
# Start the replay
sonda metrics --scenario incident-replay.yaml &

# Wait for the spike portion (adjust based on your data)
sleep 60

# Check if the alert fired
curl -s "http://localhost:8428/api/v1/query?query=cpu_usage{instance='replay-01'}" \
  | jq '.data.result[0].value'
```

### Adapting Timing

The original data may have been sampled at 30-second intervals, but you might want to test at
higher resolution. Adjust the `rate` to control replay speed:

| Original interval | Sonda `rate` | Effect |
|-------------------|-------------|--------|
| 30s | 1 | Each value lasts 1 second (30x faster) |
| 30s | 1/30 = 0.033 | Real-time replay at original speed |
| 15s | 1 | Each value lasts 1 second |
| 1s | 1 | 1:1 real-time replay |

For most alert testing, running at accelerated speed (rate=1) is fine because Prometheus evaluates
against the most recent data point regardless of when it arrived.

### Preferred Approach: Replaying From a CSV File

If you have production metric values in a CSV file, use the `csv_replay` generator instead of pasting
values into a sequence list. This is the preferred approach for replaying real data because it keeps
the YAML scenario clean and makes it easy to update the data independently.

#### Step 1: Export Values to CSV

Export the metric values from Prometheus or VictoriaMetrics into a CSV file. For example, using the
VictoriaMetrics export API:

```bash
curl -s "http://your-vm:8428/api/v1/query_range?\
query=cpu_usage{instance='prod-01'}&\
start=$(date -d '1 hour ago' +%s)&\
end=$(date +%s)&\
step=10s" \
  | jq -r '["timestamp","cpu_percent"], (.data.result[0].values[] | [.[0], .[1]]) | @csv' \
  > incident-values.csv
```

This produces a file like the included [`examples/sample-cpu-values.csv`](../examples/sample-cpu-values.csv):

```csv
timestamp,cpu_percent
1700000000,12.3
1700000010,14.1
1700000020,13.8
...
1700000180,94.8
1700000190,96.1
...
1700000450,14.1
```

#### Step 2: Build the CSV Replay Scenario

Point the `csv_replay` generator at the CSV file:

```yaml
# csv-incident-replay.yaml
# Replay a production CPU spike from a recorded CSV file.
name: cpu_usage
rate: 1
duration: 120s

generator:
  type: csv_replay
  file: incident-values.csv
  columns:
    - index: 1
      name: cpu_usage
  repeat: false

labels:
  instance: replay-01
  job: incident-test

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

The key parameters:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `file` | (required) | Path to the CSV file. Relative paths are resolved from the working directory. |
| `columns` | -- | Explicit column specs. When absent, columns are auto-discovered from the header. |
| `repeat` | `true` | When true, cycles back to the first value after reaching the end. When false, clamps to the last value. |

With `repeat: false`, the data plays once and then holds the last value -- matching how a real
incident looks. With `repeat: true`, the pattern loops continuously, which is useful for
sustained load testing.

#### Why CSV Replay Over Sequence

| Concern | `sequence` | `csv_replay` |
|---------|-----------|-------------|
| Data source | Values pasted inline in YAML | Separate CSV file |
| Data volume | Awkward beyond ~50 values | Handles thousands of rows easily |
| Updateability | Edit YAML to change values | Replace the CSV file; YAML stays the same |
| Tooling | Manual copy-paste from TSDB | Direct export from Prometheus/VictoriaMetrics |
| Version control | Large YAML diffs when values change | Small YAML + separate CSV diffs |

For short, hand-crafted patterns (fewer than ~20 values), the `sequence` generator is still
convenient. For replaying real production data, `csv_replay` is the better choice.

See [`examples/csv-replay-metrics.yaml`](../examples/csv-replay-metrics.yaml) and
[`examples/sample-cpu-values.csv`](../examples/sample-cpu-values.csv) for a complete working
example.

---

## Section 8: Testing Multi-Metric Alerts

### The Problem

Many production alerts depend on more than one metric. Compound alert rules such as:

```
ALERT HighCpuAndMemory
IF cpu_usage > 90 AND memory_usage_percent > 85
FOR 5m
```

require **both** conditions to be true simultaneously. Testing these rules is harder than testing
a single-metric alert because you need two correlated signal streams with a precise timing
relationship. If you start them independently, you have no control over when the overlap window
begins or how long it lasts.

Sonda solves this with two multi-scenario configuration fields: `phase_offset` and `clock_group`.

### How It Works

In a multi-scenario YAML file, each scenario entry can include:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `phase_offset` | Duration string | `"0s"` | Delay before this scenario starts emitting, relative to the group launch time. |
| `clock_group` | String | (none) | Groups scenarios under a shared timing reference. Scenarios in the same clock group are launched together. |

When `sonda run` launches a multi-scenario file:

1. All scenarios are spawned at the same wall-clock time (the group start).
2. A scenario with `phase_offset: "0s"` (or no offset) begins emitting events immediately.
3. A scenario with `phase_offset: "3s"` sleeps for 3 seconds inside its spawned thread before
   entering the event loop.
4. The `clock_group` field documents which scenarios are temporally related. Scenarios in the same
   group share a common start time reference.

The `phase_offset` field accepts any duration string: `"500ms"`, `"5s"`, `"1m30s"`, etc.

### Example: CPU + Memory Compound Alert

Consider the compound alert rule above: `cpu_usage > 90 AND memory_usage_percent > 85`. We want
to test that the alert fires when **both** metrics breach their thresholds at the same time.

Here is the complete scenario file (also available at
[`examples/multi-metric-correlation.yaml`](../examples/multi-metric-correlation.yaml)):

```yaml
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

Run it:

```bash
sonda run --scenario examples/multi-metric-correlation.yaml
```

### Understanding the Timing

Here is what happens at each second after launch:

```
Wall time  cpu_usage (phase_offset=0s)   memory_usage (phase_offset=3s)
--------   ----------------------------  ------------------------------
t=0s       starts emitting: 20           sleeping (not started)
t=1s       20                            sleeping
t=2s       20                            sleeping
t=3s       95  (above threshold)         starts emitting: 40
t=4s       95                            40
t=5s       95                            40
t=6s       95                            88  (above threshold)
t=7s       95                            88
t=8s       20  (below threshold)         88
t=9s       20                            88
...
```

The CPU sequence `[20, 20, 20, 95, 95, 95, 95, 95, 20, 20]` runs at `rate: 1`, so each value
lasts one second. Memory uses the same approach but starts 3 seconds later due to its
`phase_offset`.

### Calculating the Overlap Window

The overlap window -- where **both** metrics are above their respective thresholds simultaneously
-- determines whether the compound alert fires.

For the example above:

1. **CPU above 90**: ticks 3-7 of its sequence (5 seconds per cycle), starting at wall time
   t=3s because it has no offset.
2. **Memory above 85**: ticks 3-7 of its sequence (5 seconds per cycle), starting at wall time
   t=6s because it has a 3-second offset and its first 3 ticks are below threshold.
3. **Overlap**: from t=6s (memory crosses threshold) to t=8s (CPU drops below threshold) --
   a 2-second overlap per cycle.

For an alert with `for: 5m`, a 2-second overlap per 10-second cycle is not enough -- the
condition is not continuously true for 5 minutes. You would need to adjust the sequences to
create a longer sustained overlap. For example, extending the above-threshold portion of both
sequences:

```yaml
# CPU: 60 ticks above threshold (1 minute at rate=1)
generator:
  type: constant
  value: 95.0

# Memory: starts 10 seconds later, also sustained
phase_offset: "10s"
generator:
  type: constant
  value: 88.0
```

With constant generators and a 10-second offset, the overlap starts at t=10s and lasts for the
remaining duration minus 10 seconds -- easily exceeding the 5-minute `for:` requirement.

### Pushing to VictoriaMetrics

To test with a real TSDB and alerting stack, change the sinks to push to VictoriaMetrics:

```yaml
scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 1
    duration: 600s
    phase_offset: "0s"
    clock_group: alert-test
    generator:
      type: constant
      value: 95.0
    labels:
      instance: server-01
      job: node
    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"

  - signal_type: metrics
    name: memory_usage_percent
    rate: 1
    duration: 600s
    phase_offset: "30s"
    clock_group: alert-test
    generator:
      type: constant
      value: 88.0
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

This pushes CPU at 95% immediately and memory at 88% starting 30 seconds later. After 30 seconds,
both metrics are above threshold simultaneously, and the compound alert's `for:` clock begins.

### Verifying Overlap in VictoriaMetrics

Query both metrics to confirm the overlap:

```bash
# Check that both metrics exist
curl "http://localhost:8428/api/v1/query?query=cpu_usage{instance='server-01'}"
curl "http://localhost:8428/api/v1/query?query=memory_usage_percent{instance='server-01'}"

# Query the compound condition
curl "http://localhost:8428/api/v1/query?query=cpu_usage{instance='server-01'} > 90 and memory_usage_percent{instance='server-01'} > 85"
```

The compound query returns results only during the overlap window -- confirming that the alert
condition would be evaluated as true by Prometheus or vmalert during that period.

---

## Quick Reference: Common Patterns

| Scenario | Generator | Configuration | Notes |
|----------|-----------|---------------|-------|
| Alert fires at threshold X | `sine` | `amplitude=(X-min)/2`, `offset=(X+min)/2` | Crosses threshold twice per cycle |
| Alert fires and stays firing | `constant` | `value: <above threshold>` | Simplest sustained breach |
| Alert resolves during gap | `constant` + gap | `value: 95`, `gaps: {every: 60s, for: 20s}` | Metric disappears during gap |
| Alert fires after N minutes | `sequence` | N*60 values above threshold, then below | Precise `for:` duration control |
| CPU spike incident replay (inline) | `sequence` | Values from production query, `repeat: false` | One-shot replay, small datasets |
| CPU spike incident replay (file) | `csv_replay` | `file: values.csv`, `repeat: false` | Preferred for real production data |
| Micro-burst rate alert | `sine` + burst | Base rate + `bursts: {every: 30s, for: 5s, multiplier: 10}` | Tests rate-based alerts |
| Flapping alert | `sequence` | Alternating above/below threshold | Tests alert grouping and inhibition |
| Gradual degradation | `sawtooth` | `min: 50`, `max: 99`, `period_secs: 300` | Linear ramp to threshold |
| Multi-metric compound alert | multi-scenario + `phase_offset` | Two scenarios with offset timing | Tests `A > X AND B > Y` rules |
| Correlated metrics with delay | multi-scenario + `phase_offset` | `phase_offset: "30s"` on second metric | Creates controlled overlap window |

---

## Next Steps

- See the [examples/](../examples/) directory for ready-to-run scenario files.
- Use the [VictoriaMetrics compose stack](../examples/docker-compose-victoriametrics.yml) for a
  self-contained test environment.
- Explore [sonda-server](../README.md) for REST API-driven scenario management.
- Check [examples/sequence-alert-test.yaml](../examples/sequence-alert-test.yaml) for a concrete
  sequence generator example.
- Check [examples/docker-alerts.yaml](../examples/docker-alerts.yaml) for a sine wave alert
  testing example with burst windows.
- Check [examples/csv-replay-metrics.yaml](../examples/csv-replay-metrics.yaml) and
  [examples/sample-cpu-values.csv](../examples/sample-cpu-values.csv) for replaying production
  metric data from a CSV file.
- Check [examples/multi-metric-correlation.yaml](../examples/multi-metric-correlation.yaml) for
  testing compound alert rules with `phase_offset` and `clock_group`.
