# Replaying recorded incidents

Synthetic shapes prove the alert path works in the abstract. Replay proves it would
have caught the real incident. Two generators handle the replay case: `sequence` for
short hand-crafted patterns, and `csv_replay` for long recordings exported from your
TSDB.

| Generator | Best for | Storage |
|-----------|----------|---------|
| `sequence` | ≤ 20 values, hand-tuned | Inline in the YAML |
| `csv_replay` | Real incidents, long recordings | External CSV file |

## Hand-crafted patterns with sequence

The [sequence generator](../configuration/generators.md#sequence) steps through an
explicit list of values, perfect for short, deterministic threshold patterns:

```bash
sonda run examples/sequence-alert-test.yaml
```

```yaml title="examples/sequence-alert-test.yaml (key fields)"
generator:
  type: sequence
  values: [10, 10, 10, 10, 10, 95, 95, 95, 95, 95, 10, 10, 10, 10, 10, 10]
  repeat: true
```

With `repeat: true`, the pattern loops continuously. With `repeat: false`, the generator
holds the last value after the sequence ends -- useful for "the metric pegged at 100
and never recovered" scenarios.

## Production replay with csv_replay

For replaying real production data, the [csv_replay generator](../configuration/generators.md#csv_replay)
reads values from a CSV file. If you have a Grafana dashboard showing the incident, see
the [Grafana CSV Replay](grafana-csv-replay.md) guide for the full export-and-replay
workflow.

```bash
sonda run examples/csv-replay-metrics.yaml
```

```yaml title="examples/csv-replay-metrics.yaml (key fields)"
generator:
  type: csv_replay
  file: examples/sample-cpu-values.csv
  columns:
    - index: 1
      name: cpu_replay
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `file` | (required) | Path to the CSV file |
| `columns` | -- | Explicit column specs. When absent, columns are auto-discovered from the header. See [Generators](../configuration/generators.md#csv_replay). |
| `repeat` | `true` | Cycle back to the first value after reaching the end |

!!! tip "When to use csv_replay vs sequence"
    Use `csv_replay` over `sequence` when you have more than ~20 values. It keeps the
    YAML clean and makes it easy to update the data by replacing the CSV file -- the
    scenario stays identical.

??? info "Exporting values from VictoriaMetrics"
    ```bash
    curl -s "http://your-vm:8428/api/v1/query_range?\
    query=cpu_usage{instance='prod-01'}&\
    start=$(date -d '1 hour ago' +%s)&\
    end=$(date +%s)&\
    step=10s" \
      | jq -r '["timestamp","cpu_percent"], (.data.result[0].values[] | [.[0], .[1]]) | @csv' \
      > incident-values.csv
    ```

## Where to go from here

Replay closes the loop on the local-testing side. To take any of these patterns to a
real backend and prove the alert fires end-to-end, head back to the
[Alert Testing landing page](alert-testing.md#push-to-a-real-backend) for the
backend handoff, or jump straight to the
[Alerting Pipeline](alerting-pipeline.md) walkthrough that wires vmalert and
Alertmanager into the loop.
