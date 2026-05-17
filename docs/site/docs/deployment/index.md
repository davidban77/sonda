<div class="sonda-section-hero" markdown>

<span class="sonda-section-hero__eyebrow">Runtime</span>

<h1 class="sonda-section-hero__title">Deployment</h1>

<p class="sonda-section-hero__subtitle">Where the Sonda process lives — on your laptop, in CI, in Docker, in Kubernetes, or as a long-lived HTTP control plane. Pick the shape that matches whether scenarios are human-driven, automation-driven, or part of an always-on synthetic-monitoring fleet.</p>

</div>

Sonda runs three ways: as a one-shot CLI binary you invoke from a shell, as a
long-lived `sonda-server` HTTP control plane, or as a containerized deployment in
Docker or Kubernetes. This section covers the runtime side — where the process
lives, how it reaches your backends, and how scenarios are submitted. For *what*
the scenarios contain, see [Configuration](../configuration/index.md).

!!! warning "Read this first"
    [**Endpoints & networking**](endpoints.md) covers the rules for `localhost`,
    Compose service names, Docker Desktop's `host.docker.internal`, and Kubernetes
    Service DNS. The `localhost` trap catches most first-time `sonda-server`
    users -- skim it before you change a sink URL.

## Runtimes

<div class="grid cards" markdown>

-   :material-docker: __[Docker](docker.md)__

    Pulling the image, dispatch between `sonda` and `sonda-server`, and the
    bundled Compose stacks for VictoriaMetrics, Loki, Kafka, and Grafana.

-   :material-kubernetes: __[Kubernetes](kubernetes.md)__

    The Helm chart, Deployment + Service shape, health probes, ConfigMap-injected
    scenarios, and cluster-DNS sink URLs.

-   :material-server-network: __[Server API](sonda-server.md)__

    The REST surface for submitting scenarios, inspecting live stats, scraping
    metrics, and stopping runs over HTTP.

</div>
