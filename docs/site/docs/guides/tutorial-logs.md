# Generating logs

[Getting Started](../getting-started.md#generating-logs) showed basic log generation.
Sonda supports two log modes: **template** for synthetic messages with randomized
fields, and **replay** for re-emitting lines from an existing log file.

## Template mode with field pools

The CLI `--message` flag supports template syntax, but placeholder tokens like `{ip}`
render as literal text. For dynamic log messages with randomized fields, use a YAML
scenario:

```bash
sonda logs --scenario examples/log-template.yaml --duration 5s
```

```yaml title="examples/log-template.yaml (excerpt)"
scenarios:
  - signal_type: logs
    name: app_logs_template
    log_generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint} returned {status}"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2", "10.0.0.3", "192.168.1.10"]
            endpoint: ["/api/v1/health", "/api/v1/metrics", "/api/v1/logs"]
            status: ["200", "201", "400", "404", "500"]
        - message: "Service {service} processed {count} events in {duration_ms}ms"
          field_pools:
            service: ["ingest", "transform", "export"]
            count: ["1", "10", "100", "1000"]
            duration_ms: ["5", "12", "47", "200"]
      severity_weights:
        info: 0.7
        warn: 0.2
        error: 0.1
      seed: 42
```

Each tick picks a template at random, fills every `{field}` from the matching pool, and
draws a severity from the weighted distribution. `seed` makes the picks deterministic
across runs -- the exact same sequence every time.

See [Generators](../configuration/generators.md) for the full template configuration
reference (per-template weights, multi-template fan-out, severity normalisation).

## Replay mode

Replay lines from an existing log file:

```bash
sonda logs --scenario examples/log-replay.yaml
```

```yaml title="examples/log-replay.yaml"
version: 2

defaults:
  rate: 5
  duration: 30s
  encoder:
    type: json_lines
  sink:
    type: stdout

scenarios:
  - id: app_logs_replay
    signal_type: logs
    name: app_logs_replay
    log_generator:
      type: replay
      file: examples/sample-app.log
```

Lines are replayed in order and cycle back to the start when the file is exhausted.

!!! tip "Bring your own log file"
    The example uses `examples/sample-app.log` which ships with Sonda. To replay your
    own logs, point `file:` at any text file -- one log line per line.

## Pair templates with the syslog encoder

Combine template logs with the syslog encoder for RFC 5424 output:

```bash
sonda logs --mode template --rate 2 --duration 5s --encoder syslog
```

The encoder wraps each generated message in the syslog header:

```text
<14>1 2026-03-31T21:40:38.941Z sonda sonda - - [sonda] synthetic log event
```

Pair this with the `tcp` or `udp` sink to feed a syslog collector directly.

## Next

Your metrics and logs are flowing, but real telemetry has irregularities. Add some.

[Continue to **Scheduling -- gaps and bursts** -->](tutorial-scheduling.md)
