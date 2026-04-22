# Kubernetes

Sonda includes a Helm chart for deploying `sonda-server` to any Kubernetes cluster. The chart
creates a Deployment with health probes, a ClusterIP Service with a named `http` port, and
optional scenario injection via ConfigMap.

## Prerequisites

You need a running Kubernetes cluster and these CLI tools installed:

- `kubectl` -- configured to talk to your cluster
- `helm` -- v3.x

If you don't have a cluster yet, the [Synthetic Monitoring](../guides/synthetic-monitoring.md#set-up-a-local-kubernetes-cluster) guide covers lightweight
local options (kind, k3d, minikube, OrbStack) with step-by-step setup instructions.

## Install the chart

```bash
helm install sonda ./helm/sonda
```

Wait for the pod to become ready:

```bash
kubectl get pods -l app.kubernetes.io/name=sonda -w
```

You should see `1/1 Running` within 15--20 seconds. The chart defaults to
`ghcr.io/davidban77/sonda:<!--x-release-please-version-->2.0.0<!--x-release-please-end-->` (the chart's `appVersion`). Pin a different version with
`--set image.tag=<version>`.

!!! tip "Deploy to a dedicated namespace"
    Keep Sonda isolated from your application workloads:

    ```bash
    kubectl create namespace sonda
    helm install sonda ./helm/sonda -n sonda
    ```

    All `kubectl` commands in this page assume the default namespace. Add `-n sonda` if you
    installed into a different one.

## Chart values reference

The chart ships with sensible defaults. Override any value with `--set` flags or a
`-f values.yaml` file.

### Image

| Value | Default | Description |
|-------|---------|-------------|
| `image.repository` | `ghcr.io/davidban77/sonda` | Container image registry and name |
| `image.tag` | `""` (uses `appVersion`: <!--x-release-please-version-->`2.0.0`<!--x-release-please-end-->) | Image tag to pull |
| `image.pullPolicy` | `IfNotPresent` | Kubernetes image pull policy |
| `imagePullSecrets` | `[]` | Secrets for private registries |

### Server

| Value | Default | Description |
|-------|---------|-------------|
| `server.port` | `8080` | Port `sonda-server` listens on inside the container |
| `server.bind` | `0.0.0.0` | Bind address |

### Service

| Value | Default | Description |
|-------|---------|-------------|
| `service.type` | `ClusterIP` | Kubernetes Service type (`ClusterIP`, `NodePort`, `LoadBalancer`) |
| `service.port` | `8080` | Service port exposed to the cluster |

The Service exposes a named port called `http`, which is what ServiceMonitor and Ingress
resources reference.

### Resources

| Value | Default | Description |
|-------|---------|-------------|
| `resources.requests.cpu` | `100m` | CPU request |
| `resources.requests.memory` | `128Mi` | Memory request |
| `resources.limits.cpu` | `500m` | CPU limit |
| `resources.limits.memory` | `256Mi` | Memory limit |

These defaults are sized for light workloads (a handful of scenarios at moderate rates). If
you run many concurrent scenarios or high event rates, increase the limits:

```bash
helm install sonda ./helm/sonda \
  --set resources.requests.cpu=200m \
  --set resources.limits.cpu=1000m \
  --set resources.limits.memory=512Mi
```

### Security

| Value | Default | Description |
|-------|---------|-------------|
| `podSecurityContext` | `{}` | Pod-level security context (e.g., `fsGroup`) |
| `securityContext` | `{}` | Container-level security context (e.g., `runAsNonRoot`, `readOnlyRootFilesystem`, `capabilities`) |

### Authentication

| Value | Default | Description |
|-------|---------|-------------|
| `server.auth.enabled` | `false` | Enable API key authentication on `/scenarios/*` endpoints |
| `server.auth.existingSecret` | `""` | Name of an existing Secret containing the API key |
| `server.auth.secretKey` | `api-key` | Key within the Secret that holds the API key value |

When `server.auth.enabled` is `true`, the chart injects `SONDA_API_KEY` into the container
from the referenced Secret. See [API key authentication](#api-key-authentication) for setup
instructions.

### Scheduling

| Value | Default | Description |
|-------|---------|-------------|
| `replicaCount` | `1` | Number of Deployment replicas (ignored when HPA is enabled) |
| `nodeSelector` | `{}` | Node selector constraints |
| `tolerations` | `[]` | Pod tolerations |
| `affinity` | `{}` | Pod affinity/anti-affinity rules |

### Autoscaling (HPA)

| Value | Default | Description |
|-------|---------|-------------|
| `autoscaling.enabled` | `false` | Enable HorizontalPodAutoscaler |
| `autoscaling.minReplicas` | `1` | Minimum replica count |
| `autoscaling.maxReplicas` | `5` | Maximum replica count |
| `autoscaling.targetCPUUtilizationPercentage` | `80` | Target CPU utilization |
| `autoscaling.targetMemoryUtilizationPercentage` | (unset) | Target memory utilization |

### Pod Disruption Budget

| Value | Default | Description |
|-------|---------|-------------|
| `podDisruptionBudget.enabled` | `false` | Enable PodDisruptionBudget |
| `podDisruptionBudget.minAvailable` | `1` | Minimum available pods during disruption |
| `podDisruptionBudget.maxUnavailable` | (unset) | Maximum unavailable pods during disruption |

### Ingress

| Value | Default | Description |
|-------|---------|-------------|
| `ingress.enabled` | `false` | Enable Ingress resource |
| `ingress.className` | `""` | Ingress class name |
| `ingress.annotations` | `{}` | Ingress annotations |
| `ingress.hosts` | `[{host: sonda.local, paths: [{path: /, pathType: Prefix}]}]` | Ingress host rules |
| `ingress.tls` | `[]` | TLS configuration |

### ServiceMonitor

| Value | Default | Description |
|-------|---------|-------------|
| `serviceMonitor.enabled` | `false` | Enable Prometheus Operator ServiceMonitor |
| `serviceMonitor.interval` | `30s` | Scrape interval |
| `serviceMonitor.scrapeTimeout` | `10s` | Scrape timeout |
| `serviceMonitor.path` | `/health` | Metrics endpoint path |
| `serviceMonitor.additionalLabels` | `{}` | Extra labels on the ServiceMonitor resource |

### Scenarios (ConfigMap)

| Value | Default | Description |
|-------|---------|-------------|
| `scenarios` | `{}` | Map of filename to YAML content, mounted at `/scenarios` |

See [Configuring scenarios](#configuring-scenarios) below.

## Configuring scenarios

You can load scenarios into `sonda-server` two ways: bake them into the Helm release via
ConfigMap, or submit them at runtime via the API.

### ConfigMap (deploy-time)

Define scenarios under the `scenarios` key in a values file. Each key becomes a file mounted
at `/scenarios` inside the container:

```yaml title="my-values.yaml"
scenarios:
  cpu-metrics.yaml: |
    name: cpu_usage
    rate: 100
    duration: 30s
    generator:
      type: sine
      amplitude: 50
      period_secs: 60
      offset: 50
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

```bash
helm install sonda ./helm/sonda -f my-values.yaml
```

The Deployment template includes a `checksum/scenarios` annotation, so changing scenario
content in your values file triggers an automatic pod rollout on `helm upgrade`.

See [Scenario Files](../configuration/scenario-file.md) for the full YAML schema.

### API (runtime)

Once `sonda-server` is running, you can submit scenarios dynamically without redeploying:

```bash
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/basic-metrics.yaml \
  http://localhost:8080/scenarios
```

This is useful for long-running synthetic monitoring where you rotate scenarios over time.
See [Server API](sonda-server.md) for the full endpoint reference and the
[Synthetic Monitoring](../guides/synthetic-monitoring.md) guide for operational patterns.

## Health probes

The Deployment configures both liveness and readiness probes against `GET /health`:

| Probe | Initial delay | Period | Timeout | Failure threshold |
|-------|--------------|--------|---------|-------------------|
| Liveness | 5s | 10s | 3s | 3 |
| Readiness | 2s | 5s | 3s | 3 |

The `/health` endpoint returns `{"status":"ok"}` with HTTP 200 when the server is running.
Pods restart automatically if the server becomes unresponsive.

## Accessing the server

### Port-forward

The quickest way to reach `sonda-server` from your workstation:

```bash
kubectl port-forward svc/sonda 8080:8080
```

Then interact with the API at `http://localhost:8080`:

```bash
# Health check
curl http://localhost:8080/health

# Start a scenario
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @examples/basic-metrics.yaml \
  http://localhost:8080/scenarios

# List running scenarios
curl http://localhost:8080/scenarios
```

### In-cluster DNS

Other pods in the cluster can reach `sonda-server` using the Service DNS name:

```
sonda.<namespace>.svc.cluster.local:8080
```

For example, a Prometheus instance in the same namespace can scrape
`http://sonda:8080/scenarios/<id>/metrics` directly.

## Prometheus scraping

Each running scenario exposes metrics at `GET /scenarios/{id}/metrics` in Prometheus text
exposition format. You can configure Prometheus (or vmagent) to scrape this endpoint.

### Static scrape config

```yaml title="prometheus-scrape.yaml"
scrape_configs:
  - job_name: sonda
    scrape_interval: 15s
    metrics_path: /scenarios/<SCENARIO_ID>/metrics
    static_configs:
      - targets: ["sonda.default.svc:8080"]
```

Replace `<SCENARIO_ID>` with the UUID returned by `POST /scenarios`.

### ServiceMonitor

If you run the [Prometheus Operator](https://prometheus-operator.dev/) (typically via
kube-prometheus-stack), the chart includes an optional ServiceMonitor template. Enable it
with:

```bash
helm install sonda ./helm/sonda --set serviceMonitor.enabled=true
```

See the [ServiceMonitor](#servicemonitor-1) values reference for all options
(`interval`, `scrapeTimeout`, `path`, `additionalLabels`).

Alternatively, apply a custom ServiceMonitor manually for full control:

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

The `port: http` field matches the named port on the Sonda Service.

!!! warning "One path per endpoint"
    Each ServiceMonitor endpoint scrapes a single `path`. If you run multiple scenarios, add
    one `endpoints` entry per scenario ID. For dynamic discovery, consider a script that
    queries `GET /scenarios` and regenerates the ServiceMonitor.

## API key authentication

Sonda-server supports optional bearer token authentication on all `/scenarios/*` endpoints.
When enabled, clients must include an `Authorization: Bearer <key>` header. The `/health`
endpoint stays public so liveness and readiness probes work without credentials.

For the full authentication behavior (error responses, protected vs. public endpoints), see the
[Server API Authentication](sonda-server.md#authentication) section.

### Create a Secret

Store your API key in a Kubernetes Secret:

```yaml title="sonda-api-key.yaml"
apiVersion: v1
kind: Secret
metadata:
  name: sonda-api-key
type: Opaque
stringData:
  api-key: "your-secret-key-here"
```

```bash
kubectl apply -f sonda-api-key.yaml
```

!!! tip "Generate a random key"
    ```bash
    kubectl create secret generic sonda-api-key \
      --from-literal=api-key="$(openssl rand -base64 32)"
    ```

### Enable auth in the Helm chart

Point the chart at your Secret:

```bash
helm install sonda ./helm/sonda \
  --set server.auth.enabled=true \
  --set server.auth.existingSecret=sonda-api-key
```

Or in a values file:

```yaml title="my-values.yaml"
server:
  auth:
    enabled: true
    existingSecret: sonda-api-key
    secretKey: api-key          # default; change if your Secret uses a different key
```

The chart sets `SONDA_API_KEY` in the container environment from the Secret. On startup you
will see:

```text
INFO sonda_server: API key authentication enabled for /scenarios/* endpoints
```

### Authenticated API calls

Once auth is enabled, include the bearer token in all `/scenarios/*` requests:

```bash
# Port-forward to reach the server
kubectl port-forward svc/sonda 8080:8080

# Start a scenario (requires auth)
curl -X POST \
  -H "Authorization: Bearer your-secret-key-here" \
  -H "Content-Type: text/yaml" \
  --data-binary @examples/basic-metrics.yaml \
  http://localhost:8080/scenarios

# Health check (always public)
curl http://localhost:8080/health
```

### Prometheus scraping with auth

When authentication is enabled, the `/scenarios/{id}/metrics` endpoint also requires a
bearer token. Add the token to your Prometheus scrape config:

=== "Static scrape config"

    ```yaml title="prometheus-scrape.yaml"
    scrape_configs:
      - job_name: sonda
        scrape_interval: 15s
        metrics_path: /scenarios/<SCENARIO_ID>/metrics
        bearer_token: "your-secret-key-here"
        static_configs:
          - targets: ["sonda.default.svc:8080"]
    ```

=== "Bearer token from file"

    ```yaml title="prometheus-scrape.yaml"
    scrape_configs:
      - job_name: sonda
        scrape_interval: 15s
        metrics_path: /scenarios/<SCENARIO_ID>/metrics
        bearer_token_file: /etc/prometheus/sonda-token
        static_configs:
          - targets: ["sonda.default.svc:8080"]
    ```

For a ServiceMonitor, add `bearerTokenSecret` to the endpoint:

```yaml title="sonda-servicemonitor.yaml"
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: sonda
  labels:
    release: prometheus
spec:
  selector:
    matchLabels:
      app.kubernetes.io/name: sonda
  endpoints:
    - port: http
      interval: 15s
      path: /scenarios/<SCENARIO_ID>/metrics
      bearerTokenSecret:
        name: sonda-api-key
        key: api-key
```

!!! warning "Same Secret, same namespace"
    The `bearerTokenSecret` must reference a Secret in the **same namespace** as the
    Prometheus instance, not the Sonda namespace. If they differ, copy the Secret or use
    `bearer_token_file` with a mounted volume instead.

## Upgrading

Update your release after changing values or pulling a new chart version:

```bash
# Upgrade with new values
helm upgrade sonda ./helm/sonda -f my-values.yaml

# Upgrade to a new image version
helm upgrade sonda ./helm/sonda --set image.tag=<!--x-release-please-version-->2.0.0<!--x-release-please-end-->
```

If your values file includes `scenarios`, the ConfigMap checksum annotation triggers an
automatic pod rollout -- no manual restart needed.

!!! info "Rollback"
    Helm keeps release history. Roll back to the previous version with:

    ```bash
    helm rollback sonda
    ```

## Uninstalling

```bash
helm uninstall sonda
```

This removes the Deployment, Service, ConfigMap (if created), and all associated resources.
Add `-n <namespace>` if you installed into a non-default namespace.

## What's next

- [Synthetic Monitoring guide](../guides/synthetic-monitoring.md) -- deploy Sonda on Kubernetes, submit long-running scenarios, scrape with Prometheus, and build Grafana dashboards
- [Server API](sonda-server.md) -- full endpoint reference for `sonda-server`
- [Docker](docker.md) -- Docker image and Compose stacks for local development
- [Scenario Files](../configuration/scenario-file.md) -- full YAML schema for scenario configuration
