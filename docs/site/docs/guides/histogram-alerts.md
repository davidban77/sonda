# Histograms, Summaries, and Latency Alerts

Counters and gauges are straightforward: one metric, one value, one line per scrape. Histograms
and summaries are different. They break a single measurement (like request latency) into multiple
time series that work together. This guide explains how they work, when to use each, and how to
test latency alerts with Sonda.

---

## What is a histogram?

A histogram tracks the **distribution** of observed values by counting how many observations fall
into predefined buckets. When you instrument HTTP request latency as a histogram, Prometheus
doesn't store each individual request duration. Instead, it maintains cumulative counters for
each bucket boundary.

For a metric named `http_request_duration_seconds` with default Prometheus buckets, every scrape
produces these time series:

| Series | What it counts |
|--------|----------------|
| `http_request_duration_seconds_bucket{le="0.005"}` | Requests <= 5ms |
| `http_request_duration_seconds_bucket{le="0.01"}` | Requests <= 10ms |
| `http_request_duration_seconds_bucket{le="0.025"}` | Requests <= 25ms |
| ... | ... |
| `http_request_duration_seconds_bucket{le="+Inf"}` | All requests (always equals `_count`) |
| `http_request_duration_seconds_count` | Total number of observations |
| `http_request_duration_seconds_sum` | Sum of all observed values |

Every bucket is **cumulative** -- the `le="0.1"` bucket includes all observations that are also
in `le="0.05"` and below. These are counters, so they only ever go up. Prometheus uses `rate()`
to compute per-second rates, then `histogram_quantile()` to estimate percentiles from the bucket
distribution.

!!! info "Why cumulative?"
    Cumulative counters let you use `rate()` to compute accurate per-second observation rates
    over any time window. If buckets were absolute counts per scrape, you couldn't aggregate
    across time ranges or instances.

### Concrete example

Suppose 100 requests arrive in one second with these latencies: 60 requests under 100ms, 30
between 100ms and 250ms, and 10 between 250ms and 500ms. The bucket counters after that second:

```text
http_request_duration_seconds_bucket{le="0.1"}   60
http_request_duration_seconds_bucket{le="0.25"}  90   # 60 + 30
http_request_duration_seconds_bucket{le="0.5"}   100  # 60 + 30 + 10
http_request_duration_seconds_bucket{le="+Inf"}  100
http_request_duration_seconds_count              100
http_request_duration_seconds_sum                12.5
```

From this, `histogram_quantile(0.99, ...)` estimates the 99th percentile by interpolating
between bucket boundaries.

---

## What is a summary?

A summary also tracks value distributions, but instead of counting observations per bucket, it
**pre-computes quantile values** on the client side. For a metric named `rpc_duration_seconds`
with quantiles `[0.5, 0.9, 0.95, 0.99]`, each scrape produces:

```text
rpc_duration_seconds{quantile="0.5"}   0.098
rpc_duration_seconds{quantile="0.9"}   0.125
rpc_duration_seconds{quantile="0.95"}  0.131
rpc_duration_seconds{quantile="0.99"}  0.148
rpc_duration_seconds_count             1000
rpc_duration_seconds_sum               99.44
```

The quantile values change each scrape -- they reflect the distribution of observations in a
sliding time window. `_count` and `_sum` are cumulative, just like histograms.

---

## Histogram vs. summary: when to use which

| | Histogram | Summary |
|--|-----------|---------|
| **Percentile computation** | Server-side via `histogram_quantile()` | Client-side, pre-computed |
| **Aggregatable across instances?** | Yes -- you can sum bucket counters | No -- you cannot average percentiles |
| **Choose percentile after the fact?** | Yes -- any percentile from the same data | No -- only the quantiles you configured |
| **Accuracy** | Depends on bucket boundaries | Exact for the configured quantiles |
| **Cost** | One counter per bucket per label set | One gauge per quantile per label set |

!!! tip "Default to histograms"
    In most cases, histograms are the better choice. They can be aggregated across
    instances (critical for Kubernetes deployments) and let you compute any percentile from a
    single set of buckets. Use summaries only when you need exact quantile values and
    aggregation across instances is not required.

---

## Generate histogram data with Sonda

Sonda's histogram generator samples observations from a configurable distribution on each tick
and maintains cumulative bucket counters, just like a real Prometheus client library. The output
works directly with `rate()` and `histogram_quantile()`.

### Scenario file

```yaml title="examples/histogram.yaml"
name: http_request_duration_seconds
rate: 1
duration: 10s

distribution:
  type: exponential
  rate: 10.0

observations_per_tick: 100
seed: 42

labels:
  method: GET
  handler: /api/v1/query

encoder:
  type: prometheus_text

sink:
  type: stdout
```

Run it:

```bash
sonda histogram --scenario examples/histogram.yaml
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

Notice the cumulative bucket counts: 3 requests were under 5ms, 11 under 10ms (which includes
the 3 from the previous bucket), and so on. The `+Inf` bucket equals `_count` because every
observation falls within infinity.

---

## Test a histogram_quantile() alert with Sonda

This is the primary use case: you have a PromQL alert rule and you want to verify that it fires
when latency degrades.

### The alert rule

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

### Simulate latency degradation

The `mean_shift_per_sec` parameter shifts the distribution's center over time. With an
exponential distribution (mean = 1/rate = 0.1s) and a shift of `0.01` per second, the effective
mean increases from 0.1s to 0.7s after 60 seconds -- pushing the p99 well above the 500ms
threshold.

```yaml title="histogram-degradation.yaml"
name: http_request_duration_seconds
rate: 1
duration: 5m

