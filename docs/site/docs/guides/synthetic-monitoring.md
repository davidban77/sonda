# Synthetic Monitoring

Your dashboards look great -- until the data source goes quiet and you stare at flat lines
wondering if it's a real outage or a broken scrape config. Long-running synthetic monitoring
gives you a persistent baseline of known metrics flowing through your stack, so you can tell
"no data" from "data stopped arriving" at a glance.

This guide walks you through deploying `sonda-server` on Kubernetes, submitting scenarios that
run for hours or days, scraping the generated metrics with Prometheus, and building Grafana
dashboards to monitor both the synthetic data and Sonda itself.

**What you need:**

- A Kubernetes cluster (local or remote)
- `kubectl` and `helm` CLI tools installed
- `curl` and `jq` for API calls
- Familiarity with Prometheus scraping and Grafana dashboards

---

## Set up a local Kubernetes cluster

If you already have a cluster (EKS, GKE, AKS, or an existing local one), skip to
[Deploy sonda-server](#deploy-sonda-server).

For local development and testing, you need a lightweight Kubernetes distribution that runs
on your workstation. Here are the most practical options:

| Tool | Best for | Runs on |
|------|----------|---------|
| [kind](https://kind.sigs.k8s.io/) | CI pipelines, fast throwaway clusters | Linux, macOS, Windows (WSL2) |
| [k3d](https://k3d.io/) | k3s in Docker, built-in registry support | Linux, macOS, Windows (WSL2) |
| [minikube](https://minikube.sigs.k8s.io/) | Broad driver support, add-on ecosystem | Linux, macOS, Windows (WSL2) |
| [OrbStack](https://orbstack.dev/) | Native macOS experience, low resource usage | macOS only |

All four require Docker (or a compatible container runtime) installed and running.

=== "kind"

    [kind](https://kind.sigs.k8s.io/) runs Kubernetes nodes as Docker containers. It starts
    in under 30 seconds and is the lightest option.

    ```bash
    # Install (macOS/Linux)
    brew install kind

    # Or download the binary directly
    # https://kind.sigs.k8s.io/docs/user/quick-start/#installation

    # Create a cluster
    kind create cluster --name sonda-lab

    # Verify
    kubectl cluster-info --context kind-sonda-lab
    ```

    !!! tip "Port mapping for kind"
        kind clusters don't expose container ports to the host by default. If you need
        NodePort access (for Prometheus or Grafana outside the cluster), create the cluster
        with a config:

        ```yaml title="kind-config.yaml"
        kind: Cluster
        apiVersion: kind.x-k8s.io/v1alpha4
        nodes:
          - role: control-plane
            extraPortMappings:
              - containerPort: 30080
                hostPort: 30080
                protocol: TCP
        ```

        ```bash
        kind create cluster --name sonda-lab --config kind-config.yaml
        ```

=== "k3d"

    [k3d](https://k3d.io/) wraps k3s (Rancher's lightweight Kubernetes) inside Docker. It
    supports built-in port mapping and a local image registry out of the box.

    ```bash
    # Install (macOS/Linux)
    brew install k3d

    # Create a cluster with port mapping
    k3d cluster create sonda-lab -p "8080:80@loadbalancer"

    # Verify
    kubectl cluster-info
    ```

=== "minikube"

    [minikube](https://minikube.sigs.k8s.io/) is the most established option. It supports
    Docker, Hyperkit, Hyper-V, and other drivers.

    ```bash
    # Install (macOS/Linux)
    brew install minikube

    # Start with Docker driver (recommended)
    minikube start --driver=docker --profile sonda-lab

    # Verify
    kubectl cluster-info --context sonda-lab
    ```

    !!! info "Windows WSL2"
        On Windows, install minikube inside your WSL2 distribution and use the Docker
        driver. Make sure Docker Desktop's WSL2 backend is enabled. The same commands
        apply inside the WSL2 terminal.

=== "OrbStack (macOS)"

    [OrbStack](https://orbstack.dev/) provides a native macOS Kubernetes experience with
    minimal resource usage. It runs a single-node k8s cluster that starts automatically.

    ```bash
    # Install
    brew install orbstack

    # Kubernetes is enabled by default -- just verify
    kubectl cluster-info
    ```

Once your cluster is running and `kubectl get nodes` shows a `Ready` node, you're set.

---

## Deploy sonda-server

Sonda includes a Helm chart that deploys `sonda-server` as a Kubernetes Deployment with health
probes, a ClusterIP Service, and optional scenario injection via ConfigMap.

```bash
helm install sonda ./helm/sonda
```

Wait for the pod to become ready:

```bash
kubectl get pods -l app.kubernetes.io/name=sonda -w
```

You should see `1/1 Running` within 15--20 seconds. The Deployment configures liveness and
readiness probes against `GET /health`, so Kubernetes restarts the pod automatically if the
server becomes unresponsive.

??? info "Customizing the deployment"
    Override common settings with `--set`:

    ```bash
    # Pin a specific image version
    helm install sonda ./helm/sonda --set image.tag=0.4.0

    # Custom port and resource limits
    helm install sonda ./helm/sonda \
      --set server.port=9090 \
      --set resources.requests.cpu=200m \
      --set resources.limits.memory=512Mi
    ```

    See [Kubernetes deployment](../deployment/kubernetes.md) for the full chart reference.

Verify the server is healthy by port-forwarding to it:

```bash
kubectl port-forward svc/sonda 8080:8080 &
curl http://localhost:8080/health
# {"status":"ok"}
```

Now let's submit some long-running scenarios.

---

## Submit long-running scenarios

A long-running scenario is simply a scenario YAML **without a `duration` field**. It runs
indefinitely until you stop it with `DELETE /scenarios/{id}`.

```yaml title="examples/long-running-metrics.yaml"
version: 2

defaults:
  rate: 10
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: continuous_cpu
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60
      offset: 50.0
    labels:
      instance: api-server-01
      job: sonda
```

Submit it to the server:

```bash
ID=$(curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/long-running-metrics.yaml \
  http://localhost:8080/scenarios | jq -r '.id')

echo "Scenario started: $ID"
```

The scenario runs in a background thread inside the server. Submit as many as you need --
each gets its own thread and scrape endpoint.

!!! tip "Multiple scenarios for richer coverage"
    Submit several scenarios with different shapes to simulate a realistic environment:
    a sine wave for CPU, a step counter for requests, a constant for an `up` gauge.
    Each scenario gets its own `/scenarios/{id}/metrics` endpoint that Prometheus can
    scrape independently.

To verify it's running:

```bash
# List all running scenarios
curl -s http://localhost:8080/scenarios | jq '.[] | {id, name, status}'

# Check live stats for your scenario
curl -s http://localhost:8080/scenarios/$ID/stats | jq .
```

For the full API reference, see [Server API](../deployment/sonda-server.md).

---

## Scrape metrics with Prometheus

Each running scenario exposes its metrics at `GET /scenarios/{id}/metrics` in Prometheus text
exposition format. You can point Prometheus (or any compatible scraper like vmagent) at this
endpoint.

### Static scrape config

If you know the scenario ID ahead of time, configure a static scrape job:

```yaml title="prometheus-scrape.yaml"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    metrics_path: /scenarios/<SCENARIO_ID>/metrics
    static_configs:
      - targets: ["sonda.default.svc:8080"]
```

Replace `<SCENARIO_ID>` with the UUID returned by `POST /scenarios`. The target address uses
the Kubernetes Service DNS name (`sonda.<namespace>.svc`).

### Prometheus ServiceMonitor

If you run the [Prometheus Operator](https://prometheus-operator.dev/) (kube-prometheus-stack),
you can create a `ServiceMonitor` to auto-discover sonda-server. The Sonda Helm chart does
not include a ServiceMonitor template today, so create one manually:

```yaml title="sonda-servicemonitor.yaml"
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: sonda
  labels:
    release: prometheus  # must match your Prometheus Operator's selector
spec:
  selector:
    matchLabels:
      app.kubernetes.io/name: sonda
  endpoints:
    - port: http
      interval: 15s
      path: /scenarios/<SCENARIO_ID>/metrics
```

```bash
kubectl apply -f sonda-servicemonitor.yaml
```

!!! warning "One path per ServiceMonitor endpoint"
    Each ServiceMonitor endpoint scrapes a single `path`. If you have multiple running
    scenarios, you need one `endpoints` entry per scenario ID (each with a different
    `path`). For dynamic discovery, consider using a relabeling rule or a script that
    queries `GET /scenarios` and updates the scrape config.

??? tip "Using vmagent instead of Prometheus"
    vmagent supports the same `scrape_configs` format. Point it at sonda-server using
    a standard static scrape config. If you're already running the
    [VictoriaMetrics Docker Compose stack](../deployment/docker.md#victoriametrics-stack),
    add sonda-server as a scrape target in the vmagent config.

---

## Build Grafana dashboards

Once Prometheus is scraping your synthetic metrics, you can visualize them in Grafana.

Sonda ships with a **Sonda Overview** dashboard (`docker/grafana/dashboards/sonda-overview.json`)
that shows metric values, event rates, and gap/burst indicators. You can import it directly
into any Grafana instance connected to a Prometheus-compatible datasource.

### Import the shipped dashboard

1. Open Grafana and go to **Dashboards > Import**.
2. Upload `docker/grafana/dashboards/sonda-overview.json` or paste its contents.
3. Select your Prometheus datasource when prompted.
4. The dashboard uses template variables `$datasource` and `$job` -- set `$job` to `sonda`
   (or whatever `job` label your scenarios use).

### Build a custom panel

For a focused monitoring panel, create a new dashboard with a time series visualization
and query your synthetic metric directly:

```promql
continuous_cpu{job="sonda", instance="api-server-01"}
```

Add a second panel showing the emission rate over time:

```promql
rate(continuous_cpu{job="sonda"}[1m])
```

!!! tip "Threshold lines"
    Add a fixed threshold line in the Grafana panel options (e.g., at 90 for a CPU alert
    threshold). This gives you a visual reference for when the sine wave crosses your alert
    boundary.

With dashboards in place, you can see your synthetic data flowing at a glance. Next, let's
make sure Sonda itself stays healthy.

---

## Monitor sonda-server health

The stats API tells you whether each scenario is emitting as expected. Poll it periodically
or build monitoring around it.

### Health endpoint

The simplest check -- Kubernetes already uses this for liveness and readiness probes:

```bash
curl http://localhost:8080/health
# {"status":"ok"}
```

### Per-scenario stats

The `/scenarios/{id}/stats` endpoint returns live stats including event counts, current
emission rate, bytes emitted, error counts, and gap/burst state:

```bash
curl -s http://localhost:8080/scenarios/$ID/stats | jq .
```

Key fields to watch:

| Field | What it tells you |
|-------|-------------------|
| `total_events` | Running count of emitted events. Should increase steadily. For batching sinks (`loki`, `http_push`, `remote_write`, `otlp_grpc`, `kafka`) this counts *buffered* writes, not deliveries — pair it with the fields below to confirm data is actually landing. |
| `current_rate` | Actual emission rate. Compare against your scenario's `rate`. |
| `errors` | Error count. Should be 0 for healthy scenarios. |
| `uptime` | Time since scenario started. Confirms it hasn't restarted. |
| `last_successful_write_at` | Wall-clock time (Unix nanos) of the most recent successful delivery. `null` means nothing has ever landed; a stale value means the sink is wedged. |
| `consecutive_failures` | Failure streak since the last successful delivery. Resets to `0` on the next successful flush. Non-zero with a stale `last_successful_write_at` is the wedged-sink signature. |
| `total_sink_failures` | Lifetime sink-error count. Monotonic. Useful as a Prometheus alert input (`increase(...)[5m]`). |

The full reference for these fields, including the `last_sink_error` text and the `state`/gap/burst flags, lives in [Self-observability via /stats](../deployment/sonda-server.md#self-observability-via-stats).

If you only check one signal across the whole server, check `degraded` on `GET /scenarios` — it combines the three sink-failure fields above into a single boolean per scenario, true when delivery has stalled for more than 30 seconds. The scripted health check below uses it directly.

### List all scenarios

Check that all your submitted scenarios are still running:

```bash
curl -s http://localhost:8080/scenarios | jq '.[] | {name, status}'
```

If a scenario shows `status: "stopped"` unexpectedly, re-submit it.

??? tip "Scripting a health check"
    Wrap the check in a script that fails loudly when any scenario stops delivering. Read `degraded` from `GET /scenarios` — totalling `total_events` would silently miss a wedged batching sink, because buffered writes still increment the counter while nothing reaches the backend.

    ```bash title="check-sonda.sh"
    #!/bin/bash
    set -euo pipefail
    SONDA_URL="${SONDA_URL:-http://localhost:8080}"

    # Pull the list once and read the precomputed degraded flag per scenario.
    bad=$(curl -sS "$SONDA_URL/scenarios" |
          jq -r '.scenarios[] | select(.degraded) | "\(.name) (\(.id))"')

    if [[ -n "$bad" ]]; then
      echo "Degraded scenarios:"
      echo "$bad"
      exit 1
    fi

    echo "All scenarios delivering."
    ```

    Exit code `1` makes this drop-in for a Kubernetes readiness probe, a cron alert, or a CI smoke step. If you need the raw counters (per-scenario rate, failure streak, last delivery timestamp) for a richer report, follow up with `GET /scenarios/$id/stats` on each degraded ID.

---

## Rotate scenarios

Test patterns change over time. You might start with a sine wave to validate dashboards, then
switch to a sequence generator to test alert thresholds. Scenario rotation is straightforward:
stop the old scenario and start a new one.

### Stop and replace

```bash
# Stop the running scenario
curl -s -X DELETE http://localhost:8080/scenarios/$ID | jq .
# {"id":"...","status":"stopped","total_events":12345}

# Submit a new scenario
NEW_ID=$(curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/sequence-alert-test.yaml \
  http://localhost:8080/scenarios | jq -r '.id')

echo "New scenario: $NEW_ID"
```

!!! warning "Scrape config update required"
    When you replace a scenario, the new scenario gets a different UUID. If your Prometheus
    scrape config uses the scenario ID in the `metrics_path`, you need to update it to
    point at the new ID.

### Scripted rotation

For scheduled rotations (e.g., different patterns during business hours vs. overnight),
wrap the stop-and-start sequence in a cron job or Kubernetes CronJob:

```bash title="rotate-scenario.sh"
#!/bin/bash
SONDA_URL="http://localhost:8080"
SCENARIO_FILE="$1"

# Stop all running scenarios
for id in $(curl -s "$SONDA_URL/scenarios" | jq -r '.[].id'); do
  curl -s -X DELETE "$SONDA_URL/scenarios/$id" > /dev/null
done

# Start the new scenario
curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary "@$SCENARIO_FILE" \
  "$SONDA_URL/scenarios" | jq .
```

```bash
# Rotate to a new pattern
./rotate-scenario.sh examples/long-running-metrics.yaml
```

---

## Alert on Sonda itself

Synthetic monitoring is only useful if you know when it breaks. If Sonda stops emitting,
your dashboards go silent, and you need to distinguish "Sonda died" from "real outage."

### Detect missing synthetic data

Create an alert rule that fires when your synthetic metric disappears. This uses the
`absent()` function in PromQL:

```yaml title="sonda-watchdog-rules.yaml"
groups:
  - name: sonda-watchdog
    interval: 30s
    rules:
      - alert: SondaSyntheticDataMissing
        expr: absent(continuous_cpu{job="sonda"})
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "Synthetic monitoring data missing"
          description: >
            The metric continuous_cpu from Sonda has not been seen for 2 minutes.
            Either sonda-server is down or the scenario has stopped.
```

This fires if `continuous_cpu{job="sonda"}` hasn't been scraped for 2 minutes. Adjust the
`for:` duration based on your scrape interval and tolerance for gaps.

### Monitor the pod itself

Since sonda-server runs as a Kubernetes Deployment with health probes, standard kube-state-metrics
alerts cover pod-level failures:

```yaml
- alert: SondaPodNotReady
  expr: kube_pod_status_ready{pod=~"sonda.*", condition="true"} == 0
  for: 5m
  labels:
    severity: critical
  annotations:
    summary: "Sonda pod is not ready"
```

### Layer your alerting

A robust setup uses both layers:

| Layer | What it catches | Alert |
|-------|----------------|-------|
| Pod health | Server crash, OOM kill, image pull failure | `SondaPodNotReady` |
| Metric presence | Scenario stopped, scrape misconfigured, data pipeline broken | `SondaSyntheticDataMissing` |

The pod alert fires fast (infrastructure issue). The metric-absent alert fires when the data
pipeline is broken anywhere between Sonda and Prometheus -- which is exactly the kind of
problem synthetic monitoring exists to catch.

!!! info "Testing these alerts with Sonda"
    You can validate these watchdog rules using the same patterns from the
    [Alert Testing](alert-testing.md) and [Alerting Pipeline](alerting-pipeline.md) guides.
    Submit a scenario, verify the alert stays silent, then `DELETE` the scenario and watch
    the `absent()` alert fire.

---

## Quick reference

| Task | Command |
|------|---------|
| Deploy sonda-server | `helm install sonda ./helm/sonda` |
| Submit a scenario | `curl -X POST -H "Content-Type: text/yaml" --data-binary @scenario.yaml http://localhost:8080/scenarios` |
| List running scenarios | `curl http://localhost:8080/scenarios` |
| Check scenario stats | `curl http://localhost:8080/scenarios/<id>/stats` |
| Scrape metrics | `curl http://localhost:8080/scenarios/<id>/metrics` |
| Stop a scenario | `curl -X DELETE http://localhost:8080/scenarios/<id>` |
| Health check | `curl http://localhost:8080/health` |

**Related pages:**

- [Kubernetes deployment](../deployment/kubernetes.md) -- Helm chart values and configuration
- [Server API](../deployment/sonda-server.md) -- full endpoint reference
- [Alert Testing](alert-testing.md) -- generator patterns for alert threshold testing
- [Alerting Pipeline](alerting-pipeline.md) -- end-to-end alerting with vmalert and Alertmanager
