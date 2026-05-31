<div class="sonda-section-hero" markdown>

<span class="sonda-section-hero__eyebrow">How-to</span>

<h1 class="sonda-section-hero__title">Guides</h1>

<p class="sonda-section-hero__subtitle">Task-shaped pages — pick the one that matches what you are trying to do. The <a href="tutorial/"><strong>Tutorial</strong></a> is the guided tour through every part of Sonda; the rest is organized by what you are testing: alert rules, ingest pipelines, network telemetry, real-data replay, or operational scale.</p>

</div>

!!! tip "New to Sonda?"
    Start with the [**Tutorial**](../get-started/quickstart.md) -- a seven-page walkthrough of
    generators, encoders, sinks, log generation, scheduling, multi-scenario
    runs, and the Server API. Everything below assumes you have already done
    that, or you know what you are looking for.

## Browse by goal

<div class="grid cards" markdown>

-   :material-bookshelf: __[Catalog & packs](../build/catalogs-and-packs.md)__

    Pre-built scenarios you can run instantly, plus the building blocks that
    compose them. Start with [Built-in Scenarios](../build/catalogs-and-packs.md), then
    [Dynamic Labels](../build/scheduling.md), [Examples](../test/examples.md), and
    [Metric Packs](../build/catalogs-and-packs.md).

-   :material-bell-alert: __[Alert testing](../test/alert-testing.md)__

    Triggering, resolving, and validating alert rules with the right metric
    shape. The [overview](../test/alert-testing.md) maps each alert pattern to the
    right generator -- thresholds, resolution, correlation, cardinality,
    histograms, recording rules, and the full pipeline.

-   :material-pipe: __[Pipelines & scale](../test/end-to-end-pipelines.md)__

    Validating ingest changes, sizing backends, and end-to-end flow. Covers
    [pipeline validation](../test/end-to-end-pipelines.md),
    [synthetic monitoring](../test/synthetic-monitoring.md),
    [capacity planning](../test/capacity-planning.md), and
    [E2E testing](../test/end-to-end-pipelines.md).

-   :material-router-network: __[Network telemetry](../test/network-device-telemetry.md)__

    Modeling network devices and exercising automation responses.
    [Device telemetry](../test/network-device-telemetry.md) covers routers, switches,
    and link cascades; [automation testing](../test/network-automation-testing.md)
    covers remediation flows that react to those alerts.

-   :material-database-import: __[Importing real data](../import/from-csv.md)__

    Turning recorded series into reusable scenarios.
    [CSV import](../import/from-csv.md) detects patterns and generates portable YAML;
    [Grafana CSV replay](../import/grafana-exports.md) reproduces the original series
    bit-for-bit.

-   :material-tools: __[Troubleshooting](../reference/troubleshooting.md)__

    Diagnostics for connection refused, empty backends, schema mismatches, and
    the `localhost` trap.

</div>
