# Encoders

Encoders serialize events into a wire format before writing them to a sink. You select an encoder
with the `encoder.type` field. If omitted, metrics default to `prometheus_text` and logs default
to `json_lines`.

## prometheus_text

Prometheus text exposition format (v0.0.4). Each event becomes one line:

```
metric_name{label="value"} 42.0 1700000000000
```

No additional parameters.

```yaml title="Prometheus text encoder"
encoder:
  type: prometheus_text
```

```text title="Wire format"
cpu_usage{host="web-01"} 50 1774279696105
```

This encoder supports metrics only. It does not support log events.

## influx_lp

InfluxDB line protocol. Each event becomes one line with tags, a field, and a nanosecond
timestamp:

```
metric_name,tag=value field_key=42.0 1700000000000000000
```

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `field_key` | string | no | `"value"` | The InfluxDB field key for the metric value. |

```yaml title="InfluxDB line protocol encoder"
encoder:
  type: influx_lp
  field_key: cpu_percent
```

```text title="Wire format"
test_influx,host=web-01 value=0 1774279709667342000
```

This encoder supports metrics only. It does not support log events.

## json_lines

JSON Lines (NDJSON) format. Each event is one JSON object per line.

No additional parameters.

```yaml title="JSON Lines encoder"
encoder:
  type: json_lines
```

For metrics:

```json title="Metric wire format"
{"name":"cpu_usage","value":50.0,"labels":{"host":"web-01"},"timestamp":"2026-03-23T15:28:32.321Z"}
```

For logs:

```json title="Log wire format"
{"timestamp":"2026-03-23T14:59:04.840Z","severity":"info","message":"test log","fields":{}}
```

This is the default encoder for log scenarios. It supports both metrics and logs.

## syslog

RFC 5424 syslog format. Encodes log events as syslog lines.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `hostname` | string | no | `"sonda"` | The HOSTNAME field in the syslog header. |
| `app_name` | string | no | `"sonda"` | The APP-NAME field in the syslog header. |

```yaml title="Syslog encoder"
encoder:
  type: syslog
  hostname: web-01
  app_name: myapp
```

```text title="Wire format"
<14>1 2026-03-23T15:29:02.483Z sonda sonda - - - test log
```

This encoder supports logs only. It does not support metric events.

## remote_write

Prometheus remote write protobuf format. Encodes metrics as length-prefixed protobuf
`TimeSeries` messages.

!!! note
    This encoder requires the `remote-write` Cargo feature flag. Pre-built release binaries
    include this feature. If building from source: `cargo build --features remote-write -p sonda`.

No additional parameters.

```yaml title="Remote write encoder"
encoder:
  type: remote_write
```

This encoder must be paired with the `remote_write` sink, which handles batching, snappy
compression, and HTTP POSTing with the correct protocol headers. See
[Sinks - remote_write](sinks.md#remote_write) for details.

## Encoder compatibility

| Encoder | Metrics | Logs |
|---------|---------|------|
| `prometheus_text` | yes | no |
| `influx_lp` | yes | no |
| `json_lines` | yes | yes |
| `syslog` | no | yes |
| `remote_write` | yes | no |
