# Act 2 — Metrics + logs to stdout

**Time on slide:** ~5 min.

**What this shows:** sonda generates synthetic telemetry and writes it to stdout in real Prometheus exposition format (metrics) and JSON Lines (logs). No backend required. This is the "look, sonda is just producing observability data" framing.

## The command

```bash
sonda run --scenario demo/act-02-stdout/scenario.yaml
```

You should see two streams interleaving in the terminal:

```
interface_in_errors{hostname="r1",interface="GigabitEthernet0/0",job="sonda"} 10.5
{"timestamp":"...","level":"warning","message":"%LINK-3-UPDOWN: Interface GigabitEthernet0/0, changed state to down","hostname":"r1","app":"syslog"}
interface_in_errors{hostname="r1",interface="GigabitEthernet0/0",job="sonda"} 12.1
...
```

## Talking points

- One YAML, two signal types. The same engine generates both.
- Labels are real Prometheus labels — no schema mismatch when this lands in your pipeline later.
- Encoders are pluggable: `prometheus_text` for the metric, `json_lines` for the log. Could also be `remote_write`, `influx_line`, `otlp`, etc.
- Stdout is just one sink. We'll swap it next act.

## If something looks off

- Nothing scrolls → check `sonda --version` is installed and on PATH.
- Garbled output → terminal needs UTF-8 (unlikely an issue on macOS but worth knowing).
- Stops after 30s → that's `duration: 30s` in the scenario; bump it or remove it for indefinite streaming.
