# Compound and correlated alerts

Production alerts often depend on more than one metric. Compound rules like
`cpu_usage > 90 AND memory_usage_percent > 85` only fire when both conditions are
true at the same moment -- which means your test data needs an overlapping window
across two scenarios. Sonda gives you `phase_offset` and `clock_group` to build that
overlap deterministically.

| Field | Default | Description |
|-------|---------|-------------|
| `phase_offset` | `"0s"` | Delay before this scenario starts emitting |
| `clock_group` | (none) | Groups scenarios under a shared timing reference |

```bash
sonda run examples/multi-metric-correlation.yaml
```

```yaml title="examples/multi-metric-correlation.yaml (excerpt)"
version: 2
kind: runnable

defaults:
  rate: 1
  duration: 120s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: cpu_usage
    clock_group: alert-test
    generator:
      type: sequence
      values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
      repeat: true

  - signal_type: metrics
    name: memory_usage_percent
    phase_offset: "3s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
      repeat: true
```

## Reading the timeline

```text
Wall time  cpu_usage (offset=0s)   memory_usage (offset=3s)
--------   ---------------------   ------------------------
t=0s       starts: 20             sleeping
t=3s       95 (above threshold)   starts: 40
t=6s       95                     88 (above threshold)
t=8s       20 (drops)             88
```

The overlap window -- where **both** metrics are above threshold -- runs from t=6s to
t=8s (2 seconds per cycle). For a `for: 5m` compound rule, extend the above-threshold
sequences or switch to constant generators with a longer overall duration.

!!! info "clock_group ties scenarios to a shared timeline"
    Without `clock_group`, every scenario starts at its own wall-clock time and the
    overlap drifts. With `clock_group: alert-test`, all members share a reference clock
    and `phase_offset` is measured against that reference. See
    [Scenario Fields -- Temporal fields](../configuration/scenario-fields.md#temporal-fields)
    for the full ordering semantics.

See [Example Scenarios](examples.md) for the full `multi-metric-correlation.yaml` file.

## Next

Threshold and compound rules cover scalar values. The next pattern is different in
kind -- alerts that fire when the **count of series** changes, not the values inside them.

[Continue to **Cardinality explosion alerts** -->](alert-testing-cardinality.md)
