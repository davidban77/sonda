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
} from "./sonda-pure.js";

const MAX_TICKS = 240;
const DEBOUNCE_MS = 500;

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

  const run = async () => {
    el.status.textContent = "compiling…";
    try {
      await ensureWasm();
      const yaml = editor.getValue();
      const result = JSON.parse(sample_scenario(yaml, MAX_TICKS));
      lastResult = result;
      render(el, result);
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
    window.clearTimeout(debounceTimer);
    debounceTimer = window.setTimeout(run, DEBOUNCE_MS);
  };

  const shared = fromLocationHash();
  el.editor.value = shared.yaml !== null ? shared.yaml : PRESETS[0].yaml;
  editor = await mountEditor(el.editor, { onChange: scheduleRun, onRun: run });

  el.run.addEventListener("click", run);
  el.preset.addEventListener("change", () => {
    editor.setValue(PRESETS[Number(el.preset.value)].yaml);
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

  // Redraw on container resize; re-render chart and editor theme on
  // light/dark scheme change.
  new ResizeObserver(() => lastResult && render(el, lastResult)).observe(el.chart.parentElement);
  new MutationObserver(() => {
    if (lastResult) render(el, lastResult);
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

function render(el, result) {
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
  if (hasLines || !hasExtras) drawChart(el.chart, result.entries);
  syncPngButton(el);

  renderExtraCharts(el, histograms, summaries);
  renderLogs(el, logs);

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
function renderLogs(el, logs) {
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

  for (const log of logs) {
    const caption = document.createElement("p");
    caption.textContent = `${log.name} — synthetic log stream (${log.lines.length} events)`;
    caption.style.cssText = "font: 12px ui-monospace, monospace; opacity: .75; margin: 10px 0 2px;";
    const stream = document.createElement("div");
    stream.className = "sonda-playground__logstream";
    for (const line of log.lines) {
      const row = document.createElement("div");
      row.className = `sonda-playground__logline sonda-playground__logline--${line.severity}`;
      const at = document.createElement("span");
      at.className = "sonda-playground__logat";
      at.textContent = `+${line.secs.toFixed(line.secs % 1 ? 2 : 0)}s`;
      const sev = document.createElement("span");
      sev.className = "sonda-playground__logsev";
      sev.textContent = line.severity.toUpperCase().padEnd(5);
      row.append(at, sev, document.createTextNode(line.message));
      stream.appendChild(row);
    }
    pane.append(caption, stream);
  }
}

function drawHistogramHeatmap(canvas, histogram) {
  const colors = palette();
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const rows = histogram.bucket_bounds.length + 1; // +Inf row on top
  const rowHeight = Math.max(12, Math.min(20, Math.floor(240 / rows)));
  const pad = { left: 64, right: 12, top: 8, bottom: 26 };
  const cssHeight = pad.top + rows * rowHeight + pad.bottom;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const ticks = histogram.counts.length;
  if (!ticks) return;
  const offset = histogram.offset_secs || 0;
  const spanSecs = offset + ticks * histogram.tick_secs;
  const plotW = cssWidth - pad.left - pad.right;
  const x = (secs) => pad.left + (secs / spanSecs) * plotW;
  // Row 0 (lowest bucket) sits at the bottom, like a latency axis.
  const rowY = (row) => pad.top + (rows - 1 - row) * rowHeight;

  let maxCount = 1;
  for (const row of histogram.counts) {
    for (const count of row) if (count > maxCount) maxCount = count;
  }

  const cellW = Math.max(1, (histogram.tick_secs / spanSecs) * plotW);
  histogram.counts.forEach((rowCounts, tick) => {
    const px = x(offset + tick * histogram.tick_secs);
    rowCounts.forEach((count, row) => {
      if (!count) return;
      const alpha = 0.12 + 0.88 * (count / maxCount);
      ctx.fillStyle = `rgba(249, 115, 22, ${alpha.toFixed(3)})`;
      ctx.fillRect(px, rowY(row), cellW + 0.5, rowHeight - 1);
    });
  });

  ctx.fillStyle = colors.text;
  ctx.font = "10px ui-monospace, monospace";
  ctx.textAlign = "right";
  const labelEvery = Math.ceil(rows / 12);
  for (let row = 0; row < rows; row++) {
    if (row % labelEvery !== 0 && row !== rows - 1) continue;
    const label = row === rows - 1 ? "+Inf" : `≤${formatBound(histogram.bucket_bounds[row])}`;
    ctx.fillText(label, pad.left - 6, rowY(row) + rowHeight / 2 + 3);
  }
  ctx.textAlign = "center";
  const xSteps = Math.min(6, Math.max(2, Math.floor(plotW / 110)));
  for (let step = 0; step <= xSteps; step++) {
    const secs = (spanSecs * step) / xSteps;
    ctx.fillText(formatSeconds(secs), x(secs), cssHeight - 8);
  }
}

function drawSummaryBands(canvas, summary) {
  const colors = palette();
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const cssHeight = 220;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const ticks = summary.values.length;
  const quantileCount = summary.quantiles.length;
  if (!ticks || !quantileCount) return;

  const pad = { left: 48, right: 46, top: 12, bottom: 26 };
  const plotW = cssWidth - pad.left - pad.right;
  const plotH = cssHeight - pad.top - pad.bottom;
  const offset = summary.offset_secs || 0;
  const spanSecs = offset + (ticks - 1) * summary.tick_secs;

  let min = Infinity;
  let max = -Infinity;
  for (const row of summary.values) {
    for (const value of row) {
      if (value < min) min = value;
      if (value > max) max = value;
    }
  }
  if (!Number.isFinite(min)) return;
  if (max - min < 1e-12) {
    min -= 1;
    max += 1;
  }
  const range = max - min;
  min -= range * 0.08;
  max += range * 0.08;
  const x = (tick) => pad.left + ((offset + tick * summary.tick_secs) / spanSecs) * plotW;
  const y = (value) => pad.top + (1 - (value - min) / (max - min)) * plotH;

  ctx.strokeStyle = colors.grid;
  ctx.fillStyle = colors.text;
  ctx.lineWidth = 1;
  ctx.font = "10px ui-monospace, monospace";
  ctx.setLineDash([2, 5]);
  for (let row = 0; row <= 3; row++) {
    const value = min + ((max - min) * row) / 3;
    const gy = y(value);
    ctx.beginPath();
    ctx.moveTo(pad.left, gy);
    ctx.lineTo(cssWidth - pad.right, gy);
    ctx.stroke();
    ctx.textAlign = "right";
    ctx.fillText(formatNumber(value), pad.left - 6, gy + 3);
  }
  ctx.setLineDash([]);

  // Envelope between the lowest and highest quantile, then one line per
  // quantile, brightest at the median end.
  ctx.fillStyle = "rgba(59, 130, 246, 0.14)";
  ctx.beginPath();
  summary.values.forEach((row, tick) => {
    const py = y(row[0]);
    if (tick === 0) ctx.moveTo(x(tick), py);
    else ctx.lineTo(x(tick), py);
  });
  for (let tick = ticks - 1; tick >= 0; tick--) {
    ctx.lineTo(x(tick), y(summary.values[tick][quantileCount - 1]));
  }
  ctx.closePath();
  ctx.fill();

  for (let q = 0; q < quantileCount; q++) {
    const alpha = 0.35 + 0.65 * (1 - q / Math.max(1, quantileCount - 1));
    ctx.strokeStyle = `rgba(59, 130, 246, ${alpha.toFixed(3)})`;
    ctx.lineWidth = q === 0 ? 2 : 1.4;
    ctx.beginPath();
    summary.values.forEach((row, tick) => {
      const py = y(row[q]);
      if (tick === 0) ctx.moveTo(x(tick), py);
      else ctx.lineTo(x(tick), py);
    });
    ctx.stroke();
    ctx.fillStyle = colors.text;
    ctx.textAlign = "left";
    const lastY = y(summary.values[ticks - 1][q]);
    ctx.fillText(`p${Math.round(summary.quantiles[q] * 100)}`, cssWidth - pad.right + 4, lastY + 3);
  }

  ctx.fillStyle = colors.text;
  ctx.textAlign = "center";
  const xSteps = Math.min(6, Math.max(2, Math.floor(plotW / 110)));
  for (let step = 0; step <= xSteps; step++) {
    const secs = (spanSecs * step) / xSteps;
    const px = pad.left + (secs / spanSecs) * plotW;
    ctx.fillText(formatSeconds(secs), px, cssHeight - 8);
  }
}

function palette() {
  const dark = document.body.getAttribute("data-md-color-scheme") === "slate";
  return {
    grid: dark ? "rgba(148, 163, 184, 0.25)" : "rgba(100, 116, 139, 0.25)",
    text: dark ? "#94a3b8" : "#64748b",
    gap: dark ? "rgba(148, 163, 184, 0.14)" : "rgba(100, 116, 139, 0.12)",
    burst: dark ? "rgba(253, 186, 116, 0.14)" : "rgba(249, 115, 22, 0.10)",
    // Backing plate for the burst label, which is drawn INSIDE the plot
    // and would otherwise be read through whatever trace passes behind it.
    plate: dark ? "rgba(15, 23, 42, 0.82)" : "rgba(255, 255, 255, 0.82)",
  };
}

function drawChart(canvas, entries) {
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
  if (!entries.length) return;

  const pad = { left: 48, right: 12, top: 12, bottom: 26 };
  const plotW = cssWidth - pad.left - pad.right;
  const plotH = cssHeight - pad.top - pad.bottom;

  let min = Infinity;
  let max = -Infinity;
  let spanSecs = 0;
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
}

/* Bucket bounds need more precision than axis ticks — 0.005 and 0.01 are
 * distinct buckets and must not round to the same label. */
function formatBound(value) {
  if (value !== 0 && Math.abs(value) < 0.01) return value.toPrecision(1);
  return formatNumber(value);
}

function formatNumber(value) {
  if (Math.abs(value) >= 1000) return value.toFixed(0);
  if (Math.abs(value) >= 10) return value.toFixed(1);
  return value.toFixed(2);
}

function formatSeconds(secs) {
  const rounded = Math.round(secs);
  if (rounded < 60) return `${rounded}s`;
  const mins = Math.floor(rounded / 60);
  const rest = rounded % 60;
  return rest ? `${mins}m${rest}s` : `${mins}m`;
}

if (window.document$ && typeof window.document$.subscribe === "function") {
  window.document$.subscribe(boot);
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
