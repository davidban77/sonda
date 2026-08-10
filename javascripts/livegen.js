/* Sonda docs — live generator widgets.
 *
 * Progressive enhancement for generators.md: each `.sonda-livegen` placeholder
 * becomes a mini-chart with parameter sliders, sampled by the same wasm engine
 * that powers the playground (sonda_wasm.js). The static SVG above each widget
 * is the no-JS fallback and is hidden once the live chart first renders.
 *
 * The wasm binary is fetched lazily — only when the first widget scrolls near
 * the viewport — so pages without widgets (and readers who never scroll) pay
 * nothing.
 */
import init, { sample_scenario } from "./sonda_wasm.js";
import { toBase64Url } from "./sonda-pure.js";
import { WIDGETS, defaultParams } from "./livegen-presets.js";

const MAX_TICKS = 240;
const DEBOUNCE_MS = 150;

let wasmReady = null;
function ensureWasm() {
  if (!wasmReady) {
    wasmReady = init({ module_or_path: new URL("./sonda_wasm_bg.wasm", import.meta.url) });
  }
  return wasmReady;
}

const live = new Set(); // initialized widgets, for theme redraws

function boot() {
  const placeholders = document.querySelectorAll(".sonda-livegen:not([data-ready])");
  if (!placeholders.length) return;

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        observer.unobserve(entry.target);
        mount(entry.target);
      }
    },
    { rootMargin: "250px" }
  );
  placeholders.forEach((el) => {
    el.dataset.ready = "1";
    observer.observe(el);
  });
}

async function mount(root) {
  const preset = WIDGETS[root.dataset.gen];
  if (!preset) return;

  const params = defaultParams(preset);
  const canvas = document.createElement("canvas");
  canvas.className = "sonda-livegen__chart";
  canvas.setAttribute("aria-label", `Live chart of the ${root.dataset.gen} generator`);
  const controls = document.createElement("div");
  controls.className = "sonda-livegen__controls";
  const error = document.createElement("p");
  error.className = "sonda-livegen__error";
  error.hidden = true;
  root.append(canvas, controls, error);

  let currentYaml = "";
  const link = document.createElement("a");
  link.className = "sonda-livegen__open";
  link.textContent = "Open in playground →";

  for (const slider of preset.sliders) {
    const row = document.createElement("label");
    row.className = "sonda-livegen__row";
    const name = document.createElement("span");
    name.className = "sonda-livegen__key";
    name.textContent = slider.key;
    const input = document.createElement("input");
    input.type = "range";
    input.min = String(slider.min);
    input.max = String(slider.max);
    input.step = String(slider.step);
    input.value = String(slider.value);
    const readout = document.createElement("span");
    readout.className = "sonda-livegen__value";
    const show = (v) => `${v}${slider.unit || ""}`;
    readout.textContent = show(slider.value);
    input.addEventListener("input", () => {
      params[slider.key] = Number(input.value);
      readout.textContent = show(input.value);
      schedule();
    });
    row.append(name, input, readout);
    controls.appendChild(row);
  }
  controls.appendChild(link);

  let firstRender = true;
  const render = async () => {
    try {
      await ensureWasm();
      currentYaml = preset.yaml(params);
      link.href = "../../playground/#yaml=" + toBase64Url(currentYaml);
      const result = JSON.parse(sample_scenario(currentYaml, MAX_TICKS));
      if (!result.ok) {
        error.hidden = false;
        error.textContent = result.error || "compile error";
        return;
      }
      error.hidden = true;
      const entry = result.entries[0];
      root._draw = () => drawMini(canvas, entry);
      root._draw();
      live.add(root);
      if (firstRender) {
        firstRender = false;
        hideStaticImage(root);
      }
    } catch (err) {
      error.hidden = false;
      error.textContent = String(err);
    }
  };

  let timer = 0;
  const schedule = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(render, DEBOUNCE_MS);
  };

  render();
}

/* The paragraph holding the static SVG sits directly above the widget; once
 * the live chart is on screen the still image is redundant. Defensive walk —
 * if the structure isn't what we expect, leave the page alone. */
