# Capacity Planning

You need to answer "how big does my infrastructure need to be?" -- but you can't test at scale
against production, and guessing leads to either over-provisioning (wasted money) or
under-provisioning (3 AM pages). Sonda lets you generate controlled, high-volume synthetic load
against your observability backend so you can measure real ingestion limits, find cardinality
ceilings, and size your infrastructure with data instead of hope.

**What you need:**

- Sonda installed ([Getting Started](../getting-started.md))
- Docker with Compose v2 for backend testing (`docker compose`)
- `curl` and `jq` for querying results

---

## Start the test backend

All the scenarios in this guide push metrics to VictoriaMetrics. Start the included Docker
Compose stack:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

Wait for VictoriaMetrics to become healthy:

```bash
curl -s http://localhost:8428/health
# OK
```

!!! tip "Clean slate between tests"
    Tear down and recreate the stack between test runs to avoid leftover data skewing results:
    `docker compose -f examples/docker-compose-victoriametrics.yml down -v && docker compose -f examples/docker-compose-victoriametrics.yml up -d`

---

## Test throughput limits

The first question in capacity planning: how many data points per second can your pipeline
ingest before it falls behind? Sonda's `rate` field controls events per second per scenario,
and multi-scenario files let you run several streams in parallel.

### Quick CLI test

Start simple. Push 1,000 metrics per second to stdout and verify Sonda keeps up:

```bash
sonda -q metrics --name throughput_test --rate 1000 --duration 5s \
  --value-mode sine --amplitude 50 --period-secs 60 --offset 100 \
  --label job=capacity_test --label env=load-test | wc -l
# ~5000 lines
```

If the line count roughly matches `rate * duration`, Sonda is keeping up on the generation side.
Now push that load into a real backend.

### Single-stream push to VictoriaMetrics

Push a single metric stream directly to VictoriaMetrics with CLI flags -- no YAML needed:

```bash
sonda -q metrics --name throughput_test --rate 1000 --duration 30s \
  --value-mode sine --amplitude 50 --period-secs 60 --offset 100 \
  --label job=capacity_test --label env=load-test \
  --sink http_push --endpoint http://localhost:8428/api/v1/import/prometheus \
  --content-type "text/plain"
```

Verify the data arrived:

```bash
curl -s "http://localhost:8428/api/v1/query?query=throughput_test" | jq '.data.result | length'
```

### Multi-stream throughput test

This scenario runs 3 metric streams at 1,000 events/sec each -- 3,000 data points per second
hitting VictoriaMetrics:

```bash
sonda run --scenario examples/capacity-throughput-test.yaml
```

```yaml title="examples/capacity-throughput-test.yaml (excerpt)"
scenarios:
  - signal_type: metrics
    name: throughput_http_requests
    rate: 1000
    duration: 60s
    generator:
      type: sine
      amplitude: 200.0
      period_secs: 30
      offset: 500.0
    jitter: 10.0
    jitter_seed: 1
    labels:
      job: capacity_test
      service: api-gateway
      env: load-test
    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"

  - signal_type: metrics
    name: throughput_cpu_usage
    rate: 1000
    duration: 60s
    # ... (sine generator, same sink)

  - signal_type: metrics
    name: throughput_memory_bytes
    rate: 1000
    duration: 60s
    # ... (sine generator, same sink)
```

The full file contains all three scenarios with different generator shapes. Each runs on its
own thread.

After the run completes, verify all three metrics arrived:

```bash
curl -s "http://localhost:8428/api/v1/query?query=throughput_http_requests" | jq '.data.result | length'
curl -s "http://localhost:8428/api/v1/query?query=throughput_cpu_usage" | jq '.data.result | length'
curl -s "http://localhost:8428/api/v1/query?query=throughput_memory_bytes" | jq '.data.result | length'
```

### Scale the test up

To find your saturation point, increase the rate until you see ingestion errors or data loss.
Override rates from the CLI without editing the YAML:

