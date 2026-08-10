/* Sonda docs — live generator widget presets (pure).
 *
 * Data + template functions only: no DOM, no wasm, no I/O — the same
 * load-bearing property as sonda-pure.js, and for the same reason
 * (review #534 W1): docs/site/tools/tests/pure.test.mjs pins the preset
 * invariants in CI, and docs/site/tools/tests/livegen-compile.mjs runs
 * every slider-corner combination through the real compiler. livegen.js
 * imports this module and owns everything browser-shaped.
 *
 * Each widget declares `rate` and `durationSecs` as data so constraints
 * that reference the scenario duration can be derived instead of
 * hand-synchronized: the leak widget's time_to_ceiling floor IS the
 * duration (the engine rejects a leak that resets mid-run), not a number
 * that happens to match one inside a template string. Slider ranges are
 * chosen so every combination compiles — baseline/ceiling ranges are
 * disjoint where both exist — and the compile gate proves it on every CI
 * run rather than trusting this comment.
 */

function head(rate, durationSecs) {
  return `version: 2
kind: runnable
defaults:
  rate: ${rate}
  duration: ${durationSecs}s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:`;
}

const LEAK_DURATION_SECS = 120;

export const WIDGETS = {
  sine: {
    rate: 4,
    durationSecs: 60,
    sliders: [
      { key: "amplitude", min: 0, max: 60, step: 1, value: 30 },
      { key: "period_secs", min: 5, max: 60, step: 1, value: 20, unit: "s" },
      { key: "offset", min: 0, max: 100, step: 1, value: 55 },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: cpu_usage
    generator: { type: sine, amplitude: ${p.amplitude}, offset: ${p.offset}, period_secs: ${p.period_secs} }
`;
    },
  },
  spike: {
    rate: 4,
    durationSecs: 60,
    sliders: [
      { key: "baseline", min: 0, max: 100, step: 1, value: 10 },
      { key: "magnitude", min: -80, max: 80, step: 1, value: 40 },
      { key: "interval_secs", min: 5, max: 30, step: 1, value: 10, unit: "s" },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: http_errors_total
    generator: { type: spike, baseline: ${p.baseline}, magnitude: ${p.magnitude}, duration_secs: 3, interval_secs: ${p.interval_secs} }
`;
    },
  },
  steady: {
    rate: 4,
    durationSecs: 60,
    sliders: [
      { key: "center", min: 0, max: 100, step: 1, value: 50 },
      { key: "amplitude", min: 0, max: 30, step: 1, value: 8 },
      { key: "noise", min: 0, max: 10, step: 0.5, value: 2.5 },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: cpu_usage
    generator: { type: steady, center: ${p.center}, amplitude: ${p.amplitude}, period: 30s, noise: ${p.noise}, noise_seed: 7 }
`;
    },
  },
  flap: {
    rate: 2,
    durationSecs: 120,
    sliders: [
      { key: "up_duration", min: 5, max: 60, step: 1, value: 20, unit: "s" },
      { key: "down_duration", min: 2, max: 30, step: 1, value: 8, unit: "s" },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: interface_up
    generator: { type: flap, up_duration: ${p.up_duration}s, down_duration: ${p.down_duration}s }
`;
    },
  },
  saturation: {
    rate: 2,
    durationSecs: 120,
    sliders: [
      { key: "baseline", min: 0, max: 15, step: 1, value: 5 },
      { key: "ceiling", min: 40, max: 100, step: 1, value: 95 },
      { key: "time_to_saturate", min: 10, max: 120, step: 5, value: 40, unit: "s" },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: queue_fill_percent
    generator: { type: saturation, baseline: ${p.baseline}, ceiling: ${p.ceiling}, time_to_saturate: ${p.time_to_saturate}s }
`;
    },
  },
  leak: {
    rate: 2,
    durationSecs: LEAK_DURATION_SECS,
    sliders: [
      { key: "baseline", min: 0, max: 40, step: 1, value: 10 },
      { key: "ceiling", min: 50, max: 100, step: 1, value: 95 },
      // The floor is the scenario duration by construction, not by
      // coincidence: the engine requires time_to_ceiling >= duration.
      { key: "time_to_ceiling", min: LEAK_DURATION_SECS, max: 600, step: 10, value: LEAK_DURATION_SECS, unit: "s" },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: process_memory_percent
    generator: { type: leak, baseline: ${p.baseline}, ceiling: ${p.ceiling}, time_to_ceiling: ${p.time_to_ceiling}s }
`;
    },
  },
  degradation: {
    rate: 4,
    durationSecs: 60,
    sliders: [
      { key: "ceiling", min: 10, max: 100, step: 1, value: 60 },
      { key: "time_to_degrade", min: 20, max: 120, step: 5, value: 60, unit: "s" },
      { key: "noise", min: 0, max: 10, step: 0.5, value: 3 },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: request_latency_ms
    generator: { type: degradation, baseline: 5, ceiling: ${p.ceiling}, time_to_degrade: ${p.time_to_degrade}s, noise: ${p.noise}, noise_seed: 42 }
`;
    },
  },
  spike_event: {
    rate: 2,
    durationSecs: 120,
    sliders: [
      { key: "baseline", min: 0, max: 100, step: 1, value: 35 },
      { key: "spike_height", min: 10, max: 100, step: 1, value: 60 },
      { key: "spike_interval", min: 15, max: 60, step: 1, value: 30, unit: "s" },
    ],
    yaml(p) {
      return `${head(this.rate, this.durationSecs)}
  - id: live
    signal_type: metrics
    name: cpu_usage
    generator: { type: spike_event, baseline: ${p.baseline}, spike_height: ${p.spike_height}, spike_duration: 10s, spike_interval: ${p.spike_interval}s }
`;
    },
  },
};

/* Default parameter values for a widget, keyed by slider. */
export function defaultParams(widget) {
  const params = {};
  for (const slider of widget.sliders) params[slider.key] = slider.value;
  return params;
}

/* Every {min, default, max} combination of a widget's sliders (up to 3^n
 * objects — duplicate points per slider are deduped). This is the base set
 * the compile gate feeds through the real engine: the documented
 * constraints are linear in each parameter, so the range edges are the
 * binding cases, and the default row is what every reader actually sees. */
export function cornerParams(widget) {
  let corners = [{}];
  for (const slider of widget.sliders) {
    const points = [...new Set([slider.min, slider.value, slider.max])];
    corners = corners.flatMap((corner) => points.map((v) => ({ ...corner, [slider.key]: v })));
  }
  return corners;
}

/* Full sweep of one slider (every step) crossed with the min/max corners
 * of the remaining sliders — used by the compile gate for the
 * duration-coupled sliders, where "the corners pass" deserves a
 * every-step check along the axis under the worst neighbors. */
export function sweepParams(widget, key) {
  const slider = widget.sliders.find((s) => s.key === key);
  if (!slider) return [];
  let others = [{}];
  for (const other of widget.sliders) {
    if (other.key === key) continue;
    others = others.flatMap((corner) => [
      { ...corner, [other.key]: other.min },
      { ...corner, [other.key]: other.max },
    ]);
  }
  const sweeps = [];
  for (let v = slider.min; v <= slider.max; v += slider.step) {
    for (const corner of others) sweeps.push({ ...corner, [key]: v });
  }
  return sweeps;
}
