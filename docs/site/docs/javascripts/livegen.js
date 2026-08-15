/* Sonda docs — live generator widgets.
 *
 * Progressive enhancement, in two placeholder forms sharing one engine, one
 * observer and one renderer:
 *
 *   data-gen="sine"        generators.md — a preset with parameter sliders.
 *                          The static SVG above it is the no-JS fallback and
 *                          is hidden once the live chart first renders.
 *   data-yaml-b64="…"      the examples gallery on test/examples.md — an
 *                          arbitrary scenario file, no sliders, carrying its
 *                          own YAML. The markdown table above it is the no-JS
 *                          fallback and is never touched.
 *
 * Both are sampled by the same wasm engine that powers the playground
 * (sonda_wasm.js), and the wasm binary is fetched lazily — only when the
 * first widget scrolls near the viewport — so pages without widgets, and
 * readers who never scroll, pay nothing. That laziness is what makes a page
 * carrying 45 gallery widgets cost the same on load as one carrying none.
 */
import init, { sample_scenario } from "./sonda_wasm.js";
import {
  toBase64Url,
  fromBase64Url,
  galleryCardState,
  scheduleWindows,
  burstEmission,
} from "./sonda-pure.js";
import { playgroundHref } from "./playground-link.js";
// `fmt`/`fmtSecs`/`palette` used to live here as character-identical copies of
// playground.js's. Names differ from the originals in this file only; bodies
// were unchanged by the extraction.
import {
  palette,
  formatNumber,
  formatSeconds,
  drawHistogramHeatmap,
  drawSummaryBands,
  logStream,
} from "./signal-render.js";

/* Which array of the sample result a widget reads, and what draws it.
 *
 * Tables rather than a chain of ifs, for the reason the gallery's
 * `galleryCardState` exists: adding a signal type should be a row, and a
 * missing row should be a loud undefined at mount rather than a silent
 * fall-through to the metrics path that then reads `.values` off a histogram.
 */
const SIGNAL_SOURCE = {
  metrics: (result) => result.entries || [],
  histogram: (result) => result.histograms || [],
  summary: (result) => result.summaries || [],
  logs: (result) => result.logs || [],
};

/* How many log lines a widget renders before saying how many it withheld.
 *
 * The playground renders the stream ENTIRE and should: it has a preset
 * picker, a cursor that correlates a chart instant to a line, and a reader
 * who came to look at logs. A reference-page demo has none of that, and at
 * `rate: 4` over 60s the same renderer produced 240 lines — a 5,379px scroll
 * inside a 318px pane, seventeen screens of nested scrolling in the middle of
 * a page someone is reading top to bottom (review #550 M2).
 *
 * 40 keeps the pane about three of its own heights, which is enough to see
 * the severity mix the widget's sliders control and short enough not to trap
 * a scroll. The cap lives here rather than in `logStream` so the playground
 * keeps its unbounded default rather than inheriting a limit aimed at a
 * different page.
 */
const LOG_LINES_SHOWN = 40;