distribution:
  type: exponential
  rate: 10.0

observations_per_tick: 100
mean_shift_per_sec: 0.01
seed: 42

labels:
  method: GET
  handler: /api/v1/query

encoder:
  type: remote_write

sink:
  type: remote_write
  endpoint: http://localhost:8428/api/v1/write
```

```bash
sonda histogram --scenario histogram-degradation.yaml
```

As Sonda runs, the distribution center drifts higher. After about 40 seconds, most observations
land in the 0.5s+ buckets. Prometheus computes `histogram_quantile(0.99, rate(...)[5m])` and
sees the p99 cross the 0.5s threshold. After 2 minutes of sustained breach, the `HighP99Latency`
alert fires.

!!! tip "Choosing the shift rate"
    A `mean_shift_per_sec` of `0.01` with an exponential distribution (rate=10, mean=0.1s)
    means the average latency doubles in about 10 seconds and reaches 0.5s in about 40 seconds.
    Adjust the shift rate to control how quickly the alert triggers.

### Verify the alert

After starting Sonda, query your Prometheus or VictoriaMetrics instance:

```promql
histogram_quantile(0.99, rate(http_request_duration_seconds_bucket{method="GET"}[5m]))
```

You should see the p99 value climbing steadily from ~0.2s toward and beyond 0.5s. Once the
`for: 2m` condition is sustained, check the alerts endpoint:

```bash
# Prometheus
curl -s http://localhost:9090/api/v1/alerts | jq '.data.alerts[] | select(.labels.alertname == "HighP99Latency")'

# VictoriaMetrics
curl -s http://localhost:8428/api/v1/alerts | jq '.data.alerts[] | select(.labels.alertname == "HighP99Latency")'
```

---

## Summary example

Summaries are simpler to generate and query, but remember: the quantile values are computed
per-tick and cannot be aggregated across instances.

```yaml title="examples/summary.yaml"
name: rpc_duration_seconds
rate: 1
duration: 10s

distribution:
  type: normal
  mean: 0.1
  stddev: 0.02

observations_per_tick: 100
seed: 42

labels:
  service: auth
  method: GetUser

encoder:
  type: prometheus_text

sink:
  type: stdout
```

```bash
sonda summary --scenario examples/summary.yaml
```

```text title="Output (first tick)"
rpc_duration_seconds{method="GetUser",quantile="0.5",service="auth"} 0.098 1775409507904
rpc_duration_seconds{method="GetUser",quantile="0.9",service="auth"} 0.128 1775409507904
rpc_duration_seconds{method="GetUser",quantile="0.95",service="auth"} 0.136 1775409507904
rpc_duration_seconds{method="GetUser",quantile="0.99",service="auth"} 0.148 1775409507904
rpc_duration_seconds_count{method="GetUser",service="auth"} 100 1775409507904
rpc_duration_seconds_sum{method="GetUser",service="auth"} 9.802 1775409507904
```

The p50 is near the configured mean (0.1s), and the spread matches the configured standard
deviation (0.02s). Count and sum increase cumulatively across ticks, but quantile values are
fresh per-tick snapshots.

!!! warning "Summaries are not aggregatable"
    You cannot meaningfully average p99 values across multiple instances. If you need
    per-service percentiles across a fleet of pods, use histograms instead -- you can sum the
    bucket counters first, then compute `histogram_quantile()` on the aggregated data.

---

## Distribution models

Both histogram and summary generators support three distribution models. Choose the one that
best matches the real-world metric you are simulating.

| Distribution | YAML | Typical use | Parameters |
|-------------|------|-------------|------------|
| Exponential | `type: exponential` | Request latency (long tail) | `rate` -- lambda; mean = 1/rate |
| Normal | `type: normal` | Symmetric around a center value | `mean`, `stddev` |
| Uniform | `type: uniform` | Even spread across a range | `min`, `max` |

=== "Exponential"

    ```yaml title="Exponential distribution (mean = 100ms)"
    distribution:
      type: exponential
      rate: 10.0
    ```

    Most observations cluster near zero with a long tail. This is the default choice for
    HTTP latency simulation.

=== "Normal"

    ```yaml title="Normal distribution (mean = 100ms, stddev = 20ms)"
    distribution:
      type: normal
      mean: 0.1
      stddev: 0.02
    ```

    Symmetric bell curve. Good for metrics with a known center and consistent spread,
    like RPC durations in a healthy service.

=== "Uniform"

    ```yaml title="Uniform distribution (50ms to 150ms)"
    distribution:
      type: uniform
      min: 0.05
      max: 0.15
    ```

    Every value in the range is equally likely. Useful for stress-testing bucket boundaries.

For full parameter reference, see [Generators -- histogram](../configuration/generators.md#histogram)
and [Generators -- summary](../configuration/generators.md#summary).
