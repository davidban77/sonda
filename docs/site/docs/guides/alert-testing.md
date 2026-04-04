# Alert Testing

You write alert rules, deploy them, and hope they work. But common issues slip through:
a `for: 5m` alert that never fires because the metric only breaches for 3 minutes, a gap-fill
rule that triggers false positives during scrape outages, or a compound alert where two metrics
never overlap. Sonda lets you generate the exact metric shapes to trigger (and resolve) alerts
on demand, so you can catch these problems before they hit production.

---

## Threshold Alerts

The most basic alert test: verify that a `HighCPU` rule fires when `cpu_usage > 90`.

The sine generator produces a smooth wave that crosses your threshold predictably.
With `amplitude=50` and `offset=50`, it oscillates between 0 and 100, crossing 90 for
about 12 seconds per 60-second cycle.

```bash
sonda metrics --scenario examples/sine-threshold-test.yaml
```

```yaml title="examples/sine-threshold-test.yaml"
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

The metric crosses 90 around tick 9, stays above until tick 21, then drops -- giving you
roughly 12 seconds above threshold per cycle.

??? info "Sine wave math"
    The formula is: `value = offset + amplitude * sin(2 * pi * tick / period_ticks)`

    With `amplitude=50` and `offset=50`, the threshold at 90 is crossed when `sin(x) > 0.8`:

    - `sin(x) = 0.8` at `x = arcsin(0.8) = 0.927 radians`
    - The sine exceeds 0.8 from `x = 0.927` to `x = pi - 0.927 = 2.214`
    - That's `1.287 / 6.283 = 20.5%` of each cycle
    - With a 60-second period: **~12.3 seconds above 90 per cycle**

    | Tick (sec) | sin(2*pi*t/60) | Value | Above 90? |
    |------------|----------------|-------|-----------|
    | 0  | 0.000  | 50.0  | No  |
    | 5  | 0.500  | 75.0  | No  |
    | 10 | 0.866  | 93.3  | Yes |
    | 15 | 1.000  | 100.0 | Yes |
    | 20 | 0.866  | 93.3  | Yes |
    | 25 | 0.500  | 75.0  | No  |

Smooth waves are great for threshold crossings, but Prometheus `for:` clauses need precise timing control.

---

## Testing `for:` Duration Behavior

Prometheus alerts with a `for:` clause require the condition to be true for a **continuous**
duration before firing. You need exact control over how long the metric stays above threshold.

### Sequence generator

The [sequence generator](../configuration/generators.md#sequence) steps through an explicit
list of values, one per tick. Each value lasts exactly `1/rate` seconds:

```bash
sonda metrics --scenario examples/for-duration-test.yaml
```

```yaml title="examples/for-duration-test.yaml"
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

At `rate: 1`, ticks 5-9 are above 90 (5 seconds continuous), then the pattern repeats.
Adjust the number of above-threshold values to match your `for:` duration.

!!! tip "Matching your `for:` duration"
    For a `for: 5m` alert, you need 300 consecutive above-threshold values at `rate: 1`.
    Rather than typing 300 values, use the constant generator instead (next section).

### Constant generator shortcut

