---
title: Capacity planning
description: Use Sonda to generate controlled high-volume load and find real ingestion and cardinality limits in your observability backend.
---

# Capacity planning

This page shows how to size an observability backend with measured data instead of guesses. You generate controlled synthetic load with Sonda, push it at the backend, and read the actual ingestion, cardinality, and resource limits.

The page covers four tests:

- Throughput limits — how many samples per second the pipeline accepts.
- Cardinality limits — how many unique series before the index degrades.
- Traffic spikes — whether the pipeline survives sudden 10x bursts.
- Backend measurement — which TSDB metrics to record on every run.

**What you need:**

- Sonda installed ([Getting Started](../get-started/quickstart.md)).
- Docker with Compose v2 (`docker compose`).
- `curl` and `jq` for querying results.

## Start the test backend

Every scenario on this page pushes metrics to VictoriaMetrics. Start the included Docker Compose stack:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml up -d
```

Wait for VictoriaMetrics to report healthy:

```bash
curl -s http://localhost:8428/health
# OK
```

!!! tip "Clean slate between tests"
    Recreate the stack between test runs so leftover data does not skew the results.

    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml down -v
    docker compose -f examples/docker-compose-victoriametrics.yml up -d
    ```

## Test throughput limits

The first question in capacity planning: how many samples per second can the pipeline accept before it falls behind? The `rate` field controls events per second per scenario. Multi-scenario files run several streams in parallel.

### Smoke check before scaling up

Before you push thousands of events per second at a backend, confirm Sonda produces the target rate on your hardware. A 5-second stdout run is enough:

```bash
sonda -q run examples/capacity-throughput-test.yaml --rate 1000 --duration 5s | wc -l
# ~5000 lines
```

If the line count roughly matches `rate * duration`, generation keeps up. See [Pipeline Validation](end-to-end-pipelines.md) for the full smoke test pattern with exit-code checks and CI snippets. The rest of this section assumes the smoke check passed.

### Multi-stream throughput test

This scenario runs 3 metric streams at 1,000 events per second each — 3,000 samples per second hitting VictoriaMetrics:

```bash
sonda run examples/capacity-throughput-test.yaml
```

```yaml title="examples/capacity-throughput-test.yaml (excerpt)"
version: 2
kind: runnable

defaults:
  rate: 1000
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: http_push
    url: "http://localhost:8428/api/v1/import/prometheus"
    content_type: "text/plain"
  labels:
    job: capacity_test
    service: api-gateway
    env: load-test

scenarios:
  - signal_type: metrics
    name: throughput_http_requests
    generator:
      type: sine
      amplitude: 200.0
      period_secs: 30
      offset: 500.0
    jitter: 10.0
    jitter_seed: 1

  - signal_type: metrics
    name: throughput_cpu_usage
    # ... (sine generator, inherits rate/duration/sink from defaults)

  - signal_type: metrics
    name: throughput_memory_bytes
    # ... (sine generator, inherits rate/duration/sink from defaults)
```

The full file contains all three scenarios with different generator patterns. Each one runs on its own thread.

After the run completes, verify all three metrics arrived:

```bash
curl -s "http://localhost:8428/api/v1/query?query=throughput_http_requests" | jq '.data.result | length'
curl -s "http://localhost:8428/api/v1/query?query=throughput_cpu_usage" | jq '.data.result | length'
curl -s "http://localhost:8428/api/v1/query?query=throughput_memory_bytes" | jq '.data.result | length'
```

### Increase the test rate

To find the saturation point, raise the rate until you see ingestion errors or data loss. Override the rate from the CLI without editing the YAML:

```bash
# 5,000 events/sec on a single stream — uses the throughput-test scenario above
sonda -q run examples/capacity-throughput-test.yaml \
  --rate 5000 --duration 30s \
  -o /tmp/throughput-5k.txt

wc -l < /tmp/throughput-5k.txt
# ~150000 lines
```

!!! warning "Disk and network are usually the limit"
    Sonda produces tens of thousands of events per second on modest hardware. The limit is almost always on the receiving end — network bandwidth, disk I/O on the TSDB, or HTTP connection limits. Monitor the backend's resource usage during the test, not only Sonda's output.

## Find cardinality limits

