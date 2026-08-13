/* Sonda docs — alert lab.
 *
 * Drives the #sonda-alert-lab page: a preset scenario is sampled by the real
 * sonda-core engine (same wasm bundle as the playground), a Prometheus-style
 * evaluator walks the series against a threshold + `for:` duration, and a
 * playback sweep draws the signal with an alert-state lane underneath
 * (inactive / pending / firing, with resolve markers).
 */
import init, { sample_scenario } from "./sonda_wasm.js";
import {
  buildTestExport,
  defaultThreshold,
  evaluate,
  fromBase64Url,
  hashPayloadTooLarge,
  parsePromQLRule,
  tidyNumber,
  toBase64Url,
} from "./sonda-pure.js";

const MAX_TICKS = 240;
const SWEEP_SECONDS = 11;
const LANE_STACK_GAP = 18; // vertical space between two rules' state lanes // wall-clock length of one full playback sweep

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
    threshold: 70,
    forSecs: 12,
    story:
      "The leak crosses the threshold about two-thirds in — pending for 12 seconds, then firing until the end of the window. Leaks are the easy case for for:.",
    yaml: scenario(
      "process_memory_percent",
      `    generator:
      type: leak
      baseline: 12.0
      ceiling: 96.0
      time_to_ceiling: 120s
`,
      // Rate 2 so the 240-tick sample window covers the full 120s ramp.
      { rate: 2, duration: "120s" }
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

async function boot() {
  const root = document.getElementById("sonda-alert-lab");
  if (!root || root.dataset.ready) return;
  root.dataset.ready = "1";

  const el = {
    preset: document.getElementById("al-preset"),
    op: document.getElementById("al-op"),
    threshold: document.getElementById("al-threshold"),
    forSel: document.getElementById("al-for"),
    severity: document.getElementById("al-severity"),
    second: document.getElementById("al-second"),
    op2: document.getElementById("al-op2"),
    threshold2: document.getElementById("al-threshold2"),
    forSel2: document.getElementById("al-for2"),
    severity2: document.getElementById("al-severity2"),
    state2: document.getElementById("al-state2"),
    importBox: document.getElementById("al-import"),
    importBtn: document.getElementById("al-import-btn"),
    importNote: document.getElementById("al-import-note"),
    play: document.getElementById("al-play"),
    state: document.getElementById("al-state"),
    chart: document.getElementById("al-chart"),
    error: document.getElementById("al-error"),
    story: document.getElementById("al-story"),
    open: document.getElementById("al-open"),
    exportBtn: document.getElementById("al-export"),
    exportOut: document.getElementById("al-export-out"),
  };

  // A scenario carried over from the playground (#yaml=…) becomes a
  // synthetic first preset, selected on load — the other half of the
  // playground's "Test an alert →" bridge.
  const shared = fromLocationHash();
  const customYaml = shared.yaml;
  if (customYaml !== null) {
    const option = document.createElement("option");
    option.value = "custom";
    option.textContent = "Your scenario (from playground)";
    el.preset.appendChild(option);
  }
  PRESETS.forEach((preset, index) => {
    const option = document.createElement("option");
    option.value = String(index);
    option.textContent = preset.name;
    el.preset.appendChild(option);
  });

  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  let entry = null; // sampled series from the engine
  let evaled = null; // the FIRST rule's evaluation, for the sweep's pacing
  let rules = []; // every enabled rule, each carrying its own evaluation
  let animation = 0;
  let currentYaml = null; // the scenario source behind `entry`

  /* The rules the lab is currently showing, in row order.
   *
   * A list since WP12. The second row is opt-in and starts disabled, so a
   * reader who never touches it sees exactly the single-rule lab that was
   * here before — the pair is available, not imposed. */
  const currentRules = () => {
    const rules = [
      {
        severity: el.severity.value,
        op: el.op.value,
        threshold: Number(el.threshold.value),
        forSecs: Number(el.forSel.value),
      },
    ];
    if (el.second && el.second.checked) {
      rules.push({
        severity: el.severity2.value,
        op: el.op2.value,
        threshold: Number(el.threshold2.value),
        forSecs: Number(el.forSel2.value),
      });
    }
    return rules.filter((rule) => Number.isFinite(rule.threshold));
  };

  const reevaluate = () => {
    if (!entry) return;
    rules = currentRules().map((rule) => ({
      ...rule,
      evaled: evaluate(entry.values, entry.tick_secs, rule.op, rule.threshold, rule.forSecs),
    }));
    evaled = rules.length ? rules[0].evaled : null;
    stopSweep();
    draw(el, entry, rules, entry.values.length);
    syncChips(entry.values.length);
    el.play.textContent = "Play";
  };

  /* One chip per rule. The second is hidden rather than emptied when the row
   * is off, so an unused control does not sit there reading "inactive" as
   * though it were reporting on something. */
  const syncChips = (upTo) => {
    const at = (rule) =>
      upTo >= rule.evaled.states.length
        ? finalStateSummary(rule.evaled)
        : rule.evaled.states[Math.max(0, upTo - 1)];
    if (rules[0]) setStateChip(el.state, at(rules[0]));
    if (!el.state2) return;
    el.state2.hidden = rules.length < 2;
    if (rules[1]) setStateChip(el.state2, at(rules[1]));
  };

  const loadPreset = async () => {
    const isCustom = el.preset.value === "custom" && customYaml !== null;
    const preset = isCustom ? null : PRESETS[Number(el.preset.value)];
    const yaml = isCustom ? customYaml : preset.yaml;
    currentYaml = yaml;
    if (el.exportOut) el.exportOut.hidden = true;
    // Everything that doesn't need the sampled series is assigned BEFORE
    // sampling, so a scenario that fails to sample never shows rule fields
    // belonging to a previously selected preset next to the error banner.
    // Only the custom threshold has to wait — it is derived from the data.
    if (isCustom) {
      el.op.value = ">";
      el.threshold.value = "";
      el.forSel.value = "6";
      el.story.textContent = "Your scenario from the playground.";
    } else {
      el.op.value = preset.op;
      el.threshold.value = String(preset.threshold);
      el.forSel.value = String(preset.forSecs);
      el.story.textContent = preset.story;
    }
    // Relative to /playground/alert-lab/, the playground index is one level
    // up — "./" would point the link back at this very page.
    el.open.href = "../#yaml=" + toBase64Url(yaml);
    await ensureWasm();
    const result = JSON.parse(sample_scenario(yaml, MAX_TICKS));
    entry = result.ok && result.entries.length ? result.entries[0] : null;
    if (!entry) {
      // Surface the engine's own message instead of a silent blank chart —
      // either a compile error or the reason the entry was skipped.
      const reason = !result.ok
        ? result.error
        : result.skipped.length
          ? result.skipped[0].reason
          : "the scenario produced no metrics entries";
      showError(el, reason || "the scenario could not be sampled");
      return;
    }
    if (isCustom) {
      // No preset rule to apply — start from a threshold the signal
      // actually crosses and let the user tune from there.
      el.threshold.value = String(defaultThreshold(entry.values));
      const extra =
        result.entries.length > 1 ? ` (first of ${result.entries.length} series)` : "";
      el.story.textContent =
        `Your scenario from the playground — tune the threshold and for: to see when ` +
        `an alert on ${entry.name}${extra} would fire. The same expectation runs for ` +
        `real with sonda test.`;
    }
    hideError(el);
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
    if (!entry || !rules.length) return;
    stopSweep();
    const total = entry.values.length;
    const start = performance.now();
    el.play.textContent = "Replay";
    const frame = (now) => {
      const progress = Math.min(1, (now - start) / (SWEEP_SECONDS * 1000));
      const upTo = Math.max(2, Math.round(progress * total));
      draw(el, entry, rules, upTo);
      syncChips(upTo);
      if (progress < 1) {
        animation = window.requestAnimationFrame(frame);
      } else {
        animation = 0;
        syncChips(total);
      }
    };
    animation = window.requestAnimationFrame(frame);
  }

  el.preset.addEventListener("change", loadPreset);
  for (const control of [el.op, el.threshold, el.forSel, el.severity, el.op2, el.threshold2, el.forSel2, el.severity2]) {
    if (!control) continue;
    control.addEventListener(control.tagName === "INPUT" ? "input" : "change", reevaluate);
  }
  if (el.second) {
    el.second.addEventListener("change", () => {
      const on = el.second.checked;
      for (const control of [el.op2, el.threshold2, el.forSel2, el.severity2]) {
        if (control) control.disabled = !on;
      }
      // A second rule with no threshold is not a rule. Seed it below the
      // first so the pair reads as warning-then-critical on first sight
      // rather than as two lines on top of each other.
      if (on && el.threshold2 && !el.threshold2.value && entry) {
        const first = Number(el.threshold.value);
        const span = Math.max(...entry.values) - Math.min(...entry.values);
        el.threshold2.value = String(
          tidyNumber(el.op.value === ">" ? first - span * 0.25 : first + span * 0.25)
        );
      }
      reevaluate();
    });
  }
  el.play.addEventListener("click", () => {
    if (reducedMotion) reevaluate();
    else startSweep();
  });
  /* Import a Prometheus rule into the controls.
   *
   * The parse is `parsePromQLRule` in sonda-pure.js, which accepts only
   * `metric{labels} OP number` and refuses everything richer BY NAME. What
   * happens here is the other half: the rule's op/threshold/for become the
   * lab's, and the reader is told when the rule they pasted was written about
   * a different series than the one on screen.
   *
   * That last part matters more than it looks. The lab always evaluates
   * against the loaded scenario, so importing a rule for `http_errors_total`
   * while `cpu_usage` is on the chart produces a perfectly working demo of
   * the wrong thing — and nothing in the numbers would say so.
   */
  const importRule = () => {
    if (!el.importBox || !el.importNote) return;
    const result = parsePromQLRule(el.importBox.value);
    const note = el.importNote;
    if (!result.ok) {
      note.dataset.kind = "error";
      note.textContent = result.reason;
      return;
    }
    // Into the SECOND row when it is already in use, so an import does not
    // silently discard a pair the reader has been tuning.
    const second = el.second && el.second.checked;
    const target = second
      ? { op: el.op2, threshold: el.threshold2, forSel: el.forSel2 }
      : { op: el.op, threshold: el.threshold, forSel: el.forSel };
    target.op.value = result.op === ">=" ? ">" : result.op === "<=" ? "<" : result.op;
    target.threshold.value = String(result.threshold);
    // `for:` is a fixed menu; land on the nearest option rather than adding
    // one, and say so if the rule's duration is not on it.
    const options = Array.from(target.forSel.options).map((option) => Number(option.value));
    const nearest = options.reduce((best, value) =>
      Math.abs(value - result.forSecs) < Math.abs(best - result.forSecs) ? value : best
    );
    target.forSel.value = String(nearest);

    const notes = [];
    if (result.op !== target.op.value) {
      notes.push(`\`${result.op}\` shown as \`${target.op.value}\` — the lab evaluates strict comparisons`);
    }
    if (nearest !== result.forSecs) {
      notes.push(`for: ${result.forSecs}s rounded to ${nearest}s`);
    }
    if (entry && result.metric !== entry.name) {
      notes.push(
        `this rule is about \`${result.metric}\`, but the chart is showing \`${entry.name}\` — ` +
          `the lab evaluates against the loaded scenario`
      );
    }
    note.dataset.kind = notes.length ? "warn" : "ok";
    note.textContent = notes.length
      ? `Imported${result.name ? ` ${result.name}` : ""} — ${notes.join("; ")}`
      : `Imported${result.name ? ` ${result.name}` : ""}.`;
    reevaluate();
  };
  if (el.importBtn) el.importBtn.addEventListener("click", importRule);

  if (el.exportBtn) {
    el.exportBtn.addEventListener("click", () => {
      if (!entry || !rules.length || currentYaml === null) return;
      const text = buildTestExport({
        yaml: currentYaml,
        entry,
        rules,
      });
      el.exportOut.textContent = text;
      el.exportOut.hidden = false;
      const done = (label) => {
        el.exportBtn.textContent = label;
        window.setTimeout(() => {
          el.exportBtn.textContent = "Copy sonda test setup";
        }, 1600);
      };
      navigator.clipboard.writeText(text).then(
        () => done("Copied!"),
        () => done("Copy below ↓")
      );
    });
  }

  // Redraw from the CACHED rules on resize and theme flip. Re-reading the
  // controls here would be a second source of truth for what is on screen,
  // and these fire while a sweep is paused mid-way — `rules` is what was
  // drawn, which is what has to be redrawn.
  const redraw = () => {
    if (entry && rules.length && !animation) draw(el, entry, rules, entry.values.length);
  };
  new ResizeObserver(redraw).observe(el.chart.parentElement);
  new MutationObserver(redraw).observe(document.body, {
    attributes: true,
    attributeFilter: ["data-md-color-scheme"],
  });

  // Awaited so a refused hash explains itself after the first preset has
  // drawn, rather than being overwritten by it. The notice goes on the story
  // line rather than through showError: the lab itself is fine — a preset is
  // loaded and evaluating — and blanking its chart to report an ignored link
  // would break more than it explains.
  await loadPreset();
  if (shared.status) {
    el.story.textContent = `${shared.status} ${el.story.textContent}`.trim();
  }
}

function showError(el, message) {
  el.error.hidden = false;
  el.error.textContent = message;
  setStateChip(el.state, "inactive");
  const ctx = el.chart.getContext("2d");
  ctx.clearRect(0, 0, el.chart.width, el.chart.height);
}

function hideError(el) {
  el.error.hidden = true;
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

/* Read a scenario carried over from the playground via `#yaml=`.
 *
 * Returns `{ yaml, status }` — see the twin in playground.js. The size check
 * runs before the decode so an oversized link costs a length comparison
 * rather than a decode plus a compile; a malformed one falls back to the
 * built-in presets in silence, since that is a broken link rather than a
 * hostile one.
 */
function fromLocationHash() {
  const match = window.location.hash.match(/^#yaml=(.+)$/);
  if (!match) return { yaml: null, status: null };
  if (hashPayloadTooLarge(match[1])) {
    return {
      yaml: null,
      status: "The shared scenario is too large to load — showing the built-in presets instead.",
    };
  }
  try {
    return { yaml: fromBase64Url(match[1]), status: null };
  } catch {
    return { yaml: null, status: null };
  }
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

/* Severity decides a rule's colour, so a reader can tell the pair apart on
 * the chart without reading the numbers. Critical keeps the red the lab has
 * always used; warning takes amber, which is also the `pending` colour in the
 * state lane — deliberate, since a warning threshold IS the earlier, softer
 * line. */
const SEVERITY_COLORS = {
  critical: { light: "#dc2626", dark: "#f87171" },
  warning: { light: "#b45309", dark: "#fbbf24" },
};

function draw(el, entry, rules, upTo) {
  const colors = palette();
  const canvas = el.chart;
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const laneH = 26;
  const extraLanes = Math.max(0, rules.length - 1);
  const cssHeight = 380 + extraLanes * (laneH + LANE_STACK_GAP);
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const laneGap = 34;
  // Every lane past the first extends the canvas rather than squeezing the
  // plot: the trace is what the reader is reading, and a second rule must
  // not shrink it.
  const pad = {
    left: 48,
    right: 12,
    top: 12,
    bottom: 26 + laneH + laneGap + extraLanes * (laneH + LANE_STACK_GAP),
  };
  const plotW = cssWidth - pad.left - pad.right;
  const plotH = cssHeight - pad.top - pad.bottom;
  const laneY = pad.top + plotH + laneGap;

  const values = entry.values;
  const spanSecs = (values.length - 1) * entry.tick_secs;
  const thresholds = rules.map((rule) => rule.threshold).filter(Number.isFinite);
  let min = Math.min(...values, ...thresholds);
  let max = Math.max(...values, ...thresholds);
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
  // Shaded for the FIRST rule only. Two overlapping washes would make the
  // area where both fire a third colour that means nothing, and the second
  // rule's firing is already legible in its own lane below.
  ctx.fillStyle = colors.firingBand;
  if (rules[0]) {
    for (const run of stateRuns(rules[0].evaled.states, upTo)) {
      if (run.state === "firing") {
        ctx.fillRect(x(run.start), pad.top, x(run.end) - x(run.start), plotH);
      }
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
    ctx.fillText(formatSeconds(secs), pad.left + (secs / spanSecs) * plotW, pad.top + plotH + 16);
  }

  // One threshold line per rule, coloured by severity and labelled with its
  // own severity so the pair is readable without counting lines.
  for (const rule of rules) {
    const stroke = (SEVERITY_COLORS[rule.severity] || SEVERITY_COLORS.critical)[
      colors.dark ? "dark" : "light"
    ];
    ctx.strokeStyle = stroke;
    ctx.setLineDash([6, 4]);
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(pad.left, y(rule.threshold));
    ctx.lineTo(cssWidth - pad.right, y(rule.threshold));
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.textAlign = "left";
    ctx.fillStyle = stroke;
    const label =
      rules.length > 1
        ? `${rule.severity} ${rule.op} ${rule.threshold}`
        : `${rule.op} ${rule.threshold}`;
    ctx.fillText(label, pad.left + 4, y(rule.threshold) - 5);
  }

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

  // One alert-state lane per rule, drawn as merged runs for seamless
  // segments. Stacked rather than overlaid: the whole point of a
  // warning/critical pair is that the two fire at different times, and a
  // single lane could only ever show one of them.
  const stateColor = (state) => STATE_COLORS[state][colors.dark ? "dark" : "light"];
  ctx.font = "10px ui-monospace, monospace";
  rules.forEach((rule, index) => {
    const top = laneY + index * (laneH + LANE_STACK_GAP);
    ctx.fillStyle = colors.text;
    ctx.textAlign = "left";
    ctx.fillText(
      rules.length > 1 ? `${rule.severity.toUpperCase()} STATE` : "ALERT STATE",
      pad.left,
      top - 7
    );
    for (const run of stateRuns(rule.evaled.states, upTo)) {
      ctx.fillStyle = stateColor(run.state);
      ctx.fillRect(x(run.start), top, Math.max(1, x(run.end) - x(run.start)), laneH);
    }
    ctx.strokeStyle = colors.resolve;
    ctx.lineWidth = 2;
    for (const idx of rule.evaled.resolves) {
      if (idx >= upTo) continue;
      ctx.beginPath();
      ctx.moveTo(x(idx), top - 4);
      ctx.lineTo(x(idx), top + laneH + 4);
      ctx.stroke();
    }
  });

  // What the chart is claiming, for the browser smoke suite — the lesson
  // from review #543, which is that a canvas diff can say something moved
  // but never what.
  canvas.dataset.rules = String(rules.length);
  canvas.dataset.thresholds = rules.map((rule) => `${rule.severity}:${rule.threshold}`).join(",");
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
