# CI/CD Alert Rule Validation

Alert rules that pass code review can still fail in production. A threshold typo, a `for:` duration
that never fires, a label mismatch that skips the route -- these bugs are invisible until an incident
happens and the page never arrives. This guide shows you how to catch those problems automatically
by validating alert rules against real metric data in your CI pipeline.

---

## How it works

The approach is straightforward: spin up a VictoriaMetrics service container in GitHub Actions,
push synthetic metrics that match each alert rule's conditions, wait for the evaluation interval,
then query the API to verify the alert fired. If it didn't, the CI job fails and the PR is blocked.

```
GitHub Actions runner
 |
 |-- sonda push ------> VictoriaMetrics (service container, port 8428)
 |                         |
 |                         |<-- vmalert evaluates rules every 5s
 |                         |
 |-- curl query -------> vmalert API (port 8880)
 |                         |
 |-- assert: alert == firing
```

The workflow requires no external dependencies beyond Docker (which GitHub Actions runners
provide out of the box).

---

## Prerequisites

Before setting up CI, make sure you can run the alerting pipeline locally. You should be
comfortable with:

- [Alert Testing](alert-testing.md) -- generating metrics that cross thresholds
- [Alerting Pipeline](alerting-pipeline.md) -- running vmalert and Alertmanager with Docker Compose

You'll also need alert rules to validate. This guide uses the included sample rules
at `examples/alertmanager/alert-rules.yml`, which fire on `docker_alert_cpu > 90` (critical)
and `> 70` (warning).

---

## The GitHub Actions workflow

Here is the complete workflow. Paste it into your repository, then we'll walk through each section.

```yaml title=".github/workflows/alert-validation.yml"
name: Alert Rule Validation
on:
  pull_request:
    paths:
      - "examples/alertmanager/alert-rules.yml"
      - ".github/workflows/alert-validation.yml"

jobs:
  validate-alerts:
    runs-on: ubuntu-latest

    services:
      victoriametrics:
        image: victoriametrics/victoria-metrics:v1.108.1
        ports:
          - 8428:8428
        options: >-
          --health-cmd "wget -q -O /dev/null http://127.0.0.1:8428/health"
          --health-interval 5s
          --health-timeout 5s
          --health-retries 10

      vmalert:
        image: victoriametrics/vmalert:v1.108.1
        ports:
          - 8880:8880
        options: >-
          --health-cmd "wget -q -O /dev/null http://127.0.0.1:8880/health"
          --health-interval 5s
          --health-timeout 5s
          --health-retries 10

    steps:
      - uses: actions/checkout@v4

      - name: Install Sonda
        run: curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh

      - name: Copy alert rules into vmalert
        run: |
          docker cp examples/alertmanager/alert-rules.yml \
            $(docker ps -qf "ancestor=victoriametrics/vmalert:v1.108.1"):/rules/alert-rules.yml

      - name: Configure and restart vmalert
        run: |
          VMALERT_ID=$(docker ps -qf "ancestor=victoriametrics/vmalert:v1.108.1")
          docker stop "$VMALERT_ID"
          docker start "$VMALERT_ID"
          sleep 5

      - name: Push metrics above critical threshold
        run: |
          sonda -q metrics --scenario examples/ci-alert-validation.yaml

      - name: Wait for alert evaluation
        run: sleep 15

      - name: Assert HighCpuUsage alert is firing
        run: |
          STATE=$(curl -sf http://localhost:8880/api/v1/alerts \
            | jq -r '.data.alerts[]
                     | select(.labels.alertname == "HighCpuUsage")
                     | .state')
          echo "HighCpuUsage state: $STATE"
          [ "$STATE" = "firing" ] || { echo "FAIL: expected firing, got $STATE"; exit 1; }

      - name: Assert ElevatedCpuUsage alert is firing
        run: |
          STATE=$(curl -sf http://localhost:8880/api/v1/alerts \
            | jq -r '.data.alerts[]
                     | select(.labels.alertname == "ElevatedCpuUsage")
                     | .state')
          echo "ElevatedCpuUsage state: $STATE"
          [ "$STATE" = "firing" ] || { echo "FAIL: expected firing, got $STATE"; exit 1; }

      - name: Verify metric values in VictoriaMetrics
        run: |
          VALUE=$(curl -sf "http://localhost:8428/api/v1/query?query=docker_alert_cpu" \
            | jq -r '.data.result[0].value[1]')
          echo "docker_alert_cpu value: $VALUE"
          # Value should be 95 (from the constant generator)
          [ "$(echo "$VALUE > 90" | bc -l)" = "1" ] || {
            echo "FAIL: expected value > 90, got $VALUE"; exit 1;
          }
```