function hideStaticImage(root) {
  let sibling = root.previousElementSibling;
  for (let hops = 0; sibling && hops < 2; hops++) {
    const img = sibling.querySelector && sibling.querySelector('img[src*="img/generators/"]');
    if (img) {
      sibling.hidden = true;
      return;
    }
    sibling = sibling.previousElementSibling;
  }
}

function palette() {
  const dark = document.body.getAttribute("data-md-color-scheme") === "slate";
  return {
    grid: dark ? "rgba(148, 163, 184, 0.25)" : "rgba(100, 116, 139, 0.25)",
    text: dark ? "#94a3b8" : "#64748b",
    line: "#f97316",
  };
}

function drawMini(canvas, entry) {
  const colors = palette();
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const cssHeight = 150;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const values = entry.values;
  if (!values.length) return;
  const pad = { left: 42, right: 10, top: 8, bottom: 20 };
  const plotW = cssWidth - pad.left - pad.right;
  const plotH = cssHeight - pad.top - pad.bottom;

  let min = Infinity;
  let max = -Infinity;
  for (const v of values) {
    if (v < min) min = v;
    if (v > max) max = v;
  }
  if (!Number.isFinite(min)) return;
  if (max - min < 1e-9) {
    min -= 1;
    max += 1;
  }
  const range = max - min;
  min -= range * 0.1;
  max += range * 0.1;

  const x = (i) => pad.left + (i / (values.length - 1 || 1)) * plotW;
  const y = (v) => pad.top + (1 - (v - min) / (max - min)) * plotH;

  ctx.strokeStyle = colors.grid;
  ctx.fillStyle = colors.text;
  ctx.lineWidth = 1;
  ctx.font = "10px ui-monospace, monospace";
  ctx.setLineDash([2, 5]);
  for (let row = 0; row <= 2; row++) {
    const value = min + ((max - min) * row) / 2;
    const gy = y(value);
    ctx.beginPath();
    ctx.moveTo(pad.left, gy);
    ctx.lineTo(cssWidth - pad.right, gy);
    ctx.stroke();
    ctx.textAlign = "right";
    ctx.fillText(fmt(value), pad.left - 5, gy + 3);
  }
  ctx.setLineDash([]);

  const spanSecs = (entry.offset_secs || 0) + values.length * entry.tick_secs;
  ctx.textAlign = "center";
  for (let step = 0; step <= 3; step++) {
    const secs = (spanSecs * step) / 3;
    ctx.fillText(fmtSecs(secs), pad.left + (plotW * step) / 3, cssHeight - 6);
  }

  ctx.strokeStyle = colors.line;
  ctx.lineWidth = 1.8;
  ctx.lineJoin = "round";
  ctx.beginPath();
  values.forEach((v, i) => {
    if (i === 0) ctx.moveTo(x(i), y(v));
    else ctx.lineTo(x(i), y(v));
  });
  ctx.stroke();
}

function fmt(value) {
  if (Math.abs(value) >= 1000) return value.toFixed(0);
  if (Math.abs(value) >= 10) return value.toFixed(1);
  return value.toFixed(2);
}

function fmtSecs(secs) {
  const rounded = Math.round(secs);
  if (rounded < 60) return `${rounded}s`;
  const mins = Math.floor(rounded / 60);
  const rest = rounded % 60;
  return rest ? `${mins}m${rest}s` : `${mins}m`;
}

// Theme flips and resizes redraw every live widget from its cached samples;
// widgets detached by instant navigation are pruned as we go.
function redrawAll() {
  live.forEach((root) => {
    if (!root.isConnected) {
      live.delete(root);
      return;
    }
    if (root._draw) root._draw();
  });
}
new MutationObserver(redrawAll).observe(document.body, {
  attributes: true,
  attributeFilter: ["data-md-color-scheme"],
});
window.addEventListener("resize", redrawAll);

if (window.document$ && typeof window.document$.subscribe === "function") {
  window.document$.subscribe(boot);
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
