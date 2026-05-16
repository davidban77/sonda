# Encoders

Your monitoring backend expects data in a specific wire format. Sonda can speak all of
them. Encoders are the layer that turns each generated value into bytes.

The same metric looks different in each format. Pick the encoder in the YAML's `encoder:` block (under `defaults:` or per entry); the `--encoder` flag on `sonda run` overrides whatever the file declares.

Starter scenario used by every tab below:

```yaml title="http-rps.yaml"
version: 2
kind: runnable
defaults:
  rate: 1
  duration: 3s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
  labels:
    env: prod
scenarios:
  - id: http_rps
    signal_type: metrics
    name: http_rps
    generator:
      type: constant
      value: 42.0
```

=== "prometheus_text (default)"

    ```bash
    sonda run http-rps.yaml
    ```

    ```text
    http_rps{env="prod"} 42 1711900000000
    ```

=== "influx_lp"

    ```bash
    sonda run http-rps.yaml --encoder influx_lp
    ```

    ```text
    http_rps,env=prod value=42 1711900000000000000
    ```

=== "json_lines"

    ```bash
    sonda run http-rps.yaml --encoder json_lines
    ```

    ```json
    {"name":"http_rps","value":42.0,"labels":{"env":"prod"},"timestamp":"2026-03-31T20:00:00.000Z"}
    ```

=== "syslog (logs only)"

    Swap the entry for a log generator and pick the `syslog` encoder in the file:

    ```yaml title="app-logs-syslog.yaml"
    version: 2
    kind: runnable
    defaults:
      rate: 1
      duration: 3s
      encoder:
        type: syslog
      sink:
        type: stdout
      labels:
        app: myservice
    scenarios:
      - id: app_logs
        signal_type: logs
        name: app_logs
        log_generator:
          type: template
          templates:
            - message: "synthetic log event"
    ```

    ```bash
    sonda run app-logs-syslog.yaml
    ```

    ```text
    <14>1 2026-03-31T21:40:38.941Z sonda sonda - - [sonda app="myservice"] synthetic log event
    ```

## Pick by what you are testing

| Backend | Use this encoder | Pair with sink |
|---|---|---|
| Prometheus / VictoriaMetrics scrape | `prometheus_text` | `stdout`, `http_push` |
| VictoriaMetrics import API | `prometheus_text` | `http_push` |
| Prometheus / VM remote write | `remote_write` | `remote_write` |
| InfluxDB / Telegraf line protocol consumers | `influx_lp` | `http_push`, `tcp`, `udp` |
| Loki / log search backends | `json_lines` | `loki`, `file`, `http_push` |
| Syslog collectors (RFC 5424) | `syslog` | `tcp`, `udp` |
| OpenTelemetry Collector | `otlp` | `otlp_grpc` |

The full per-encoder field reference is in [Encoders](../configuration/encoders.md).

## Feature-gated encoders

Two encoders require Cargo feature flags when building from source:

!!! warning "remote_write and otlp"
    The `remote_write` encoder produces Prometheus remote write protobuf format. It
    requires the `remote-write` feature flag (`cargo build --features remote-write`).
    Pre-built binaries and Docker images include it by default.

    The `otlp` encoder produces OTLP protobuf format for metrics and logs. It requires
    the `otlp` feature flag (`cargo build --features otlp`). Pre-built binaries and
    Docker images do **not** include this feature -- you must build from source.

    See [Encoders](../configuration/encoders.md) for the full feature-flag matrix.

## Next

With the right format chosen, the next question is where the bytes should go.

[Continue to **Sinks** -->](tutorial-sinks.md)
