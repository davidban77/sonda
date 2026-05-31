---
title: Alert testing patterns
description: Test Prometheus alert rules end-to-end with the right synthetic metric shape — thresholds, resolution, correlation, cardinality, replay, and histogram-based alerts.
---

# Alert testing

3 a.m. The pager goes off for `HighRequestLatency`. By the time you log in, latency is back below threshold and the alert has cleared. You spend an hour reading dashboards and find nothing -- the spike was real, but it lasted 90 seconds and your `for: 5m` clause silently swallowed it. The alert is doing exactly what you told it to. You just told it the wrong thing.

That whole class of problem -- `for:` durations that swallow real spikes, gap-fill rules that fire during scrape outages, compound `A AND B` rules where the two signals never overlap -- only shows up in production because nothing else generates the right metric shape. Sonda does. You write the alert, run a scenario that crosses the threshold for exactly the duration you care about, and watch whether the alert fires.

This page collects the six patterns into one place. Each tab below stands on its own — jump straight to the one that matches the rule you are testing. The table maps common alert shapes to the right tab.

## Pick your pattern

| You want to test... | Tab | Generator |
|---------------------|-----|-----------|
| A simple `> threshold` rule | [Thresholds](#thresholds) | `sine` |
| A short `for:` clause (≤ 30s) | [Thresholds](#thresholds) | `sequence` |
| A long `for:` clause (minutes) | [Thresholds](#thresholds) | `constant` |
| Resolution / flapping behavior | [Resolution and recovery](#resolution-and-recovery) | any + `gaps` |
| Compound `A AND B` rules | [Compound and correlated](#compound-and-correlated) | multi-scenario |
| Cardinality guardrails | [Cardinality explosion](#cardinality-explosion) | any + `cardinality_spikes` |
| Replaying a known incident | [Replaying incidents](#replaying-incidents) | `sequence` or `csv_replay` |
| Latency / histogram alerts | [Histogram and summary alerts](#histogram-and-summary-alerts) | `histogram` |

## Setup

Every tab below assumes a local backend is running and reachable. The bundled stack ships VictoriaMetrics, vmagent, and Grafana behind one Compose file:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

| Service | Port | Purpose |
|---------|------|---------|
| sonda-server | 8080 | REST API for scenario management |
| VictoriaMetrics | 8428 | Time series database |
| vmagent | 8429 | Metrics relay agent |
| Grafana | 3000 | Dashboards (auto-provisioned) |

Verify the stack is up:

```bash
curl -s "http://localhost:8428/health"
# OK
```

After running any scenario below, query VictoriaMetrics with `curl | jq` to confirm the metric arrived (wait ~15s for ingestion):

```bash
curl -s "http://localhost:8428/api/v1/query?query=<metric_name>" | jq '.data.result | length'
```

Tear down when finished:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

See [Docker Deployment](../deploy/docker.md) for the full stack configuration. For the full alert-flow loop (vmalert + Alertmanager + webhook receiver), use the same Compose file with `--profile alerting` and walk through the [Alerting pipeline tab on End-to-end pipelines](end-to-end-pipelines.md).

## Push to a real backend

The two scenarios you will reach for first when pushing data into the running VictoriaMetrics are `examples/vm-push-scenario.yaml` (Prometheus text via `http_push`) and `examples/remote-write-vm.yaml` (`remote_write` to VictoriaMetrics, vmagent, or upstream Prometheus):

```bash
# Push test data via http_push
sonda run examples/vm-push-scenario.yaml

# Verify the metric exists
curl "http://localhost:8428/api/v1/query?query=cpu_usage"
```

!!! tip "Close the loop with Alertmanager"
    This stack verifies that data arrives in VictoriaMetrics, but does not prove alerts fire. To add vmalert, Alertmanager, and a webhook receiver, see the [Alerting pipeline tab](end-to-end-pipelines.md) on End-to-end pipelines.

## Scrape model instead of push

If you prefer the Prometheus pull model, sonda-server exposes a scrape endpoint for each running scenario:

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

See [Server API](../deploy/server.md) for the full API reference.

## Patterns

<a id="thresholds"></a>

=== "Thresholds"

    The two most common alert shapes are also the two easiest to get wrong: a `> threshold` rule that never fires because the metric only breaches for 30 seconds, and a `for: 5m` clause that fires three minutes early because the test data was lumpier than expected. Sonda gives you three generators that cover both cases deterministically.

    | Pattern | Generator | When to reach for it |
    |---------|-----------|----------------------|
    | Crosses threshold predictably | `sine` | Verifying that the rule fires at all |
    | Stays above threshold for an exact duration | `sequence` | Validating short `for:` clauses (≤ 30s) |
    | Holds above threshold indefinitely | `constant` | Validating long `for:` clauses (minutes) |

    ### Threshold crossings with sine

    The sine generator produces a smooth wave that crosses your threshold predictably. With `amplitude=50` and `offset=50` it oscillates between 0 and 100, crossing 90 for about 12 seconds per 60-second cycle -- enough to trigger a bare `> 90` rule on every period.

    ```bash
    sonda run examples/sine-threshold-test.yaml
    ```

    ```yaml title="examples/sine-threshold-test.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 180s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - signal_type: metrics
        name: cpu_usage
        generator:
          type: sine
          amplitude: 50.0
          period_secs: 60
          offset: 50.0
        labels:
          instance: server-01
          job: node
    ```

    The metric crosses 90 around tick 9, stays above until tick 21, then drops -- giving you roughly 12 seconds above threshold per cycle.

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

    Sine works for unbounded threshold rules. For a `for:` clause you need the breach to last an exact, predictable number of seconds.

    ### Exact `for:` durations with sequence

    Prometheus alerts with a `for:` clause require the condition to be true for a **continuous** duration before firing. The [sequence generator](../build/generators.md#sequence) steps through an explicit list of values, one per tick, so you control the breach window down to the second:

    ```bash
    sonda run examples/for-duration-test.yaml
    ```

    ```yaml title="examples/for-duration-test.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 80s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - signal_type: metrics
        name: cpu_usage
        generator:
          type: sequence
          values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
          repeat: true
        labels:
          instance: server-01
          job: node
    ```

    At `rate: 1`, ticks 5-9 are above 90 -- exactly 5 seconds continuous breach -- then the pattern repeats. To match a `for: 30s` alert, extend the run of `95`s to 30 entries.

    !!! tip "When sequence stops being practical"
        Typing 300 values to satisfy a `for: 5m` alert is no fun. Past about 30 values, switch to the constant generator below and let the runtime duration do the work.

    ### Constant generator shortcut

    For sustained-breach tests longer than ~30 seconds, the [constant generator](../build/generators.md#constant) is more practical:

    ```bash
    sonda run examples/constant-threshold-test.yaml
    ```

    ```yaml title="examples/constant-threshold-test.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 360s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - signal_type: metrics
        name: cpu_usage
        generator:
          type: constant
          value: 95.0
        labels:
          instance: server-01
          job: node
    ```

    Run this for 6 minutes to test a `for: 5m` alert. The value stays at 95 for the entire duration -- the alert should fire after 5 minutes of continuous breach.

<a id="resolution-and-recovery"></a>

=== "Resolution and recovery"

    A rule that fires but never clears is a paging incident waiting to happen. When a metric goes silent during a gap, Prometheus treats it as stale and resolves the alert -- the same path a real scrape failure or restart takes. Use [gap windows](../reference/scenario-fields.md) to control when metrics disappear, so you can confirm both the firing and the resolution side of the rule.

    ```text
    Time:  0s          40s         60s         100s        120s
           |-----------|xxxxxxxxxxx|-----------|xxxxxxxxxxx|
           emit events   gap (20s)  emit events   gap (20s)
    ```

    Gaps occupy the **tail** of each cycle. With `every: 60s` and `for: 20s`, the gap runs from second 40 to second 60 of each cycle.

    ```bash
    sonda run examples/gap-alert-test.yaml
    ```

    ```yaml title="examples/gap-alert-test.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 300s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - signal_type: metrics
        name: cpu_usage
        generator:
          type: constant
          value: 95.0
        gaps:
          every: 60s
          for: 20s
        labels:
          instance: server-01
          job: node
    ```

    The value stays at 95 (above threshold) but goes silent for 20 seconds every 60-second cycle. The alert enters pending state during the 40-second emit window but may not reach the `for:` duration before the gap resets it -- which is exactly the flapping pattern you want to validate against.

    !!! tip "Combine gaps with any generator"
        Gaps work with any generator. A sine wave with periodic gaps creates a realistic "flapping service" pattern -- useful for testing that your alert hysteresis or `keep_firing_for` clause actually suppresses the noise.

    Gaps drive recovery passively — Prometheus resolves the alert on its own once the metric goes silent for the lookback-delta window. For active recovery, where Sonda emits a stale marker or an explicit recovery value the moment a `while:`-gated entry pauses, see [Recovering Prometheus alerts on gate close](../build/scenario-files.md#recovering-prometheus-alerts-on-gate-close). Gate close clears alerts on the next scrape rather than waiting for the lookback window, at the cost of being `remote_write`-specific unless you pair it with `snap_to:`.

<a id="compound-and-correlated"></a>

=== "Compound and correlated"

    Production alerts often depend on more than one metric. Compound rules like `cpu_usage > 90 AND memory_usage_percent > 85` only fire when both conditions are true at the same moment -- which means your test data needs an overlapping window across two scenarios. Sonda gives you `phase_offset` and `clock_group` to build that overlap deterministically.

    | Field | Default | Description |
    |-------|---------|-------------|
    | `phase_offset` | `"0s"` | Delay before this scenario starts emitting |
    | `clock_group` | (none) | Groups scenarios under a shared timing reference |

    ```bash
    sonda run examples/multi-metric-correlation.yaml
    ```

    ```yaml title="examples/multi-metric-correlation.yaml (excerpt)"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 120s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - signal_type: metrics
        name: cpu_usage
        clock_group: alert-test
        generator:
          type: sequence
          values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
          repeat: true

      - signal_type: metrics
        name: memory_usage_percent
        phase_offset: "3s"
        clock_group: alert-test
        generator:
          type: sequence
          values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
          repeat: true
    ```

    ### Reading the timeline

    ```text
    Wall time  cpu_usage (offset=0s)   memory_usage (offset=3s)
    --------   ---------------------   ------------------------
    t=0s       starts: 20             sleeping
    t=3s       95 (above threshold)   starts: 40
    t=6s       95                     88 (above threshold)
    t=8s       20 (drops)             88
    ```

    The overlap window -- where **both** metrics are above threshold -- runs from t=6s to t=8s (2 seconds per cycle). For a `for: 5m` compound rule, extend the above-threshold sequences or switch to constant generators with a longer overall duration.

    !!! info "clock_group ties scenarios to a shared timeline"
        Without `clock_group`, every scenario starts at its own wall-clock time and the overlap drifts. With `clock_group: alert-test`, all members share a reference clock and `phase_offset` is measured against that reference. See [Scenario Fields -- Temporal fields](../reference/scenario-fields.md#temporal-fields) for the full ordering semantics.

    See [Example Scenarios](examples.md) for the full `multi-metric-correlation.yaml` file.

<a id="cardinality-explosion"></a>

=== "Cardinality explosion"

    Many monitoring stacks page when series cardinality crosses a guardrail (`count(up) > 10000`, `prometheus_tsdb_symbol_table_size_bytes > N`, etc.). The rule fires the first time a deploy ships a label with too many distinct values -- and the only way to know it works is to push a controlled explosion through it. Sonda's [cardinality spikes](../reference/scenario-fields.md) generate a bounded burst of unique label values on a recurring schedule, so you can verify the alert fires during the spike and resolves after.

    ```bash
    sonda run examples/cardinality-alert-test.yaml
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

    During the 10-second spike window, each tick injects a `pod_name` label drawn from a pool of up to 500 unique values (`pod-0` through `pod-499`). The actual per-spike series count is `min(cardinality, ticks_in_window)` — at `rate: 10, for: 10s` that's 100 ticks per spike, so each spike grows the visible series count by up to 100 new `pod-N` values until the 500-value pool fills across recurrences. Outside the spike window the label is absent and only one series is emitted. This on/off pattern exercises both the firing and resolution paths of the cardinality rule.

    ### Tuning the spike

    Three knobs shape the explosion:

    | Field | Effect |
    |-------|--------|
    | `cardinality` | Number of unique label values per spike. Set this just above your alert threshold. |
    | `for` | How long the spike lasts. Set this longer than your rule's `for:` clause. |
    | `every` | How often the spike recurs. Useful for proving the rule re-fires after a quiet window. |

    For a rule like `ALERT HighCardinality IF count(...) > 400 FOR 5m`, set `cardinality: 500` and `for: 360s` and watch the alert pend, fire, then clear after the spike ends.

<a id="replaying-incidents"></a>

=== "Replaying incidents"

    Synthetic shapes prove the alert path works in the abstract. Replay proves it would have caught the real incident. Two generators handle the replay case: `sequence` for short hand-crafted patterns, and `csv_replay` for long recordings exported from your TSDB.

    | Generator | Best for | Storage |
    |-----------|----------|---------|
    | `sequence` | ≤ 20 values, hand-tuned | Inline in the YAML |
    | `csv_replay` | Real incidents, long recordings | External CSV file |

    ### Hand-crafted patterns with sequence

    The [sequence generator](../build/generators.md#sequence) steps through an explicit list of values, perfect for short, deterministic threshold patterns:

    ```bash
    sonda run examples/sequence-alert-test.yaml
    ```

    ```yaml title="examples/sequence-alert-test.yaml (key fields)"
    generator:
      type: sequence
      values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
      repeat: true
    ```

    With `repeat: true`, the pattern loops continuously. With `repeat: false`, the generator holds the last value after the sequence ends -- useful for "the metric pegged at 100 and never recovered" scenarios.

    ### Production replay with csv_replay

    For replaying real production data, the [csv_replay generator](../build/generators.md#csv_replay) reads values from a CSV file. If you have a Grafana dashboard showing the incident, see the [Grafana CSV Replay](../import/grafana-exports.md) guide for the full export-and-replay workflow.

    ```bash
    sonda run examples/csv-replay-metrics.yaml
    ```

    ```yaml title="examples/csv-replay-metrics.yaml (key fields)"
    generator:
      type: csv_replay
      file: examples/sample-cpu-values.csv
      columns:
        - index: 1
          name: cpu_replay
    ```

    | Parameter | Default | Description |
    |-----------|---------|-------------|
    | `file` | (required) | Path to the CSV file |
    | `columns` | -- | Explicit column specs. When absent, columns are auto-discovered from the header. See [Generators](../build/generators.md#csv_replay). |
    | `repeat` | `true` | Cycle back to the first value after reaching the end |

    !!! tip "When to use csv_replay vs sequence"
        Use `csv_replay` over `sequence` when you have more than ~20 values. It keeps the YAML clean and makes it easy to update the data by replacing the CSV file -- the scenario stays identical.

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

<a id="histogram-and-summary-alerts"></a>

=== "Histogram and summary alerts"

    Counters and gauges are straightforward: one metric, one value, one line per scrape. Histograms and summaries are different. They break a single measurement (like request latency) into multiple time series that work together. This tab explains how they work, when to use each, and how to test latency alerts.

    ### What is a histogram?

    A histogram tracks the **distribution** of observed values by counting how many observations fall into predefined buckets. When you instrument HTTP request latency as a histogram, Prometheus doesn't store each individual request duration. Instead, it maintains cumulative counters for each bucket boundary.

    For a metric named `http_request_duration_seconds` with default Prometheus buckets, every scrape produces these time series:

    | Series | What it counts |
    |--------|----------------|
    | `http_request_duration_seconds_bucket{le="0.005"}` | Requests <= 5ms |
    | `http_request_duration_seconds_bucket{le="0.01"}` | Requests <= 10ms |
    | `http_request_duration_seconds_bucket{le="0.025"}` | Requests <= 25ms |
    | ... | ... |
    | `http_request_duration_seconds_bucket{le="+Inf"}` | All requests (always equals `_count`) |
    | `http_request_duration_seconds_count` | Total number of observations |
    | `http_request_duration_seconds_sum` | Sum of all observed values |

    Every bucket is **cumulative** -- the `le="0.1"` bucket includes all observations that are also in `le="0.05"` and below. These are counters, so they only ever go up. Prometheus uses `rate()` to compute per-second rates, then `histogram_quantile()` to estimate percentiles from the bucket distribution.

    !!! info "Why cumulative?"
        Cumulative counters let you use `rate()` to compute accurate per-second observation rates over any time window. If buckets were absolute counts per scrape, you couldn't aggregate across time ranges or instances.

    #### Concrete example

    Suppose 100 requests arrive in one second with these latencies: 60 requests under 100ms, 30 between 100ms and 250ms, and 10 between 250ms and 500ms. The bucket counters after that second:

    ```text
    http_request_duration_seconds_bucket{le="0.1"}   60
    http_request_duration_seconds_bucket{le="0.25"}  90   # 60 + 30
    http_request_duration_seconds_bucket{le="0.5"}   100  # 60 + 30 + 10
    http_request_duration_seconds_bucket{le="+Inf"}  100
    http_request_duration_seconds_count              100
    http_request_duration_seconds_sum                12.5
    ```

    From this, `histogram_quantile(0.99, ...)` estimates the 99th percentile by interpolating between bucket boundaries.

    ### What is a summary?

    A summary also tracks value distributions, but instead of counting observations per bucket, it **pre-computes quantile values** on the client side. For a metric named `rpc_duration_seconds` with quantiles `[0.5, 0.9, 0.95, 0.99]`, each scrape produces:

    ```text
    rpc_duration_seconds{quantile="0.5"}   0.098
    rpc_duration_seconds{quantile="0.9"}   0.125
    rpc_duration_seconds{quantile="0.95"}  0.131
    rpc_duration_seconds{quantile="0.99"}  0.148
    rpc_duration_seconds_count             1000
    rpc_duration_seconds_sum               99.44
    ```

    The quantile values change each scrape -- they reflect the distribution of observations in a sliding time window. `_count` and `_sum` are cumulative, just like histograms.

    ### Histogram vs. summary: when to use which

    | | Histogram | Summary |
    |--|-----------|---------|
    | **Percentile computation** | Server-side via `histogram_quantile()` | Client-side, pre-computed |
    | **Aggregatable across instances?** | Yes -- you can sum bucket counters | No -- you cannot average percentiles |
    | **Choose percentile after the fact?** | Yes -- any percentile from the same data | No -- only the quantiles you configured |
    | **Accuracy** | Depends on bucket boundaries | Exact for the configured quantiles |
    | **Cost** | One counter per bucket per label set | One gauge per quantile per label set |

    !!! tip "Default to histograms"
        In most cases, histograms are the better choice. They can be aggregated across instances (critical for Kubernetes deployments) and let you compute any percentile from a single set of buckets. Use summaries only when you need exact quantile values and aggregation across instances is not required.

    ### Generate histogram data with Sonda

    Sonda's histogram generator samples observations from a configurable distribution on each tick and maintains cumulative bucket counters, just like a real Prometheus client library. The output works directly with `rate()` and `histogram_quantile()`.

    ```yaml title="examples/histogram.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 10s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - signal_type: histogram
        name: http_request_duration_seconds
        distribution:
          type: exponential
          rate: 10.0
        observations_per_tick: 100
        seed: 42
        labels:
          method: GET
          handler: /api/v1/query
    ```

    Run it:

    ```bash
    sonda run examples/histogram.yaml
    ```

    ```text title="Output (first tick)"
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.005",method="GET"} 3 1775409497421
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.01",method="GET"} 11 1775409497421
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.025",method="GET"} 26 1775409497421
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.05",method="GET"} 46 1775409497421
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.1",method="GET"} 66 1775409497421
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.25",method="GET"} 90 1775409497421
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="0.5",method="GET"} 100 1775409497421
    ...
    http_request_duration_seconds_bucket{handler="/api/v1/query",le="+Inf",method="GET"} 100 1775409497421
    http_request_duration_seconds_count{handler="/api/v1/query",method="GET"} 100 1775409497421
    http_request_duration_seconds_sum{handler="/api/v1/query",method="GET"} 9.505 1775409497421
    ```

    Notice the cumulative bucket counts: 3 requests were under 5ms, 11 under 10ms (which includes the 3 from the previous bucket), and so on. The `+Inf` bucket equals `_count` because every observation falls within infinity.

    ### Test a histogram_quantile() alert with Sonda

    This is the primary use case: you have a PromQL alert rule and you want to verify that it fires when latency degrades.

    #### The alert rule

    ```yaml title="alert-rules.yaml"
    groups:
      - name: latency
        rules:
          - alert: HighP99Latency
            expr: |
              histogram_quantile(0.99,
                rate(http_request_duration_seconds_bucket[5m])
              ) > 0.5
            for: 2m
            labels:
              severity: critical
            annotations:
              summary: "P99 latency exceeds 500ms"
    ```

    This fires when the estimated 99th percentile of request latency exceeds 500ms for 2 minutes.

    #### Simulate latency degradation

    The `mean_shift_per_sec` parameter shifts the distribution's center over time. With an exponential distribution (mean = 1/rate = 0.1s) and a shift of `0.01` per second, the effective mean increases from 0.1s to 0.7s after 60 seconds -- pushing the p99 well above the 500ms threshold.

    ```yaml title="histogram-degradation.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 5m
      encoder:
        type: remote_write
      sink:
        type: remote_write
        url: http://localhost:8428/api/v1/write

    scenarios:
      - signal_type: histogram
        name: http_request_duration_seconds
        distribution:
          type: exponential
          rate: 10.0
        observations_per_tick: 100
        mean_shift_per_sec: 0.01
        seed: 42
        labels:
          method: GET
          handler: /api/v1/query
    ```

    ```bash
    sonda run histogram-degradation.yaml
    ```

    As Sonda runs, the distribution center drifts higher. After about 40 seconds, most observations land in the 0.5s+ buckets. Prometheus computes `histogram_quantile(0.99, rate(...)[5m])` and sees the p99 cross the 0.5s threshold. After 2 minutes of sustained breach, the `HighP99Latency` alert fires.

    !!! tip "Choosing the shift rate"
        A `mean_shift_per_sec` of `0.01` with an exponential distribution (rate=10, mean=0.1s) means the average latency doubles in about 10 seconds and reaches 0.5s in about 40 seconds. Adjust the shift rate to control how quickly the alert triggers.

    #### Verify the alert

    After starting Sonda, query your Prometheus or VictoriaMetrics instance:

    ```promql
    histogram_quantile(0.99, rate(http_request_duration_seconds_bucket{method="GET"}[5m]))
    ```

    You should see the p99 value climbing steadily from ~0.2s toward and beyond 0.5s. Once the `for: 2m` condition is sustained, check the alerts endpoint:

    ```bash
    # Prometheus
    curl -s http://localhost:9090/api/v1/alerts | jq '.data.alerts[] | select(.labels.alertname == "HighP99Latency")'

    # VictoriaMetrics
    curl -s http://localhost:8428/api/v1/alerts | jq '.data.alerts[] | select(.labels.alertname == "HighP99Latency")'
    ```

    ### Summary example

    Summaries are simpler to generate and query, but remember: the quantile values are computed per-tick and cannot be aggregated across instances.

    ```yaml title="examples/summary.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 10s
      encoder:
        type: prometheus_text
      sink:
        type: stdout

    scenarios:
      - signal_type: summary
        name: rpc_duration_seconds
        distribution:
          type: normal
          mean: 0.1
          stddev: 0.02
        observations_per_tick: 100
        seed: 42
        labels:
          service: auth
          method: GetUser
    ```

    ```bash
    sonda run examples/summary.yaml
    ```

    ```text title="Output (first tick)"
    rpc_duration_seconds{method="GetUser",quantile="0.5",service="auth"} 0.098 1775409507904
    rpc_duration_seconds{method="GetUser",quantile="0.9",service="auth"} 0.128 1775409507904
    rpc_duration_seconds{method="GetUser",quantile="0.95",service="auth"} 0.136 1775409507904
    rpc_duration_seconds{method="GetUser",quantile="0.99",service="auth"} 0.148 1775409507904
    rpc_duration_seconds_count{method="GetUser",service="auth"} 100 1775409507904
    rpc_duration_seconds_sum{method="GetUser",service="auth"} 9.802 1775409507904
    ```

    The p50 is near the configured mean (0.1s), and the spread matches the configured standard deviation (0.02s). Count and sum increase cumulatively across ticks, but quantile values are fresh per-tick snapshots.

    !!! warning "Summaries are not aggregatable"
        You cannot meaningfully average p99 values across multiple instances. If you need per-service percentiles across a fleet of pods, use histograms instead -- you can sum the bucket counters first, then compute `histogram_quantile()` on the aggregated data.

    ### Distribution models

    Both histogram and summary generators support three distribution models. Choose the one that best matches the real-world metric you are simulating.

    | Distribution | YAML | Typical use | Parameters |
    |-------------|------|-------------|------------|
    | Exponential | `type: exponential` | Request latency (long tail) | `rate` -- lambda; mean = 1/rate |
    | Normal | `type: normal` | Symmetric around a center value | `mean`, `stddev` |
    | Uniform | `type: uniform` | Even spread across a range | `min`, `max` |

    ```yaml title="Exponential distribution (mean = 100ms)"
    distribution:
      type: exponential
      rate: 10.0
    ```

    Most observations cluster near zero with a long tail. This is the default choice for HTTP latency simulation.

    ```yaml title="Normal distribution (mean = 100ms, stddev = 20ms)"
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    ```

    Symmetric bell curve. Good for metrics with a known center and consistent spread, like RPC durations in a healthy service.

    ```yaml title="Uniform distribution (50ms to 150ms)"
    distribution:
      type: uniform
      min: 0.05
      max: 0.15
    ```

    Every value in the range is equally likely. Useful for stress-testing bucket boundaries.

    For full parameter reference, see [Generators -- histogram](../build/generators.md#histogram) and [Generators -- summary](../build/generators.md#summary).

## Quick reference

| Pattern | Generator | Example file |
|---------|-----------|--------------|
| Threshold crossing | `sine` | `sine-threshold-test.yaml` |
| Sustained breach | `constant` | `constant-threshold-test.yaml` |
| Alert resolution via gap | `constant` + `gaps` | `gap-alert-test.yaml` |
| Precise `for:` duration | `sequence` | `for-duration-test.yaml` |
| Compound alert | multi-scenario | `multi-metric-correlation.yaml` |
| Cardinality explosion | any + `cardinality_spikes` | `cardinality-alert-test.yaml` |
| Periodic spike / anomaly | `spike` | `spike-alert-test.yaml` |
| Incident replay (inline) | `sequence` | `sequence-alert-test.yaml` |
| Incident replay (file) | `csv_replay` | `csv-replay-metrics.yaml` |
| Histogram latency degradation | `histogram` + `mean_shift_per_sec` | `histogram-degradation.yaml` |
| Push to VictoriaMetrics | any | `vm-push-scenario.yaml` |
| Remote write | any | `remote-write-vm.yaml` |

## Where to next

- [End-to-end pipelines](end-to-end-pipelines.md) — verify alerts fire all the way through vmalert, Alertmanager, and a webhook, in dev or CI.
- [Recording rules](recording-rules.md) — validate that aggregations land before the alert rule queries them.
- [Generators](../build/generators.md) — pick the right generator for the pattern you're testing.
- [Example Scenarios](examples.md) — every example scenario file with its purpose.
- [Troubleshooting](../reference/troubleshooting.md) — diagnostics when the alert isn't firing.
