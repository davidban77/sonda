# Multi-scenario runs

Production systems emit multiple signals simultaneously. `sonda run` lets you orchestrate
several scenarios concurrently from a single YAML file, each on its own thread.

```bash
sonda run --scenario examples/multi-scenario.yaml
```

```yaml title="examples/multi-scenario.yaml"
version: 2

scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 100
    duration: 30s
    generator:
      type: sine
      amplitude: 50
      period_secs: 60
      offset: 50
    encoder:
      type: prometheus_text
    sink:
      type: stdout

  - signal_type: logs
    name: app_logs
    rate: 10
    duration: 30s
    log_generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint}"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3"]
            endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs"]
      severity_weights:
        info: 0.7
        warn: 0.2
        error: 0.1
      seed: 42
    encoder:
      type: json_lines
    sink:
      type: file
      path: /tmp/sonda-logs.json
```

Each scenario runs on its own thread. Use different sinks per scenario to keep outputs
separate -- here, metrics go to stdout while logs land in `/tmp/sonda-logs.json`.

!!! tip "Shared defaults shrink the file"
    Common fields belong under `defaults:` so each scenario only declares what differs.
    See [v2 Scenario Files](../configuration/v2-scenarios.md) for the full inheritance
    rules.

## Correlated metrics with phase_offset

Two scenarios that fire at the same wall-clock time test independent alert rules. Two
scenarios offset by a controlled delay test **compound** alert rules -- the kind that
need both signals above threshold for a window before firing.

Use `phase_offset` to delay a scenario's start relative to its `clock_group` peers:

```bash
sonda run --scenario examples/multi-metric-correlation.yaml
```

```yaml title="examples/multi-metric-correlation.yaml (excerpt)"
version: 2

defaults:
  rate: 1
  duration: 120s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    instance: server-01
    job: node

scenarios:
  - id: cpu_usage
    signal_type: metrics
    name: cpu_usage
    clock_group: alert-test
    generator:
      type: sequence
      values: [20, 20, 20, 95, 95, 95, 95, 95, 20, 20]
      repeat: true

  - id: memory_usage
    signal_type: metrics
    name: memory_usage_percent
    phase_offset: "3s"
    clock_group: alert-test
    generator:
      type: sequence
      values: [40, 40, 40, 88, 88, 88, 88, 88, 40, 40]
      repeat: true
```

Here is how the phase offset creates an overlapping window:

```text
t=0s    cpu_usage starts        (sequence: 20, 20, 20, 95, 95, ...)
t=3s    cpu_usage crosses 90;   memory_usage starts (3s phase offset, sequence: 40, 40, 40, 88, ...)
t=6s    memory_usage crosses 85; compound alert fires (cpu > 90 AND memory > 85)
```

CPU crosses its threshold at t=3, memory follows 3 seconds later -- exactly the shape needed
to test a rule like `cpu > 90 AND memory > 85 FOR 1m`.

!!! info "clock_group ties scenarios to a shared timeline"
    Without `clock_group`, every scenario starts at its own wall-clock time. With
    `clock_group: alert-test`, all members share a reference clock, and `phase_offset`
    is measured against that reference. See
    [Scenario Fields -- Temporal fields](../configuration/scenario-fields.md#temporal-fields)
    for the full ordering semantics.

For more alert-testing patterns -- including `for:` duration testing and
recording-rule validation -- see [Alert Testing](alert-testing.md).

## Next

For long-running or programmatic use, the same multi-scenario shape is available over
HTTP through the Sonda Server API.

[Continue to **The Server API** -->](tutorial-server.md)