```bash
# 5,000 events/sec on a single stream
sonda -q metrics --name throughput_test --rate 5000 --duration 30s \
  --value-mode sine --amplitude 50 --period-secs 60 --offset 100 \
  --label job=capacity_test --label env=load-test \
  --output /tmp/throughput-5k.txt

wc -l < /tmp/throughput-5k.txt
# ~150000 lines
```

!!! warning "Disk and network are usually the bottleneck"
    Sonda can generate tens of thousands of events per second on modest hardware. The
    limit is almost always on the receiving end -- network bandwidth, disk I/O on the TSDB,
    or HTTP connection limits. Monitor your backend's resource usage during the test,
    not just Sonda's output.

---

## Find cardinality limits

High series cardinality (many unique label combinations) is the most common cause of TSDB
performance degradation. Pod churn in Kubernetes, ephemeral containers, and misconfigured
service discovery all create cardinality explosions. Sonda's `cardinality_spikes` feature
lets you simulate these explosions in a controlled way.

### How cardinality spikes work

A cardinality spike injects a dynamic label with many unique values during a recurring time
window. Outside the window, the label is absent (single series). During the window, each tick
gets a different label value, creating `cardinality` unique series.

```
Time ──────────────────────────────────────────────────────►

  1 series   │ 500 series  │  1 series   │ 500 series  │
  (baseline) │ (spike)     │  (baseline) │ (spike)     │
─────────────┼─────────────┼─────────────┼─────────────┤
  0s         20s          60s           80s          120s
             ◄── for:20s ──►             ◄── for:20s ──►
             ◄──────── every:60s ────────►
```

### Run a cardinality stress test

This scenario pushes two metrics with overlapping cardinality spikes -- 500 unique pod names
and 200 unique endpoints:

```bash
sonda run --scenario examples/capacity-cardinality-stress.yaml
```

```yaml title="examples/capacity-cardinality-stress.yaml (excerpt)"
scenarios:
  - signal_type: metrics
    name: http_requests_total
    rate: 50
    duration: 180s
    generator:
      type: constant
      value: 1.0
    labels:
      job: capacity_test
      service: api-gateway
      env: load-test
    cardinality_spikes:
      - label: pod_name
        every: 60s
        for: 20s
        cardinality: 500
        strategy: counter
        prefix: "pod-"
    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"

  - signal_type: metrics
    name: http_request_duration_seconds
    rate: 50
    duration: 180s
    # ... (sine generator, 200 endpoint cardinality spike)
```

After the run, measure how many unique series were created:

```bash
# Count unique series for http_requests_total
curl -s "http://localhost:8428/api/v1/series?match[]=http_requests_total" \
  | jq '.data | length'
# Expected: ~501 (1 baseline + 500 from spike)
```

### Scale cardinality progressively

Run multiple tests with increasing cardinality to find where your TSDB starts struggling.
Track query latency and memory usage at each level:

| Test | `cardinality` | Expected unique series | What to watch |
|------|---------------|----------------------|---------------|
| Baseline | 100 | ~101 | Response time for `count()` queries |
| Medium | 1,000 | ~1,001 | TSDB memory usage, compaction time |
| High | 5,000 | ~5,001 | Query timeouts, ingestion backpressure |
| Extreme | 10,000 | ~10,001 | OOM events, disk I/O saturation |

You can override the cardinality from the CLI for a quick single-metric test:

```bash
sonda -q metrics --name cardinality_test --rate 50 --duration 60s \
  --value 1 \
  --label job=capacity_test --label env=load-test \
  --spike-label pod_name --spike-every 60s --spike-for 30s \
  --spike-cardinality 5000 --spike-prefix "pod-"
```

!!! info "Cardinality vs. throughput"
    Cardinality and throughput stress different parts of your TSDB. High throughput stresses
    the write path (WAL, network, CPU). High cardinality stresses the index (memory,
    compaction, query planning). Test both dimensions independently, then together.

---

## Simulate traffic spikes with bursts

Real traffic doesn't arrive at a steady rate. Black Friday, a viral post, or a cascading
retry storm can spike your ingest rate by 10x in seconds. Sonda's `bursts` feature lets you
simulate these spikes to test whether your pipeline handles sudden load without dropping data.

