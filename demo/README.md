# Sonda demo — netobs team, Friday 2026-05-15

A 45–50 minute hands-on walkthrough of sonda for network observability engineers. Workshop-first: most of the slot is live commands; slides are a thin frame between acts.

This folder is self-contained. The `examples/` directory in the repo root is the source of truth for production-grade examples — this folder is *curated for the demo*.

## Prereqs (do once, before the meeting)

- `sonda` binary on PATH. From this repo: `cargo install --path sonda` or use a release artifact.
- Docker + docker compose.
- `curl` and `jq`.

## Bring up the stack

One command, ~30 seconds to ready:

```bash
docker compose -f demo/stack/docker-compose.yml up -d
```

What you get:

| Service        | URL                    | Purpose                                            |
|----------------|------------------------|----------------------------------------------------|
| Prometheus     | http://localhost:9090  | Metrics store + remote_write receiver + scraper    |
| Grafana        | http://localhost:3000  | Anonymous admin, Prometheus pre-provisioned        |
| sonda-server   | http://localhost:8080  | HTTP control plane (Act 4)                         |

Sanity check:

```bash
curl -s http://localhost:9090/-/ready
curl -s http://localhost:3000/api/health
curl -s http://localhost:8080/health
```

All three should respond.

Tear down at the end:

```bash
docker compose -f demo/stack/docker-compose.yml down -v
```

## The arc

| Act | Folder                    | Mode        | Time   | One-line takeaway                                          |
|-----|---------------------------|-------------|--------|------------------------------------------------------------|
| 1   | (slide only)              | slide       | ~3 min | Why synthetic telemetry for netobs                         |
| 2   | `act-02-stdout/`          | CLI         | ~5 min | Sonda is just an emitter — metrics + logs in real formats  |
| 3   | `act-03-remote-write/`    | CLI         | ~7 min | One sink swap puts the same data in real Prometheus        |
| 4   | `act-04-server-mode/`     | API         | ~8 min | Server mode for CI / dashboards — same engine, HTTP-driven |
| 5   | `act-05-csv-replay/`      | CLI ⭐      | ~12 min| Replay any past Grafana export — the headliner             |
| 6   | `act-06-smoke-test/`      | CLI + shell | ~5 min | Same engine, pipeline-validation framing                   |
| 7   | (slide)                   | slide       | ~3 min | Quick YAML model tour                                      |
| 8   | (slide)                   | slide       | ~2 min | Wrap, repo link, "what would you replay first?"            |
|     | **Total**                 |             | ~45 min |                                                          |

Each `act-*/` folder has its own `README.md` — the act-specific cheat-sheet with exact commands, expected output, and fallbacks.

## Recommended terminal layout for the demo

- **Tab 1**: this folder (`~/projects/sonda/demo/`) — for running commands.
- **Tab 2**: docker compose logs follow, in case anything dies live:
  ```bash
  docker compose -f demo/stack/docker-compose.yml logs -f
  ```
- **Browser tab 1**: Grafana Explore (http://localhost:3000/explore) — pre-load it.
- **Browser tab 2**: this `README.md` rendered, in case you want a fallback view.

## Pre-flight rehearsal (run this the night before)

```bash
cd ~/projects/sonda
docker compose -f demo/stack/docker-compose.yml up -d
sleep 10

# Act 2
sonda run --scenario demo/act-02-stdout/scenario.yaml &
SONDA_PID=$!
sleep 5
kill $SONDA_PID

# Act 3
timeout 15 sonda metrics --scenario demo/act-03-remote-write/scenario.yaml
curl -s "http://localhost:9090/api/v1/query?query=interface_in_errors" | jq '.data.result | length'

# Act 4
ID=$(curl -s -X POST -H "Content-Type: text/yaml" \
  --data-binary @demo/act-04-server-mode/scenario.yaml \
  http://localhost:8080/scenarios | jq -r .id)
sleep 5
curl -s "http://localhost:8080/scenarios/$ID/stats" | jq
curl -s -X DELETE "http://localhost:8080/scenarios/$ID" | jq

# Act 5 (requires real interface-flaps.csv, but the placeholder works for rehearsal)
timeout 15 sonda metrics --scenario demo/act-05-csv-replay/scenario.yaml
curl -s "http://localhost:9090/api/v1/query?query=ifOperStatus" | jq '.data.result | length'

# Act 6
sonda metrics --scenario demo/act-06-smoke-test/scenario.yaml &
sleep 2
./demo/act-06-smoke-test/verify.sh

docker compose -f demo/stack/docker-compose.yml down -v
```

If any of those steps fail in rehearsal, debug before Friday — not in front of the team.

## Act 7 — YAML tour (slide-side)

Have one scenario file open on screen — `act-03-remote-write/scenario.yaml` is the best one to show because it's compact and uses all four core concepts:

- `defaults:` — block of settings inherited by every scenario.
- `scenarios:` — array of independent generation tasks running in parallel.
- `generator:` — what shape the data takes (sine, constant, csv_replay, template, etc.).
- `encoder` + `sink:` — how it's serialized + where it goes.

Don't go deeper than this. The audience does NOT need to understand every knob — they need to understand that the model is "schedule generators into sinks via encoders."

## Materials checklist

- [ ] `interface-flaps.csv` exported from team Grafana (see `CSV-EXPORT-GUIDE.md`)
- [ ] Slide deck imported into Google Slides (from `slides/sonda-demo.pptx`)
- [ ] Pre-flight rehearsal completed without errors
- [ ] Repo link prepared for the wrap slide
- [ ] Browser tabs pre-loaded (Grafana Explore)

## After the demo

Possible follow-ups depending on audience interest:

- **Move this folder to a standalone repo** for easier sharing — currently lives inside the sonda repo for development speed.
- **Add a Loki/logs replay act** — show `log_replay` generator pulling from a real logs export.
- **Wire to the netobs lab Grafana** — point Act 3/5's `PROMETHEUS_URL` at the prod Mimir instead of the local one.