const SIGNAL_DRAW = {
  metrics: (canvas, sample) => drawMini(canvas, sample),
  histogram: drawHistogramHeatmap,
  summary: drawSummaryBands,
};
// MAX_TICKS comes from the preset module rather than being declared here: it
// constrains what a preset may ask for, so the pure invariants need it too,
// and two definitions that agree by inspection are one edit away from not.
import { WIDGETS, defaultParams, MAX_TICKS } from "./livegen-presets.js";

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

  // The gallery is `display: none` in CSS until this line runs. Its cards are
  // empty frames the engine fills, so a reader without JavaScript should see
  // the markdown table and nothing else — the table is the content, the
  // gallery is the enhancement. Claiming it here, before any mounting, means
  // the cards appear together rather than popping in one intersection at a
  // time.
  document.querySelectorAll(".sonda-gallery:not([data-live])").forEach((gallery) => {
    gallery.dataset.live = "1";
  });

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        observer.unobserve(entry.target);
        if (entry.target.dataset.yamlB64) mountScenario(entry.target);
        else mount(entry.target);
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
  // Three output shapes, chosen by the preset rather than sniffed from the
  // result. `preview` shows the engine's encoded bytes (the encoders widget:
  // the same sine encodes three ways and the LINE is identical in all of
  // them, so a chart there would answer a question nobody asked). A logs
  // widget renders elements, because a log line is text and drawing text into
  // a canvas would make it unselectable and invisible to a screen reader.
  // Everything else is a canvas.
  const signal = preset.signal || "metrics";
  const kind = preset.preview ? "preview" : signal === "logs" ? "logs" : "chart";
  const output = document.createElement(
    kind === "preview" ? "pre" : kind === "logs" ? "div" : "canvas"
  );
  output.className = `sonda-livegen__${kind}`;
  output.setAttribute(
    "aria-label",
    kind === "preview"
      ? `Encoded output of the ${root.dataset.gen} example`
      : kind === "logs"
        ? `Live log stream from the ${root.dataset.gen} generator`
        : `Live chart of the ${root.dataset.gen} generator`
  );
  const canvas = kind === "chart" ? output : null;
  const controls = document.createElement("div");
  controls.className = "sonda-livegen__controls";
  const error = document.createElement("p");
  error.className = "sonda-livegen__error";
  error.hidden = true;
  root.append(output, controls, error);

  let currentYaml = "";
  const link = document.createElement("a");
  link.className = "sonda-livegen__open";
  link.textContent = "Open in playground →";

  // `sliders` is optional. A widget whose entire input is a <select> is a
  // legitimate shape — `sequence`'s parameter is a LIST, with no range to
  // drag — and dereferencing it unguarded threw before the choices below
  // were ever rendered, so the widget mounted with no controls and no chart.
  // The pure module's `cornerParams` carried the same assumption; both were
  // written when every widget happened to have sliders.
  for (const slider of preset.sliders || []) {
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
  for (const choice of preset.choices || []) {
    const row = document.createElement("label");
    row.className = "sonda-livegen__row";
    const name = document.createElement("span");
    name.className = "sonda-livegen__key";
    name.textContent = choice.label || choice.key;
    const select = document.createElement("select");
    select.className = "sonda-livegen__select";
    // Names which control this is, so a test can drive a specific one. The
    // browser suite resolved its target as "the first <select> in the widget"
    // (review #549 M1); that is correct only until a widget's choices are
    // reordered, at which point the check silently exercises a different
    // control and stays green.
    select.dataset.key = choice.key;
    for (const option of choice.options) {
      const el = document.createElement("option");
      el.value = option;
      el.textContent = option;
      if (option === choice.value) el.selected = true;
      select.appendChild(el);
    }
    select.addEventListener("change", () => {
      params[choice.key] = select.value;
      schedule();
    });
    row.append(name, select);
    controls.appendChild(row);
  }
  controls.appendChild(link);

  let firstRender = true;
  const render = async () => {
    try {
      await ensureWasm();
      currentYaml = preset.yaml(params);
      // Resolved from the nav where possible: these widgets now appear on
      // pages at more than one depth, and the relative path below is only
      // correct for the one-directory-deep ones.
      link.href = `${playgroundHref() || "../../playground/"}#yaml=${toBase64Url(currentYaml)}`;
      const result = JSON.parse(sample_scenario(currentYaml, MAX_TICKS));
      if (!result.ok) {
        error.hidden = false;
        error.textContent = result.error || "compile error";
        return;
      }
      error.hidden = true;
      // `ok` from the engine does not mean this widget's signal is present:
      // a scenario the compiler accepts can still be SKIPPED at sampling (a
      // csv_replay reaching for a file, an encoder built out of this wasm).
      // Saying so beats an empty frame the reader has to interpret.
      const sample = SIGNAL_SOURCE[signal](result)[0];
      if (!sample) {
        error.hidden = false;
        error.textContent =
          result.skipped?.[0]?.reason || `the engine produced no ${signal} to draw`;
        return;
      }
      if (kind === "preview") {
        // The engine's own encoded bytes, verbatim and as text — never as
        // markup. Trailing newline trimmed so the block does not carry an
        // empty last line.
        output.textContent = (sample.encoded_preview || "").replace(/\n+$/, "");
      } else if (kind === "logs") {
        // Not registered for redraw: the stream is elements, so a theme flip
        // is restyled by CSS and a resize reflows it. Only canvases need to
        // be repainted, and adding this root to `live` would rebuild the
        // whole pane — losing the reader's scroll position — for nothing.
        // The count goes ABOVE the pane, not only in its footer. The footer
        // sat 911px down a 318px window, so the view at rest was 40 lines
        // ending at +9.75s of a 60s scenario with nothing saying 200 were
        // withheld — the exact state the cap's own rationale says must not
        // exist (review #550 round 2 M1). The footer stays as the link; this
        // is the part that has to be true without scrolling.
        const total = sample.lines.length;
        const tally = document.createElement("p");
        tally.className = "sonda-livegen__logtally";
        tally.textContent =
          total > LOG_LINES_SHOWN
            ? `Showing the first ${LOG_LINES_SHOWN} of ${total} events`
            : `${total} events`;
        output.replaceChildren(
          tally,
          logStream(sample, { prefix: "sonda-livegen", limit: LOG_LINES_SHOWN })
        );
      } else {
        const draw = SIGNAL_DRAW[signal];
        root._draw = () => draw(canvas, sample);
        root._draw();
        live.add(root);
      }
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

/* Mount a gallery widget: one example file, sampled and shown as whatever it
 * actually is.
 *
 * The card always ends up with a working "Open in playground →" link, whether
 * or not there is anything to draw — a scenario the sparkline cannot render
 * is exactly the one a reader most wants to open. The link is built from the
 * SAME base64 string the placeholder carries rather than re-encoding the
 * decoded text, so the reader lands on the file's bytes, not on a round trip
 * through our own encoder.
 *
 * What to show is decided by `galleryCardState` (sonda-pure.js), not here:
 * "ok" from the engine does not mean "there is a chart", and the difference
 * is a rule with a case table rather than a chain of ifs in a DOM function.
 */
async function mountScenario(root) {
  const encoded = root.dataset.yamlB64;
  const canvas = document.createElement("canvas");
  canvas.className = "sonda-livegen__chart";
  canvas.hidden = true;
  const note = document.createElement("p");
  note.className = "sonda-livegen__note";
  note.hidden = true;
  const foot = document.createElement("div");
  foot.className = "sonda-livegen__controls";
  root.append(canvas, note, foot);

  const href = playgroundHref();
  if (href) {
    const link = document.createElement("a");
    link.className = "sonda-livegen__open";
    link.href = `${href}#yaml=${encoded}`;
    link.textContent = "Open in playground →";
    const label = root.dataset.title;
    link.setAttribute(
      "aria-label",
      label ? `Open ${label} in the Sonda playground` : "Open this scenario in the Sonda playground"
    );
    foot.appendChild(link);
  }

  const say = (text, kind) => {
    note.hidden = false;
    note.textContent = text;
    note.dataset.kind = kind;
  };

  let yaml;
  try {
    yaml = fromBase64Url(encoded);
  } catch {
    // A corrupt payload is a build-time bug, not a reader's problem: the card
    // keeps its heading and its link and says so once, rather than throwing
    // and taking the rest of the gallery's mounts down with it.
    say("This example could not be decoded.", "error");
    return;
  }

  let state;
  let result = null;
  try {
    await ensureWasm();
    result = JSON.parse(sample_scenario(yaml, MAX_TICKS));
    state = galleryCardState(result);
  } catch (err) {
    state = { mode: "error", message: String(err) };
  }

  if (state.mode !== "chart") {
    say(state.message, state.mode);
    return;
  }

  const entry = result.entries[0];
  canvas.hidden = false;
  canvas.setAttribute(
    "aria-label",
    `Live chart of ${entry.name || root.dataset.title || "this scenario"}`
  );
  root._draw = () => drawMini(canvas, entry);
  root._draw();
  live.add(root);
  if (state.extraSeries > 0) {
    const n = state.extraSeries;
    say(`Showing ${entry.name} — ${n} more ${n === 1 ? "series" : "series"} here.`, "more");
  }
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
    ctx.fillText(formatNumber(value), pad.left - 5, gy + 3);
  }
  ctx.setLineDash([]);

  const spanSecs = (entry.offset_secs || 0) + values.length * entry.tick_secs;

  // Gap and burst shading, from the same helper the playground's full chart
  // uses. A widget whose sliders change `every`/`for` is showing the reader
  // WHEN the signal stops, so the shading is the point of it, not decoration.
  //
  // Mapped through the SAMPLE domain rather than the axis labels: the trace
  // below runs from the first sample to the last, so the shading has to use
  // the same two ends or a window drifts away from the dip it explains.
  const offsetSecs = entry.offset_secs || 0;
  const seriesEnd = offsetSecs + (values.length - 1) * entry.tick_secs;
  const secsToX = (secs) =>
    pad.left + ((secs - offsetSecs) / (seriesEnd - offsetSecs || 1)) * plotW;
  const windows = scheduleWindows(entry, seriesEnd);
  let firstBurst = null;
  for (const window of windows) {
    ctx.fillStyle = window.kind === "burst" ? colors.burst : colors.gap;
    ctx.fillRect(
      secsToX(window.start),
      pad.top,
      secsToX(window.end) - secsToX(window.start),
      plotH
    );
    if (window.kind === "burst" && !firstBurst) firstBurst = window;
  }

  // The burst multiplier's channel. `every` and `for` are visible in the
  // shading above; the multiplier is not, and cannot be — the trace is the
  // metric's value and a burst does not change the value, it changes how
  // often the value is emitted. So the band says what it does: the emission
  // rate outside it and inside it, computed by `burstEmission` from what the
  // compiler returned. Drawn on the FIRST band only; repeating it on all four
  // would be four copies of one fact.
  const emission = firstBurst && burstEmission(entry);
  if (emission) {
    ctx.font = "10px ui-monospace, monospace";
    ctx.textAlign = "left";
    const width = ctx.measureText(emission.label).width;
    // Anchored to the band, then clamped into the plot so a burst near the
    // right edge labels itself rather than running off the canvas.
    const left = Math.max(
      pad.left + 2,
      Math.min(secsToX(firstBurst.start) + 3, cssWidth - pad.right - width)
    );
    // Plate first: the label sits inside the plot, and a trace crossing it
    // would otherwise make the one number the multiplier moves unreadable.
    ctx.fillStyle = colors.plate;
    ctx.fillRect(left - 2, pad.top + 3, width + 4, 12);
    ctx.fillStyle = colors.text;
    ctx.fillText(emission.label, left, pad.top + 12);
  }

  // Stamped for the browser smoke suite, which cannot read a canvas: these
  // are the two things the shading is claiming, in a form a test can assert
  // exactly instead of diffing pixels.
  canvas.dataset.windows = String(windows.length);
  canvas.dataset.burstRate = emission ? emission.label : "";

  ctx.textAlign = "center";
  for (let step = 0; step <= 3; step++) {
    const secs = (spanSecs * step) / 3;
    ctx.fillText(formatSeconds(secs), pad.left + (plotW * step) / 3, cssHeight - 6);
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
