---
title: Test pipelines
description: Use Sonda to drive alert rules, recording rules, and full observability pipelines.
---

# Test pipelines

Sonda exists so you can exercise your observability pipeline without waiting for production to break. Write the alert rule, shape a scenario that crosses the threshold for exactly the duration you care about, and watch whether the rule fires. Same idea for recording rules, capacity planning, synthetic monitoring, and end-to-end pipeline validation.

This section covers the patterns. Each page starts from a real failure mode you've probably seen on call, shows the Sonda scenario that reproduces it, and verifies the result against a real backend.

<div class="grid cards" markdown>

-   :material-bell-alert: __[Alert testing](alert-testing.md)__

    Six patterns in one page: thresholds, resolution, compound `A AND B`, cardinality explosion, incident replay, histogram-based latency alerts.

-   :material-calculator-variant: __[Recording rules](recording-rules.md)__

    Test that precomputed PromQL series produce the right values — sum rules, rate-based rules, multi-rule chains.

-   :material-source-pull: __[End-to-end pipelines](end-to-end-pipelines.md)__

    Four validation shapes in one page: a local alerting pipeline (metric → vmalert → Alertmanager → webhook), an encoder × sink coverage matrix, the alerting loop wired into CI, and lighter-weight production smoke checks (exit codes, line counts, multi-format diffs).

-   :material-monitor-eye: __[Synthetic monitoring](synthetic-monitoring.md)__

    Generate a steady baseline that lets you spot drift in dashboards, alerts, or ingest paths long before production traffic does.

-   :material-chart-bell-curve-cumulative: __[Capacity planning](capacity-planning.md)__

    Stress-test ingest paths with high-cardinality fleets, label-bomb spikes, and sustained high rates.

-   :material-router-network: __[Network device telemetry](network-device-telemetry.md)__

    Telegraf SNMP shapes from a single scenario file — interface counters, BGP neighbor state, environmental sensors.

-   :material-flask-outline: __[Network automation testing](network-automation-testing.md)__

    Drive Nautobot, Prefect, and other automation tooling with synthetic device telemetry.

-   :material-folder-open-outline: __[Examples](examples.md)__

    Annotated example scenarios from the `examples/` directory in the Sonda repo.

</div>
