# Cardinality explosion alerts

Many monitoring stacks page when series cardinality crosses a guardrail
(`count(up) > 10000`, `prometheus_tsdb_symbol_table_size_bytes > N`, etc.). The rule
fires the first time a deploy ships a label with too many distinct values -- and the
only way to know it works is to push a controlled explosion through it. Sonda's
[cardinality spikes](../configuration/scenario-fields.md) generate a bounded burst of
unique label values on a recurring schedule, so you can verify the alert fires during
the spike and resolves after.

```bash
sonda metrics --scenario examples/cardinality-alert-test.yaml
```

```yaml title="examples/cardinality-alert-test.yaml (key fields)"
cardinality_spikes:
  - label: pod_name
    every: 30s
    for: 10s
    cardinality: 500
    strategy: counter
    prefix: "pod-"
```

During the 10-second spike window, each tick injects a `pod_name` label drawn from a
pool of up to 500 unique values (`pod-0` through `pod-499`). The actual per-spike
series count is `min(cardinality, ticks_in_window)` — at `rate: 10, for: 10s` that's
100 ticks per spike, so each spike grows the visible series count by up to 100 new
`pod-N` values until the 500-value pool fills across recurrences. Outside the spike
window the label is absent and only one series is emitted. This on/off pattern
exercises both the firing and resolution paths of the cardinality rule.

!!! info "Docker stack required"
    The bundled example pushes to VictoriaMetrics via `http_push`. Start the backend
    first:
    `docker compose -f examples/docker-compose-victoriametrics.yml up -d`

## Tuning the spike

Three knobs shape the explosion:

| Field | Effect |
|-------|--------|
| `cardinality` | Number of unique label values per spike. Set this just above your alert threshold. |
| `for` | How long the spike lasts. Set this longer than your rule's `for:` clause. |
| `every` | How often the spike recurs. Useful for proving the rule re-fires after a quiet window. |

For a rule like `ALERT HighCardinality IF count(...) > 400 FOR 5m`, set
`cardinality: 500` and `for: 360s` and watch the alert pend, fire, then clear after the
spike ends.

## Next

Synthetic spikes are great for testing the alert path. To replay an actual production
incident -- with the values that paged you last week -- use the replay generators next.

[Continue to **Replaying recorded incidents** -->](alert-testing-replay.md)