!!! warning "Service container networking"
    GitHub Actions service containers run in the same Docker network as the runner. They are
    accessible at `localhost` on their mapped ports. The `vmalert` container needs to reach
    `victoriametrics` -- in service containers, use the service name as the hostname in
    the vmalert command flags.

---

## Breaking it down

### Trigger on alert rule changes

The workflow only runs when alert rules or the workflow itself change. This keeps CI fast
for unrelated PRs.

```yaml
on:
  pull_request:
    paths:
      - "examples/alertmanager/alert-rules.yml"
      - ".github/workflows/alert-validation.yml"
```

Adjust the `paths` filter to match where your alert rules live. If you have rules in multiple
files, use a glob: `"alerts/**/*.yml"`.

### Service containers

VictoriaMetrics and vmalert run as GitHub Actions
[service containers](https://docs.github.com/en/actions/using-containerized-services/about-service-containers).
They start automatically before the first step and stop when the job finishes -- no manual
Docker Compose setup needed.

```yaml
services:
  victoriametrics:
    image: victoriametrics/victoria-metrics:v1.108.1
    ports:
      - 8428:8428
    options: >-
      --health-cmd "wget -q -O /dev/null http://127.0.0.1:8428/health"
      --health-interval 5s
      --health-timeout 5s
      --health-retries 10
```

The health check ensures the service is ready before steps run.

### Push metrics that trigger the alert

The scenario pushes `docker_alert_cpu` at a constant `95.0` for 30 seconds. This is above
both the warning threshold (70) and the critical threshold (90) defined in the alert rules.

```yaml title="examples/ci-alert-validation.yaml"
name: docker_alert_cpu
rate: 1
duration: 30s

generator:
  type: constant
  value: 95.0

labels:
  host: ci-test-node
  region: us-east-1
  service: payment-service
  env: ci

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

The constant generator is ideal here -- you need the value to stay above threshold for long
enough to satisfy the `for:` clause. See [Alert Testing](alert-testing.md#constant-generator-shortcut)
for more on choosing the right generator.

### Wait for evaluation

After pushing metrics, you need to wait for vmalert to evaluate rules and transition alerts
from `pending` to `firing`. The wait time depends on two factors:

| Factor | Value in this example |
|--------|----------------------|
| Rule evaluation interval | `5s` (vmalert `--evaluationInterval`) |
| Alert `for:` duration | `5s` |

The minimum wait is `evaluation_interval + for_duration`. In this case that's 10 seconds,
but we use 15 to provide a safety margin for CI variability.

!!! tip "Scaling wait times for longer `for:` durations"
    If your alert rules use `for: 5m`, you'll need to push metrics for at least 5 minutes
    and wait at least 5 minutes plus one evaluation interval. Adjust both the scenario's
    `duration:` and the `sleep` accordingly. For very long durations, consider using shorter
    `for:` values in your CI-specific rules.

### Assert alert state

The assertion step queries vmalert's API and checks that each expected alert is in `firing` state.

```bash
STATE=$(curl -sf http://localhost:8880/api/v1/alerts \
  | jq -r '.data.alerts[]
           | select(.labels.alertname == "HighCpuUsage")
           | .state')
echo "HighCpuUsage state: $STATE"
[ "$STATE" = "firing" ] || { echo "FAIL: expected firing, got $STATE"; exit 1; }
```

This is a simple string comparison. If the alert isn't `firing`, the step exits with code 1
and the workflow fails.

### Verify metric values

As a secondary check, query VictoriaMetrics directly to confirm the metric value is what you
expect. This catches scenarios where the metric name or labels don't match the alert rule's
`expr:`.

```bash
VALUE=$(curl -sf "http://localhost:8428/api/v1/query?query=docker_alert_cpu" \
  | jq -r '.data.result[0].value[1]')
echo "docker_alert_cpu value: $VALUE"
[ "$(echo "$VALUE > 90" | bc -l)" = "1" ] || {
  echo "FAIL: expected value > 90, got $VALUE"; exit 1;
}
```

---

## A simpler alternative: Docker Compose in CI

If managing service container flags feels heavy, you can use the existing Docker Compose stack
instead. This approach reuses the same `docker-compose-victoriametrics.yml` from the
[Alerting Pipeline](alerting-pipeline.md) guide.

```yaml title=".github/workflows/alert-validation-compose.yml"
name: Alert Rule Validation (Compose)
on:
  pull_request:
    paths:
      - "examples/alertmanager/**"

jobs:
  validate-alerts:
    runs-on: ubuntu-latest

    steps:
      - uses: actions/checkout@v4

      - name: Install Sonda
        run: curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh

      - name: Start alerting stack
        run: |
          docker compose -f examples/docker-compose-victoriametrics.yml \
            --profile alerting up -d
          # Wait for all services to be healthy
          for i in $(seq 1 30); do
            if docker compose -f examples/docker-compose-victoriametrics.yml \
              --profile alerting ps | grep -q "unhealthy\|starting"; then
              sleep 2
            else
              break
            fi
          done

      - name: Push metrics
        run: sonda -q metrics --scenario examples/ci-alert-validation.yaml

      - name: Wait for evaluation
        run: sleep 15

      - name: Assert alerts are firing
        run: |
          # Check HighCpuUsage
          STATE=$(curl -sf http://localhost:8880/api/v1/alerts \
            | jq -r '.data.alerts[]
                     | select(.labels.alertname == "HighCpuUsage")
                     | .state')
          echo "HighCpuUsage: $STATE"
          [ "$STATE" = "firing" ] || exit 1

          # Check ElevatedCpuUsage
          STATE=$(curl -sf http://localhost:8880/api/v1/alerts \
            | jq -r '.data.alerts[]
                     | select(.labels.alertname == "ElevatedCpuUsage")
                     | .state')
          echo "ElevatedCpuUsage: $STATE"
          [ "$STATE" = "firing" ] || exit 1

      - name: Tear down
        if: always()
        run: |
          docker compose -f examples/docker-compose-victoriametrics.yml \
            --profile alerting down -v
```

The Docker Compose approach is simpler to configure and includes Alertmanager and the webhook
receiver, so you can also verify that notifications are delivered. The tradeoff is slightly
longer startup times (building the sonda-server image on first run).

---

## Testing multiple alert rules

Real repositories have dozens of alert rules. Rather than one giant workflow, structure your
validation as one scenario per rule (or rule group), each pushing the specific metric shape
that should trigger it.

```yaml title="examples/ci-high-memory-alert.yaml"
name: node_memory_usage_percent
rate: 1
duration: 30s

generator:
  type: constant
  value: 92.0

labels:
  host: ci-test-node
  env: ci

encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

Then run them sequentially or use `sonda run` with a multi-scenario file to push all metrics
concurrently:

```bash
# Sequential: one scenario per rule
sonda -q metrics --scenario examples/ci-alert-validation.yaml
sonda -q metrics --scenario examples/ci-high-memory-alert.yaml

# Concurrent: all rules in one file
sonda -q run --scenario examples/ci-all-alerts.yaml
```

??? tip "Organizing scenarios by rule group"
    Keep CI alert scenarios in a dedicated directory (e.g., `tests/alerts/`) separate from
    your example scenarios. Name them after the alert they validate:
    `tests/alerts/high-cpu.yaml`, `tests/alerts/high-memory.yaml`, etc.

---

## Integrating with PR review

The final step is making alert rule validation a required check for PRs that touch alert
configurations. This ensures no broken rule reaches production.

In your GitHub repository settings:

1. Go to **Settings > Branches > Branch protection rules**.
2. Select your main branch rule (or create one).
3. Under **Require status checks to pass**, add **Alert Rule Validation**.
4. Enable **Require branches to be up to date**.

Now any PR that modifies files matching the `paths` filter must pass the alert validation
job before merging. Reviewers can see the check status directly in the PR timeline.

!!! tip "Combine with other validations"
    Alert validation pairs well with the [Pipeline Validation](pipeline-validation.md) smoke
    tests. Run both as separate jobs in the same workflow file, or keep them in separate
    workflow files with different `paths` triggers.

---

## Debugging failed checks

When the CI job fails, work through the pipeline hop by hop -- the same debugging sequence
from the [Alerting Pipeline](alerting-pipeline.md#verify-each-stage) guide.

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Metric not found in VictoriaMetrics | Metric name mismatch between scenario and rule | Ensure `name:` in scenario matches `expr:` in rule |
| Alert stuck in `pending` | `sleep` too short for the `for:` duration | Increase wait time to `evaluation_interval + for + margin` |
| Alert never appears | Label selector in rule doesn't match pushed labels | Check that `labels:` in the scenario include required selectors |
| `curl` connection refused | Service container not ready | Add or increase health check retries |
| vmalert returns empty alerts | Rules file not loaded | Verify the rules file is mounted and vmalert was restarted |

---

## Quick reference

| File | Purpose |
|------|---------|
| `examples/ci-alert-validation.yaml` | Sonda scenario: constant 95.0 to VictoriaMetrics |
| `examples/alertmanager/alert-rules.yml` | vmalert rules: HighCpuUsage and ElevatedCpuUsage |
| `.github/workflows/alert-validation.yml` | GitHub Actions workflow (service containers) |

---

## Next steps

**Testing more alert patterns locally?** See [Alert Testing](alert-testing.md) for threshold,
gap, sequence, and multi-metric scenarios.

**Running the full Docker Compose alerting stack?** See [Alerting Pipeline](alerting-pipeline.md).

**Adding pipeline smoke tests to CI?** See [Pipeline Validation](pipeline-validation.md).

**Running automated e2e tests?** See [E2E Testing](e2e-testing.md).
