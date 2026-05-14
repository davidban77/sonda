# Act 3 — Remote_write into local Prometheus

**Time on slide:** ~7 min.

**What this shows:** Swap one knob — the sink — and the same scenario now pushes into a real Prometheus via remote_write. The audience can query it in Grafana within seconds.

## Setup (once, before the demo)

Stack must be up:

```bash
docker compose -f demo/stack/docker-compose.yml up -d
```

Wait for Prometheus to be ready:

```bash
curl -s http://localhost:9090/-/ready
```

## The command

```bash
sonda metrics --scenario demo/act-03-remote-write/scenario.yaml
```

It will run for 5 minutes by default. Stop early with Ctrl-C — data already in Prometheus stays.

## Verify it arrived

```bash
curl -s "http://localhost:9090/api/v1/query?query=interface_in_errors" | jq '.data.result[0].metric'
```

Should print the labels you set in the scenario.

## In Grafana

Open http://localhost:3000 → **Explore** → **Prometheus** datasource. Query:

```promql
interface_in_errors
```

You'll see the sine wave climbing and falling. Switch to **Time series** visualization. Mention:

- The points landed within 1–2 scrape intervals (real remote_write latency).
- This is identical to what Mimir, Cortex, Grafana Cloud, Thanos, VictoriaMetrics would see — it's the standard protocol.

## Talking points

- One sink swap = same scenario, totally different delivery path.
- No collector in the loop — sonda speaks the protocol directly.
- For shops that have Mimir or Grafana Cloud: point the URL at your real endpoint and it just works. (We're using local Prometheus today so we don't pollute prod.)
