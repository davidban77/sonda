# Exporting interface-flaps data from Grafana for Act 5

You need a CSV exported from Grafana in the **"Series joined by time"** format. Sonda's `csv_replay` generator with auto-column discovery reads that exact shape. Other formats (e.g. "Time series", "Wide", or Excel) will NOT work without manual column mapping — stick to "Series joined by time".

## What sonda expects

A CSV that looks like this:

```csv
"Time","{__name__=""ifInErrors"", instance=""r1"", interface=""Gi0/0""}","{__name__=""ifInErrors"", instance=""r1"", interface=""Gi0/1""}"
1715000000000,0,0
1715000015000,12,0
1715000030000,47,0
1715000045000,103,5
```

- **Column 1**: `Time` — epoch milliseconds.
- **Columns 2+**: one column per series, header is the full Prometheus label set including `__name__`. Sonda parses this header and rebuilds the metric + labels automatically.

## How to export from Grafana

1. **Pick the incident time range** in Grafana's time picker. Tight window is better — 10–30 min around the interface-flap event. The replay will be 1:1 wall-clock by default, so a 30-min export = 30 min of replay (we can speed it up later, see "Speed control" below).

2. **Write a PromQL query** that surfaces the flaps. A few options depending on what your collector exposes:
   - `rate(ifInErrors[1m])` — error rate per second
   - `changes(ifOperStatus[1m])` — count of state transitions per minute (this is usually the cleanest "flap" signal)
   - `ifOperStatus` — raw 1/0 state, useful if you want the audience to literally see the bit flip
   - `irate(ifInDiscards[1m])` — packet discards

   Pick ONE query for simplicity. Filter to the affected interface(s) — 2 to 6 series total is the sweet spot for the demo. Too few looks thin, too many is unreadable.

3. **Run the query** in either Explore or a Dashboard panel.

4. **Inspect → Data**:
   - **Explore**: click the panel-level menu (three dots near the visualization) → **Inspect** → **Data** tab.
   - **Dashboard**: panel title → **Inspect** → **Data** tab.

5. **In the Data inspector**, set:
   - **Data options → "Series joined by time"** — REQUIRED. This is the wide format sonda expects.
   - **Formatted data: OFF** — we want raw timestamps and numbers, not formatted strings.
   - **Download for Excel: OFF** — plain CSV, not Excel-flavored.

6. **Click "Download CSV"**. Save it as:

   ```
   demo/act-05-csv-replay/interface-flaps.csv
   ```

   (replacing the placeholder file already in that folder).

## Verifying the format before the demo

Open the file in a text editor. The first line should look like:

```
"Time","{__name__=""...""}",...
```

If the header is just plain column names (`Time, value, value`) you got the wrong export format — go back to step 5 and toggle "Series joined by time".

You can also dry-run the scenario to confirm sonda parses it:

```bash
cd ~/projects/sonda
sonda run --scenario demo/act-05-csv-replay/scenario.yaml --dry-run
```

Should print a resolved config showing one scenario per series, with the labels extracted from the CSV header.

## Speed control (optional polish)

By default `csv_replay` walks the file at the original wall-clock rate. If your incident spans 30 min and your demo slot is 45 min, you can:

- **Speed up**: add `time_scale: 60` to the generator (replays at 60× — 30 min becomes 30 sec)
- **Slow down**: `time_scale: 0.5` (half speed)

We'll decide the speed once we see the CSV's actual time span. Leave the field out of the scenario for now; I'll add it during dry-run if needed.

## Anything to scrub before sharing

The CSV will contain real hostnames / interface names from production. If the audience is your immediate team, that's probably fine. If you'll share the slide deck or the repo more widely later, you may want to:

- `sed` the hostnames in the CSV header to something generic (`r1`, `r2`, `core-sw-1`, etc.)
- Or commit only the scenario YAML and keep the CSV gitignored

I'll wire `demo/act-05-csv-replay/interface-flaps.csv` into `.gitignore` by default — you can choose to commit it later.
