# Encoders

Encoders serialize events into a wire format before writing them to a sink. You select an encoder
with the `encoder.type` field. If omitted, metrics default to `prometheus_text` and logs default
to `json_lines`.

## prometheus_text

Prometheus text exposition format (v0.0.4). Each event becomes one line:

```
metric_name{label="value"} 42.0 1700000000000
```

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `precision` | integer | no | full f64 precision | Decimal places for metric values (0--17). |

```yaml title="Prometheus text encoder"
encoder:
  type: prometheus_text
```

```yaml title="With precision"
encoder:
  type: prometheus_text
  precision: 2
```

```text title="Wire format (precision: 2, value 99.60573)"
cpu_usage{host="web-01"} 99.61 1774279696105
```

Text-based formats preserve trailing zeros: a value of `100.0` with `precision: 2` renders
as `100.00`.

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
| `precision` | integer | no | full f64 precision | Decimal places for metric values (0--17). |

```yaml title="InfluxDB line protocol encoder"
encoder:
  type: influx_lp
  field_key: cpu_percent
```

```yaml title="With precision"
encoder:
  type: influx_lp
  precision: 4
```

```text title="Wire format (precision: 4, value 99.60573)"
test_influx,host=web-01 value=99.6057 1774279709667342000
```

This encoder supports metrics only. It does not support log events.

## json_lines

JSON Lines (NDJSON) format. Each event is one JSON object per line.

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `precision` | integer | no | full f64 precision | Decimal places for metric values (0--17). |

```yaml title="JSON Lines encoder"
encoder:
  type: json_lines
```

```yaml title="With precision"
encoder:
  type: json_lines
  precision: 3
```

For metrics:

```json title="Metric wire format"
{"name":"cpu_usage","value":50.0,"labels":{"host":"web-01"},"timestamp":"2026-03-23T15:28:32.321Z"}
```

```json title="Metric wire format (precision: 3, value 99.60573)"
{"name":"cpu_usage","value":99.606,"labels":{"host":"web-01"},"timestamp":"2026-03-23T15:28:32.321Z"}
```

!!! note
    JSON has no trailing-zero concept. With `precision: 2`, a value of `100.0` still renders
    as `100.0` in JSON output (not `100.00`). The rounding is still applied -- it just does not
    add trailing zeros that JSON would strip.

For logs:

```json title="Log wire format"
{"timestamp":"2026-03-23T14:59:04.840Z","severity":"info","message":"test log","fields":{}}
```

The `precision` field only affects metric values. Log events have no numeric value to format.

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

## Value precision

The `prometheus_text`, `influx_lp`, and `json_lines` encoders accept an optional `precision`
field that controls how many decimal places appear in metric values.

- **Range**: 0 to 17 (an f64 has approximately 15--17 significant digits).
- **Default**: omit the field to keep full f64 precision.
- **Effect**: values are rounded to the specified number of decimal places using standard rounding.

```yaml title="precision-formatting.yaml"
encoder:
  type: prometheus_text
  precision: 2    # 99.60573 becomes 99.61
```

The `syslog` and `remote_write` encoders do not support `precision`. Syslog encodes log events
only (no numeric values), and remote write uses binary protobuf encoding.

See [`examples/precision-formatting.yaml`](https://github.com/davidban77/sonda/blob/main/examples/precision-formatting.yaml)
for a complete scenario that demonstrates precision with multiple encoders.

## Encoder compatibility

| Encoder | Metrics | Logs | Precision |
|---------|---------|------|-----------|
| `prometheus_text` | yes | no | yes |
| `influx_lp` | yes | no | yes |
| `json_lines` | yes | yes | yes |
| `syslog` | no | yes | -- |
| `remote_write` | yes | no | -- |