### How bursts work

A burst window multiplies the base event rate for a short duration on a recurring schedule:

```
Rate ──►
         ┌──────┐                         ┌──────┐
  5000/s │      │                         │      │
         │      │                         │      │
         │      │                         │      │
   500/s ├──────┴─────────────────────────┴──────┴──────
         0s    5s                        30s   35s
               ◄─ for:5s ─►              ◄─ for:5s ─►
               ◄──────── every:30s ───────►
```

### Run a burst test

This scenario runs at 500 events/sec with 10x bursts (5,000/sec) every 30 seconds:

```bash
sonda run --scenario examples/capacity-burst-test.yaml
```

```yaml title="examples/capacity-burst-test.yaml (excerpt)"
scenarios:
  - signal_type: metrics
    name: burst_http_requests
    rate: 500
    duration: 120s
    generator:
      type: sine
      amplitude: 100.0
      period_secs: 30
      offset: 300.0
    jitter: 5.0
    jitter_seed: 10
    labels:
      job: capacity_test
      service: api-gateway
      env: load-test
    bursts:
      every: 30s
      for: 5s
      multiplier: 10.0
    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"

  - signal_type: metrics
    name: burst_queue_depth
    rate: 100
    duration: 120s
    # ... (sine generator, same burst config)
```

After the run, verify that data arrived during both steady-state and burst windows:

```bash
curl -s "http://localhost:8428/api/v1/query_range?\
query=burst_http_requests&start=$(date -v-3M +%s)&end=$(date +%s)&step=5s" \
  | jq '.data.result[0].values | length'
```

??? tip "Combining bursts with cardinality spikes"
    For a worst-case scenario, combine both features in a single metric. This simulates a
    Kubernetes deployment rollout where pod churn (cardinality spike) and traffic ramp-up
    (burst) happen simultaneously:

    ```yaml
    name: worst_case_test
    rate: 200
    duration: 120s
    generator:
      type: constant
      value: 1.0
    labels:
      job: capacity_test
      env: load-test
    bursts:
      every: 30s
      for: 10s
      multiplier: 5.0
    cardinality_spikes:
      - label: pod_name
        every: 30s
        for: 10s
        cardinality: 1000
        strategy: counter
        prefix: "pod-"
    encoder:
      type: prometheus_text
    sink:
      type: stdout
    ```

    During the 10-second overlap window, Sonda emits 1,000 events/sec (200 * 5) across
    1,000 unique series. That's a sharp, realistic stress test.

---

## Measure backend performance

Generating load is only half the story. You need to measure how your backend responds
to that load. Here are the key metrics to capture during each test run.

### VictoriaMetrics

Query these after your test completes:

```bash
# Ingestion rate (rows/sec received)
curl -s "http://localhost:8428/api/v1/query?\
query=rate(vm_rows_inserted_total[1m])" | jq '.data.result[0].value[1]'

# Active time series (cardinality)
curl -s "http://localhost:8428/api/v1/query?\
query=vm_cache_entries{type=\"storage/metricName\"}" | jq '.data.result[0].value[1]'

# Memory usage
curl -s "http://localhost:8428/api/v1/query?\
query=process_resident_memory_bytes" | jq '.data.result[0].value[1]'
```

### Prometheus

If you're testing against Prometheus instead, use these queries:

```bash
# Head series count (active cardinality)
curl -s "http://localhost:9090/api/v1/query?\
query=prometheus_tsdb_head_series" | jq '.data.result[0].value[1]'

# Ingestion rate (samples/sec)
curl -s "http://localhost:9090/api/v1/query?\
query=rate(prometheus_tsdb_head_samples_appended_total[1m])" | jq '.data.result[0].value[1]'

# WAL corruption or truncation errors
curl -s "http://localhost:9090/api/v1/query?\
query=prometheus_tsdb_wal_truncations_failed_total" | jq '.data.result[0].value[1]'
```

### What to record

Capture these metrics for each test run to build a sizing model:

