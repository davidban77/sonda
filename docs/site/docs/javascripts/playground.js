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

function boot() {
  const root = document.getElementById("sonda-playground");
  if (!root || root.dataset.ready) return;
  root.dataset.ready = "1";

  const el = {
    preset: document.getElementById("sp-preset"),
    run: document.getElementById("sp-run"),
    share: document.getElementById("sp-share"),
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

  const run = async () => {
    el.status.textContent = "compiling…";
    try {
      await ensureWasm();
      const result = JSON.parse(sample_scenario(el.editor.value, MAX_TICKS));
      lastResult = result;
      render(el, result);
      el.status.textContent = result.ok ? "" : "compile error";
    } catch (err) {
      el.status.textContent = "engine failed to load";
      showError(el, String(err));
    }
  };

  const scheduleRun = () => {
    window.clearTimeout(debounceTimer);
    debounceTimer = window.setTimeout(run, DEBOUNCE_MS);
  };

  el.editor.addEventListener("input", scheduleRun);
  el.run.addEventListener("click", run);
  el.preset.addEventListener("change", () => {
    el.editor.value = PRESETS[Number(el.preset.value)].yaml;
    run();
  });
  el.share.addEventListener("click", () => {
    const url = new URL(window.location.href);
    url.hash = "yaml=" + toBase64Url(el.editor.value);
    navigator.clipboard.writeText(url.toString()).then(() => {
      el.share.textContent = "Copied!";
      window.setTimeout(() => {
        el.share.textContent = "Copy link";
      }, 1400);
    });
  });

  // Redraw on container resize and on light/dark scheme change.
  new ResizeObserver(() => lastResult && render(el, lastResult)).observe(el.chart.parentElement);
  new MutationObserver(() => lastResult && render(el, lastResult)).observe(document.body, {
    attributes: true,
    attributeFilter: ["data-md-color-scheme"],
  });

  const shared = fromLocationHash();
  el.editor.value = shared !== null ? shared : PRESETS[0].yaml;
  run();
}

function fromLocationHash() {
  const match = window.location.hash.match(/^#yaml=(.+)$/);
  if (!match) return null;
  try {
    return fromBase64Url(match[1]);
  } catch {
    return null;
  }
}

function toBase64Url(text) {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  bytes.forEach((b) => (binary += String.fromCharCode(b)));
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromBase64Url(encoded) {
  const base64 = encoded.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64);
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

function showError(el, message) {
  el.error.hidden = false;
  el.error.textContent = message;
}

function render(el, result) {
  if (!result.ok) {
    showError(el, result.error || "unknown compile error");
    return;
  }
  el.error.hidden = true;

  drawChart(el.chart, result.entries);

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
    .join("\n")
    .trim();
}

function palette() {
  const dark = document.body.getAttribute("data-md-color-scheme") === "slate";
  return {
    grid: dark ? "rgba(148, 163, 184, 0.25)" : "rgba(100, 116, 139, 0.25)",
    text: dark ? "#94a3b8" : "#64748b",
    gap: dark ? "rgba(148, 163, 184, 0.14)" : "rgba(100, 116, 139, 0.12)",
    burst: dark ? "rgba(253, 186, 116, 0.14)" : "rgba(249, 115, 22, 0.10)",
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
    spanSecs = Math.max(spanSecs, (entry.values.length - 1) * entry.tick_secs);
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

  // Schedule windows first, underneath the traces. Bursts occupy the head of
  // each cycle, gaps the tail — matching the engine's window math.
  for (const entry of entries) {
    if (entry.burst) {
      ctx.fillStyle = colors.burst;
      for (let start = 0; start < spanSecs; start += entry.burst.every_secs) {
        const end = Math.min(start + entry.burst.for_secs, spanSecs);
        ctx.fillRect(x(start), pad.top, x(end) - x(start), plotH);
      }
    }
    if (entry.gap) {
      ctx.fillStyle = colors.gap;
      const { every_secs, for_secs } = entry.gap;
      for (let cycle = 0; cycle * every_secs < spanSecs; cycle++) {
        const start = cycle * every_secs + (every_secs - for_secs);
        const end = Math.min(start + for_secs, spanSecs);
        if (start >= spanSecs) break;
        ctx.fillRect(x(start), pad.top, x(end) - x(start), plotH);
      }
    }
  }

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
    ctx.strokeStyle = SERIES_COLORS[index % SERIES_COLORS.length];
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.beginPath();
    entry.values.forEach((value, tick) => {
      const px = x(tick * entry.tick_secs);
      const py = y(value);
      if (tick === 0) ctx.moveTo(px, py);
      else ctx.lineTo(px, py);
    });
    ctx.stroke();
  });
}

function formatNumber(value) {
  if (Math.abs(value) >= 1000) return value.toFixed(0);
  if (Math.abs(value) >= 10) return value.toFixed(1);
  return value.toFixed(2);
}

function formatSeconds(secs) {
  if (secs >= 60) {
    const mins = Math.floor(secs / 60);
    const rest = Math.round(secs % 60);
    return rest ? `${mins}m${rest}s` : `${mins}m`;
  }
  return `${Math.round(secs)}s`;
}

if (window.document$ && typeof window.document$.subscribe === "function") {
  window.document$.subscribe(boot);
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
