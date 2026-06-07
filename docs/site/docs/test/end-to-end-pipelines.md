---
title: End-to-end pipeline testing
description: Validate the full path from synthetic signal generation through TSDB, alert evaluator, and notification — in CI or in production.
---

# End-to-end pipelines

You've written alert rules and shipped them. You changed a vmagent relabel rule. You added a new encoder, swapped a sink, pointed at a different backend. The unit tests pass — but does the data actually arrive, in the shape your downstream consumers expect, and does the alert at the end of the chain actually fire?

This page covers four shapes of end-to-end validation. The local **Alerting pipeline** runs vmalert + Alertmanager + a webhook receiver so you can watch one alert flow from synthetic metric to delivered notification. The **End-to-end pipeline test** is a coverage matrix: every signal × encoder × sink combo, with a curl + jq assertion at the end. **CI validation** wires the alerting loop into GitHub Actions as a required check on every PR that touches alert rules. **Production pipeline validation** covers the lighter-weight smoke checks — exit codes, line counts, multi-format diffs — for catching regressions before they reach the backend.

Pick the tab that matches your scenario.

<a id="alerting-pipeline-dev"></a>

=== "Alerting pipeline (dev)"

    You'll run a complete **Sonda -> VictoriaMetrics -> vmalert -> Alertmanager -> webhook** pipeline and watch an alert flow from synthetic metric to delivered notification.

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

    All services except Sonda run in Docker. The alerting profile adds vmalert, Alertmanager, and a webhook echo server to the existing VictoriaMetrics stack.

    ### Prerequisites

    - [Docker](https://docs.docker.com/get-docker/) with the Compose v2 plugin (`docker compose`)
    - Sonda CLI installed ([Quickstart](../get-started/quickstart.md#installation))
    - `curl` and `jq` in PATH (for verification commands)

    ### Start the stack

    The alerting services are behind a Docker Compose profile, so the base stack stays lightweight for users who don't need them.

    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml \
      --profile alerting up -d
    ```

    Wait for all services to show `(healthy)` status (about 15--20 seconds):

    !!! note "First-run build time"
        On first run, Docker builds the `sonda-server` image from source. This can take a few minutes depending on your machine. Subsequent runs use the cached image and start in seconds.

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

    ### Push metrics that cross alert thresholds

    The included alerting scenario generates a sine wave (`docker_alert_cpu`) that oscillates between 0 and 100 with a 30-second period. It crosses the warning threshold (70) and critical threshold (90) twice per cycle, and pushes directly to VictoriaMetrics.

    ```bash
    sonda run examples/alertmanager/alerting-scenario.yaml
    ```

    This runs for 5 minutes at 2 events/second. You'll see alerts fire within the first 30 seconds.

    ??? info "What the scenario looks like"
        ```yaml title="examples/alertmanager/alerting-scenario.yaml"
        version: 2
        kind: runnable

        defaults:
          rate: 2
          duration: 300s
          encoder:
            type: prometheus_text
          sink:
            type: http_push
            url: "${VICTORIAMETRICS_URL:-http://localhost:8428/api/v1/import/prometheus}"
            content_type: "text/plain"
          labels:
            host: docker-alert-demo
            region: us-east-1
            service: payment-service
            env: staging

        scenarios:
          - signal_type: metrics
            name: docker_alert_cpu
            generator:
              type: sine
              amplitude: 50.0
              period_secs: 30
              offset: 50.0
        ```

    ### Verify each stage

    Work through the pipeline one hop at a time. This is the same debugging sequence you'd use in production when an alert isn't firing.

    #### 1. Data in VictoriaMetrics

    Confirm the metric exists and has recent values:

    ```bash
    curl -s "http://localhost:8428/api/v1/query?query=docker_alert_cpu" | jq .
    ```

    You should see the current value and labels. If this returns no data, Sonda isn't pushing successfully -- check that the VictoriaMetrics container is healthy.

    #### 2. Rules evaluating in vmalert

    Check that vmalert is evaluating rules and detecting threshold breaches:

    ```bash
    curl -s http://localhost:8880/api/v1/alerts | jq '.data.alerts[] | {alertname: .labels.alertname, state: .state, value: .value}'
    ```

    You should see alerts in `pending` or `firing` state. The vmalert UI at [http://localhost:8880](http://localhost:8880) shows rule groups, evaluation results, and alert history.

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

    #### 3. Alerts in Alertmanager

    Verify Alertmanager received the firing alerts:

    ```bash
    curl -s http://localhost:9093/api/v2/alerts | jq '.[].labels'
    ```

    The Alertmanager UI at [http://localhost:9093](http://localhost:9093) shows active alerts, silences, and routing groups.

    #### 4. Webhook payload delivered

    This is the end of the chain. Check the webhook receiver logs for the delivered notification:

    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml \
      --profile alerting logs webhook-receiver | head -50
    ```

    You'll see the full Alertmanager notification JSON, including alert name, labels, annotations, and status. This is the same payload a PagerDuty/Slack/OpsGenie integration would receive.

    !!! tip "Follow the webhook logs in real time"
        ```bash
        docker compose -f examples/docker-compose-victoriametrics.yml \
          --profile alerting logs -f webhook-receiver
        ```
        Keep this running in a separate terminal to watch alerts arrive as the sine wave crosses thresholds.

    ### Customize the rules

    The included alert rules fire on `docker_alert_cpu > 90` (critical) and `> 70` (warning). To test your own rules, edit `examples/alertmanager/alert-rules.yml` and restart vmalert:

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

    ### Tear down

    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml \
      --profile alerting down -v
    ```

    ### Quick reference

    | File | Purpose |
    |------|---------|
    | `examples/alertmanager/alerting-scenario.yaml` | Sonda scenario: sine wave pushing to VictoriaMetrics |
    | `examples/alertmanager/alert-rules.yml` | vmalert rules: HighCpuUsage and ElevatedCpuUsage |
    | `examples/alertmanager/alertmanager.yml` | Alertmanager config: route all alerts to webhook |
    | `examples/docker-compose-victoriametrics.yml` | Docker Compose (use `--profile alerting`) |

=== "End-to-end pipeline test"

    You changed an encoder, swapped a sink, or pointed at a new backend. Unit tests pass and the smoke checks show bytes leaving the wire — but did the data actually land in the backend you query against? This tab shows the canonical end-to-end loop: start a real backend, push a known value, query it back.

    ### The pattern

    Every e2e check is the same three steps. The encoder, sink, and backend change; the shape does not.

    1. **Start the backend** — `docker compose up -d` against an `examples/docker-compose-*.yml` stack.
    2. **Push a known value** — `sonda run examples/<scenario>.yaml` with a unique metric or log name.
    3. **Query the backend** — `curl ... | jq ...` and assert the value arrived.

    This is the heavier sibling of the smoke check in the **Production pipeline validation** tab: same loop, but the backend is a real service container instead of `wc -l`.

    ### Prerequisites

    - [Docker](https://docs.docker.com/get-docker/) with the Compose v2 plugin (`docker compose`).
    - `sonda` on `PATH` — see [Installation](../get-started/quickstart.md#installation).
    - `curl` and [`jq`](https://jqlang.github.io/jq/) for backend queries.

    ### Worked example: metrics into VictoriaMetrics

    The fastest path from zero to a verified pipeline. Pushes a constant `99.0` to VictoriaMetrics for ten seconds, queries the series, and tears down.

    ```bash title="Start the backend"
    docker compose -f examples/docker-compose-victoriametrics.yml up -d
    ```

    ```bash title="Push a known value"
    sonda run examples/e2e-scenario.yaml
    ```

    ```yaml title="examples/e2e-scenario.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 10s
      encoder:
        type: prometheus_text
      sink:
        type: http_push
        url: "http://localhost:8428/api/v1/import/prometheus"
        content_type: "text/plain"

    scenarios:
      - signal_type: metrics
        name: e2e_pipeline_check
        generator:
          type: constant
          value: 99.0
        labels:
          test: pipeline
          env: ci
    ```

    ```bash title="Verify the data arrived"
    sleep 5
    curl -s "http://localhost:8428/api/v1/query?query=e2e_pipeline_check" \
      | jq '.data.result | length'
    # Expected: 1 (one series with labels env=ci, test=pipeline)
    ```

    ```bash title="Tear down"
    docker compose -f examples/docker-compose-victoriametrics.yml down -v
    ```

    That same shape — start, push, query — works for every signal × encoder × sink combo below. Swap the scenario file and the verification command.

    ### Coverage matrix

    Every row below is a real `examples/*.yaml` you can run today. Start the matching backend profile from `examples/docker-compose-victoriametrics.yml` first.

    | Signal | Encoder | Sink | Scenario | Verify |
    |---|---|---|---|---|
    | Metrics | `prometheus_text` | `http_push` (VictoriaMetrics) | `examples/e2e-scenario.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=e2e_pipeline_check' \| jq '.data.result \| length'` |
    | Metrics | `prometheus_text` | `http_push` (VictoriaMetrics, sine) | `examples/vm-push-scenario.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage' \| jq '.data.result \| length'` |
    | Metrics | `remote_write` | `remote_write` (VictoriaMetrics) | `examples/remote-write-vm.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage_rw' \| jq '.data.result \| length'` |
    | Metrics | `remote_write` | `remote_write` (vmagent → VM) | `examples/remote-write-vmagent.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage_vmagent' \| jq '.data.result \| length'` |
    | Metrics | `remote_write` | `remote_write` (Prometheus) | `examples/remote-write-prometheus.yaml` | `curl -s 'http://localhost:9090/api/v1/query?query=cpu_usage_prom' \| jq '.data.result \| length'` |
    | Metrics | `otlp` | `otlp_grpc` (OTel Collector → VM) | `examples/otlp-metrics.yaml` | `curl -s 'http://localhost:8428/api/v1/query?query=cpu_usage' \| jq '.data.result \| length'` |
    | Logs | `otlp` | `otlp_grpc` (OTel Collector → Loki) | `examples/otlp-logs.yaml` | `curl -sG 'http://localhost:3100/loki/api/v1/query_range' --data-urlencode 'query={service_name="sonda"}' \| jq '.data.result \| length'` |
    | Metrics | `prometheus_text` | `kafka` | `examples/kafka-sink.yaml` | `docker exec <kafka> /opt/kafka/bin/kafka-console-consumer.sh --bootstrap-server kafka:9092 --topic sonda-metrics --from-beginning --timeout-ms 5000` |
    | Logs | `json_lines` | `loki` | `examples/loki-json-lines.yaml` | `curl -sG 'http://localhost:3100/loki/api/v1/query_range' --data-urlencode 'query={job="sonda"}' \| jq '.data.result \| length'` |
    | Logs | `json_lines` | `kafka` | `examples/kafka-json-logs.yaml` | `docker exec <kafka> /opt/kafka/bin/kafka-console-consumer.sh --bootstrap-server kafka:9092 --topic sonda-logs --from-beginning --timeout-ms 5000` |
    | Metrics | `influx_lp` | `file` | `examples/influx-file.yaml` | `wc -l < /tmp/sonda-influx-output.txt` |

    !!! info "Compose profiles"
        Loki, Kafka, Prometheus, and the OTel Collector are behind profiles to keep the base stack lean. Bring up only what each row needs. The vmagent row uses the default stack — no extra profile.
        ```bash
        docker compose -f examples/docker-compose-victoriametrics.yml \
          --profile loki --profile kafka --profile prometheus --profile otel-collector up -d
        ```
        The OTLP-logs row needs both `--profile otel-collector` and `--profile loki` so the collector has somewhere to forward log records.

    !!! tip "Feature-gated sinks"
        The `remote_write`, `kafka`, and `otlp_grpc` sinks are included in the pre-built binaries from the install script and the Docker image. Custom builds need to enable them — see [Sinks](../build/sinks.md) for the details.

    ### Intentionally out of scope

    The matrix covers sinks that talk to a queryable backend over HTTP, gRPC, or a broker. A few sinks intentionally fall outside that pattern:

    - **`tcp`, `udp`, `json-tcp`** — raw socket sinks. The fixtures (`examples/tcp-sink.yaml`, `examples/udp-sink.yaml`, `examples/json-tcp.yaml`) push to whatever process is listening on the configured port; verification is "did `nc -l 5000` print anything?", not a backend query. Use them when you're integrating with a custom collector or socket-based ingest path.
    - **`stdout`** — pipes to the terminal. Already covered by the smoke checks in the **Production pipeline validation** tab.

    ### The localhost trap

    The matrix above runs `sonda` on your host, so `url: http://localhost:8428` reaches the Compose-published port. POST the same scenario to a containerized `sonda-server` and the URL resolves inside the server container — `localhost` is the container, and the push silently fails.

    Two ways to make one scenario file work from both paths:

    - Use `${VAR:-default}` in the URL — the bundled examples already do this. See [Environment variable interpolation](../build/scenario-files.md#environment-variable-interpolation).
    - Rewrite the URL with `sed` before POSTing — see [Networking](../deploy/server.md#networking).

    ### Visual exploration

    Want to eyeball the data before bolting it into CI? The same Compose stack ships Grafana with a pre-provisioned VictoriaMetrics datasource:

    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml up -d
    sonda run examples/vm-push-scenario.yaml
    open http://localhost:3000
    ```

    For the full alert-flow loop (vmalert + Alertmanager + a webhook receiver), bring up the alerting profile and walk through the [Alerting pipeline tab](#alerting-pipeline-dev):

    ```bash
    docker compose -f examples/docker-compose-victoriametrics.yml --profile alerting up -d
    ```

    To verify the alert rules themselves cross thresholds correctly, see [Alert Testing](alert-testing.md).

=== "CI validation"

    Alert rules that pass code review can still fail in production. A threshold typo, a `for:` duration that never fires, a label mismatch that skips the route -- these bugs are invisible until an incident happens and the page never arrives. This tab shows you how to catch those problems automatically by validating alert rules against real metric data in your CI pipeline.

    ### How it works

    The approach is straightforward: spin up VictoriaMetrics as a service container in GitHub Actions, start vmalert via `docker run` (so you can mount your alert rules file from the workspace), push synthetic metrics that match each alert rule's conditions, wait for the evaluation interval, then query the API to verify the alert fired. If it didn't, the CI job fails and the PR is blocked.

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

    The workflow requires no external dependencies beyond Docker (which GitHub Actions runners provide out of the box).

    ### Prerequisites

    Before setting up CI, make sure you can run the alerting pipeline locally. You should be comfortable with:

    - [Alert Testing](alert-testing.md) -- generating metrics that cross thresholds
    - The [Alerting pipeline tab](#alerting-pipeline-dev) above -- running vmalert and Alertmanager with Docker Compose

    You'll also need alert rules to validate. This tab uses the included sample rules at `examples/alertmanager/alert-rules.yml`, which fire on `docker_alert_cpu > 90` (critical) and `> 70` (warning).

    ### The GitHub Actions workflow

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

        steps:
          - uses: actions/checkout@v4

          - name: Install Sonda
            run: curl -fsSL https://raw.githubusercontent.com/davidban77/sonda/main/install.sh | sh

          - name: Start vmalert
            run: |
              docker run -d --name vmalert \
                --network ${{ job.container.network }} \
                -v ${{ github.workspace }}/examples/alertmanager/alert-rules.yml:/rules/alert-rules.yml:ro \
                -p 8880:8880 \
                victoriametrics/vmalert:v1.108.1 \
                --datasource.url=http://victoriametrics:8428 \
                --remoteWrite.url=http://victoriametrics:8428 \
                --rule=/rules/alert-rules.yml \
                --httpListenAddr=:8880 \
                --evaluationInterval=5s
              # Wait for vmalert to become healthy
              for i in $(seq 1 15); do
                if wget -q -O /dev/null http://localhost:8880/health 2>/dev/null; then
                  echo "vmalert is healthy"
                  break
                fi
                echo "Waiting for vmalert... ($i/15)"
                sleep 2
              done

          - name: Push metrics above critical threshold
            run: |
              sonda -q run examples/ci-alert-validation.yaml

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

          - name: Stop vmalert
            if: always()
            run: docker rm -f vmalert 2>/dev/null || true
    ```

    !!! warning "Why vmalert is not a service container"
        GitHub Actions service containers don't support volume mounts from the workspace. Since vmalert needs the alert rules file at startup, it runs as a `docker run` step after checkout instead. The `--network ${{ job.container.network }}` flag connects it to the same Docker network as the service containers, so it can reach `victoriametrics` by hostname. VictoriaMetrics stays as a service container because it doesn't need any workspace files.

    ### Breaking it down

    #### Trigger on alert rule changes

    The workflow only runs when alert rules or the workflow itself change. This keeps CI fast for unrelated PRs.

    ```yaml
    on:
      pull_request:
        paths:
          - "examples/alertmanager/alert-rules.yml"
          - ".github/workflows/alert-validation.yml"
    ```

    Adjust the `paths` filter to match where your alert rules live. If you have rules in multiple files, use a glob: `"alerts/**/*.yml"`.

    #### Service containers and vmalert

    VictoriaMetrics runs as a GitHub Actions [service container](https://docs.github.com/en/actions/using-containerized-services/about-service-containers). It starts automatically before the first step and stops when the job finishes -- no manual Docker setup needed. The health check ensures VictoriaMetrics is ready before steps run.

    vmalert runs as a separate `docker run` step instead of a service container. This is necessary because vmalert needs the alert rules file from your repository, and GitHub Actions service containers don't support volume mounts from the workspace.

    ```yaml
    - name: Start vmalert
      run: |
        docker run -d --name vmalert \
          --network ${{ job.container.network }} \
          -v ${{ github.workspace }}/examples/alertmanager/alert-rules.yml:/rules/alert-rules.yml:ro \
          -p 8880:8880 \
          victoriametrics/vmalert:v1.108.1 \
          --datasource.url=http://victoriametrics:8428 \
          --remoteWrite.url=http://victoriametrics:8428 \
          --rule=/rules/alert-rules.yml \
          --httpListenAddr=:8880 \
          --evaluationInterval=5s
    ```

    The `--network` flag connects vmalert to the same Docker network as the service containers, so it can reach `victoriametrics` by hostname. The `-v` flag mounts the alert rules file from your checked-out repository.

    #### Push metrics that trigger the alert

    The scenario pushes `docker_alert_cpu` at a constant `95.0` for 30 seconds. This is above both the warning threshold (70) and the critical threshold (90) defined in the alert rules.

    ```yaml title="examples/ci-alert-validation.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 30s
      encoder:
        type: prometheus_text
      sink:
        type: http_push
        url: "http://localhost:8428/api/v1/import/prometheus"
        content_type: "text/plain"

    scenarios:
      - signal_type: metrics
        name: docker_alert_cpu
        generator:
          type: constant
          value: 95.0
        labels:
          host: ci-test-node
          region: us-east-1
          service: payment-service
          env: ci
    ```

    The constant generator is ideal here -- you need the value to stay above threshold for long enough to satisfy the `for:` clause. See [Threshold and `for:` duration](alert-testing.md#thresholds) for more on choosing the right generator.

    #### Wait for evaluation

    After pushing metrics, you need to wait for vmalert to evaluate rules and transition alerts from `pending` to `firing`. The wait time depends on two factors:

    | Factor | Value in this example |
    |--------|----------------------|
    | Rule evaluation interval | `5s` (vmalert `--evaluationInterval`) |
    | Alert `for:` duration | `5s` |

    The minimum wait is `evaluation_interval + for_duration`. In this case that's 10 seconds, but we use 15 to provide a safety margin for CI variability.

    !!! tip "Scaling wait times for longer `for:` durations"
        If your alert rules use `for: 5m`, you'll need to push metrics for at least 5 minutes and wait at least 5 minutes plus one evaluation interval. Adjust both the scenario's `duration:` and the `sleep` accordingly. For very long durations, consider using shorter `for:` values in your CI-specific rules.

    #### Assert alert state

    The assertion step queries vmalert's API and checks that each expected alert is in `firing` state.

    ```bash
    STATE=$(curl -sf http://localhost:8880/api/v1/alerts \
      | jq -r '.data.alerts[]
               | select(.labels.alertname == "HighCpuUsage")
               | .state')
    echo "HighCpuUsage state: $STATE"
    [ "$STATE" = "firing" ] || { echo "FAIL: expected firing, got $STATE"; exit 1; }
    ```

    This is a simple string comparison. If the alert isn't `firing`, the step exits with code 1 and the workflow fails.

    #### Verify metric values

    As a secondary check, query VictoriaMetrics directly to confirm the metric value is what you expect. This catches scenarios where the metric name or labels don't match the alert rule's `expr:`.

    ```bash
    VALUE=$(curl -sf "http://localhost:8428/api/v1/query?query=docker_alert_cpu" \
      | jq -r '.data.result[0].value[1]')
    echo "docker_alert_cpu value: $VALUE"
    [ "$(echo "$VALUE > 90" | bc -l)" = "1" ] || {
      echo "FAIL: expected value > 90, got $VALUE"; exit 1;
    }
    ```

    ### A simpler alternative: Docker Compose in CI

    If managing service container flags feels heavy, you can use the existing Docker Compose stack instead. This approach reuses the same `docker-compose-victoriametrics.yml` from the [Alerting pipeline tab](#alerting-pipeline-dev).

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
            run: sonda -q run examples/ci-alert-validation.yaml

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

    The Docker Compose approach is simpler to configure and includes Alertmanager and the webhook receiver, so you can also verify that notifications are delivered. The tradeoff is slightly longer startup times (building the sonda-server image on first run).

    ### Testing multiple alert rules

    Real repositories have dozens of alert rules. Rather than one giant workflow, structure your validation as one scenario per rule (or rule group), each pushing the specific metric shape that should trigger it.

    ```yaml title="examples/ci-high-memory-alert.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 1
      duration: 30s
      encoder:
        type: prometheus_text
      sink:
        type: http_push
        url: "http://localhost:8428/api/v1/import/prometheus"
        content_type: "text/plain"

    scenarios:
      - signal_type: metrics
        name: node_memory_usage_percent
        generator:
          type: constant
          value: 92.0
        labels:
          host: ci-test-node
          env: ci
    ```

    Then run them sequentially or use `sonda run` with a multi-scenario file to push all metrics concurrently:

    ```bash
    # Sequential: one scenario per rule
    sonda -q run examples/ci-alert-validation.yaml
    sonda -q run examples/ci-high-memory-alert.yaml

    # Concurrent: all rules in one file
    sonda -q run examples/ci-all-alerts.yaml
    ```

    ??? tip "Organizing scenarios by rule group"
        Keep CI alert scenarios in a dedicated directory (e.g., `tests/alerts/`) separate from your example scenarios. Name them after the alert they validate: `tests/alerts/high-cpu.yaml`, `tests/alerts/high-memory.yaml`, etc.

    ### Integrating with PR review

    The final step is making alert rule validation a required check for PRs that touch alert configurations. This ensures no broken rule reaches production.

    In your GitHub repository settings:

    1. Go to **Settings > Branches > Branch protection rules**.
    2. Select your main branch rule (or create one).
    3. Under **Require status checks to pass**, add **Alert Rule Validation**.
    4. Enable **Require branches to be up to date**.

    Now any PR that modifies files matching the `paths` filter must pass the alert validation job before merging. Reviewers can see the check status directly in the PR timeline.

    !!! tip "Combine with other validations"
        Alert validation pairs well with the smoke tests in the **Production pipeline validation** tab. Run both as separate jobs in the same workflow file, or keep them in separate workflow files with different `paths` triggers.

    ### Debugging failed checks

    When the CI job fails, work through the pipeline hop by hop -- the same debugging sequence from the [Alerting pipeline tab](#alerting-pipeline-dev).

    | Symptom | Likely cause | Fix |
    |---------|-------------|-----|
    | Metric not found in VictoriaMetrics | Metric name mismatch between scenario and rule | Ensure `name:` in scenario matches `expr:` in rule |
    | Alert stuck in `pending` | `sleep` too short for the `for:` duration | Increase wait time to `evaluation_interval + for + margin` |
    | Alert never appears | Label selector in rule doesn't match pushed labels | Check that `labels:` in the scenario include required selectors |
    | `curl` connection refused on 8428 | VictoriaMetrics service container not ready | Add or increase health check retries |
    | `curl` connection refused on 8880 | vmalert not running or still starting | Check `docker logs vmalert` and increase the health wait loop |
    | vmalert returns empty alerts | Rules file not loaded | Verify the `-v` mount path in `docker run` matches your rules file location |

    ### Quick reference

    | File | Purpose |
    |------|---------|
    | `examples/ci-alert-validation.yaml` | Sonda scenario: constant 95.0 to VictoriaMetrics |
    | `examples/alertmanager/alert-rules.yml` | vmalert rules: HighCpuUsage and ElevatedCpuUsage |
    | `.github/workflows/alert-validation.yml` | GitHub Actions workflow (VM + vmalert via `docker run`) |

=== "Production pipeline validation"

    You shipped a one-line change to a vmagent relabel rule on Friday. By Monday morning, half the dashboards for `service=payments` are blank. The metrics still arrive, the counts are normal -- but the rule rewrote `service` to lowercase and the dashboards filter for `Payments`. Nothing in your pipeline noticed: the data flowed, the writes succeeded, the only thing that broke was the contract with downstream consumers.

    This is the gap CI is supposed to catch. Sonda fills it by giving you a known input on one end of the pipeline and a check at the other end -- exit code, line count, backend query -- so any rewrite, drop, or schema drift surfaces as a failed step before it reaches the dashboards.

    ### Smoke testing with the CLI

    The simplest validation: run a one-entry scenario, check the exit code, count the output lines. Scaffold a starter file with `sonda new --template`, edit the metric name to taste, then run it with `-q` to suppress status banners in scripts:

    ```yaml title="smoke.yaml"
    version: 2
    kind: runnable
    defaults:
      rate: 5
      duration: 2s
      encoder:
        type: prometheus_text
      sink:
        type: stdout
    scenarios:
      - id: smoke_test
        signal_type: metrics
        name: smoke_test
        generator:
          type: constant
          value: 1.0
    ```

    ```bash
    sonda -q run smoke.yaml > /tmp/smoke.txt
    echo "Exit code: $?"
    wc -l < /tmp/smoke.txt
    ```

    A successful run exits with code `0` and produces approximately `rate * duration` lines (roughly 10 for rate=5 and duration=2s).

    | Exit code | Meaning |
    |-----------|---------|
    | `0` | Success -- all events emitted |
    | `1` | Runtime error -- bad scenario file, sink connection failure, validation reject |
    | `2` | Argument parse error -- unknown flag, missing argument |

    !!! tip "Quick validation in scripts"
        Use the exit code in CI or shell scripts: `sonda -q run smoke.yaml > /dev/null && echo "OK"`.

    Now let's verify that every wire format makes it through your pipeline.

    ### Multi-format validation

    Run the same metric through each encoder to verify that every format arrives at its destination. This catches encoding regressions and misconfigured parsers. The encoder lives in the YAML; swap the `type:` field to compare formats. Override at the command line with `--encoder` when you need a one-off variant:

    ```bash title="Prometheus text"
    sonda run pipeline-test.yaml
    # pipeline_test 0 1700000000000
    # pipeline_test 0 1700000000500
    ```

    ```bash title="InfluxDB line protocol"
    sonda run pipeline-test.yaml --encoder influx_lp
    # pipeline_test value=0 1700000000000000000
    # pipeline_test value=0 1700000000500000000
    ```

    ```bash title="JSON Lines"
    sonda run pipeline-test.yaml --encoder json_lines
    # {"name":"pipeline_test","value":0.0,"labels":{},"timestamp":"2026-03-23T12:00:00.000Z"}
    ```

    The starter `pipeline-test.yaml` is two ticks of the constant generator:

    ```yaml title="pipeline-test.yaml"
    version: 2
    kind: runnable
    defaults:
      rate: 2
      duration: 2s
      encoder:
        type: prometheus_text
      sink:
        type: stdout
    scenarios:
      - id: pipeline_test
        signal_type: metrics
        name: pipeline_test
        generator:
          type: constant
          value: 0.0
    ```

    To push a specific format to a file for inspection, use a scenario file:

    ```bash
    sonda run examples/multi-format-test.yaml
    wc -l < /tmp/pipeline-influx.txt
    ```

    ```yaml title="examples/multi-format-test.yaml"
    version: 2
    kind: runnable

    defaults:
      rate: 2
      duration: 10s
      encoder:
        type: influx_lp
      sink:
        type: file
        path: /tmp/pipeline-influx.txt

    scenarios:
      - signal_type: metrics
        name: pipeline_test
        generator:
          type: constant
          value: 42.0
        labels:
          env: test
    ```

    See [Encoders](../build/encoders.md) and [Sinks](../build/sinks.md) for the full list of supported formats and destinations.

    Individual format checks are good for development. For systematic validation, add Sonda to CI.

    ### CI integration

    Add Sonda as a step in your GitHub Actions workflow to validate your pipeline on every push. The `--duration` flag ensures the step finishes in bounded time.

    ```yaml title=".github/workflows/pipeline-test.yml"
    name: Pipeline Smoke Test
    on: [push, pull_request]

    jobs:
      smoke-test:
        runs-on: ubuntu-latest
        steps:
          - uses: actions/checkout@v4

          - name: Install Rust
            uses: dtolnay/rust-toolchain@stable

          - name: Install Sonda
            run: cargo install sonda

          - name: Scaffold a smoke-test scenario
            run: sonda -q new --template -o /tmp/ci-smoke.yaml

          - name: Smoke test (Prometheus text)
            run: |
              sonda -q run /tmp/ci-smoke.yaml --rate 10 --duration 5s \
                --sink file --endpoint /tmp/ci-smoke-prom.txt
              LINES=$(wc -l < /tmp/ci-smoke-prom.txt)
              echo "Produced $LINES lines"
              [ "$LINES" -ge 40 ] || { echo "FAIL: too few lines"; exit 1; }

          - name: Smoke test (JSON Lines)
            run: |
              sonda -q run /tmp/ci-smoke.yaml --rate 10 --duration 5s \
                --encoder json_lines --sink file --endpoint /tmp/ci-smoke-json.txt
              LINES=$(wc -l < /tmp/ci-smoke-json.txt)
              echo "Produced $LINES lines"
              [ "$LINES" -ge 40 ] || { echo "FAIL: too few lines"; exit 1; }
    ```

    !!! tip "Pre-built binaries"
        If a Sonda release binary is available for your platform, download it instead of building from source to save CI time. Check the [GitHub Releases](https://github.com/davidban77/sonda/releases) page.

    CI catches regressions automatically. For deeper validation against real backends, use the **End-to-end pipeline test** tab.

    ### Multi-scenario validation

    Use `sonda run` to push metrics and logs concurrently from a single YAML file. This validates that your pipeline handles multiple signal types at the same time:

    ```bash
    sonda run examples/multi-pipeline-test.yaml
    echo "Exit: $?"
    wc -l < /tmp/pipeline-logs.json
    ```

    ```yaml title="examples/multi-pipeline-test.yaml"
    version: 2
    kind: runnable

    scenarios:
      - signal_type: metrics
        name: pipeline_metrics
        rate: 5
        duration: 10s
        generator:
          type: constant
          value: 1.0
        encoder:
          type: prometheus_text
        sink:
          type: stdout

      - signal_type: logs
        name: pipeline_logs
        rate: 5
        duration: 10s
        log_generator:
          type: template
          templates:
            - message: "Pipeline validation event"
          severity_weights:
            info: 1.0
          seed: 42
        encoder:
          type: json_lines
        sink:
          type: file
          path: /tmp/pipeline-logs.json
    ```

    Each scenario runs on its own thread. Use different sinks per scenario to keep outputs separate.

    See [Scenario Fields](../reference/scenario-fields.md) for the full multi-scenario YAML reference.

## Where to next

- [Alert testing](alert-testing.md) — generate metric shapes that cross thresholds.
- [Recording rules](recording-rules.md) — validate that aggregations land before alerts query them.
- [Synthetic monitoring](synthetic-monitoring.md) — long-running baseline scenarios that surface degradations.
- [Deploy as a CLI](../deploy/cli.md) and [Deploy as a server](../deploy/server.md) — pick the deployment shape that matches your pipeline.
- [Example scenarios](examples.md) — every example scenario file with its purpose.
