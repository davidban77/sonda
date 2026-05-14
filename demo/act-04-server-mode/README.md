# Act 4 — sonda-server, containerized, API-driven

**Time on slide:** ~8 min.

**What this shows:** sonda isn't just a CLI. The same engine runs as an HTTP control plane: POST a scenario, get an ID back, query stats live, scrape metrics in Prometheus text format. Useful for CI pipelines, dashboards, test harnesses.

## Pre-flight

Demo stack must be up:

```bash
docker compose -f demo/stack/docker-compose.yml up -d
curl -s http://localhost:8080/health   # should print {"status":"ok"}
```

## The flow (run these one at a time, narrating)

### 1. Start a scenario by POSTing YAML

```bash
curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @demo/act-04-server-mode/scenario.yaml \
  http://localhost:8080/scenarios | jq
```

Response:

```json
{
  "id": "9c0e...uuid",
  "name": "interface_oper_state",
  "state": "running"
}
```

Capture the id:

```bash
ID=$(curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @demo/act-04-server-mode/scenario.yaml \
  http://localhost:8080/scenarios | jq -r .id)
echo "$ID"
```

### 2. List what's running

```bash
curl -s http://localhost:8080/scenarios | jq
```

Shows every active scenario with elapsed time.

### 3. Inspect live stats

```bash
curl -s "http://localhost:8080/scenarios/$ID/stats" | jq
```

Returns `total_events`, `current_rate`, `target_rate`, `bytes_emitted`, `errors`, `uptime_secs`, `state`, `in_gap`, `in_burst`. Re-run a few times — the audience sees `total_events` ticking up.

For a compact view:

```bash
curl -s "http://localhost:8080/scenarios/$ID/stats" | jq '{total_events, current_rate, target_rate, state}'
```

### 4. Scrape the Prometheus endpoint

```bash
curl -s "http://localhost:8080/scenarios/$ID/metrics"
```

Output is real Prometheus exposition format:

```
# TYPE interface_oper_state untyped
interface_oper_state{hostname="r1",interface="GigabitEthernet0/0",job="sonda-server"} 1 1715620000000
...
```

This is the integration point with any Prometheus-compatible scraper. Show:

- "Prometheus can scrape this directly with a `static_configs` target pointed at `/scenarios/<id>/metrics`."
- "Or use `http_sd_configs` to discover all scenarios dynamically."

### 5. Stop the scenario cleanly

```bash
curl -s -X DELETE "http://localhost:8080/scenarios/$ID" | jq
```

Returns the scenario with `state: "stopped"` and final stats embedded. It's removed from the list.

## Talking points

- Same scenario YAML, different delivery path: CLI ran it locally, server runs it lifecycle-managed.
- Every CLI behavior maps to an HTTP endpoint — the design is "API mirrors the CLI".
- For CI: POST a known scenario before your test, DELETE after, assert stats.
- For dashboards: poll `/stats` for live SLO views.

## If something looks off

- POST returns 400 with `v1 → v2 migration` text → you're posting an old v1 YAML. Should not happen with the demo files but worth knowing.
- POST returns 422 → encoder/sink config invalid; fix the YAML.
- GET stats returns 404 → scenario already stopped (or wrong ID); list with `curl /scenarios`.
