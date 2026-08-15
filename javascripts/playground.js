/* Sonda docs — scenario playground.
 *
 * Drives the #sonda-playground page: scenario YAML is compiled and sampled by
 * the real sonda-core engine (sonda_wasm.js / sonda_wasm_bg.wasm, generated
 * by wasm-bindgen from the sonda-wasm crate — see `task site:wasm`), and the
 * result is drawn on a canvas with gap/burst shading plus an encoded-output
 * preview. Loaded as an ES module on every page; the wasm binary itself is
 * fetched only when the playground container exists.
 */
import init, { sample_scenario } from "./sonda_wasm.js";
import {
  toBase64Url,
  fromBase64Url,
  hashPayloadTooLarge,
  exportFilename,
  scheduleWindows,
  burstEmission,
  cursorSecsAt,
  cursorSamples,
  logLinesNear,
} from "./sonda-pure.js";
// Shared with livegen.js so the two pages cannot drift on what a histogram,
// a summary or a log line looks like. Extracted verbatim — see signal-render.js.
import {
  palette,
  formatNumber,
  formatSeconds,
  drawHistogramHeatmap,
  drawSummaryBands,
  logStream,
} from "./signal-render.js";

const MAX_TICKS = 240;
const DEBOUNCE_MS = 500;
// How long a burst of edits stays open once typing stops. Longer than the
// debounce on purpose, so the run that ends a burst is still inside it.
const GHOST_IDLE_MS = 1500;

const SERIES_COLORS = ["#f97316", "#3b82f6", "#10b981", "#8b5cf6", "#ec4899", "#eab308"];

