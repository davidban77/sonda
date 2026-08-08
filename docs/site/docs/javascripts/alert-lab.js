/* Sonda docs — alert lab.
 *
 * Drives the #sonda-alert-lab page: a preset scenario is sampled by the real
 * sonda-core engine (same wasm bundle as the playground), a Prometheus-style
 * evaluator walks the series against a threshold + `for:` duration, and a
 * playback sweep draws the signal with an alert-state lane underneath
 * (inactive / pending / firing, with resolve markers).
 */
import init, { sample_scenario } from "./sonda_wasm.js";

const MAX_TICKS = 240;
const SWEEP_SECONDS = 11; // wall-clock length of one full playback sweep

const STATE_COLORS = {
  inactive: { light: "rgba(100, 116, 139, 0.25)", dark: "rgba(148, 163, 184, 0.25)" },
  pending: { light: "#eab308", dark: "#facc15" },
  firing: { light: "#dc2626", dark: "#f87171" },
};

/* The blip preset needs an irregular shape: two short dips a `for:` guard
 * should swallow, then one outage long enough to page. A sequence generator
 * expresses that exactly; the values array is built here to keep the YAML
 * readable. Rate 2/s, so counts below are in half-seconds. */
const BLIP_VALUES = [].concat(
  Array(56).fill(1), // 28s healthy
  Array(4).fill(0), //  2s blip
  Array(50).fill(1), // 25s healthy
  Array(6).fill(0), //  3s blip
  Array(44).fill(1), // 22s healthy
  Array(36).fill(0), // 18s real outage
  Array(44).fill(1) // recovery
);

function scenario(name, generatorYaml, { rate = 4, duration = "60s", labels = "" } = {}) {
  return `version: 2
kind: runnable
defaults:
  rate: ${rate}
  duration: ${duration}
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: lab
    signal_type: metrics
    name: ${name}
${generatorYaml}${labels}
`;
}

const PRESETS = [
  {
    name: "Link blips, then a real outage",
    op: "<",
    threshold: 1,
    forSecs: 6,
    story:
      "Two short blips stay pending and never page — the real 18-second outage fires and resolves on recovery. Set for: to 0s to feel the difference.",
    yaml: scenario(
      "link_up",
      `    generator:
      type: sequence
      values: [${BLIP_VALUES.join(", ")}]
      repeat: false
`,
      { rate: 2, duration: "120s" }
    ),
  },
  {
    name: "Memory leak",
    op: ">",
    threshold: 85,
    forSecs: 20,
    story:
      "The leak crosses the threshold with plenty of runway left — pending for 20 seconds, then firing until the end of the window. Leaks are the easy case for for:.",
    yaml: scenario(
      "process_memory_percent",
      `    generator:
      type: leak
      baseline: 12.0
      ceiling: 96.0
      time_to_ceiling: 100s
`,
      { rate: 4, duration: "120s" }
    ),
  },
  {
    name: "Latency spikes",
    op: ">",
    threshold: 300,
    forSecs: 12,
    story:
      "Each spike lasts 5 seconds — shorter than for: 12s, so the alert only ever reaches pending. Drop for: to 0s and every spike pages. Which on-call rotation do you want?",
    yaml: scenario(
      "request_latency_ms",
      `    generator:
      type: spike_event
      baseline: 120.0
      spike_height: 400.0
      spike_duration: 5s
      spike_interval: 30s
`,
      { rate: 4, duration: "120s" }
    ),
  },
  {
    name: "Noisy CPU near the line",
    op: ">",
    threshold: 64,
    forSecs: 12,
    story:
      "Noise brushes the threshold on every peak — without for: this alert flaps constantly. With it, only a sustained excursion could page.",
    yaml: scenario(
      "cpu_usage",
      `    generator:
      type: steady
      center: 55.0
      amplitude: 9.0
      period: 30s
      noise: 4.0
      noise_seed: 3
`,
      { rate: 4, duration: "120s" }
    ),
  },
];

let wasmReady = null;
function ensureWasm() {
  if (!wasmReady) {
    wasmReady = init({ module_or_path: new URL("./sonda_wasm_bg.wasm", import.meta.url) });
  }
  return wasmReady;
}

/* Walk the series the way a Prometheus rule walks scrape samples: pending
 * while the condition holds but hasn't lasted `for:` yet, firing once it
 * has, back to inactive (recording a resolve) when it goes false. */
