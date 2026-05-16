# Testing Recording Rules

Recording rules pre-compute expressions and store results as new time series. If they compute
incorrectly, you won't notice until a dashboard goes blank or an alert never fires. Sonda lets
you push metrics with **known values** so you can verify the computed output matches your
expectation -- before deploying to production.

---

## The Approach

1. Push a metric with a **known, constant value** using Sonda.
2. Wait for at least **two evaluation intervals** (default: 1 minute each).
3. Query the recording rule output and verify the computed value matches.

This works because a constant input produces a predictable output: `sum()` of one instance
pushing `100.0` equals `100.0`.

---

## Testing a Sum Rule

The repository includes a ready-to-use recording rule test. The scenario pushes a constant
value of 100 for `http_requests_total`, and the companion Prometheus config computes a
sum per job.

- **Scenario**: `examples/recording-rule-test.yaml`
- **Rule config**: `examples/recording-rule-prometheus.yml`

### Step 1: Start VictoriaMetrics

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

Wait for the service to become healthy:

```bash
curl -sf http://localhost:8428/health && echo "VM is ready"
```

### Step 2: Push known values

The [constant generator](../configuration/generators.md#constant) emits the same value every
tick -- perfect for deterministic rule testing:

```bash
sonda run examples/recording-rule-test.yaml &
```

```yaml title="examples/recording-rule-test.yaml (key fields)"
version: 2

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
    The `&` runs Sonda in the background so you can continue in the same terminal.
    It will stop automatically after the configured `duration`.

### Step 3: Wait for evaluation

Recording rules evaluate on a fixed interval (default 1 minute in Prometheus, configurable in
vmalert). Wait at least two intervals:

```bash
sleep 120
```

### Step 4: Verify the computed value

Suppose your recording rule computes a sum per job:

```yaml title="recording-rule.yml"
groups:
  - name: test_rules
    rules:
      - record: job:http_requests_total:sum
        expr: sum(http_requests_total) by (job)
```

With one instance pushing `100.0`, query the result:

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

If the value matches, your recording rule is correct.

Now let's test something more complex: rate-based rules.

---

## Testing Rate-Based Rules

For `rate()` or `irate()` rules, you need a metric whose value increases over time. The
[sawtooth generator](../configuration/generators.md#sawtooth) ramps linearly and resets --
producing a predictable rate.

```bash
sonda run examples/rate-rule-input.yaml &
```

```yaml title="examples/rate-rule-input.yaml (key fields)"
version: 2

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

The sawtooth ramps from 0 to 1000 over 60 seconds, then resets. After sufficient data,
`rate(http_requests_total[1m])` should return approximately `16.67` (1000 / 60 seconds).

```bash
# After pushing for at least 2 minutes
curl -s "http://localhost:8428/api/v1/query?query=rate(http_requests_total[1m])" \
  | jq '.data.result[0].value[1]'
```

!!! warning "Wait for enough data"
    `rate()` needs at least two data points within the range window. With a 1-minute
    sawtooth period, wait at least 2 minutes before querying.

---

## Loading Rules Into Your Stack

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

---

## Tear Down

```bash
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

---

## Next Steps

**Testing alert rules?** Start with [Alert Testing](alert-testing.md).

**Validating a pipeline change?** See [Pipeline Validation](pipeline-validation.md).

**Full generator reference?** See [Generators](../configuration/generators.md).

**All sink options?** See [Sinks](../configuration/sinks.md).