For simple sustained-threshold tests, the [constant generator](../configuration/generators.md#constant) is more practical:

```bash
sonda metrics --scenario examples/constant-threshold-test.yaml
```

```yaml title="examples/constant-threshold-test.yaml"
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
duration -- the alert should fire after 5 minutes of continuous breach.

Now that you can trigger alerts, what about testing when they resolve?

---

## Testing Alert Resolution

When a metric goes silent during a gap, Prometheus treats it as stale and the alert resolves.
Use [gap windows](../configuration/scenario-file.md) to control when metrics disappear.

```
Time:  0s          40s         60s         100s        120s
       |-----------|xxxxxxxxxxx|-----------|xxxxxxxxxxx|
       emit events   gap (20s)  emit events   gap (20s)
```

Gaps occupy the **tail** of each cycle. With `every: 60s` and `for: 20s`, the gap runs from
second 40 to second 60 of each cycle.

```bash
sonda metrics --scenario examples/gap-alert-test.yaml
```

```yaml title="examples/gap-alert-test.yaml"
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

The value stays at 95 (above threshold) but goes silent for 20 seconds every 60-second cycle.
The alert enters pending state during the 40-second emit window but may not reach the `for:`
duration before the gap resets it.

!!! tip "Combine with generators"
    Gaps work with any generator. A sine wave with periodic gaps creates a realistic
    "flapping service" pattern for alert testing.

With single-metric alerts covered, let's move on to compound alerts that depend on multiple metrics.

---

## Multi-Metric Correlation

Production alerts often depend on more than one metric. Compound rules like
`cpu_usage > 90 AND memory_usage_percent > 85` require both conditions to be true
simultaneously. Sonda supports this with `phase_offset` and `clock_group` in
multi-scenario YAML files.

| Field | Default | Description |
|-------|---------|-------------|
| `phase_offset` | `"0s"` | Delay before this scenario starts emitting |
| `clock_group` | (none) | Groups scenarios under a shared timing reference |

```bash
sonda run --scenario examples/multi-metric-correlation.yaml
```

```yaml title="examples/multi-metric-correlation.yaml (excerpt)"
scenarios:
  - signal_type: metrics
    name: cpu_usage
    phase_offset: "0s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
      repeat: true
    # ...

  - signal_type: metrics
    name: memory_usage_percent
    phase_offset: "3s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
      repeat: true
    # ...
```

### Understanding the timing

```
Wall time  cpu_usage (offset=0s)   memory_usage (offset=3s)
--------   ---------------------   ------------------------
t=0s       starts: 20             sleeping
t=3s       95 (above threshold)   starts: 40
t=6s       95                     88 (above threshold)
t=8s       20 (drops)             88
```

The overlap window -- where **both** metrics are above threshold -- runs from t=6s to t=8s
(2 seconds per cycle). For a `for: 5m` alert, extend the above-threshold sequences or use
constant generators with a longer duration.

See [Example Scenarios](examples.md) for the full `multi-metric-correlation.yaml` file.

With local testing covered, let's push metrics to a real backend.

---

## Pushing to a Backend

The fastest path to end-to-end alert testing: push metrics into a TSDB and verify alerts fire.

=== "HTTP Push"

    POST metrics directly to VictoriaMetrics using the Prometheus text import API:

    ```bash
    sonda metrics --scenario examples/vm-push-scenario.yaml
    ```

    ```yaml title="examples/vm-push-scenario.yaml (key fields)"
    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
    ```

=== "Remote Write"

    Use the Prometheus remote write protocol for native compatibility with any
    remote-write receiver:

    ```bash
    sonda metrics --scenario examples/remote-write-vm.yaml
    ```

    ```yaml title="examples/remote-write-vm.yaml (key fields)"
    encoder:
      type: remote_write
    sink:
      type: remote_write
      url: "http://localhost:8428/api/v1/write"
      batch_size: 100
    ```

!!! warning "Remote write requires a feature flag when building from source"
    Pre-built binaries and Docker images include remote-write support. When building from
    source, add `--features remote-write`: `cargo build --features remote-write -p sonda`.

| Target | URL |
|--------|-----|
| VictoriaMetrics | `http://localhost:8428/api/v1/write` |
| vmagent | `http://localhost:8429/api/v1/write` |
| Prometheus | `http://localhost:9090/api/v1/write` |
| Thanos Receive | `http://localhost:19291/api/v1/receive` |
| Cortex/Mimir | `http://localhost:9009/api/v1/push` |

### Verify data arrived

```bash
# Check that the series exists
curl "http://localhost:8428/api/v1/series?match[]={__name__='cpu_usage'}"

# Query the latest value
curl "http://localhost:8428/api/v1/query?query=cpu_usage"
```

!!! info "Docker Compose stack required"
    These push scenarios require a running backend. Start the included stack with:
    `docker compose -f examples/docker-compose-victoriametrics.yml up -d`.
    See [Docker Deployment](../deployment/docker.md) for details.

You can also use the pull model instead of pushing.

---

## Scrape-Based Integration

If you prefer the Prometheus pull model, sonda-server exposes a scrape endpoint for each
running scenario.

Start sonda-server and submit a scenario:

```bash
cargo run -p sonda-server -- --port 8080

# In another terminal:
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/sine-threshold-test.yaml \
  http://localhost:8080/scenarios
```

The response includes a scenario ID. Configure Prometheus to scrape it:

```yaml title="prometheus.yml (scrape config)"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    static_configs:
      - targets: ['localhost:8080']
    metrics_path: /scenarios/<scenario-id>/metrics
```

!!! tip "Scrape path"
    Replace `<scenario-id>` with the UUID returned from `POST /scenarios`. Each running
    scenario exposes its own metrics endpoint.

See [Server API](../deployment/sonda-server.md) for the full API reference.

Beyond simple threshold alerts, Sonda can also test cardinality explosions and replay real incidents.

---

## Cardinality Explosion Alerts

Many monitoring stacks alert when series cardinality crosses a threshold (e.g.,
`count(up) > 10000`). Sonda's [cardinality spikes](../configuration/scenario-file.md)
generate a controlled burst of unique label values to verify your cardinality-limiting
rules fire correctly.

```bash
sonda metrics --scenario examples/cardinality-alert-test.yaml
```

```yaml title="examples/cardinality-alert-test.yaml (key fields)"
cardinality_spikes:
  - label: pod_name
    every: 30s
    for: 10s
    cardinality: 500
    strategy: counter
    prefix: "pod-"
```

During the 10-second spike window, each tick injects a `pod_name` label with one of 500 unique
values (`pod-0` through `pod-499`), producing 500 distinct series. Outside the window the label
is absent and only one series is emitted. This on/off pattern lets you verify that alerts fire
during the spike and resolve after it ends.

!!! info "Docker stack required"
    This example pushes to VictoriaMetrics via `http_push`. Start the backend first:
    `docker compose -f examples/docker-compose-victoriametrics.yml up -d`

For testing with recorded production data instead of synthetic patterns, use the replay generators.

---

## Replay Generators

### Sequence generator

The [sequence generator](../configuration/generators.md#sequence) steps through an explicit
list of values, perfect for hand-crafted threshold patterns:

```bash
sonda metrics --scenario examples/sequence-alert-test.yaml
```

```yaml title="examples/sequence-alert-test.yaml (key fields)"
generator:
  type: sequence
  values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
  repeat: true
```

With `repeat: true`, the pattern loops continuously. With `repeat: false`, the generator
holds the last value after the sequence ends.

### CSV replay generator

For replaying real production data, the [csv_replay generator](../configuration/generators.md#csv_replay) reads values from a CSV file:

```bash
sonda metrics --scenario examples/csv-replay-metrics.yaml
```

```yaml title="examples/csv-replay-metrics.yaml (key fields)"
generator:
  type: csv_replay
  file: examples/sample-cpu-values.csv
  column: 1
  has_header: true
  repeat: true
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `file` | (required) | Path to the CSV file |
| `column` | `0` | Zero-based column index containing numeric values |
| `has_header` | `true` | Whether the first row is a header |
| `repeat` | `true` | Cycle back to the first value after reaching the end |

!!! tip "When to use csv_replay vs sequence"
    Use `csv_replay` over `sequence` when you have more than ~20 values. It keeps the YAML
    clean and makes it easy to update the data by replacing the CSV file.

??? info "Exporting values from VictoriaMetrics"
    ```bash
    curl -s "http://your-vm:8428/api/v1/query_range?\
    query=cpu_usage{instance='prod-01'}&\
    start=$(date -d '1 hour ago' +%s)&\
    end=$(date +%s)&\
    step=10s" \
      | jq -r '["timestamp","cpu_percent"], (.data.result[0].values[] | [.[0], .[1]]) | @csv' \
      > incident-values.csv
    ```

---

## Full Docker Compose Example

For a complete end-to-end setup with VictoriaMetrics, Grafana, and sonda-server, use the
included Docker Compose stack.

```bash
# Start the stack
docker compose -f examples/docker-compose-victoriametrics.yml up -d

# Push test data
sonda metrics --scenario examples/vm-push-scenario.yaml

# Verify the metric exists (wait ~15s for ingestion)
curl "http://localhost:8428/api/v1/query?query=cpu_usage"

# Open Grafana at http://localhost:3000
# Navigate to Dashboards > Sonda > Sonda Overview

# Tear down
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

| Service | Port | Purpose |
|---------|------|---------|
| sonda-server | 8080 | REST API for scenario management |
| VictoriaMetrics | 8428 | Time series database |
| vmagent | 8429 | Metrics relay agent |
| Grafana | 3000 | Dashboards (auto-provisioned) |

See [Docker Deployment](../deployment/docker.md) for the full stack configuration.

!!! tip "Close the loop with Alertmanager"
    This stack verifies that data arrives in VictoriaMetrics, but doesn't prove alerts fire.
    To add vmalert, Alertmanager, and a webhook receiver to the stack, see the
    [Alerting Pipeline](alerting-pipeline.md) guide.

---

## Quick Reference

| Scenario | Generator | Example File |
|----------|-----------|------------|
| Threshold crossing | `sine` | `sine-threshold-test.yaml` |
| Sustained breach | `constant` | `constant-threshold-test.yaml` |
| Alert resolution via gap | `constant` + gaps | `gap-alert-test.yaml` |
| Precise `for:` duration | `sequence` | `for-duration-test.yaml` |
| Compound alert | multi-scenario | `multi-metric-correlation.yaml` |
| Cardinality explosion | any + `cardinality_spikes` | `cardinality-alert-test.yaml` |
| Periodic spike / anomaly | `spike` | `spike-alert-test.yaml` |
| Incident replay (inline) | `sequence` | `sequence-alert-test.yaml` |
| Incident replay (file) | `csv_replay` | `csv-replay-metrics.yaml` |
| Push to VictoriaMetrics | any | `vm-push-scenario.yaml` |
| Remote write | any | `remote-write-vm.yaml` |

---

## Next Steps

**Verifying alerts fire end-to-end?** See [Alerting Pipeline](alerting-pipeline.md) to run
vmalert, Alertmanager, and a webhook receiver with Docker Compose.

**Validating alert rules in CI?** See [CI Alert Validation](ci-alert-validation.md) to catch
broken rules before they reach production.

**Validating a pipeline change?** See [Pipeline Validation](pipeline-validation.md).

**Verifying recording rules?** Check [Recording Rules](recording-rules.md).

**Browsing all example scenarios?** See [Example Scenarios](examples.md).