const PRESETS = [
  {
    name: "CPU sine wave",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 4
  duration: 60s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator: { type: sine, amplitude: 30.0, offset: 55.0, period_secs: 20 }
    labels: { host: web-01, region: us-east }
`,
  },
  {
    name: "BGP neighbor flap",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 2
  duration: 90s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: bgp
    signal_type: metrics
    name: bgp_neighbor_state
    generator:
      type: flap
      up_duration: 20s
      down_duration: 8s
      enum: neighbor_state
    labels: { device: pe-router-1, neighbor: "10.0.0.2" }
`,
  },
  {
    name: "Memory leak",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 2
  duration: 120s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: leak
    signal_type: metrics
    name: process_memory_percent
    generator:
      type: leak
      baseline: 12.0
      ceiling: 96.0
      time_to_ceiling: 120s
    labels: { service: checkout, pod: checkout-7d4f9 }
`,
  },
  {
    name: "Latency spikes + scrape gaps",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 4
  duration: 120s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: latency
    signal_type: metrics
    name: request_latency_ms
    generator:
      type: spike_event
      baseline: 120.0
      spike_height: 400.0
      spike_duration: 5s
      spike_interval: 30s
    gaps: { every: 60s, for: 8s }
    labels: { service: api-gateway }
`,
  },
  {
    name: "Queue saturation + ingest burst",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 4
  duration: 120s
  encoder: { type: influx_lp }
  sink: { type: stdout }
scenarios:
  - id: queue
    signal_type: metrics
    name: queue_fill_percent
    generator:
      type: saturation
      baseline: 5.0
      ceiling: 98.0
      time_to_saturate: 40s
    bursts: { every: 40s, for: 6s, multiplier: 3.0 }
    labels: { queue: ingest-main }
`,
  },
  {
    name: "Cascading failure (after: chain)",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 4
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: memory
    signal_type: metrics
    name: memory_percent
    rate: 2
    duration: 120s
    generator:
      type: leak
      baseline: 10.0
      ceiling: 95.0
      time_to_ceiling: 120s
    labels: { service: checkout }
  - id: latency
    signal_type: metrics
    name: latency_ms
    duration: 30s
    after: { ref: memory, op: ">", value: 40.0 }
    generator:
      type: leak
      baseline: 120.0
      ceiling: 450.0
      time_to_ceiling: 30s
    labels: { service: checkout }
  - id: errors
    signal_type: metrics
    name: http_errors_total
    duration: 40s
    after: { ref: latency, op: ">", value: 350.0 }
    generator:
      type: spike
      baseline: 0.0
      magnitude: 40.0
      duration_secs: 3
      interval_secs: 8
    labels: { service: checkout }
`,
  },
  {
    name: "Latency histogram + quantiles",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 2
  duration: 120s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: latency_hist
    signal_type: histogram
    name: http_request_duration_seconds
    distribution: { type: exponential, rate: 10.0 }
    observations_per_tick: 40
    mean_shift_per_sec: 0.004
    seed: 42
    labels: { service: api }
  - id: latency_quantiles
    signal_type: summary
    name: rpc_duration_seconds
    distribution: { type: normal, mean: 0.1, stddev: 0.02 }
    observations_per_tick: 40
    mean_shift_per_sec: 0.002
    seed: 7
    labels: { service: api }
`,
  },
  {
    name: "Synthetic log stream",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 4
  duration: 60s
  encoder: { type: json_lines }
  sink: { type: stdout }
scenarios:
  - id: app_logs
    signal_type: logs
    name: checkout_logs
    log_generator:
      type: template
      templates:
        - message: "Request from {ip} to {endpoint} took {latency}ms"
          field_pools:
            ip: ["10.0.0.1", "10.0.0.2", "10.0.0.7"]
            endpoint: ["/api/cart", "/api/checkout", "/api/health"]
            latency: ["12", "48", "230", "870"]
        - message: "payment gateway timeout after {timeout}s"
          field_pools:
            timeout: ["5", "10"]
      severity_weights: { info: 0.75, warn: 0.15, error: 0.1 }
      seed: 42
    labels: { service: checkout }
`,
  },
  {
    /* The one preset carrying metrics AND logs in the same scenario file.
     *
     * It exists because WP9's log correlation needs one: hovering the chart
     * highlights the log lines from that instant, and with a logs-only
     * scenario there is no chart to hover. Found in UAT — every other preset
     * is one signal type or the other, so the feature shipped unreachable
     * and untestable until this landed.
     *
     * The spike and the error-weighted templates are deliberately the same
     * story: sweep the cursor into a latency spike and the log lines beside
     * it are the timeouts that caused it. */
    name: "Latency spike + correlated logs",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 4
  duration: 60s
  encoder: { type: json_lines }
  sink: { type: stdout }
scenarios:
  - id: latency
    signal_type: metrics
    name: request_latency_ms
    generator:
      type: spike_event
      baseline: 120.0
      spike_height: 520.0
      spike_duration: 4s
      spike_interval: 20s
    labels: { service: checkout }
  - id: checkout_logs
    signal_type: logs
    name: checkout_logs
    log_generator:
      type: template
      templates:
        - message: "GET {endpoint} -> {status} in {latency}ms"
          field_pools:
            endpoint: ["/api/cart", "/api/checkout"]
            status: ["200", "200", "503"]
            latency: ["48", "121", "870"]
        - message: "upstream payment-gw timed out after {timeout}s"
          field_pools:
            timeout: ["5", "10"]
      severity_weights: { info: 0.7, warn: 0.15, error: 0.15 }
      seed: 42
    labels: { service: checkout }
`,
  },
  {
    name: "Correlated pair (CPU + errors)",
    yaml: `version: 2
kind: runnable
defaults:
  rate: 4
  duration: 90s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: steady
      center: 45.0
      amplitude: 8.0
      period: 30s
      noise: 2.5
      noise_seed: 7
    labels: { host: web-01 }
  - id: errors
    signal_type: metrics
    name: http_errors_total
    generator:
      type: spike
      baseline: 0.0
      magnitude: 24.0
      duration_secs: 6
      interval_secs: 45
    labels: { host: web-01 }
`,
  },
];

let wasmReady = null;

function ensureWasm() {
  if (!wasmReady) {
    wasmReady = init({ module_or_path: new URL("./sonda_wasm_bg.wasm", import.meta.url) });
  }
  return wasmReady;
}

/* Wrap the fallback textarea in the same interface the CodeMirror factory
 * returns, so the rest of the page never cares which editor is mounted. */
function textareaEditor(textarea, { onChange, onRun }) {
  textarea.addEventListener("input", onChange);
  textarea.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      onRun();
    }
  });
  return {
    getValue: () => textarea.value,
    setValue: (text) => {
      textarea.value = text;
    },
    setDark: () => {},
    setEngineError: () => {},
    focus: () => textarea.focus(),
  };
}

async function mountEditor(textarea, hooks) {
  try {
    const { createScenarioEditor } = await import("./sonda-editor.js");
    const editor = createScenarioEditor({
      parent: textarea.parentElement,
      doc: textarea.value,
      dark: document.body.getAttribute("data-md-color-scheme") === "slate",
      onChange: hooks.onChange,
      onRun: hooks.onRun,
    });
    textarea.hidden = true;
    return editor;
  } catch {
    // Bundle missing or import unsupported — the plain textarea still works.
    return textareaEditor(textarea, hooks);
  }
}

async function boot() {
  const root = document.getElementById("sonda-playground");
  if (!root || root.dataset.ready) return;
  root.dataset.ready = "1";

  const el = {
    preset: document.getElementById("sp-preset"),
    run: document.getElementById("sp-run"),
    share: document.getElementById("sp-share"),
    download: document.getElementById("sp-download"),
    png: document.getElementById("sp-png"),
    testAlert: document.getElementById("sp-test-alert"),
    status: document.getElementById("sp-status"),
    editor: document.getElementById("sp-editor"),
    error: document.getElementById("sp-error"),
    chart: document.getElementById("sp-chart"),
    legend: document.getElementById("sp-legend"),
    skipped: document.getElementById("sp-skipped"),
    output: document.getElementById("sp-output"),
  };

  PRESETS.forEach((preset, index) => {
    const option = document.createElement("option");
    option.value = String(index);
    option.textContent = preset.name;
    el.preset.appendChild(option);
  });

  let lastResult = null;
  let debounceTimer = 0;
  let editor = null;

  /* The cursor's position on the timeline, in scenario seconds, or null.
   * Lives here rather than on the element because it survives a re-render:
   * a resize or a theme flip should not drop the reading under the pointer. */
  let cursorSecs = null;

  /* The ghost baseline, and whether an edit burst is currently open.
   *
   * David's call, over the spec's "previous run": the ghost is the scenario
   * as it was BEFORE the current burst of edits, not one debounce step back.
   * With a 500 ms debounce, dragging a scrubbable number produces a run every
   * half second, and a ghost of the previous run is a faint near-copy of the
   * live trace — it carries no information exactly when you are looking for
   * some. Pinned to the pre-burst state, dragging amplitude 30 -> 50 shows
   * the 30 curve the whole way down.
   *
   * The burst closes after GHOST_IDLE_MS of no edits, which only re-arms the
   * baseline for the NEXT burst — the ghost itself stays on screen until then,
   * so nothing vanishes on a timer while the reader is looking at it. The
   * idle window is deliberately longer than the debounce, so the run that
   * ends a burst is still inside it. */
  let ghostEntries = null;
  let burstOpen = false;
  let burstIdleTimer = 0;

  const view = () => ({ cursorSecs, ghost: ghostEntries });

  const run = async () => {
    el.status.textContent = "compiling…";
    try {
      await ensureWasm();
      const yaml = editor.getValue();
      const result = JSON.parse(sample_scenario(yaml, MAX_TICKS));
      lastResult = result;
      render(el, result, view());
      editor.setEngineError(result.ok ? null : result.error);
      el.status.textContent = result.ok ? "" : "compile error";
      // Bridge to the alert lab: carry the current scenario across so a
      // threshold + for: rule can be tuned against this exact signal.
      if (el.testAlert) el.testAlert.href = "alert-lab/#yaml=" + toBase64Url(yaml);
    } catch (err) {
      el.status.textContent = "engine failed to load";
      showError(el, String(err));
    }
  };

  const scheduleRun = () => {
    // The first edit of a burst pins the ghost. `lastResult.entries` is safe
    // to hold by reference: every run parses fresh JSON, so the previous
    // run's entries are never mutated under us.
    if (!burstOpen) {
      burstOpen = true;
      ghostEntries = lastResult && lastResult.ok ? lastResult.entries : null;
    }
    window.clearTimeout(burstIdleTimer);
    burstIdleTimer = window.setTimeout(() => {
      burstOpen = false;
    }, GHOST_IDLE_MS);
    window.clearTimeout(debounceTimer);
    debounceTimer = window.setTimeout(run, DEBOUNCE_MS);
  };

  /* A different scenario has nothing to be compared with. Preset changes and
   * shared links replace the whole document, so a ghost of the old one would
   * be two unrelated curves on one pair of axes. */
  const dropGhost = () => {
    window.clearTimeout(burstIdleTimer);
    burstOpen = false;
    ghostEntries = null;
  };

  const shared = fromLocationHash();
  el.editor.value = shared.yaml !== null ? shared.yaml : PRESETS[0].yaml;
  editor = await mountEditor(el.editor, { onChange: scheduleRun, onRun: run });

  el.run.addEventListener("click", run);
  el.preset.addEventListener("change", () => {
    editor.setValue(PRESETS[Number(el.preset.value)].yaml);
    dropGhost();
    clearCursor();
    run();
  });
  // Download YAML: one gesture leaves the reader with both halves of the
  // next step — the file on disk and the command that runs it on the
  // clipboard. The status line always shows the command, so a browser that
  // refuses clipboard access costs the convenience, not the information
  // (same degradation the alert lab's "Copy below ↓" already uses).
  el.download.addEventListener("click", () => {
    const yaml = editor.getValue();
    const filename = exportFilename(exportSource(lastResult), "yaml");
    saveBlob(new Blob([yaml], { type: "text/yaml;charset=utf-8" }), filename);
    const command = `sonda run ${filename}`;
    const flash = (prefix) => flashStatus(el, `${prefix} ${command}`);
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(command).then(
        () => flash("Saved — run:"),
        () => flash("Saved — run (copy manually):")
      );
    } else {
      flash("Saved — run (copy manually):");
    }
  });

  // Chart PNG: whatever is on the canvas, at whatever DPR it was drawn for.
  // toBlob can hand back null (a zero-size or otherwise untainted-but-empty
  // canvas), so the null branch reports rather than silently doing nothing.
  el.png.addEventListener("click", () => {
    if (el.png.disabled) return;
    const filename = exportFilename(exportSource(lastResult), "png");
    el.chart.toBlob((blob) => {
      if (!blob) {
        flashStatus(el, "nothing to export — the chart is empty");
        return;
      }
      saveBlob(blob, filename);
      flashStatus(el, `Saved ${filename}`);
    });
  });

  el.share.addEventListener("click", () => {
    const url = new URL(window.location.href);
    url.hash = "yaml=" + toBase64Url(editor.getValue());
    navigator.clipboard.writeText(url.toString()).then(() => {
      el.share.textContent = "Copied!";
      window.setTimeout(() => {
        el.share.textContent = "Copy link";
      }, 1400);
    });
  });

  /* The time cursor.
   *
   * Repaints through `paintCursor` rather than `render`, because moving a
   * pointer must not rebuild the log pane, the legend and the extra charts
   * sixty times a second — and rebuilding the log rows would throw away the
   * highlight this is trying to set. rAF-coalesced, so a burst of pointer
   * events costs one redraw per frame however fast the device reports them.
   */
  let cursorFrame = 0;
  const paintCursor = () => {
    if (cursorFrame) return;
    cursorFrame = window.requestAnimationFrame(() => {
      cursorFrame = 0;
      if (!lastResult || !lastResult.ok) return;
      if (el.chart.style.display !== "none") {
        drawChart(el.chart, lastResult.entries, view());
      } else {
        // The stamps live inside drawChart, which a hidden chart never
        // reaches — so a logs-only scenario would keep advertising the cursor
        // from the metrics scenario before it (review #544 M1). The BEHAVIOUR
        // was already right, since the readout empties and no log line is
        // spuriously highlighted; what went stale was the smoke suite's
        // oracle for where the cursor is, which is worse than a wrong pixel
        // because a future check would read it and believe it.
        el.chart.dataset.cursor = "";
        el.chart.dataset.ghosts = "0";
        el.chart.dataset.ghostPeak = "";
        el.chart.dataset.peak = "";
      }
      renderReadout(el, lastResult.entries, cursorSecs);
      highlightLogs(lastResult.logs || [], cursorSecs);
    });
  };

  const clearCursor = () => {
    if (cursorSecs === null) return;
    cursorSecs = null;
    paintCursor();
  };

  const moveCursor = (event) => {
    const at = cursorSecsAt(el.chart._geom, event.offsetX);
    if (at === cursorSecs) return;
    cursorSecs = at;
    paintCursor();
  };

  el.chart.addEventListener("pointermove", (event) => {
    // Touch reaches here too, but a finger's "move" is a drag, and treating
    // it as hover would make the cursor follow a scroll gesture. Touch is
    // handled by pointerdown below, as tap-to-set / tap-again-to-clear.
    if (event.pointerType === "touch") return;
    moveCursor(event);
  });
  el.chart.addEventListener("pointerleave", clearCursor);
  el.chart.addEventListener("pointerdown", (event) => {
    if (event.pointerType !== "touch") return;
    // Second tap in the same place clears, so a reader on a phone can put the
    // chart back the way they found it without a control to explain.
    const at = cursorSecsAt(el.chart._geom, event.offsetX);
    if (at === null || (cursorSecs !== null && Math.abs(at - cursorSecs) < 1e-9)) clearCursor();
    else {
      cursorSecs = at;
      paintCursor();
    }
  });

  // Redraw on container resize; re-render chart and editor theme on
  // light/dark scheme change.
  new ResizeObserver(() => lastResult && render(el, lastResult, view())).observe(el.chart.parentElement);
  new MutationObserver(() => {
    if (lastResult) render(el, lastResult, view());
    editor.setDark(document.body.getAttribute("data-md-color-scheme") === "slate");
  }).observe(document.body, {
    attributes: true,
    attributeFilter: ["data-md-color-scheme"],
  });

  // Awaited so a refused hash reports AFTER the first run has settled the
  // status line — otherwise the run's own "" would erase the explanation.
  await run();
  if (shared.status) el.status.textContent = shared.status;
}

/* Read a shared scenario out of the `#yaml=` hash.
 *
 * Returns `{ yaml, status }`: `yaml` is null when there is nothing usable to
 * load, and `status` carries a message to show the reader when the hash was
 * present but refused.
 *
 * The size check runs BEFORE the decode. A link is attacker-supplied input,
 * and an unbounded one buys a base64 decode plus an engine compile of
 * arbitrary size on page load; refusing by length is O(1) and cannot be
 * talked out of. Malformed base64 still falls back to the default preset
 * silently — that is a broken link, not a hostile one.
 */
function fromLocationHash() {
  const match = window.location.hash.match(/^#yaml=(.+)$/);
  if (!match) return { yaml: null, status: null };
  if (hashPayloadTooLarge(match[1])) {
    return { yaml: null, status: "shared scenario too large — showing the default preset" };
  }
  try {
    return { yaml: fromBase64Url(match[1]), status: null };
  } catch {
    return { yaml: null, status: null };
  }
}

function showError(el, message) {
  el.error.hidden = false;
  el.error.textContent = message;
}

/* Which sampled entries should name the downloaded file.
 *
 * Metrics entries first, because they are what the chart shows. A logs-only
 * scenario has an empty `entries` array but named log entries, and falling
 * straight through to exportFilename's "scenario" default would hand every
 * logs preset the same anonymous filename. */
function exportSource(result) {
  if (!result) return [];
  if (result.entries && result.entries.length) return result.entries;
  return result.logs || [];
}

/* Hand a Blob to the browser's download machinery.
 *
 * The object URL is revoked on the next frame rather than immediately: the
 * click has to have been dispatched before the URL dies, and revoking in the
 * same tick loses the download in some browsers. */
function saveBlob(blob, filename) {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 0);
}

/* Put a transient message on the status line without stomping on whatever a
 * later run wants to say. The token guards against an earlier flash's timer
 * clearing a newer message. */
let statusToken = 0;
function flashStatus(el, message, holdMs = 4000) {
  el.status.textContent = message;
  const token = ++statusToken;
  window.setTimeout(() => {
    if (statusToken === token) el.status.textContent = "";
  }, holdMs);
}

/* The PNG button is only meaningful when the main chart is on screen — a
 * logs-only scenario hides it (render() sets display:none), and exporting a
 * blank canvas would be a worse answer than refusing. */
function syncPngButton(el) {
  if (!el.png) return;
  const hidden = el.chart.style.display === "none";
  el.png.disabled = hidden;
  el.png.title = hidden
    ? "This scenario has no line chart to export"
    : "Download the chart as a PNG";
}

function render(el, result, view = {}) {
  if (!result.ok) {
    showError(el, result.error || "unknown compile error");
    return;
  }
  el.error.hidden = true;

  const histograms = result.histograms || [];
  const summaries = result.summaries || [];
  const logs = result.logs || [];

  // A scenario with only histogram/summary/log entries doesn't need the
  // empty line chart taking up space.
  const hasLines = result.entries.length > 0;
  const hasExtras = histograms.length || summaries.length || logs.length;
  el.chart.style.display = hasLines || !hasExtras ? "" : "none";
  if (hasLines || !hasExtras) drawChart(el.chart, result.entries, view);
  // Same reason as the hidden branch of paintCursor: render() reaches here on
  // a preset change too, and a hidden chart must not keep the last visible
  // scenario's stamps.
  else {
    el.chart.dataset.cursor = "";
    el.chart.dataset.ghosts = "0";
    el.chart.dataset.ghostPeak = "";
    el.chart.dataset.peak = "";
  }
  syncPngButton(el);

  renderExtraCharts(el, histograms, summaries);
  renderLogs(el, logs, el.chart.style.display !== "none");
  // The cursor's two readers, refreshed with the chart: a re-render on
  // resize or theme flip must not leave a readout describing the old scales
  // or a highlight on a log pane that was just rebuilt from scratch.
  const cursorSecs = typeof view.cursorSecs === "number" ? view.cursorSecs : null;
  renderReadout(el, result.entries, cursorSecs);
  highlightLogs(logs, cursorSecs);

  el.legend.replaceChildren(
    ...result.entries.map((entry, index) => {
      const chip = document.createElement("span");
      chip.className = "sonda-playground__chip";
      const swatch = document.createElement("i");
      swatch.style.background = SERIES_COLORS[index % SERIES_COLORS.length];
      chip.append(swatch, document.createTextNode(entry.name));
      return chip;
    })
  );

  el.skipped.replaceChildren(
    ...result.skipped.map((skip) => {
      const note = document.createElement("p");
      note.textContent = `${skip.id}: ${skip.reason}`;
      return note;
    })
  );

  el.output.textContent = result.entries
    .map((entry) => entry.encoded_preview.trimEnd())
    .concat(logs.map((log) => log.encoded_preview.trimEnd()))
    .join("\n")
    .trim();
}

/* Histogram heatmaps and summary quantile bands get one canvas each, in a
 * container created on demand below the main chart. Rebuilt per render —
 * the counts are small and this keeps resize/theme redraws trivial. */
function renderExtraCharts(el, histograms, summaries) {
  let extra = document.getElementById("sp-extra");
  if (!extra) {
    extra = document.createElement("div");
    extra.id = "sp-extra";
    el.chart.parentElement.appendChild(extra);
  }
  extra.replaceChildren();

  const addBlock = (title, draw) => {
    const caption = document.createElement("p");
    caption.textContent = title;
    caption.style.cssText =
      "font: 12px ui-monospace, monospace; opacity: .75; margin: 10px 0 2px;";
    const canvas = document.createElement("canvas");
    extra.append(caption, canvas);
    draw(canvas);
  };

  histograms.forEach((histogram) => {
    addBlock(`${histogram.name} — bucket heatmap (observations per tick)`, (canvas) =>
      drawHistogramHeatmap(canvas, histogram)
    );
  });
  summaries.forEach((summary) => {
    addBlock(`${summary.name} — quantile bands`, (canvas) => drawSummaryBands(canvas, summary));
  });
}

/* Synthetic log stream pane: one scrollable block per log entry, each line
 * stamped with its offset on the scenario timeline and colored by severity.
 * Rebuilt per render, same lifecycle as the extra charts. */
function renderLogs(el, logs, correlatable) {
  let pane = document.getElementById("sp-logs");
  if (!logs.length) {
    if (pane) pane.remove();
    return;
  }
  if (!pane) {
    pane = document.createElement("div");
    pane.id = "sp-logs";
    el.chart.parentElement.appendChild(pane);
  }
  pane.replaceChildren();

  // Said here because here is where the question arises (review #544).
  // Correlation hangs off the chart cursor, and a logs-only scenario has no
  // chart — so the preset most about logs is the one place the log feature
  // does nothing. That is a real consequence of the design rather than a bug,
  // and the honest response is to name it rather than let a reader hunt for a
  // control that was never there. A scrubber over the pane itself would be
  // the other answer; that is a second instrument, not a sentence.
  if (!correlatable) {
    const note = document.createElement("p");
    note.className = "sonda-playground__lognote";
    note.textContent =
      "Hovering a chart highlights the events at that moment — this scenario has no metrics series to hover.";
    pane.appendChild(note);
  }

  for (const log of logs) {
    const caption = document.createElement("p");
    caption.textContent = `${log.name} — synthetic log stream (${log.lines.length} events)`;
    caption.style.cssText = "font: 12px ui-monospace, monospace; opacity: .75; margin: 10px 0 2px;";
    // Same element tree as before, built in signal-render.js. The prefix keeps
    // the class names this page's stylesheet, its cursor correlation
    // (`highlightLogs`) and its smoke assertions already depend on.
    const stream = logStream(log, { prefix: "sonda-playground" });
    pane.append(caption, stream);
  }
}

/* The cursor's reading, as DOM rather than canvas text.
 *
 * Spec said a readout box; making it real elements rather than `fillText` is
 * a deliberate delta and it buys three things: the numbers are selectable and
 * copyable, a screen reader can announce them (`aria-live="polite"`), and the
 * browser smoke suite can assert the exact strings instead of diffing pixels
 * — the lesson review #543 taught about the burst label.
 *
 * It sits UNDER the chart rather than floating at the pointer: a tooltip
 * would cover the trace it is describing, and at the moment you want to
 * compare two series you want to see both.
 */
function renderReadout(el, entries, cursorSecs) {
  let box = document.getElementById("sp-readout");
  if (!box) {
    box = document.createElement("div");
    box.id = "sp-readout";
    box.className = "sonda-playground__readout";
    box.setAttribute("aria-live", "polite");
    el.chart.insertAdjacentElement("afterend", box);
  }
  const rows = cursorSecs === null ? [] : cursorSamples(entries, cursorSecs);
  // Hidden rather than removed: the element is rebuilt on every pointer move,
  // and adding/removing a block on each one would reflow the page under the
  // pointer.
  box.hidden = !rows.length;
  if (!rows.length) {
    box.replaceChildren();
    delete box.dataset.secs;
    return;
  }
  box.dataset.secs = String(Math.round(cursorSecs * 100) / 100);

  const at = document.createElement("span");
  at.className = "sonda-playground__readat";
  at.textContent = formatSeconds(cursorSecs);
  const children = [at];
  for (const row of rows) {
    const index = entries.findIndex((entry) => entry.id === row.id);
    const chip = document.createElement("span");
    chip.className = "sonda-playground__readrow";
    const swatch = document.createElement("i");
    swatch.style.background = SERIES_COLORS[Math.max(0, index) % SERIES_COLORS.length];
    const label = document.createElement("b");
    label.textContent = row.name;
    chip.append(swatch, label, document.createTextNode(formatNumber(row.value)));
    children.push(chip);
  }
  box.replaceChildren(...children);
}

/* Highlight the log lines belonging to the cursor's instant.
 *
 * Which lines is `logLinesNear`; this walks the rendered rows and toggles a
 * class. The rows are in the same order as `log.lines` because renderLogs
 * appends them in order — stated because it is the assumption that makes the
 * index lookup valid, and it would break silently if that loop ever filtered.
 *
 * The first hit is scrolled into view WITHIN ITS OWN PANE — a deliberate
 * delta from the spec, which named `scrollIntoView({ block: "nearest" })`.
 * That API walks every scrollable ancestor, so bringing a log line into view
 * also scrolls the PAGE; the chart then slides out from under the pointer,
 * the browser fires pointerleave, and the cursor that asked for the scroll
 * is cleared. The highlight flashed and vanished on every hover. Adjusting
 * only `stream.scrollTop` confines the movement to the pane, which is what
 * the spec wanted the flag to mean.
 *
 * Nothing moves when the line is already visible: sweeping the cursor across
 * a chart should not yank a pane that is already showing the right lines.
 */
function highlightLogs(logs, cursorSecs) {
  const pane = document.getElementById("sp-logs");
  if (!pane) return;
  const streams = pane.querySelectorAll(".sonda-playground__logstream");
  let scrolled = false;
  streams.forEach((stream, streamIndex) => {
    const log = logs[streamIndex];
    const hits = cursorSecs === null || !log ? [] : logLinesNear(log, cursorSecs);
    const wanted = new Set(hits);
    const rows = stream.children;
    for (let i = 0; i < rows.length; i++) {
      rows[i].classList.toggle("sonda-playground__logline--at", wanted.has(i));
    }
    if (!scrolled && hits.length && rows[hits[0]]) {
      scrollRowIntoPane(stream, rows[hits[0]]);
      scrolled = true;
    }
  });
}

/* Bring one row into view inside its scroll container and nowhere else.
 *
 * Rect arithmetic rather than `offsetTop`, because the row's offsetParent is
 * not necessarily the stream — the pane is statically positioned, so the
 * offsets would be measured against whichever ancestor happens to establish
 * the containing block. Rects are always in the same coordinate space.
 */
function scrollRowIntoPane(stream, row) {
  const pane = stream.getBoundingClientRect();
  const rect = row.getBoundingClientRect();
  if (rect.top < pane.top) stream.scrollTop -= pane.top - rect.top;
  else if (rect.bottom > pane.bottom) stream.scrollTop += rect.bottom - pane.bottom;
}

/* The line chart.
 *
 * `opts.cursorSecs` is a point on the timeline to read out (null for none)
 * and `opts.ghost` is the previous shape of the same scenario, drawn faintly
 * underneath. Both are pure decoration over the same scales — WP9 added them
 * without an overlay canvas because a full redraw here is a few hundred
 * lineTo calls, and one drawing path is easier to keep honest than two that
 * must agree on geometry.
 */
function drawChart(canvas, entries, opts = {}) {
  const cursorSecs = typeof opts.cursorSecs === "number" ? opts.cursorSecs : null;
  const ghost = Array.isArray(opts.ghost) ? opts.ghost : null;
  const colors = palette();
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const cssHeight = 320;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);
  // A stale geometry would let the pointer read a chart that is no longer
  // drawn, so every early return clears it rather than leaving the last
  // scenario's mapping in place.
  canvas._geom = null;
  canvas.dataset.ghosts = "0";
  canvas.dataset.cursor = "";
  canvas.dataset.ghostPeak = "";
  canvas.dataset.peak = "";
  if (!entries.length) return;

  const pad = { left: 48, right: 12, top: 12, bottom: 26 };
  const plotW = cssWidth - pad.left - pad.right;
  const plotH = cssHeight - pad.top - pad.bottom;

  let min = Infinity;
  let max = -Infinity;
  let spanSecs = 0;
  // The ghost is inside BOTH domains, not clipped against them. A comparison
  // the reader cannot see both halves of is not a comparison, and clamping an
  // off-scale ghost to the plot edge would draw a flat line that never
  // existed. The cost is that the live trace re-scales when a ghost appears —
  // which is honest, because the chart is answering a different question at
  // that moment.
  //
  // The x-span matters as much as the y-domain, and it is the one that bites:
  // an edit to `rate` or `duration` changes how much time the samples cover,
  // so a ghost sized only by the live entries runs off the right edge and the
  // feature looks broken rather than wrong. Found by driving a scrub drag in
  // Chromium, not by reading the code.
  for (const entry of ghost || []) {
    if (!entry || !Array.isArray(entry.values) || !entry.values.length) continue;
    for (const value of entry.values) {
      if (value < min) min = value;
      if (value > max) max = value;
    }
    const tick = Number(entry.tick_secs);
    if (!Number.isFinite(tick) || tick <= 0) continue;
    spanSecs = Math.max(spanSecs, (entry.offset_secs || 0) + (entry.values.length - 1) * tick);
  }
  for (const entry of entries) {
    for (const value of entry.values) {
      if (value < min) min = value;
      if (value > max) max = value;
    }
    const offset = entry.offset_secs || 0;
    const seriesEnd = offset + (entry.values.length - 1) * entry.tick_secs;
    // Bound each series' window contribution to where it actually emits.
    spanSecs = Math.max(spanSecs, seriesEnd);
    entry._end_secs = seriesEnd;
  }
  if (!Number.isFinite(min)) return;
  if (max - min < 1e-9) {
    min -= 1;
    max += 1;
  }
  const range = max - min;
  min -= range * 0.08;
  max += range * 0.08;

  const x = (secs) => pad.left + (secs / spanSecs) * plotW;
  const y = (value) => pad.top + (1 - (value - min) / (max - min)) * plotH;

  // Schedule windows first, underneath the traces. Where the windows fall is
  // `scheduleWindows` in sonda-pure.js — the same function the docs widgets
  // shade with, so a gap means the same thing on both charts, and the slider
  // extremes that used to spin this loop forever (`every: 0`) are answered
  // once, under test, instead of here.
  //
  // A burst band also carries what it does to the emission rate, because that
  // is the one schedule setting the traces cannot show: the chart plots each
  // metric's VALUE, and a burst does not change the value — it changes how
  // often the value is emitted. One label per entry, on that entry's first
  // band, in that entry's series color and stacked by index so two bursting
  // entries do not print over each other.
  entries.forEach((entry, index) => {
    let firstBurst = null;
    for (const window of scheduleWindows(entry, entry._end_secs)) {
      ctx.fillStyle = window.kind === "burst" ? colors.burst : colors.gap;
      ctx.fillRect(x(window.start), pad.top, x(window.end) - x(window.start), plotH);
      if (window.kind === "burst" && !firstBurst) firstBurst = window;
    }
    const emission = firstBurst && burstEmission(entry);
    if (!emission) return;
    ctx.font = "11px ui-monospace, monospace";
    ctx.textAlign = "left";
    const width = ctx.measureText(emission.label).width;
    const left = Math.max(
      pad.left + 3,
      Math.min(x(firstBurst.start) + 4, cssWidth - pad.right - width)
    );
    const baseline = pad.top + 13 + index * 14;
    // Plate first: the label sits inside the plot, over traces.
    ctx.fillStyle = colors.plate;
    ctx.fillRect(left - 3, baseline - 10, width + 6, 13);
    ctx.fillStyle = SERIES_COLORS[index % SERIES_COLORS.length];
    ctx.fillText(emission.label, left, baseline);
  });

  ctx.strokeStyle = colors.grid;
  ctx.fillStyle = colors.text;
  ctx.lineWidth = 1;
  ctx.font = "11px ui-monospace, monospace";
  ctx.setLineDash([2, 5]);
  for (let row = 0; row <= 4; row++) {
    const value = min + ((max - min) * row) / 4;
    const gy = y(value);
    ctx.beginPath();
    ctx.moveTo(pad.left, gy);
    ctx.lineTo(cssWidth - pad.right, gy);
    ctx.stroke();
    ctx.textAlign = "right";
    ctx.fillText(formatNumber(value), pad.left - 6, gy + 4);
  }
  ctx.setLineDash([]);
  ctx.textAlign = "center";
  const xSteps = Math.min(6, Math.max(2, Math.floor(plotW / 110)));
  for (let step = 0; step <= xSteps; step++) {
    const secs = (spanSecs * step) / xSteps;
    ctx.fillText(formatSeconds(secs), x(secs), cssHeight - 8);
  }

  // The ghost: the same scenario as it was before this burst of edits,
  // underneath the live trace in its own colour at low alpha. Matched by
  // ENTRY ID, not by position — adding a scenario above an existing one
  // would otherwise re-pair every ghost with the wrong series and show a
  // change nobody made. An id present only in the ghost is dropped: a
  // scenario that no longer exists has no live trace to be compared with.
  let ghostsDrawn = 0;
  let ghostPeak = null;
  for (const before of ghost || []) {
    if (!before || !Array.isArray(before.values) || !before.values.length) continue;
    const index = entries.findIndex((entry) => entry.id === before.id);
    if (index < 0) continue;
    ghostsDrawn += 1;
    for (const value of before.values) {
      if (Number.isFinite(value) && (ghostPeak === null || value > ghostPeak)) ghostPeak = value;
    }
    const offset = before.offset_secs || 0;
    ctx.save();
    ctx.globalAlpha = 0.3;
    ctx.strokeStyle = SERIES_COLORS[index % SERIES_COLORS.length];
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.setLineDash([5, 4]);
    ctx.beginPath();
    before.values.forEach((value, tick) => {
      const px = x(offset + tick * before.tick_secs);
      const py = y(value);
      if (tick === 0) ctx.moveTo(px, py);
      else ctx.lineTo(px, py);
    });
    ctx.stroke();
    ctx.restore();
  }

  entries.forEach((entry, index) => {
    const offset = entry.offset_secs || 0;
    ctx.strokeStyle = SERIES_COLORS[index % SERIES_COLORS.length];
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.beginPath();
    entry.values.forEach((value, tick) => {
      const px = x(offset + tick * entry.tick_secs);
      const py = y(value);
      if (tick === 0) ctx.moveTo(px, py);
      else ctx.lineTo(px, py);
    });
    ctx.stroke();
  });

  // Causal connectors: where an `after:` chain resolved, drop a dashed line
  // from the upstream trace at the crossing time down to the dependent
  // series' first point, with an arrowhead and label.
  ctx.font = "10px ui-monospace, monospace";
  entries.forEach((entry, index) => {
    const offset = entry.offset_secs || 0;
    if (entry.after_ref) {
      const upstream = entries.find((e) => e.id === entry.after_ref);
      if (upstream && entry.values.length) {
        const upOffset = upstream.offset_secs || 0;
        const upTick = Math.round((offset - upOffset) / upstream.tick_secs);
        const upValue = upstream.values[Math.max(0, Math.min(upTick, upstream.values.length - 1))];
        const fromY = y(upValue);
        const toY = y(entry.values[0]);
        const px = x(offset);
        const color = SERIES_COLORS[index % SERIES_COLORS.length];
        ctx.strokeStyle = color;
        ctx.fillStyle = color;
        ctx.lineWidth = 1.5;
        ctx.setLineDash([3, 4]);
        ctx.beginPath();
        ctx.moveTo(px, fromY);
        ctx.lineTo(px, toY);
        ctx.stroke();
        ctx.setLineDash([]);
        const dir = toY >= fromY ? 1 : -1;
        ctx.beginPath();
        ctx.moveTo(px, toY);
        ctx.lineTo(px - 4, toY - dir * 7);
        ctx.lineTo(px + 4, toY - dir * 7);
        ctx.closePath();
        ctx.fill();
        ctx.textAlign = "left";
        ctx.fillText(`after ${entry.after_ref}`, px + 6, (fromY + toY) / 2 + 3);
      }
    }
    if (entry.while_label && entry.values.length) {
      ctx.fillStyle = SERIES_COLORS[index % SERIES_COLORS.length];
      ctx.textAlign = "left";
      ctx.fillText(entry.while_label, x(offset) + 6, y(entry.values[0]) - 8);
    }
  });

  // The time cursor, last so it sits over everything it is reading.
  //
  // The dots are drawn at the SNAPPED sample, not under the pointer: the
  // chart is a sampled signal, and a dot that slid smoothly along the line
  // would claim a resolution the engine never produced. At coarse rates the
  // gap between the rule and the dot is visible, and that gap is the truth.
  if (cursorSecs !== null) {
    const rows = cursorSamples(entries, cursorSecs);
    ctx.save();
    ctx.strokeStyle = colors.text;
    ctx.globalAlpha = 0.6;
    ctx.lineWidth = 1;
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(x(cursorSecs), pad.top);
    ctx.lineTo(x(cursorSecs), pad.top + plotH);
    ctx.stroke();
    ctx.restore();
    for (const row of rows) {
      const index = entries.findIndex((entry) => entry.id === row.id);
      if (index < 0) continue;
      ctx.fillStyle = SERIES_COLORS[index % SERIES_COLORS.length];
      ctx.beginPath();
      ctx.arc(x(row.secs), y(row.value), 3.5, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  // The mapping the pointer handler inverts. Stashed rather than recomputed
  // because a second copy of this arithmetic is a second place for it to be
  // wrong: `pad.left` and the span both depend on the entries and the
  // container width, and the cursor has to agree with the axis exactly.
  canvas._geom = { padLeft: pad.left, plotW, spanSecs };

  // What the chart is claiming, in a form a test can read exactly. The
  // browser suite cannot see a dashed line or a 3.5px dot, and a canvas
  // diff can only say that SOMETHING moved — the distinction review #543
  // was about.
  canvas.dataset.ghosts = String(ghostsDrawn);
  canvas.dataset.cursor = cursorSecs === null ? "" : String(Math.round(cursorSecs * 100) / 100);
  // The ghost's peak, which is what makes "pinned to the pre-edit state"
  // testable at all: a count alone cannot tell that baseline apart from the
  // spec's previous-run one, because both draw exactly one ghost. Under
  // previous-run semantics this number creeps toward the live trace on every
  // debounce; pinned, it does not move until the burst ends.
  canvas.dataset.ghostPeak = ghostPeak === null ? "" : String(Math.round(ghostPeak * 100) / 100);
  // The live peak, in the same units, so the two can be compared without the
  // test needing a window global to reach the entries.
  let peak = null;
  for (const entry of entries) {
    for (const value of entry.values) {
      if (Number.isFinite(value) && (peak === null || value > peak)) peak = value;
    }
  }
  canvas.dataset.peak = peak === null ? "" : String(Math.round(peak * 100) / 100);
}

/* Bucket bounds need more precision than axis ticks — 0.005 and 0.01 are
 * distinct buckets and must not round to the same label. */
if (window.document$ && typeof window.document$.subscribe === "function") {
  window.document$.subscribe(boot);
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