function evaluate(values, tickSecs, op, threshold, forSecs) {
  const states = new Array(values.length);
  const resolves = [];
  let run = 0;
  let wasFiring = false;
  for (let i = 0; i < values.length; i++) {
    const cond = op === ">" ? values[i] > threshold : values[i] < threshold;
    if (!cond) {
      if (wasFiring) resolves.push(i);
      run = 0;
      wasFiring = false;
      states[i] = "inactive";
    } else {
      run += 1;
      const heldSecs = (run - 1) * tickSecs;
      states[i] = heldSecs >= forSecs ? "firing" : "pending";
      wasFiring = states[i] === "firing";
    }
  }
  return { states, resolves };
}

function boot() {
  const root = document.getElementById("sonda-alert-lab");
  if (!root || root.dataset.ready) return;
  root.dataset.ready = "1";

  const el = {
    preset: document.getElementById("al-preset"),
    op: document.getElementById("al-op"),
    threshold: document.getElementById("al-threshold"),
    forSel: document.getElementById("al-for"),
    play: document.getElementById("al-play"),
    state: document.getElementById("al-state"),
    chart: document.getElementById("al-chart"),
    story: document.getElementById("al-story"),
    open: document.getElementById("al-open"),
  };

  PRESETS.forEach((preset, index) => {
    const option = document.createElement("option");
    option.value = String(index);
    option.textContent = preset.name;
    el.preset.appendChild(option);
  });

  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  let entry = null; // sampled series from the engine
  let evaled = null;
  let animation = 0;

  const currentRule = () => ({
    op: el.op.value,
    threshold: Number(el.threshold.value),
    forSecs: Number(el.forSel.value),
  });

  const reevaluate = () => {
    if (!entry) return;
    const rule = currentRule();
    evaled = evaluate(entry.values, entry.tick_secs, rule.op, rule.threshold, rule.forSecs);
    stopSweep();
    draw(el, entry, evaled, rule, entry.values.length);
    setStateChip(el.state, finalStateSummary(evaled));
    el.play.textContent = "Play";
  };

  const loadPreset = async () => {
    const preset = PRESETS[Number(el.preset.value)];
    el.op.value = preset.op;
    el.threshold.value = String(preset.threshold);
    el.forSel.value = String(preset.forSecs);
    el.story.textContent = preset.story;
    el.open.href = "./#yaml=" + toBase64Url(preset.yaml);
    await ensureWasm();
    const result = JSON.parse(sample_scenario(preset.yaml, MAX_TICKS));
    entry = result.ok && result.entries.length ? result.entries[0] : null;
    reevaluate();
    if (!reducedMotion) startSweep();
  };

  function stopSweep() {
    if (animation) {
      window.cancelAnimationFrame(animation);
      animation = 0;
    }
  }

  function startSweep() {
    if (!entry || !evaled) return;
    stopSweep();
    const rule = currentRule();
    const total = entry.values.length;
    const start = performance.now();
    el.play.textContent = "Replay";
    const frame = (now) => {
      const progress = Math.min(1, (now - start) / (SWEEP_SECONDS * 1000));
      const upTo = Math.max(2, Math.round(progress * total));
      draw(el, entry, evaled, rule, upTo);
      setStateChip(el.state, evaled.states[upTo - 1]);
      if (progress < 1) {
        animation = window.requestAnimationFrame(frame);
      } else {
        animation = 0;
        setStateChip(el.state, finalStateSummary(evaled));
      }
    };
    animation = window.requestAnimationFrame(frame);
  }

  el.preset.addEventListener("change", loadPreset);
  el.op.addEventListener("change", reevaluate);
  el.threshold.addEventListener("input", reevaluate);
  el.forSel.addEventListener("change", reevaluate);
  el.play.addEventListener("click", () => {
    if (reducedMotion) reevaluate();
    else startSweep();
  });

  new ResizeObserver(() => {
    if (entry && evaled && !animation) {
      draw(el, entry, evaled, currentRule(), entry.values.length);
    }
  }).observe(el.chart.parentElement);
  new MutationObserver(() => {
    if (entry && evaled && !animation) {
      draw(el, entry, evaled, currentRule(), entry.values.length);
    }
  }).observe(document.body, { attributes: true, attributeFilter: ["data-md-color-scheme"] });

  loadPreset();
}

function finalStateSummary(evaled) {
  if (evaled.states.includes("firing")) return "firing";
  if (evaled.states.includes("pending")) return "pending";
  return "inactive";
}

function setStateChip(chip, state) {
  chip.textContent = state;
  chip.className = `sonda-lab-chip sonda-lab-chip--${state}`;
}

