---
title: Testing recording rules
description: Verify that Prometheus recording rules compute the expected values by pushing known inputs and comparing the output.
---

# Testing recording rules

This page shows how to verify Prometheus recording rules with Sonda. You push a metric with known values, wait for the rule to evaluate, and compare the computed result against the expected one.

Recording rules pre-compute PromQL expressions and store the results as new series. A wrong rule fails silently until a dashboard goes blank or an alert never fires.

## How the test works

The test has three steps:

1. Push a metric with a known, constant value.
2. Wait for two evaluation intervals. The default Prometheus interval is one minute.
3. Query the recording rule output and compare it against the expected value.

A constant input produces a predictable output. For example, `sum()` over one series at `100.0` returns `100.0`.

## Test a sum rule

The repository includes a ready-to-run example. The scenario pushes the constant `100.0` for `http_requests_total`. The companion Prometheus config computes a sum per `job` label.

- Scenario: `examples/recording-rule-test.yaml`
- Rule config: `examples/recording-rule-prometheus.yml`

### Step 1: Start VictoriaMetrics

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

Wait for the service to report healthy:

```bash
curl -sf http://localhost:8428/health && echo "VM is ready"
```

### Step 2: Push known values

The [constant generator](../build/generators.md#constant) emits the same value on every tick. This makes it ideal for deterministic rule testing.

```bash
sonda run examples/recording-rule-test.yaml &
```

```yaml title="examples/recording-rule-test.yaml (key fields)"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 120s
  encoder:
    type: prometheus_text
  sink:
    type: http_push
    url: "http://localhost:8428/api/v1/import/prometheus"
    content_type: "text/plain"

scenarios:
  - signal_type: metrics
    name: http_requests_total
    generator:
      type: constant
      value: 100.0
    labels:
      method: GET
      status: "200"
      job: api
```

!!! tip "Background execution"
    The `&` runs Sonda in the background so you can continue in the same terminal. Sonda stops automatically after the configured `duration`.

### Step 3: Wait for evaluation

Recording rules evaluate on a fixed interval. The default is one minute in Prometheus, configurable in vmalert. Wait at least two intervals:

```bash
sleep 120
```

### Step 4: Verify the computed value

This recording rule computes a sum per `job`:

```yaml title="recording-rule.yml"
groups:
  - name: test_rules
    rules:
      - record: job:http_requests_total:sum
        expr: sum(http_requests_total) by (job)
```

With one series at `100.0`, query the result:

```bash
curl -s "http://localhost:8428/api/v1/query?query=job:http_requests_total:sum" \
  | jq '.data.result'
```

Expected output:

```json
[
  {
    "metric": {"job": "api"},
    "value": [1700000000, "100"]
  }
]
```

If the value matches, the recording rule is correct.

## Test a rate-based rule

For `rate()` or `irate()` rules, you need a counter that increases over time. The [sawtooth generator](../build/generators.md#sawtooth) increases linearly and then resets, which produces a predictable rate.

```bash
sonda run examples/rate-rule-input.yaml &
```

```yaml title="examples/rate-rule-input.yaml (key fields)"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 300s
  encoder:
    type: prometheus_text
  sink:
    type: http_push
    url: "http://localhost:8428/api/v1/import/prometheus"
    content_type: "text/plain"

scenarios:
  - signal_type: metrics
    name: http_requests_total
    generator:
      type: sawtooth
      min: 0.0
      max: 1000.0
      period_secs: 60
    labels:
      instance: api-01
      job: web
```

The sawtooth rises from 0 to 1000 over 60 seconds, then resets. After enough data, `rate(http_requests_total[1m])` returns about `16.67` (1000 / 60 seconds).

```bash
# After pushing for at least 2 minutes
curl -s "http://localhost:8428/api/v1/query?query=rate(http_requests_total[1m])" \
  | jq '.data.result[0].value[1]'
```

!!! warning "Wait for enough data"
    `rate()` needs at least two data points inside the range window. With a 60-second sawtooth period, wait at least 2 minutes before you query.

## Load rules into your stack

=== "Prometheus"

    Add the rule file to `rule_files` in `prometheus.yml`:

    ```yaml title="prometheus.yml (snippet)"
    rule_files:
      - recording-rule.yml
    ```

=== "vmalert"

    Pass the rule file as a flag:

    ```bash
    vmalert -rule=recording-rule.yml \
      -datasource.url=http://localhost:8428 \
      -remoteWrite.url=http://localhost:8428
    ```

## Tear down

```bash
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

## Next steps

- [Alert Testing](alert-testing.md) — test alert rules with the same backend.
- [Pipeline Validation](end-to-end-pipelines.md) — validate a full pipeline change.
- [Generators](../build/generators.md) — full reference for `constant`, `sawtooth`, and others.
- [Sinks](../build/sinks.md) — full reference for `http_push` and other destinations.
