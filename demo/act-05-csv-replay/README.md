# Act 5 — Replay a real Grafana incident (HEADLINER)

**Time on slide:** ~10–12 min. This is the act the rest of the demo is building toward.

**What this shows:** Export a past incident from your existing Grafana → feed the CSV to sonda → watch the exact same data shape replay through a clean pipeline, queryable in real time. The audience sees that any past pattern can be rehydrated on demand.

## Pre-flight checklist

1. Demo stack up:
   ```bash
   docker compose -f demo/stack/docker-compose.yml up -d
   ```
2. Real CSV in place: `demo/act-05-csv-replay/interface-flaps.csv` is the exported file (see `demo/CSV-EXPORT-GUIDE.md`).
3. Optional sanity check — dry-run prints the scenarios sonda discovered from the CSV header:
   ```bash
   sonda metrics --scenario demo/act-05-csv-replay/scenario.yaml --dry-run
   ```
   Look for one scenario per series, each with the labels parsed from the column header.

## The flow

### 1. Show the CSV (slide-side)

Open the CSV in a side panel or quick-look:

- Header line — point at the `{__name__="ifOperStatus", instance="r1", interface="..."}` shape. "This is exactly how Grafana exports it. Sonda reads this header and reconstructs the metric + labels — nothing to map by hand."
- A few data rows — point at the epoch-millis timestamps and the 1/0 oper-state flips. "This is what the team saw during the incident."

### 2. Run sonda against the CSV

```bash
sonda metrics --scenario demo/act-05-csv-replay/scenario.yaml
```

You'll see one scenario start per CSV column. Sonda walks the rows at the original wall-clock rate and writes each row's value to Prometheus via remote_write.

### 3. Pull it up in Grafana (live)

http://localhost:3000 → **Explore** → **Prometheus** → query:

```promql
ifOperStatus
```

You see the exact same flip pattern from the incident reappear in this clean lab Grafana, second by second.

Switch to **Time series** view, then **State timeline** for a more visual "where it flapped" panel.

### 4. The hook line

> "Whatever pattern your dashboard recorded — alert spike, dropout, queue depth, latency tail — you can pull it out as a CSV and replay it. Same pipeline, different data shape. Useful for retros, alert tuning, regression tests, training new engineers on what a real incident looks like."

## Time scaling (if your CSV is too long for the slot)

If the incident export covers 30 min and you've got 5 min left in the act, edit `scenario.yaml` to add:

```yaml
    generator:
      type: csv_replay
      file: demo/act-05-csv-replay/interface-flaps.csv
      time_scale: 10   # 10× faster — 30 min becomes 3 min
```

Tradeoff: faster replay loses the "this looks like real time" feel. 2–5× is usually a sweet spot.

## Talking points

- The CSV header is the entire schema declaration. No YAML edits needed when you switch datasets.
- Wall-clock replay preserves the *shape* of the incident — gaps, rate changes, bursts.
- Combined with Act 6 (smoke testing), this becomes a way to validate that alert rules would have caught the incident this time around.

## Fallback if Act 5 misbehaves live

If for any reason the live replay glitches (CSV parsing, Prometheus stalls), have one of these ready:

- Pre-recorded screenshot of the resulting Grafana panel — keep it open in a background tab.
- The fixture file `examples/grafana-export.csv` already works end-to-end; you can pivot the scenario to point at it as a known-good fallback.
