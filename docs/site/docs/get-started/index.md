---
title: Get started
description: Install Sonda, run your first scenario, and send synthetic telemetry to a real backend.
---

# Get started

This section takes you from no Sonda installed to a synthetic metric reaching a real Prometheus or Loki backend. Allow about fifteen minutes from end to end.

<div class="grid cards" markdown>

-   :material-rocket-launch: __[Quickstart](quickstart.md)__

    Install Sonda, generate a starter YAML file with `sonda new --template`, and stream a metric to stdout. Five minutes.

-   :material-shape-outline: __[Your first scenario](your-first-scenario.md)__

    The four parts of a Sonda scenario: scenario file, generator, encoder, sink. Each part comes with a small YAML example.

-   :material-connection: __[Send to a real backend](send-to-a-backend.md)__

    Point your scenario at Prometheus (`remote_write`) or Loki. Includes the `docker run` commands to start each backend locally.

</div>

!!! tip "Where to next"
    After you finish here, read [Build scenarios](../build/index.md) to write YAML you can check into git. To produce data that drives your alert rules, see [Test pipelines](../test/index.md).
