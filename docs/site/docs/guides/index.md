# Guides

Task-shaped pages: pick the one that matches what you are trying to do.

The [**Tutorial**](tutorial.md) is the guided tour through every part of Sonda.
The rest of this section is organized by what you are testing: alert rules,
ingest pipelines, network telemetry, real-data replay, or operational scale.

!!! tip "New to Sonda?"
    Start with the [**Tutorial**](tutorial.md) -- a seven-page walkthrough of
    generators, encoders, sinks, log generation, scheduling, multi-scenario
    runs, and the Server API. Everything below assumes you have already done
    that, or you know what you are looking for.

## Browse by goal

<div class="grid cards" markdown>

-   :material-bookshelf: __[Catalog & packs](scenarios.md)__

    Pre-built scenarios you can run instantly, plus the building blocks that
    compose them. Start with [Built-in Scenarios](scenarios.md), then
    [Dynamic Labels](dynamic-labels.md), [Examples](examples.md), and
    [Metric Packs](metric-packs.md).

-   :material-bell-alert: __[Alert testing](alert-testing.md)__

    Triggering, resolving, and validating alert rules with the right metric
    shape. The [overview](alert-testing.md) maps each alert pattern to the
    right generator -- thresholds, resolution, correlation, cardinality,
    histograms, recording rules, and the full pipeline.

-   :material-pipe: __[Pipelines & scale](pipeline-validation.md)__

    Validating ingest changes, sizing backends, and end-to-end flow. Covers
    [pipeline validation](pipeline-validation.md),
    [synthetic monitoring](synthetic-monitoring.md),
    [capacity planning](capacity-planning.md), and
    [E2E testing](e2e-testing.md).

-   :material-router-network: __[Network telemetry](network-device-telemetry.md)__

    Modeling network devices and exercising automation responses.
    [Device telemetry](network-device-telemetry.md) covers routers, switches,
    and link cascades; [automation testing](network-automation-testing.md)
    covers remediation flows that react to those alerts.

-   :material-database-import: __[Importing real data](csv-import.md)__

    Turning recorded series into reusable scenarios.
    [CSV import](csv-import.md) detects patterns and generates portable YAML;
    [Grafana CSV replay](grafana-csv-replay.md) reproduces the original series
    bit-for-bit.

-   :material-tools: __[Troubleshooting](troubleshooting.md)__

    Diagnostics for connection refused, empty backends, schema mismatches, and
    the `localhost` trap.

</div>
