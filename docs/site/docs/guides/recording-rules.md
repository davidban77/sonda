# Testing Recording Rules

You have Prometheus recording rules that pre-compute expressions and store results as new time
series. You need to verify they compute correctly against known input before deploying to
production.

## The Approach

1. Push a metric with a **known, constant value** using Sonda.
2. Wait for at least **two evaluation intervals** (default: 1 minute each).
3. Query the recording rule output and verify the computed value matches your expectation.

This works because a constant input produces a predictable output: `sum()` of one instance
pushing `42.0` equals `42.0`.

## Working Example

The repository includes a ready-to-use recording rule test:

- `examples/recording-rule-test.yaml` -- Sonda scenario pushing a constant value of 100 for
  `http_requests_total`.
- `examples/recording-rule-prometheus.yml` -- Prometheus recording rule config computing
  `job:http_requests_total:rate5m`.

### Step 1: Start VictoriaMetrics

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

Wait for the service to become healthy:

```bash
curl -sf http://localhost:8428/health && echo "VM is ready"
```

### Step 2: Push Known Values

Use the constant [generator](../configuration/generators.md#constant) to push a known value:

```yaml title="recording-rule-input.yaml"
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

```bash
sonda metrics --scenario recording-rule-input.yaml &
```

### Step 3: Wait for Evaluation

Recording rules evaluate on a fixed interval (default 1 minute in Prometheus, configurable in
vmalert). Wait at least two intervals:

```bash
sleep 120
```

### Step 4: Verify the Computed Value

Suppose your recording rule computes a sum per job:

```yaml title="recording-rule.yml"
groups:
  - name: test_rules
    rules:
      - record: job:http_requests_total:sum
        expr: sum(http_requests_total) by (job)
```

With one instance pushing `42.0`, query the result:

```bash
curl -s "http://localhost:8428/api/v1/query?query=job:http_requests_total:sum" \
  | jq '.data.result'
```

Expected output:

```json
[
  {
    "metric": {"job": "web"},
    "value": [1700000000, "42"]
  }
]
```

If the value matches, your recording rule is correct.

## Testing Rate-Based Rules

For `rate()` or `irate()` rules, you need a metric whose value increases over time. Use the
[sawtooth generator](../configuration/generators.md#sawtooth), which ramps linearly and resets:

```yaml title="rate-rule-input.yaml"
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

The sawtooth ramps from 0 to 1000 over 60 seconds, then resets. After sufficient data,
`rate(http_requests_total[1m])` should return approximately `16.67` (1000 / 60 seconds).

```bash
# After pushing for at least 2 minutes
curl -s "http://localhost:8428/api/v1/query?query=rate(http_requests_total[1m])" \
  | jq '.data.result[0].value[1]'
```

## Loading Rules Into Your Stack

You can load recording rules into Prometheus or vmalert:

**Prometheus**: Add the rule file to `rule_files` in `prometheus.yml`:

```yaml title="prometheus.yml (snippet)"
rule_files:
  - recording-rule.yml
```

**vmalert**: Pass the rule file as a flag:

```bash
vmalert -rule=recording-rule.yml \
  -datasource.url=http://localhost:8428 \
  -remoteWrite.url=http://localhost:8428
```

## Tear Down

```bash
docker compose -f examples/docker-compose-victoriametrics.yml down -v
```

## What Next

- [Pipeline Validation](pipeline-validation.md) -- verify your ingest pipeline handles all
  encoders correctly.
- [Generators reference](../configuration/generators.md) -- full list of available generators.
- [Sinks reference](../configuration/sinks.md) -- all available sink destinations.
