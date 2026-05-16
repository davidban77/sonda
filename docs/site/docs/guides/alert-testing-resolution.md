# Resolution and recovery

A rule that fires but never clears is a paging incident waiting to happen. When a metric
goes silent during a gap, Prometheus treats it as stale and resolves the alert -- the
same path a real scrape failure or restart takes. Use [gap windows](../configuration/scenario-fields.md)
to control when metrics disappear, so you can confirm both the firing and the resolution
side of the rule.

```text
Time:  0s          40s         60s         100s        120s
       |-----------|xxxxxxxxxxx|-----------|xxxxxxxxxxx|
       emit events   gap (20s)  emit events   gap (20s)
```

Gaps occupy the **tail** of each cycle. With `every: 60s` and `for: 20s`, the gap runs
from second 40 to second 60 of each cycle.

```bash
sonda run examples/gap-alert-test.yaml
```

```yaml title="examples/gap-alert-test.yaml"
version: 2

defaults:
  rate: 1
  duration: 300s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 95.0
    gaps:
      every: 60s
      for: 20s
    labels:
      instance: server-01
      job: node
```

The value stays at 95 (above threshold) but goes silent for 20 seconds every 60-second
cycle. The alert enters pending state during the 40-second emit window but may not reach
the `for:` duration before the gap resets it -- which is exactly the flapping pattern
you want to validate against.

!!! tip "Combine gaps with any generator"
    Gaps work with any generator. A sine wave with periodic gaps creates a realistic
    "flapping service" pattern -- useful for testing that your alert hysteresis or
    `keep_firing_for` clause actually suppresses the noise.

## Next

Single-metric resolution is straightforward. Compound rules that depend on two or more
metrics need careful timing -- that is the next pattern.

[Continue to **Compound and correlated alerts** -->](alert-testing-correlation.md)
