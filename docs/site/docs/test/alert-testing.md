---
title: Alert testing patterns
description: Test Prometheus alert rules end-to-end with Sonda — thresholds, resolution, correlation, cardinality, replay, and histogram alerts.
---

# Alert testing

This page covers six patterns for testing Prometheus alert rules with Sonda. Each pattern targets a bug class that is hard to reproduce in production: threshold and `for:` durations, resolution and flapping, compound rules, cardinality limits, incident replay, and histogram alerts.

For each pattern, Sonda generates a metric stream with the values and labels your rule expects. You define the alert, run the scenario, and check whether the alert fires.

Use the table below to find the pattern that matches your rule. Each section is self-contained.

## Pick your pattern

The table maps a rule shape to the tab that covers it.

| You want to test | Tab | Generator |
|---------------------|-----|-----------|
| A simple `> threshold` rule | [Thresholds](#thresholds) | `sine` |
| A short `for:` clause (≤ 30s) | [Thresholds](#thresholds) | `sequence` |
| A long `for:` clause (minutes) | [Thresholds](#thresholds) | `constant` |
| Resolution and flapping behaviour | [Resolution and recovery](#resolution-and-recovery) | any + `gaps` |
| Compound `A AND B` rules | [Compound and correlated](#compound-and-correlated) | multi-scenario |
| Cardinality limits | [Cardinality explosion](#cardinality-explosion) | any + `cardinality_spikes` |
| Replay of a known incident | [Replaying incidents](#replaying-incidents) | `sequence` or `csv_replay` |
| Latency and histogram alerts | [Histogram and summary alerts](#histogram-and-summary-alerts) | `histogram` |

!!! info "Get the example files"
    This page uses YAML scenarios and a Compose file from the `examples/` directory in the Sonda repository. The pre-built binary does not include them. Clone the repository to get every file at once:

    ```bash
    git clone https://github.com/davidban77/sonda.git
    cd sonda
    ```

    Or download a single file with `curl`. Replace `<filename>` with the file you need:

    ```bash
    curl -O https://raw.githubusercontent.com/davidban77/sonda/main/examples/<filename>
    ```

## Setup

Every tab below assumes a local backend is running. Sonda includes a Compose file that starts VictoriaMetrics, vmagent, and Grafana together.

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

After running any scenario, query VictoriaMetrics to confirm the metric arrived. Wait about 15 seconds for ingestion.

```bash
curl -s "http://localhost:8428/api/v1/query?query=<metric_name>" | jq '.data.result | length'
```

Stop the stack when finished:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

See [Docker Deployment](../deploy/docker.md) for the full stack configuration. For the full alert path (vmalert + Alertmanager + webhook receiver), use the same Compose file with `--profile alerting` and follow the [Alerting pipeline tab on End-to-end pipelines](end-to-end-pipelines.md).

## Push to a real backend

Two example scenarios send data into the running VictoriaMetrics. Use `examples/vm-push-scenario.yaml` for Prometheus text over `http_push`. Use `examples/remote-write-vm.yaml` for `remote_write` to VictoriaMetrics, vmagent, or upstream Prometheus.

```bash
# Push test data via http_push
sonda run examples/vm-push-scenario.yaml

# Verify the metric exists
curl "http://localhost:8428/api/v1/query?query=cpu_usage"
```

!!! tip "Complete the alert path"
    This stack confirms that data arrives in VictoriaMetrics. It does not confirm that alerts fire. To add vmalert, Alertmanager, and a webhook receiver, see the [Alerting pipeline tab](end-to-end-pipelines.md) on End-to-end pipelines.

## Scrape model instead of push

If you prefer the Prometheus pull model, `sonda-server` exposes a scrape endpoint for each running scenario. The install script and Docker image both include the `sonda-server` binary.

```bash
sonda-server --port 8080

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

    This tab covers three threshold-rule shapes: a bare `> threshold` rule, a short `for:` clause, and a long `for:` clause. Three generators cover them deterministically.

    | Pattern | Generator | When to use it |
    |---------|-----------|----------------------|
    | Crosses threshold at a predictable interval | `sine` | Verify the rule fires at all |
    | Stays above threshold for an exact duration | `sequence` | Validate short `for:` clauses (≤ 30s) |
    | Holds above threshold indefinitely | `constant` | Validate long `for:` clauses (minutes) |

    ### Threshold crossings with sine

    The sine generator produces a smooth wave that crosses your threshold at a known interval. With `amplitude=50` and `offset=50` the value oscillates between 0 and 100. It crosses 90 for about 12 seconds per 60-second cycle. This is enough to trigger a bare `> 90` rule on every period.

    The scenario below uses `amplitude`, `period_secs`, and `offset` to set the wave. The `labels` field attaches `instance` and `job` to the series.

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

    The metric crosses 90 around tick 9, stays above until tick 21, then drops. Each cycle produces about 12 seconds above threshold.

    ??? info "Sine wave math"
        The formula is: `value = offset + amplitude * sin(2 * pi * tick / period_ticks)`.

        With `amplitude=50` and `offset=50`, the value crosses 90 when `sin(x) > 0.8`:

        - `sin(x) = 0.8` at `x = arcsin(0.8) = 0.927 radians`
        - The sine value exceeds 0.8 from `x = 0.927` to `x = pi - 0.927 = 2.214`
        - That covers `1.287 / 6.283 = 20.5%` of each cycle
        - With a 60-second period, the value is above 90 for about 12.3 seconds per cycle

        | Tick (sec) | sin(2*pi*t/60) | Value | Above 90? |
        |------------|----------------|-------|-----------|
        | 0  | 0.000  | 50.0  | No  |
        | 5  | 0.500  | 75.0  | No  |
        | 10 | 0.866  | 93.3  | Yes |
        | 15 | 1.000  | 100.0 | Yes |
        | 20 | 0.866  | 93.3  | Yes |
        | 25 | 0.500  | 75.0  | No  |

    Sine works for an unbounded threshold rule. For a `for:` clause you need the breach to last a known number of seconds.

    ### Exact `for:` durations with sequence

    A Prometheus alert with a `for:` clause requires the condition to be true for a continuous duration before firing. The [sequence generator](../build/generators.md#sequence) emits a list of values, one per tick. Each entry in `values` is the metric value at that tick. The `repeat` field controls whether the list loops.

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

    At `rate: 1`, ticks 5 to 9 hold the value at 95. That is exactly 5 seconds of continuous breach. The pattern then repeats. To match a `for: 30s` alert, extend the run of `95` values to 30 entries.

    !!! tip "When to switch from sequence"
        Typing 300 values for a `for: 5m` alert is tedious. Past about 30 values, use the constant generator below and let the runtime duration do the work.

    ### Sustained breach with constant

    For a breach longer than 30 seconds, the [constant generator](../build/generators.md#constant) is more practical. It holds a single value for the full run duration.

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

    Run this for 6 minutes to test a `for: 5m` alert. The value stays at 95 for the full duration. The alert should fire after 5 minutes of continuous breach.

<a id="resolution-and-recovery"></a>

=== "Resolution and recovery"

    This tab covers alert resolution. A rule that fires but never clears causes paging fatigue. When a metric stops arriving, Prometheus treats the series as stale and resolves the alert. That is the same path a real scrape failure or restart takes.

    The `gaps` field controls when a scenario stops emitting. Use it to confirm both the firing and the resolution side of the rule. The `every` field sets the cycle length. The `for` field sets how long the gap lasts at the end of each cycle.

    ```text
    Time:  0s          40s         60s         100s        120s
           |-----------|xxxxxxxxxxx|-----------|xxxxxxxxxxx|
           emit events   gap (20s)  emit events   gap (20s)
    ```

    The gap occupies the tail of each cycle. With `every: 60s` and `for: 20s`, the gap runs from second 40 to second 60 of each cycle.

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

    The value stays at 95 during the emit window. The metric then disappears for 20 seconds every cycle. The alert may enter pending state during the 40-second emit window but reset before the `for:` duration. This is the flapping pattern you want to validate.

    !!! tip "Combine gaps with any generator"
        The `gaps` field works with any generator. A sine wave with periodic gaps creates a flapping pattern. Use it to test that an alert hysteresis or `keep_firing_for` clause suppresses the noise.

    Gaps drive recovery passively. Prometheus resolves the alert once the metric is silent for the lookback-delta window. For active recovery, where Sonda emits a stale marker or an explicit recovery value the moment a `while:`-gated entry pauses, see [Recovering Prometheus alerts on gate close](../build/scenario-files.md#recovering-prometheus-alerts-on-gate-close). Gate close clears the alert on the next scrape rather than waiting for the lookback window. The cost is that it is `remote_write`-specific unless you pair it with `snap_to:`.

<a id="compound-and-correlated"></a>

=== "Compound and correlated"

    This tab covers compound rules like `cpu_usage > 90 AND memory_usage_percent > 85`. The rule fires only when both conditions are true at the same time. Your test data needs an overlap window across two scenarios.

    Two fields produce that overlap. The `phase_offset` field delays a scenario before it starts emitting. The `clock_group` field ties scenarios to a shared reference clock.

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

    The table below shows what each scenario emits at each tick.

    ```text
    Wall time  cpu_usage (offset=0s)   memory_usage (offset=3s)
    --------   ---------------------   ------------------------
    t=0s       starts: 20              not started
    t=3s       95 (above threshold)    starts: 40
    t=6s       95                      88 (above threshold)
    t=8s       20 (drops)              88
    ```

    Both metrics are above threshold from t=6s to t=8s. That overlap is 2 seconds per cycle. For a `for: 5m` compound rule, extend the above-threshold runs or switch to constant generators with a longer total duration.

    !!! info "clock_group ties scenarios to a shared timeline"
        Without `clock_group`, each scenario starts at its own wall-clock time and the overlap drifts. With `clock_group: alert-test`, all members share a reference clock. The `phase_offset` value is measured against that reference. See [Scenario Fields — Temporal fields](../reference/scenario-fields.md#temporal-fields) for the full ordering semantics.

    See [Example Scenarios](examples.md) for the full `multi-metric-correlation.yaml` file.

<a id="cardinality-explosion"></a>

=== "Cardinality explosion"

    This tab covers cardinality alerts. Many monitoring stacks page when series count crosses a limit, for example `count(up) > 10000` or `prometheus_tsdb_symbol_table_size_bytes > N`. The rule fires the first time a deploy adds a label with too many distinct values. The only way to confirm it works is to push a controlled burst through it.

    Sonda's [cardinality spikes](../reference/scenario-fields.md) emit a bounded burst of unique label values on a recurring schedule. The `label` field names the label to inject. The `cardinality` field caps the unique values per spike. The `every` and `for` fields control the schedule.

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

    During the 10-second spike window, each tick attaches a `pod_name` label from a pool of up to 500 unique values (`pod-0` through `pod-499`). The actual series count per spike is `min(cardinality, ticks_in_window)`. At `rate: 10, for: 10s` that is 100 ticks per spike. Each spike adds up to 100 new `pod-N` values until the 500-value pool fills across recurrences. Outside the spike window the label is absent and only one series is emitted. This on-and-off pattern tests both the firing and resolution paths of the cardinality rule.

    ### Tuning the spike

    Three fields shape the burst.

    | Field | Effect |
    |-------|--------|
    | `cardinality` | Number of unique label values per spike. Set this slightly above your alert threshold. |
    | `for` | How long the spike lasts. Set this longer than your rule's `for:` clause. |
    | `every` | How often the spike recurs. Useful for confirming the rule re-fires after a quiet window. |

    For a rule like `ALERT HighCardinality IF count(...) > 400 FOR 5m`, set `cardinality: 500` and `for: 360s`. The alert should pend, fire, and then clear after the spike ends.

<a id="replaying-incidents"></a>

=== "Replaying incidents"

    This tab covers two replay generators. Synthetic shapes prove the alert path works for a class of failure. Replay confirms it would have caught the actual incident.

    Use `sequence` for short hand-crafted patterns. Use `csv_replay` for long recordings exported from your TSDB.

    | Generator | Best for | Storage |
    |-----------|----------|---------|
    | `sequence` | ≤ 20 values, hand-tuned | Inline in the YAML |
    | `csv_replay` | Real incidents, long recordings | External CSV file |

    ### Hand-crafted patterns with sequence

    The [sequence generator](../build/generators.md#sequence) emits an explicit list of values, one per tick. The `repeat` field controls whether the list loops.

    ```bash
    sonda run examples/sequence-alert-test.yaml
    ```

    ```yaml title="examples/sequence-alert-test.yaml (key fields)"
    generator:
      type: sequence
      values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
      repeat: true
    ```

    With `repeat: true`, the pattern loops continuously. With `repeat: false`, the generator holds the last value after the sequence ends. The second mode models a metric that pegged at 100 and never recovered.

    ### Production replay with csv_replay

    For real production data, the [csv_replay generator](../build/generators.md#csv_replay) reads values from a CSV file. The `file` field is the path. The `columns` field maps a column to a metric name. If you have a Grafana dashboard showing the incident, see the [Grafana CSV Replay](../import/grafana-exports.md) guide for the full export-and-replay workflow.

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
    | `columns` | — | Column-to-metric mapping. When absent, columns are auto-discovered from the header. See [Generators](../build/generators.md#csv_replay). |
    | `repeat` | `true` | Cycle back to the first value after reaching the end |

    !!! tip "When to use csv_replay vs sequence"
        Use `csv_replay` when you have more than about 20 values. The YAML stays short. You can update the data by replacing the CSV file. The scenario stays identical.

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

    This tab covers latency alerts based on histograms and summaries. A counter or gauge holds one value per series. A histogram or summary represents a distribution across many series. The sections below define each type, compare them, and walk through a `histogram_quantile()` alert test.

    ### What is a histogram

    A histogram counts how many observations fall into predefined buckets. When you instrument HTTP request latency as a histogram, Prometheus does not store each request duration. It maintains a cumulative counter for each bucket boundary.

    For a metric named `http_request_duration_seconds` with default Prometheus buckets, every scrape produces these series.

    | Series | What it counts |
    |--------|----------------|
    | `http_request_duration_seconds_bucket{le="0.005"}` | Requests <= 5ms |
    | `http_request_duration_seconds_bucket{le="0.01"}` | Requests <= 10ms |
    | `http_request_duration_seconds_bucket{le="0.025"}` | Requests <= 25ms |
    | ... | ... |
    | `http_request_duration_seconds_bucket{le="+Inf"}` | All requests (always equals `_count`) |
    | `http_request_duration_seconds_count` | Total number of observations |
    | `http_request_duration_seconds_sum` | Sum of all observed values |

    Every bucket is cumulative. The `le="0.1"` bucket includes all observations also in `le="0.05"` and below. These series are counters and only increase. Prometheus uses `rate()` to compute the per-second observation rate. It then uses `histogram_quantile()` to estimate percentiles from the bucket distribution.

    !!! info "Why cumulative"
        Cumulative counters let `rate()` compute an accurate per-second rate over any time window. If buckets were absolute counts per scrape, you could not aggregate across time ranges or instances.

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

    ### What is a summary

    A summary also tracks a value distribution. Instead of counting observations per bucket, it pre-computes quantile values on the client side. For a metric named `rpc_duration_seconds` with quantiles `[0.5, 0.9, 0.95, 0.99]`, each scrape produces:

    ```text
    rpc_duration_seconds{quantile="0.5"}   0.098
    rpc_duration_seconds{quantile="0.9"}   0.125
    rpc_duration_seconds{quantile="0.95"}  0.131
    rpc_duration_seconds{quantile="0.99"}  0.148
    rpc_duration_seconds_count             1000
    rpc_duration_seconds_sum               99.44
    ```

    The quantile values change each scrape. They reflect the distribution of observations in a sliding time window. The `_count` and `_sum` series are cumulative, like a histogram.

    ### Histogram or summary: when to use which

    The table below compares both types on five criteria.

    | | Histogram | Summary |
    |--|-----------|---------|
    | **Percentile computation** | Server-side via `histogram_quantile()` | Client-side, pre-computed |
    | **Aggregatable across instances?** | Yes — you can sum bucket counters | No — you cannot average percentiles |
    | **Choose percentile after the fact?** | Yes — any percentile from the same data | No — only the quantiles you configured |
    | **Accuracy** | Depends on bucket boundaries | Exact for the configured quantiles |
    | **Cost** | One counter per bucket per label set | One gauge per quantile per label set |

    !!! tip "Default to histograms"
        In most cases, a histogram is the better choice. Histograms aggregate across instances, which is critical for Kubernetes deployments. They also let you compute any percentile from a single set of buckets. Use a summary only when you need exact quantile values and you do not need to aggregate across instances.

    ### Generate histogram data with Sonda

    Sonda's histogram generator samples observations from a configurable distribution on each tick. It maintains cumulative bucket counters, like a real Prometheus client library. The output works directly with `rate()` and `histogram_quantile()`.

    The scenario below uses `signal_type: histogram`. The `distribution` field selects the model and its parameters. The `observations_per_tick` field sets how many samples to draw per tick. The `seed` field makes the run reproducible.

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

    The bucket counts are cumulative: 3 requests under 5ms, 11 under 10ms (which includes the 3 from the previous bucket), and so on. The `+Inf` bucket equals `_count` because every observation falls within infinity.

    ### Test a histogram_quantile() alert with Sonda

    This is the primary use case. You have a PromQL alert rule and you want to confirm it fires when latency degrades.

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

    The rule fires when the estimated 99th percentile of request latency exceeds 500ms for 2 minutes.

    #### Simulate latency degradation

    The `mean_shift_per_sec` field shifts the distribution's centre over time. The exponential distribution has mean `1/rate`, so `rate: 10.0` gives a starting mean of 0.1s. With a shift of 0.01 per second, the mean reaches about 0.7s after 60 seconds. That pushes the p99 well above the 500ms threshold.

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

    As Sonda runs, the distribution centre drifts higher. After about 40 seconds, most observations fall in the 0.5s and higher buckets. Prometheus computes `histogram_quantile(0.99, rate(...)[5m])` and sees the p99 cross 0.5s. After 2 minutes of sustained breach, the `HighP99Latency` alert fires.

    !!! tip "Choosing the shift rate"
        With `mean_shift_per_sec: 0.01` and an exponential distribution at `rate: 10.0` (starting mean = 0.1s), the average latency doubles in about 10 seconds and reaches 0.5s in about 40 seconds. Increase the shift to trigger the alert sooner. Decrease it for a slower ramp.

    #### Verify the alert

    After starting Sonda, query your Prometheus or VictoriaMetrics instance:

    ```promql
    histogram_quantile(0.99, rate(http_request_duration_seconds_bucket{method="GET"}[5m]))
    ```

    The p99 value should climb steadily from about 0.2s past 0.5s. Once the `for: 2m` condition holds, check the alerts endpoint.

    ```bash
    # Prometheus
    curl -s http://localhost:9090/api/v1/alerts | jq '.data.alerts[] | select(.labels.alertname == "HighP99Latency")'

    # VictoriaMetrics
    curl -s http://localhost:8428/api/v1/alerts | jq '.data.alerts[] | select(.labels.alertname == "HighP99Latency")'
    ```

    ### Summary example

    A summary is simpler to generate and query. Remember that quantile values are computed per tick and cannot be aggregated across instances.

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

    The p50 is near the configured mean (0.1s). The spread matches the configured standard deviation (0.02s). The `_count` and `_sum` series increase across ticks. Quantile values are fresh per-tick snapshots.

    !!! warning "Summaries are not aggregatable"
        You cannot meaningfully average p99 values across instances. If you need per-service percentiles across a fleet of pods, use a histogram. Sum the bucket counters first, then compute `histogram_quantile()` on the aggregated data.

    ### Distribution models

    Both histogram and summary generators support three distribution models. Choose the one that matches the metric you are simulating.

    | Distribution | YAML | Typical use | Parameters |
    |-------------|------|-------------|------------|
    | Exponential | `type: exponential` | Request latency (long tail) | `rate` (lambda); mean = 1/rate |
    | Normal | `type: normal` | Symmetric around a centre value | `mean`, `stddev` |
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

    Symmetric bell curve. Good for a metric with a known centre and a consistent spread, like RPC durations in a healthy service.

    ```yaml title="Uniform distribution (50ms to 150ms)"
    distribution:
      type: uniform
      min: 0.05
      max: 0.15
    ```

    Every value in the range is equally likely. Useful for stress-testing bucket boundaries.

    For the full parameter reference, see [Generators — histogram](../build/generators.md#histogram) and [Generators — summary](../build/generators.md#summary).

## Quick reference

The table below maps every pattern on this page to its generator and example file.

| Pattern | Generator | Example file |
|---------|-----------|--------------|
| Threshold crossing | `sine` | `sine-threshold-test.yaml` |
| Sustained breach | `constant` | `constant-threshold-test.yaml` |
| Alert resolution via gap | `constant` + `gaps` | `gap-alert-test.yaml` |
| Precise `for:` duration | `sequence` | `for-duration-test.yaml` |
| Compound alert | multi-scenario | `multi-metric-correlation.yaml` |
| Cardinality burst | any + `cardinality_spikes` | `cardinality-alert-test.yaml` |
| Periodic spike or anomaly | `spike` | `spike-alert-test.yaml` |
| Incident replay (inline) | `sequence` | `sequence-alert-test.yaml` |
| Incident replay (file) | `csv_replay` | `csv-replay-metrics.yaml` |
| Histogram latency degradation | `histogram` + `mean_shift_per_sec` | `histogram-degradation.yaml` |
| Push to VictoriaMetrics | any | `vm-push-scenario.yaml` |
| Remote write | any | `remote-write-vm.yaml` |

## Where to next

- [End-to-end pipelines](end-to-end-pipelines.md) — confirm alerts fire through vmalert, Alertmanager, and a webhook receiver.
- [Recording rules](recording-rules.md) — confirm that aggregations arrive before the alert rule queries them.
- [Generators](../build/generators.md) — choose the generator for the pattern you are testing.
- [Example Scenarios](examples.md) — every example scenario file with its purpose.
