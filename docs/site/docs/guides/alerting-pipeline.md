# Alerting Pipeline

You've written alert rules, pushed metrics to a TSDB, and checked that the data looks right. But
does the alert actually fire? Does it reach Alertmanager? Does the notification arrive at your
webhook? This guide closes that loop: you'll run a complete
**Sonda -> VictoriaMetrics -> vmalert -> Alertmanager -> webhook** pipeline and watch an alert
flow from synthetic metric to delivered notification.

---

## What you'll build

```
sonda (host CLI)        VictoriaMetrics        vmalert         Alertmanager     webhook-receiver
 |                         |                    |                  |                 |
 |-- push metrics -------->|                    |                  |                 |
 |                         |<-- query rules ----|                  |                 |
 |                         |--- results ------->|                  |                 |
 |                         |                    |-- fire alert --->|                 |
 |                         |                    |                  |-- POST JSON --->|
 |                         |                    |                  |                 |
```

All services except Sonda run in Docker. The alerting profile adds vmalert, Alertmanager, and a
webhook echo server to the existing VictoriaMetrics stack.

---

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) with the Compose v2 plugin (`docker compose`)
- Sonda CLI installed ([Getting Started](../getting-started.md#installation))
- `curl` and `jq` in PATH (for verification commands)

---

## Start the stack

The alerting services are behind a Docker Compose profile, so the base stack stays lightweight
for users who don't need them.

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting up -d
```

Wait for all services to show `(healthy)` status (about 15--20 seconds):

!!! note "First-run build time"
    On first run, Docker builds the `sonda-server` image from source. This can take a few
    minutes depending on your machine. Subsequent runs use the cached image and start in seconds.

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting ps
```

| Service | Port | Purpose |
|---------|------|---------|
| VictoriaMetrics | 8428 | Time series database |
| vmagent | 8429 | Metrics relay agent |
| vmalert | 8880 | Rule evaluation engine |
| Alertmanager | 9093 | Alert routing and notification |
| webhook-receiver | 8090 | HTTP echo server (shows alert payloads) |
| Grafana | 3000 | Dashboards |
| sonda-server | 8080 | Sonda HTTP API |

---

## Push metrics that cross alert thresholds

The included alerting scenario generates a sine wave (`docker_alert_cpu`) that oscillates
between 0 and 100 with a 30-second period. It crosses the warning threshold (70) and critical
threshold (90) twice per cycle, and pushes directly to VictoriaMetrics.

```bash
sonda metrics --scenario examples/alertmanager/alerting-scenario.yaml
```

This runs for 5 minutes at 2 events/second. You'll see alerts fire within the first 30 seconds.

??? info "What the scenario looks like"
    ```yaml title="examples/alertmanager/alerting-scenario.yaml"
    name: docker_alert_cpu
    rate: 2
    duration: 300s

    generator:
      type: sine
      amplitude: 50.0
      period_secs: 30
      offset: 50.0

    labels:
      host: docker-alert-demo
      region: us-east-1
      service: payment-service
      env: staging

    encoder:
      type: prometheus_text
    sink:
      type: http_push
      url: "http://localhost:8428/api/v1/import/prometheus"
      content_type: "text/plain"
    ```

---

## Verify each stage

Work through the pipeline one hop at a time. This is the same debugging sequence you'd use
in production when an alert isn't firing.

### 1. Data in VictoriaMetrics

Confirm the metric exists and has recent values:

```bash
curl -s "http://localhost:8428/api/v1/query?query=docker_alert_cpu" | jq .
```

You should see the current value and labels. If this returns no data, Sonda isn't pushing
successfully -- check that the VictoriaMetrics container is healthy.

### 2. Rules evaluating in vmalert

Check that vmalert is evaluating rules and detecting threshold breaches:

```bash
curl -s http://localhost:8880/api/v1/alerts | jq '.data.alerts[] | {alertname: .labels.alertname, state: .state, value: .value}'
```

You should see alerts in `pending` or `firing` state. The vmalert UI at
[http://localhost:8880](http://localhost:8880) shows rule groups, evaluation results, and
alert history.

??? info "Alert rules being evaluated"
    ```yaml title="examples/alertmanager/alert-rules.yml"
    groups:
      - name: sonda-alerts
        interval: 5s
        rules:
          - alert: HighCpuUsage
            expr: docker_alert_cpu > 90
            for: 5s
            labels:
              severity: critical
            annotations:
              summary: "CPU usage is critically high"
              description: "docker_alert_cpu on {{ $labels.host }} is {{ $value | printf \"%.1f\" }}%"

          - alert: ElevatedCpuUsage
            expr: docker_alert_cpu > 70
            for: 5s
            labels:
              severity: warning
            annotations:
              summary: "CPU usage is elevated"
              description: "docker_alert_cpu on {{ $labels.host }} is {{ $value | printf \"%.1f\" }}%"
    ```

### 3. Alerts in Alertmanager

Verify Alertmanager received the firing alerts:

```bash
curl -s http://localhost:9093/api/v2/alerts | jq '.[].labels'
```

The Alertmanager UI at [http://localhost:9093](http://localhost:9093) shows active alerts,
silences, and routing groups.

### 4. Webhook payload delivered

This is the end of the chain. Check the webhook receiver logs for the delivered notification:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting logs webhook-receiver | head -50
```

You'll see the full Alertmanager notification JSON, including alert name, labels, annotations,
and status. This is the same payload a PagerDuty/Slack/OpsGenie integration would receive.

!!! tip "Follow the webhook logs in real time"
    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml \
      --profile alerting logs -f webhook-receiver
    ```
    Keep this running in a separate terminal to watch alerts arrive as the sine wave crosses
    thresholds.

---

## Customize the rules

The included alert rules fire on `docker_alert_cpu > 90` (critical) and `> 70` (warning).
To test your own rules, edit `examples/alertmanager/alert-rules.yml` and restart vmalert:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting restart vmalert
```

Common adjustments:

| Change | What to modify |
|--------|---------------|
| Different metric name | Change `expr:` in the alert rule and `name:` in the scenario |
| Longer `for:` duration | Increase `for:` in the rule; increase `duration:` in the scenario |
| Different thresholds | Adjust `> 90` / `> 70` in the rule; tweak `amplitude` and `offset` |
| Route by severity | Edit `route:` in `examples/alertmanager/alertmanager.yml` |

??? info "Alertmanager routing config"
    ```yaml title="examples/alertmanager/alertmanager.yml"
    global:
      resolve_timeout: 1m

    route:
      receiver: webhook
      group_by: ['alertname', 'severity']
      group_wait: 10s
      group_interval: 10s
      repeat_interval: 1m

    receivers:
      - name: webhook
        webhook_configs:
          - url: http://webhook-receiver:8080
            send_resolved: true
    ```

---

## Tear down

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting down -v
```

---

## Quick reference

| File | Purpose |
|------|---------|
| `examples/alertmanager/alerting-scenario.yaml` | Sonda scenario: sine wave pushing to VictoriaMetrics |
| `examples/alertmanager/alert-rules.yml` | vmalert rules: HighCpuUsage and ElevatedCpuUsage |
| `examples/alertmanager/alertmanager.yml` | Alertmanager config: route all alerts to webhook |
| `examples/docker-compose-victoriametrics.yml` | Docker Compose (use `--profile alerting`) |

---

## Next steps

**Testing more alert patterns?** See [Alert Testing](alert-testing.md) for threshold, gap,
sequence, and multi-metric scenarios.

**Validating recording rules?** Check [Recording Rules](recording-rules.md).

**Running automated e2e tests?** See [E2E Testing](e2e-testing.md).
