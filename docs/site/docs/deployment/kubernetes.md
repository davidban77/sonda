# Kubernetes

Sonda includes a Helm chart for deploying `sonda-server` to Kubernetes clusters. The chart
configures health probes, scenario injection via ConfigMap, and follows Helm best practices
for labels and resource management.

## Installing the Chart

```bash
# Default values (port 8080, 1 replica)
helm install sonda ./helm/sonda

# Custom port
helm install sonda ./helm/sonda --set server.port=9090

# Custom resource limits
helm install sonda ./helm/sonda \
  --set resources.requests.cpu=200m \
  --set resources.limits.cpu=1000m
```

The chart pulls `ghcr.io/davidban77/sonda:latest` by default. Override the image tag
with `--set image.tag=0.3.0` to pin a specific version.

## Configuring Scenarios

You inject scenarios as a ConfigMap mounted at `/scenarios` inside the container. Define
them under the `scenarios` key in your values file:

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

Install with your values:

```bash
helm install sonda ./helm/sonda -f my-values.yaml
```

See [Scenario Files](../configuration/scenario-file.md) for the full YAML schema.

## Health Probes

The Deployment configures both liveness and readiness probes using `GET /health` on the
server port. This endpoint returns `{"status":"ok"}` with HTTP 200 when the server is
running, so pods restart automatically if the server becomes unresponsive.

## Accessing the Server

Use `kubectl port-forward` to reach the API from your workstation:

```bash
export POD_NAME=$(kubectl get pods -l "app.kubernetes.io/name=sonda" \
  -o jsonpath="{.items[0].metadata.name}")
kubectl port-forward $POD_NAME 8080:8080

# Health check
curl http://localhost:8080/health

# Start a scenario
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @scenario.yaml \
  http://localhost:8080/scenarios
```

For the full API reference, see [Server API](sonda-server.md).

## Uninstalling

```bash
helm uninstall sonda
```