High series cardinality (many unique label combinations) is the most common cause of TSDB performance degradation. Pod churn in Kubernetes, ephemeral containers, and misconfigured service discovery all produce cardinality explosions. The `cardinality_spikes` field simulates them in a controlled way.

### How cardinality spikes work

A cardinality spike injects a dynamic label with many unique values during a recurring time window. Outside the window, the label is absent (one series). During the window, each tick uses a different label value, which produces `cardinality` unique series.

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

This scenario pushes two metrics with overlapping cardinality spikes — 500 unique pod names and 200 unique endpoints:

```bash
sonda run examples/capacity-cardinality-stress.yaml
```

```yaml title="examples/capacity-cardinality-stress.yaml (excerpt)"
version: 2
kind: runnable

defaults:
  rate: 50
  duration: 180s
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

  - signal_type: metrics
    name: http_request_duration_seconds
    # ... (sine generator, 200 endpoint cardinality spike)
```

After the run, measure how many unique series were created:

```bash
# Count unique series for http_requests_total
curl -s "http://localhost:8428/api/v1/series?match[]=http_requests_total" \
  | jq '.data | length'
# Expected: ~501 (1 baseline + 500 from spike)
```

### Increase cardinality progressively

Run multiple tests with rising cardinality to find where the TSDB starts to struggle. Track query latency and memory usage at each level:

| Test | `cardinality` | Expected unique series | What to watch |
|------|---------------|----------------------|---------------|
| Baseline | 100 | ~101 | Response time for `count()` queries |
| Medium | 1,000 | ~1,001 | TSDB memory usage, compaction time |
| High | 5,000 | ~5,001 | Query timeouts, ingestion backpressure |
| Extreme | 10,000 | ~10,001 | OOM events, disk I/O saturation |

The cardinality and spike window are in the scenario YAML. For a quick single-metric cardinality test, follow these steps:

- Generate a minimal scenario with `sonda new --template`.
- Replace the `generator:` block with `type: constant`.
- Add a `cardinality_spikes:` clause.
- Run it:

```yaml title="cardinality-quick.yaml"
version: 2
kind: runnable
defaults:
  rate: 50
  duration: 60s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    job: capacity_test
    env: load-test
scenarios:
  - id: cardinality_test
    signal_type: metrics
    name: cardinality_test
    generator:
      type: constant
      value: 1.0
    cardinality_spikes:
      - label: pod_name
        every: 60s
        for: 30s
        cardinality: 5000
        strategy: counter
        prefix: "pod-"
```

```bash
sonda -q run cardinality-quick.yaml
```

!!! info "Cardinality versus throughput"
    Cardinality and throughput test different parts of the TSDB. High throughput tests the write path (WAL, network, CPU). High cardinality tests the index (memory, compaction, query planning). Test both dimensions on their own first, then together.