function toBase64Url(text) {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  bytes.forEach((b) => (binary += String.fromCharCode(b)));
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function palette() {
  const dark = document.body.getAttribute("data-md-color-scheme") === "slate";
  return {
    dark,
    grid: dark ? "rgba(148, 163, 184, 0.25)" : "rgba(100, 116, 139, 0.25)",
    text: dark ? "#94a3b8" : "#64748b",
    trace: "#f97316",
    thresholdLine: dark ? "#f87171" : "#dc2626",
    firingBand: dark ? "rgba(248, 113, 113, 0.10)" : "rgba(220, 38, 38, 0.07)",
    resolve: dark ? "#4ade80" : "#16a34a",
  };
}

function draw(el, entry, evaled, rule, upTo) {
  const colors = palette();
  const canvas = el.chart;
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const cssHeight = 380;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const laneH = 26;
  const laneGap = 34;
  const pad = { left: 48, right: 12, top: 12, bottom: 26 + laneH + laneGap };
  const plotW = cssWidth - pad.left - pad.right;
  const plotH = cssHeight - pad.top - pad.bottom;
  const laneY = pad.top + plotH + laneGap;

  const values = entry.values;
  const spanSecs = (values.length - 1) * entry.tick_secs;
  let min = Math.min(...values, rule.threshold);
  let max = Math.max(...values, rule.threshold);
  if (max - min < 1e-9) {
    min -= 1;
    max += 1;
  }
  const range = max - min;
  min -= range * 0.1;
  max += range * 0.1;

  const x = (i) => pad.left + ((i * entry.tick_secs) / spanSecs) * plotW;
  const y = (v) => pad.top + (1 - (v - min) / (max - min)) * plotH;

  // Firing bands behind the trace, only up to the sweep cursor. Consecutive
  // ticks are merged into one rect so bands render seamlessly at any DPR.
  ctx.fillStyle = colors.firingBand;
  for (const run of stateRuns(evaled.states, upTo)) {
    if (run.state === "firing") {
      ctx.fillRect(x(run.start), pad.top, x(run.end) - x(run.start), plotH);
    }
  }

  // Grid + axis labels.
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
    const label = secs >= 60 ? `${Math.floor(secs / 60)}m${Math.round(secs % 60) || ""}${secs % 60 ? "s" : ""}` : `${Math.round(secs)}s`;
    ctx.fillText(label, pad.left + (secs / spanSecs) * plotW, pad.top + plotH + 16);
  }

  // Threshold line.
  ctx.strokeStyle = colors.thresholdLine;
  ctx.setLineDash([6, 4]);
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  ctx.moveTo(pad.left, y(rule.threshold));
  ctx.lineTo(cssWidth - pad.right, y(rule.threshold));
  ctx.stroke();
  ctx.setLineDash([]);
  ctx.textAlign = "left";
  ctx.fillStyle = colors.thresholdLine;
  ctx.fillText(`${rule.op} ${rule.threshold}`, pad.left + 4, y(rule.threshold) - 5);

  // Signal trace up to the sweep cursor.
  ctx.strokeStyle = colors.trace;
  ctx.lineWidth = 2;
  ctx.lineJoin = "round";
  ctx.beginPath();
  for (let i = 0; i < upTo; i++) {
    const px = x(i);
    const py = y(values[i]);
    if (i === 0) ctx.moveTo(px, py);
    else ctx.lineTo(px, py);
  }
  ctx.stroke();

  // Alert-state lane, drawn as merged runs for seamless segments.
  ctx.fillStyle = colors.text;
  ctx.textAlign = "left";
  ctx.font = "10px ui-monospace, monospace";
  ctx.fillText("ALERT STATE", pad.left, laneY - 7);
  const stateColor = (s) => STATE_COLORS[s][colors.dark ? "dark" : "light"];
  for (const run of stateRuns(evaled.states, upTo)) {
    ctx.fillStyle = stateColor(run.state);
    ctx.fillRect(x(run.start), laneY, Math.max(1, x(run.end) - x(run.start)), laneH);
  }
  // Resolve markers.
  ctx.strokeStyle = colors.resolve;
  ctx.lineWidth = 2;
  for (const idx of evaled.resolves) {
    if (idx >= upTo) continue;
    ctx.beginPath();
    ctx.moveTo(x(idx), laneY - 4);
    ctx.lineTo(x(idx), laneY + laneH + 4);
    ctx.stroke();
  }
}

/* Group consecutive same-state ticks into runs [start, end) for seamless
 * rectangle rendering. */
function stateRuns(states, upTo) {
  const runs = [];
  let start = 0;
  for (let i = 1; i <= upTo; i++) {
    if (i === upTo || states[i] !== states[start]) {
      runs.push({ state: states[start], start, end: i });
      start = i;
    }
  }
  return runs;
}

function formatNumber(value) {
  if (Math.abs(value) >= 1000) return value.toFixed(0);
  if (Math.abs(value) >= 10) return value.toFixed(1);
  return value.toFixed(2);
}

if (window.document$ && typeof window.document$.subscribe === "function") {
  window.document$.subscribe(boot);
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
