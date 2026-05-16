# Generating logs

[Getting Started](../getting-started.md#generating-logs) showed basic log generation. Sonda supports two log modes: **template** for synthetic messages with randomized fields, and **csv_replay** for replaying a structured CSV of real log events at the recorded cadence.

## Template mode with field pools

Templates render `{field}` placeholders by sampling from a per-field pool. They live in the YAML's `log_generator:` block on a `signal_type: logs` entry:

```bash
sonda run examples/log-template.yaml --duration 5s
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

## CSV replay mode

Replay a structured CSV of real log events. The CSV has a `timestamp` column that drives the emission cadence, plus optional `severity` and `message` columns and any number of free-form field columns.

```bash
sonda run examples/log-csv-replay.yaml
```

```yaml title="examples/log-csv-replay.yaml"
version: 2
kind: runnable

defaults:
  duration: 60s
  encoder:
    type: json_lines
  sink:
    type: stdout

scenarios:
  - signal_type: logs
    name: app_logs_csv_replay
    rate: 1
    log_generator:
      type: csv_replay
      file: examples/sample-logs.csv
      default_severity: info
      repeat: true
```

The CSV looks like this:

```csv title="examples/sample-logs.csv"
timestamp,severity,message,user_id
1700000000,info,GET /api/v1/health returned 200,u-42
1700000003,info,GET /api/v1/metrics returned 200,u-17
1700000006,warn,GET /api/v1/users returned 200 with high latency,u-91
```

Sonda derives the replay rate from the median Δt of the timestamp column (3 seconds here → 0.33 events/s). The `rate:` in YAML is ignored -- the CSV cadence wins. Free-form columns like `user_id` become entries on the emitted event's `fields` map.

!!! tip "Where this shines"
    For the full workflow -- exporting a window from Loki via `logcli`, projecting it into a CSV with `jq`, and replaying it back through your pipeline tagged with `source="replay"` -- see the [Log CSV Replay](log-csv-replay.md) guide.

## Pair templates with the syslog encoder

Combine template logs with the syslog encoder for RFC 5424 output. Swap the `encoder:` block in the YAML, or override at the CLI with `--encoder syslog`:

```yaml title="log-syslog.yaml"
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 5s
  encoder:
    type: syslog
  sink:
    type: stdout
scenarios:
  - id: app_logs_syslog
    signal_type: logs
    name: app_logs_syslog
    log_generator:
      type: template
      templates:
        - message: "synthetic log event"
```

```bash
sonda run log-syslog.yaml
```

The encoder wraps each generated message in the syslog header:

```text
<14>1 2026-03-31T21:40:38.941Z sonda sonda - - [sonda] synthetic log event
```

Pair this with the `tcp` or `udp` sink to feed a syslog collector directly.

## Next

Your metrics and logs are flowing, but real telemetry has irregularities. Add some.

[Continue to **Scheduling -- gaps and bursts** -->](tutorial-scheduling.md)