| Metric | Where to get it | Why it matters |
|--------|----------------|----------------|
| Ingestion rate (samples/sec) | TSDB internal metrics | Confirms actual vs. intended write rate |
| Active time series | TSDB head series count | Tracks cardinality growth |
| Memory usage | `process_resident_memory_bytes` | Cardinality scales linearly with RAM |
| Disk growth rate | `du -sh` on TSDB data dir | Storage sizing for retention period |
| Query latency at load | Time a `count()` query | Detects index bloat from high cardinality |

---

## Calculate infrastructure sizing

With test data from the previous sections, you can build a practical sizing model. The formula
is straightforward:

### Storage estimate

```
daily_storage = (samples_per_second) * 86400 * (bytes_per_sample)
monthly_storage = daily_storage * 30 * (1 - compression_ratio)
```

Typical `bytes_per_sample` values (compressed, on disk):

| Backend | Bytes per sample | Notes |
|---------|-----------------|-------|
| VictoriaMetrics | 0.4--1.0 | Aggressive compression; lower end for uniform data |
| Prometheus | 1.0--2.0 | TSDB blocks with index overhead |
| Thanos / Mimir | 1.5--3.0 | Includes object storage overhead |

**Example calculation:** You measured a sustained ingestion rate of 3,000 samples/sec during
the throughput test.

```
daily_storage  = 3,000 * 86,400 * 1.0 bytes = ~259 MB/day  (VictoriaMetrics)
monthly_storage = 259 * 30 = ~7.8 GB/month
```

With 90-day retention: **~23 GB of disk**.

### Memory estimate

Cardinality is the primary driver of memory usage. From your cardinality stress tests, note
the memory delta between the baseline and each spike level:

```bash
# Measure memory before and after a cardinality test
BEFORE=$(curl -s "http://localhost:8428/api/v1/query?query=process_resident_memory_bytes" \
  | jq -r '.data.result[0].value[1]')

# Run the cardinality stress test
sonda run --scenario examples/capacity-cardinality-stress.yaml

AFTER=$(curl -s "http://localhost:8428/api/v1/query?query=process_resident_memory_bytes" \
  | jq -r '.data.result[0].value[1]')

echo "Memory delta: $(( (AFTER - BEFORE) / 1024 / 1024 )) MB"
```

A rough rule of thumb: plan for **1--4 KB of RAM per active time series**, depending on your
backend and label complexity.

### Sizing checklist

Run through this checklist for each environment you're sizing:

| Dimension | Test scenario | Key measurement |
|-----------|--------------|-----------------|
| Peak throughput | `capacity-throughput-test.yaml` at increasing rates | Max sustained samples/sec before errors |
| Cardinality ceiling | `capacity-cardinality-stress.yaml` at increasing cardinality | Max unique series before memory/query degradation |
| Burst headroom | `capacity-burst-test.yaml` with high multipliers | Whether data survives 10x spikes without loss |
| Disk budget | Any scenario at target rate for 10+ minutes | Measured bytes/sample for storage projection |

---

## Quick reference

| Task | Command |
|------|---------|
| Start backend | `docker compose -f examples/docker-compose-victoriametrics.yml up -d` |
| Validate throughput scenario | `sonda --dry-run run --scenario examples/capacity-throughput-test.yaml` |
| Run throughput test | `sonda run --scenario examples/capacity-throughput-test.yaml` |
| Run cardinality stress test | `sonda run --scenario examples/capacity-cardinality-stress.yaml` |
| Run burst test | `sonda run --scenario examples/capacity-burst-test.yaml` |
| Count series in VM | `curl -s "http://localhost:8428/api/v1/series?match[]=<metric>" \| jq '.data \| length'` |
| Tear down | `docker compose -f examples/docker-compose-victoriametrics.yml down -v` |

**Related pages:**

- [Scenario Files](../configuration/scenario-file.md) -- cardinality_spikes, bursts, and rate reference
- [Sinks](../configuration/sinks.md) -- http_push configuration for backend targets
- [E2E Testing](e2e-testing.md) -- full Docker Compose test suite
- [Pipeline Validation](pipeline-validation.md) -- quick smoke tests without Docker
- [Example Scenarios](examples.md) -- all example scenario files
