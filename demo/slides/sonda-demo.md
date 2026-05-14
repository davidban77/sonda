---
marp: true
theme: default
paginate: true
size: 16:9
header: "Sonda — synthetic telemetry for netobs"
footer: "David Flores · 2026-05-15"
---

# Sonda
## Synthetic telemetry for network observability

David Flores
Network observability team · 2026-05-15

---

## What we'll do today (~45 min)

1. **Why** synthetic telemetry
2. **stdout** — sonda is just an emitter
3. **remote_write** — same data into local Prometheus
4. **server mode** — sonda over HTTP
5. **CSV replay** — rehydrate a real incident from our Grafana ⭐
6. **smoke testing** — same engine, different framing
7. **YAML** — quick model tour
8. **Wrap** — repo, install, what to try

---

## The netobs problems sonda solves

- "Does this **alert** actually fire when the condition appears?"
- "Does this **dashboard** look right when traffic shifts?"
- "Did the **ingest pipeline** survive last deploy?"
- "Can I **rehearse a past incident** without breaking the network?"
- "Can I **train new engineers** on what a real flap pattern looks like?"

Real network gear is expensive to break for testing. Sonda lets you generate the signals on demand.

---

## What sonda is

A single binary (Rust, static musl) that generates:

- **Metrics** — Prometheus text, remote_write, InfluxDB line protocol
- **Logs** — JSON Lines, syslog, OTLP
- **Histograms / summaries** — full Prometheus shapes

…in scenarios defined by short YAML.

Same engine runs from the CLI **or** as an HTTP server. Same scenarios run either way.

---

## Act 2 — stdout

```bash
sonda run --scenario demo/act-02-stdout/scenario.yaml
```

Metrics + logs streaming straight to your terminal. No backend needed.

**Takeaway:** sonda is just producing observability data. Whatever consumes Prometheus text or JSON Lines can consume this.

---

## Act 3 — remote_write into Prometheus

```bash
sonda metrics --scenario demo/act-03-remote-write/scenario.yaml
```

Same scenario, sink swap: `stdout` → `remote_write`.

→ Grafana → query `interface_in_errors`.

**Takeaway:** one knob changes delivery. Production-shaped data flowing through the same protocol Mimir, Cortex, Grafana Cloud, Thanos all speak.

---

## Act 4 — sonda-server, HTTP control plane

```bash
curl -X POST -H "Content-Type: text/yaml" \
  --data-binary @demo/act-04-server-mode/scenario.yaml \
  http://localhost:8080/scenarios
```

Returns an `id`. Then:

- `GET /scenarios/{id}/stats` — live event rate, target rate, gap state
- `GET /scenarios/{id}/metrics` — Prometheus-scrapeable
- `DELETE /scenarios/{id}` — stops cleanly with final stats

**Takeaway:** drop sonda into CI, dashboards, test harnesses. Same engine, HTTP-driven lifecycle.

---

## Act 5 — Replay a real incident ⭐

The killer feature for our team.

1. Export interface-flaps incident from team Grafana → CSV
2. `sonda metrics --scenario demo/act-05-csv-replay/scenario.yaml`
3. Watch the exact same flip pattern reappear in local Grafana

**Takeaway:** any past observation in our Grafana → CSV → fully replayable signal. Useful for:

- Retros — "watch the incident again, but slower"
- Alert tuning — "would this rule have caught it?"
- Onboarding — "this is what a real flap looks like"
- Regression tests — "PR review against a known incident shape"

---

## Act 6 — Sonda as pipeline smoke test

```bash
sonda metrics --scenario demo/act-06-smoke-test/scenario.yaml &
./demo/act-06-smoke-test/verify.sh
```

Injects a known heartbeat, asserts it arrives in Prometheus within 30s.

If the heartbeat doesn't show — pipeline is broken. Gate deploys on it. Run it during incidents to triage "is the bottom of the stack alive?".

**Takeaway:** same engine, validation framing. Synthetic monitoring for your *own* pipeline.

---

## Act 7 — The YAML model

```yaml
version: 2
defaults:
  rate: 5
  encoder: { type: remote_write }
  sink:    { type: remote_write, url: ... }
scenarios:
  - signal_type: metrics
    name: interface_in_errors
    generator:
      type: sine
      amplitude: 5
      period_secs: 20
    labels:
      hostname: r1
      interface: GigabitEthernet0/0
```

Four moving parts: **generator → encoder → sink**, scheduled by `defaults` + `scenarios`. Everything else is a knob.

---

## What's in the box

**Generators** — sine, constant, ramp, step, random walk, gaussian, csv_replay, template (logs), replay (logs), pareto, exponential, …

**Encoders** — prometheus_text, remote_write, influx_line, json_lines, syslog, otlp, kafka_json, …

**Sinks** — stdout, file, http_push, remote_write, kafka, loki, otlp_grpc, syslog_udp, …

Mix and match. New combinations don't need code changes — they need a YAML edit.

---

## Get started

**Repo:** github.com/davidban77/sonda *(adjust if different)*

**Install** (one of):

```bash
cargo install --path sonda                # from this repo
brew install davidban77/tap/sonda         # if you publish a tap
docker run ghcr.io/davidban77/sonda:latest
```

**First thing to try:**

```bash
sonda init                  # interactive scenario builder
sonda catalog               # browse built-in scenarios + packs
```

---

## What would you replay first?

Pull an incident CSV from your Grafana, drop it in `demo/act-05-csv-replay/`, run it.

Questions?

David Flores · network observability team · 2026-05-15
