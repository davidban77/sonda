# Alert Testing

3 a.m. The pager goes off for `HighRequestLatency`. By the time you log in, latency
is back below threshold and the alert has cleared. You spend an hour reading dashboards
and find nothing -- the spike was real, but it lasted 90 seconds and your `for: 5m`
clause silently swallowed it. The alert is doing exactly what you told it to. You just
told it the wrong thing.

That whole class of problem -- `for:` durations that swallow real spikes, gap-fill rules
that fire during scrape outages, compound `A AND B` rules where the two signals never
overlap -- only shows up in production because nothing else generates the right metric
shape. Sonda does. You write the alert, run a scenario that crosses the threshold for
exactly the duration you care about, and watch whether the alert fires.

This page is the entry point. Five focused sub-pages cover the patterns; the table
below maps each common alert shape to the right one.

## Pick your pattern

| You want to test... | Go to | Generator |
|---------------------|-------|-----------|
| A simple `> threshold` rule | [Threshold and `for:` duration](alert-testing-thresholds.md) | `sine` |
| A short `for:` clause (≤ 30s) | [Threshold and `for:` duration](alert-testing-thresholds.md) | `sequence` |
| A long `for:` clause (minutes) | [Threshold and `for:` duration](alert-testing-thresholds.md) | `constant` |
| Resolution / flapping behavior | [Resolution and recovery](alert-testing-resolution.md) | any + `gaps` |
| Compound `A AND B` rules | [Compound and correlated alerts](alert-testing-correlation.md) | multi-scenario |
| Cardinality guardrails | [Cardinality explosion alerts](alert-testing-cardinality.md) | any + `cardinality_spikes` |
| Replaying a known incident | [Replaying recorded incidents](alert-testing-replay.md) | `sequence` or `csv_replay` |

The pages are written as a tour and link forward to one another, but each one stands
on its own -- jump straight to the one that matches the rule you are testing.

## The tour

1. [**Threshold and `for:` duration**](alert-testing-thresholds.md) -- sine for
   predictable crossings, sequence for exact breach windows, constant for sustained
   load.
2. [**Resolution and recovery**](alert-testing-resolution.md) -- gap windows that drop
   the metric so you can confirm the alert clears.
3. [**Compound and correlated alerts**](alert-testing-correlation.md) -- `phase_offset`
   and `clock_group` to overlap two scenarios for `A AND B` rules.
4. [**Cardinality explosion alerts**](alert-testing-cardinality.md) --
   `cardinality_spikes` for testing series-count guardrails.
5. [**Replaying recorded incidents**](alert-testing-replay.md) -- `sequence` for short
   patterns, `csv_replay` for production exports.

## Push to a real backend

Once you can shape the alert pattern locally, push it into a real TSDB and verify the
alert fires there. The push-and-query loop -- start the backend, run the scenario,
`curl` the query API -- is the same one [E2E Testing](e2e-testing.md) walks through,
with the full coverage matrix of encoder and sink combinations.

For alerting specifically, the two scenarios you will reach for first are
`examples/vm-push-scenario.yaml` (Prometheus text via `http_push`) and
`examples/remote-write-vm.yaml` (`remote_write` to VictoriaMetrics, vmagent, or
upstream Prometheus). Both land in the stack from
`examples/docker-compose-victoriametrics.yml`:

```bash
# Start the stack
docker compose -f examples/docker-compose-victoriametrics.yml up -d

# Push test data
sonda metrics --scenario examples/vm-push-scenario.yaml

# Verify the metric exists (wait ~15s for ingestion)
curl "http://localhost:8428/api/v1/query?query=cpu_usage"

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
    This stack verifies that data arrives in VictoriaMetrics, but does not prove alerts
    fire. To add vmalert, Alertmanager, and a webhook receiver to the stack, see the
    [Alerting Pipeline](alerting-pipeline.md) guide.

## Scrape model instead of push

If you prefer the Prometheus pull model, sonda-server exposes a scrape endpoint for each
running scenario. Start the server and submit a scenario:

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

See [Server API](../deployment/sonda-server.md) for the full API reference.

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
| Push to VictoriaMetrics | any | `vm-push-scenario.yaml` |
| Remote write | any | `remote-write-vm.yaml` |

## Next steps

**Verifying alerts fire end-to-end?** See [Alerting Pipeline](alerting-pipeline.md) to
run vmalert, Alertmanager, and a webhook receiver with Docker Compose.

**Validating alert rules in CI?** See [CI Alert Validation](ci-alert-validation.md) to
catch broken rules before they reach production.

**Validating a pipeline change?** See [Pipeline Validation](pipeline-validation.md).

**Verifying recording rules?** Check [Recording Rules](recording-rules.md).

**Browsing all example scenarios?** See [Example Scenarios](examples.md).
