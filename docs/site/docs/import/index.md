---
title: Import real data
description: Turn CSV exports, Grafana panels, and log files into replayable Sonda scenarios.
---

# Import real data

Sometimes the easiest way to get a realistic shape is to start from real data. Sonda's import paths take a CSV, a Grafana panel export, or a log file and produce a replayable scenario YAML — same cadence, same values, runnable on demand for incident replay, regression tests, or dashboard validation.

<div class="grid cards" markdown>

-   :material-file-table-outline: __[From a CSV file](from-csv.md)__

    `sonda new --from <csv>` reads numeric columns, runs pattern detection, and emits a scenario entry per column with a sensible generator alias.

-   :material-chart-areaspline: __[Grafana exports](grafana-exports.md)__

    Export a Grafana panel as CSV, point `sonda new --from` at it, and you have a replayable scenario that reproduces the incident.

-   :material-text-box-outline: __[Log files](log-files.md)__

    Replay a structured log file at the original cadence with `csv_replay` on a `signal_type: logs` entry.

</div>
