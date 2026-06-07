---
title: Import real data
description: Convert CSV exports, Grafana panels, and log files into replayable Sonda scenarios.
---

# Import real data

This section covers three ways to turn real telemetry into a Sonda scenario. Each page handles one input format: a CSV file, a Grafana panel export, or a structured log file. The result is a YAML scenario you can replay for incident reproduction, regression tests, or dashboard validation.

<div class="grid cards" markdown>

-   :material-file-table-outline: __[From a CSV file](from-csv.md)__

    `sonda new --from <csv>` reads each numeric column, classifies the pattern, and writes one scenario entry per column.

-   :material-chart-areaspline: __[Grafana exports](grafana-exports.md)__

    Export a Grafana panel as CSV. Point `sonda new --from` at it and replay the incident at the original cadence.

-   :material-text-box-outline: __[Log files](log-files.md)__

    Replay a structured log file at the original cadence with `csv_replay` on a `signal_type: logs` entry.

</div>