??? tip "Steady-state simulation with dynamic labels"
    Cardinality spikes are time-windowed and are ideal for testing label explosions that come and go. If you want **always-on** cardinality (for example, a stable set of 50 hosts), use `dynamic_labels` instead. The label is present on every event, which produces a constant number of series for the full duration. See [Dynamic labels](../reference/scenario-fields.md#dynamic-labels) in the scenario field reference.

## Simulate traffic spikes with bursts

Real traffic does not arrive at a steady rate. Black Friday, a viral post, or a retry storm can multiply the ingest rate by 10x in seconds. The `bursts` field simulates these spikes so you can test whether the pipeline handles sudden load without dropping data.

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

This scenario runs at 500 events per second with 10x bursts (5,000 per second) every 30 seconds:

```bash
sonda run examples/capacity-burst-test.yaml
```

```yaml title="examples/capacity-burst-test.yaml (excerpt)"
version: 2
kind: runnable

defaults:
  duration: 120s
  encoder:
    type: prometheus_text
  sink:
    type: http_push
    url: "http://localhost:8428/api/v1/import/prometheus"
    content_type: "text/plain"

scenarios:
  - signal_type: metrics
    name: burst_http_requests
    rate: 500
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

  - signal_type: metrics
    name: burst_queue_depth
    rate: 100
    # ... (sine generator, same burst config)
```

After the run, verify that data arrived during both the steady-state and burst windows:

```bash
curl -s "http://localhost:8428/api/v1/query_range?\
query=burst_http_requests&start=$(date -v-3M +%s)&end=$(date +%s)&step=5s" \
  | jq '.data.result[0].values | length'
```

??? tip "Combining bursts with cardinality spikes"
    For a worst-case test, combine both fields in a single metric. This simulates a Kubernetes deployment rollout where pod churn (cardinality spike) and traffic increase (burst) happen at the same time:

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

    During the 10-second overlap window, Sonda emits 1,000 events per second (200 * 5) across 1,000 unique series. That is a sharp, realistic test.

## Measure backend performance

Generating load is only half the work. You also need to measure how the backend responds. These are the key metrics to capture during every test run.

### VictoriaMetrics

Run these queries after the test completes:

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

If you test against Prometheus instead, use these queries:

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

Capture these metrics for every test run. They build the sizing model:

| Metric | Where to get it | Why it matters |
|--------|----------------|----------------|
| Ingestion rate (samples/sec) | TSDB internal metrics | Confirms actual versus intended write rate |
| Active time series | TSDB head series count | Tracks cardinality growth |
| Memory usage | `process_resident_memory_bytes` | Cardinality scales linearly with RAM |
| Disk growth rate | `du -sh` on the TSDB data directory | Storage size for the retention period |
| Query latency at load | Time a `count()` query | Detects index bloat from high cardinality |

## Calculate infrastructure sizing

With test data from the previous sections, you can build a sizing model. The formula is direct:

### Storage estimate

```
daily_storage = (samples_per_second) * 86400 * (bytes_per_sample)
monthly_storage = daily_storage * 30 * (1 - compression_ratio)
```

Typical `bytes_per_sample` values (compressed, on disk):

| Backend | Bytes per sample | Notes |
|---------|-----------------|-------|
| VictoriaMetrics | 0.4–1.0 | Aggressive compression; lower end for uniform data |
| Prometheus | 1.0–2.0 | TSDB blocks with index overhead |
| Thanos / Mimir | 1.5–3.0 | Includes object storage overhead |

**Example calculation:** You measured a sustained ingestion rate of 3,000 samples per second during the throughput test.

```
daily_storage  = 3,000 * 86,400 * 1.0 bytes = ~259 MB/day  (VictoriaMetrics)
monthly_storage = 259 * 30 = ~7.8 GB/month
```

With 90-day retention: **about 23 GB of disk**.

### Memory estimate

Cardinality is the main driver of memory usage. From your cardinality stress tests, note the memory delta between the baseline and each spike level:

```bash
# Measure memory before and after a cardinality test
BEFORE=$(curl -s "http://localhost:8428/api/v1/query?query=process_resident_memory_bytes" \
  | jq -r '.data.result[0].value[1]')

# Run the cardinality stress test
sonda run examples/capacity-cardinality-stress.yaml

AFTER=$(curl -s "http://localhost:8428/api/v1/query?query=process_resident_memory_bytes" \
  | jq -r '.data.result[0].value[1]')

echo "Memory delta: $(( (AFTER - BEFORE) / 1024 / 1024 )) MB"
```

A rough rule: plan for **1 to 4 KB of RAM per active time series**. The exact value depends on the backend and label complexity.

### Sizing checklist

Run through this checklist for each environment you size:

| Dimension | Test scenario | Key measurement |
|-----------|--------------|-----------------|
| Peak throughput | `capacity-throughput-test.yaml` at increasing rates | Maximum sustained samples/sec before errors |
| Cardinality ceiling | `capacity-cardinality-stress.yaml` at increasing cardinality | Maximum unique series before memory or query degradation |
| Burst headroom | `capacity-burst-test.yaml` with high multipliers | Whether data survives 10x spikes without loss |
| Disk budget | Any scenario at the target rate for 10+ minutes | Measured bytes per sample for the storage projection |

## Performance baselines

These tables show measured performance from benchmark runs. The numbers describe Sonda-side resource usage — generation rates, memory, and encoder size — not backend capacity.

!!! info "Measured on Apple Silicon"
    Benchmarks ran on macOS with Apple Silicon, Sonda v0.8.0 release build, file sink, one thread per scenario. Your numbers vary by hardware and sink type. The conclusion holds: Sonda produces millions of events per second on a single core with a flat ~7.5 MB memory footprint. Use the [throughput](#test-throughput-limits) and [cardinality](#find-cardinality-limits) tests above to measure your own environment.

### Generator throughput

One CPU core, `prometheus_text` encoder, file sink:

| Generator | Events/sec | Notes |
|-----------|-----------|-------|
| `constant` | ~7,200,000 | Minimal computation per event |
| `csv_replay` | ~6,200,000 | Values pre-loaded into memory, cycled through |
| `sawtooth` | ~5,800,000 | Simple linear calculation |
| `uniform` | ~5,700,000 | RNG evaluation per event |
| `sine` | ~5,400,000 | Trigonometric calculation per event |
| `histogram` | ~257,000 ticks/sec | Each tick emits 14 lines (12 buckets + count + sum) |
| `summary` | ~349,000 ticks/sec | Each tick emits 6 lines (4 quantiles + count + sum) |

!!! tip "Histogram and summary throughput"
    These generators are measured in ticks per second because each tick produces multiple output lines. In raw line throughput, histogram produces about 3.6M lines per second and summary about 2.1M lines per second.

### Encoder bytes per event

Measured with 3 labels (`job`, `instance`, `env`), a typical metric name, and a float value:

| Encoder | Bytes/event | Notes |
|---------|------------|-------|
| `prometheus_text` | ~76 | Human-readable, uncompressed |
| `influx_lp` | ~81 | InfluxDB line protocol |
| `otlp` | ~95 | Protobuf, written to file (no gRPC framing) |
| `remote_write` | ~97 | Snappy-compressed protobuf |
| `json_lines` | ~136 | JSON overhead from keys and quoting |

!!! note
    `remote_write` and `otlp` bytes are measured via the file sink. The over-the-wire size with network sinks may differ because of batching and compression. The size also varies with metric name length, number of labels, and value precision.

### Memory footprint

Sonda does not store per-series state. It generates events on the fly. Memory is essentially flat regardless of cardinality:

| Scenario | RSS |
|----------|-----|
| Single metric, any cardinality (100 to 50,000 series) | ~7.5 MB |
| 5 concurrent scenarios, 25,000 total events/sec | ~7.5 MB |

Memory is driven by the number of concurrent scenarios and sink buffering, not by series cardinality.

### Kubernetes resource recommendations

Sonda's low resource footprint means you can run it almost anywhere. Suggested resource requests for Sonda pods:

| Profile | Event rate | CPU request | Memory request | Use case |
|---------|-----------|-------------|----------------|----------|
| Small | up to 1,000/sec | 50m | 32 Mi | Development, CI pipeline checks |
| Medium | 1,000–100,000/sec | 100m | 64 Mi | Integration testing, alert validation |
| Large | 100,000–1,000,000+/sec | 250m | 128 Mi | Load testing, capacity planning |

Set resource limits to 2x the requests to absorb bursts.

## Quick reference

| Task | Command |
|------|---------|
| Start backend | `docker compose -f examples/docker-compose-victoriametrics.yml up -d` |
| Validate throughput scenario | `sonda --dry-run run examples/capacity-throughput-test.yaml` |
| Run throughput test | `sonda run examples/capacity-throughput-test.yaml` |
| Run cardinality stress test | `sonda run examples/capacity-cardinality-stress.yaml` |
| Run burst test | `sonda run examples/capacity-burst-test.yaml` |
| Count series in VM | `curl -s "http://localhost:8428/api/v1/series?match[]=<metric>" \| jq '.data \| length'` |
| Tear down | `docker compose -f examples/docker-compose-victoriametrics.yml down -v` |

## Related pages

- [Scenario Fields](../reference/scenario-fields.md) — reference for `cardinality_spikes`, `bursts`, and `rate`.
- [Sinks](../build/sinks.md) — `http_push` configuration for backend targets.
- [E2E Testing](end-to-end-pipelines.md) — full Docker Compose test suite.
- [Pipeline Validation](end-to-end-pipelines.md) — quick smoke tests without Docker.
- [Example Scenarios](examples.md) — all example scenario files.
- [Troubleshooting](../reference/troubleshooting.md) — common issues and how to fix them.
