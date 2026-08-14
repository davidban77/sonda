// Node test harness for the playground's pure helpers (sonda-pure.js).
//
// Run: node docs/site/tools/tests/pure.test.mjs  (from the repo root; any
// cwd works — the import is relative to this file). Zero dependencies; uses
// node's built-in assert and exits non-zero on the first failure. Wired
// into the docs CI workflow, this is the only automated coverage the
// playground JS has — keep every function under test free of DOM/wasm.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  MAX_HASH_PAYLOAD,
  buildTestExport,
  burstEmission,
  cursorSamples,
  cursorSecsAt,
  defaultThreshold,
  deriveAlertName,
  escapeQuoted,
  evaluate,
  exportFilename,
  fromBase64Url,
  galleryCardState,
  hashPayloadTooLarge,
  logLinesNear,
  MAX_SCHEDULE_CYCLES,
  niceDeadlineSecs,
  normalizeFence,
  parsePromQLRule,
  yamlPathAt,
  schemaCompletions,
  numberSpanAt,
  runnableScenario,
  scheduleWindows,
  scrubNumber,
  toBase64Url,
} from "../../docs/javascripts/sonda-pure.js";
import {
  WIDGETS,
  cornerParams,
  defaultParams,
  sweepParams,
  sampledTicks,
  MAX_TICKS,
} from "../../docs/javascripts/livegen-presets.js";

let passed = 0;
function test(name, fn) {
  try {
    fn();
    passed += 1;
  } catch (err) {
    console.error(`FAIL ${name}`);
    console.error(err && err.message ? err.message : err);
    process.exit(1);
  }
}

// --- base64url ---------------------------------------------------------

test("base64url round-trips ASCII YAML", () => {
  const yaml = "version: 2\nkind: runnable\n";
  assert.equal(fromBase64Url(toBase64Url(yaml)), yaml);
});

test("base64url round-trips UTF-8 (accents + CJK)", () => {
  const yaml = "labels: { service: café, region: 東京 }\n";
  assert.equal(fromBase64Url(toBase64Url(yaml)), yaml);
});

test("base64url output is URL-safe", () => {
  // Enough binary-ish variety to hit +, / and padding in plain base64.
  const text = Array.from({ length: 64 }, (_, i) => String.fromCharCode(i * 3 + 1)).join("");
  const encoded = toBase64Url(text);
  assert.match(encoded, /^[A-Za-z0-9_-]+$/);
  assert.equal(fromBase64Url(encoded), text);
});

// --- defaultThreshold --------------------------------------------------

const range = (a, b, n = 50) => Array.from({ length: n }, (_, i) => a + ((b - a) * i) / (n - 1));
const fires = (values, t) => values.some((v) => v > t);

test("threshold is crossable across magnitudes and signs", () => {
  for (const values of [
    range(0, 100),
    range(-100, -50),
    range(-30, 30),
    range(0.0001, 0.001),
    range(1e9, 1e10),
  ]) {
    const t = defaultThreshold(values);
    assert.ok(Number.isFinite(t));
    assert.ok(fires(values, t), `threshold ${t} never fires`);
    assert.ok(values.some((v) => v <= t), `threshold ${t} fires everywhere — nothing to watch`);
  }
});

test("flat series seat the threshold below the value (review #531 W1)", () => {
  for (const values of [Array(50).fill(42), Array(50).fill(0), Array(50).fill(-50), [7], Array(50).fill(0.005)]) {
    const t = defaultThreshold(values);
    assert.ok(Number.isFinite(t));
    assert.ok(fires(values, t), `flat series with threshold ${t} must fire`);
  }
});

test("degenerate inputs return a finite fallback", () => {
  assert.ok(Number.isFinite(defaultThreshold([])));
  assert.ok(Number.isFinite(defaultThreshold([NaN, NaN])));
  assert.ok(Number.isFinite(defaultThreshold([Infinity])));
});

// --- evaluate ----------------------------------------------------------

test("for: swallows short excursions, sustains long ones", () => {
  // tick = 1s; two ticks above threshold then recovery, later six ticks.
  const values = [0, 5, 5, 0, 0, 5, 5, 5, 5, 5, 5, 0];
  const { states, resolves } = evaluate(values, 1, ">", 3, 3);
  assert.equal(states[1], "pending");
  assert.equal(states[2], "pending"); // held 1s < 3s
  assert.equal(states[3], "inactive");
  assert.equal(states[8], "firing"); // held 3s at index 8 (run started at 5)
  assert.deepEqual(resolves, [11]); // resolved when it dropped while firing
});

test("for: 0 fires immediately and < operator works", () => {
  const { states } = evaluate([5, 1, 5], 1, "<", 3, 0);
  assert.deepEqual(states, ["inactive", "firing", "inactive"]);
});

// --- deriveAlertName / niceDeadlineSecs --------------------------------

test("alert names are PascalCase with a direction suffix", () => {
  assert.equal(deriveAlertName("cpu_usage", ">"), "CpuUsageHigh");
  assert.equal(deriveAlertName("link_up", "<"), "LinkUpLow");
  assert.equal(deriveAlertName("http.request-latency", ">"), "HttpRequestLatencyHigh");
  assert.equal(deriveAlertName("", ">"), "LabAlertHigh");
  assert.equal(deriveAlertName("9lives", ">"), "Lab9livesHigh");
});

test("deadlines round up to human numbers and never undercut the firing time", () => {
  for (const secs of [0, 3, 20, 44, 100, 500, 1000]) {
    const d = niceDeadlineSecs(secs);
    assert.ok(d > secs, `deadline ${d} must exceed firing time ${secs}`);
  }
  assert.equal(niceDeadlineSecs(0), 15);
  assert.equal(niceDeadlineSecs(30), 60);
});

// --- buildTestExport ---------------------------------------------------

const ENTRY = {
  name: "cpu_usage",
  tick_secs: 0.25,
  offset_secs: 0,
  labels: { host: "web-01", service: "api" },
};
const YAML = "version: 2\nkind: runnable\nscenarios: []\n";

test("export carries a label-scoped rule and expect block with derived deadlines", () => {
  // Fires at index 80 → 20s → nice deadline 45s; ends resolved.
  const states = Array(240).fill("inactive");
  for (let i = 80; i < 200; i++) states[i] = "firing";
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [{ severity: "critical", op: ">", threshold: 64, forSecs: 12, evaled: { states, resolves: [200] } }],
  });
  assert.match(out, /alert: CpuUsageHigh/);
  assert.match(out, /expr: 'cpu_usage\{host="web-01",service="api"\} > 64'/);
  assert.match(out, /for: 12s/);
  assert.match(out, /firing_within: 45s/);
  assert.match(out, /resolves_within: 2m/);
  assert.match(out, /host: "web-01"/); // expect.labels scoping, always quoted
  assert.match(out, /severity: critical/);
  assert.match(out, /sonda test lab-scenario\.yaml/);
  assert.ok(out.includes(YAML.trimEnd()), "scenario YAML embedded verbatim");
});

test("export omits resolution when still firing at the end", () => {
  const states = Array(40).fill("inactive").concat(Array(40).fill("firing"));
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [{ severity: "critical", op: ">", threshold: 64, forSecs: 12, evaled: { states, resolves: [] } }],
  });
  assert.doesNotMatch(out, /resolves_within:/);
  assert.match(out, /resolution not asserted/);
});

test("export flags a rule that never fired instead of inventing a deadline", () => {
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [{ severity: "critical", op: ">", threshold: 9999, forSecs: 12, evaled: { states: Array(80).fill("inactive"), resolves: [] } }],
  });
  assert.match(out, /never fired in the sampled preview/);
  // Review #546 M3: dropping `fired &&` from `endsResolved` left all 146
  // tests green, because this case checked the firing comment and never that
  // a resolution deadline is absent. An export asserting that a rule which
  // never fired nonetheless resolves is an expectation that cannot pass.
  assert.doesNotMatch(out, /resolves_within/);
});

test("a warning/critical pair exports two rules with distinct names", () => {
  // Two alerts sharing one name in one group is a rules file that does not
  // mean what it looks like — Prometheus keys alerts by name plus labels, so
  // the pair would collapse in ways that depend on the labels rather than on
  // anything the reader wrote.
  const early = Array(240).fill("inactive");
  for (let i = 40; i < 240; i++) early[i] = "firing";
  const late = Array(240).fill("inactive");
  for (let i = 120; i < 200; i++) late[i] = "firing";
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [
      { severity: "warning", op: ">", threshold: 60, forSecs: 6, evaled: { states: early, resolves: [] } },
      { severity: "critical", op: ">", threshold: 90, forSecs: 12, evaled: { states: late, resolves: [200] } },
    ],
  });
  assert.match(out, /alert: CpuUsageHighWarning/);
  assert.match(out, /alert: CpuUsageHighCritical/);
  assert.match(out, /severity: warning/);
  assert.match(out, /severity: critical/);
  assert.match(out, /> 60/);
  assert.match(out, /> 90/);
  assert.match(out, /for: 6s/);
  assert.match(out, /for: 12s/);
  // Deadlines are PER RULE. tick_secs is 0.25, so the warning crosses at
  // tick 40 (10s -> a 30s deadline) and the critical at tick 120 (30s -> 60s);
  // asserting one against the other's states would produce an expectation
  // that passes for the wrong reason.
  assert.match(out, /firing_within: 30s/);
  assert.match(out, /firing_within: 60s/);
  // And only the critical recovers, so only it asserts resolution.
  assert.equal((out.match(/resolves_within: 2m/g) || []).length, 1);
  assert.equal((out.match(/resolution not asserted/g) || []).length, 1);
});

test("two rules of the SAME severity still get distinct names", () => {
  // Review #546 W2. Both dropdowns offer both severities, so a critical pair
  // at different `for:` durations is two clicks away — and suffixing by
  // severity alone produced two identical names with identical labels in one
  // group, which is precisely the rules file the suffix exists to prevent.
  const evaled = { states: Array(40).fill("firing"), resolves: [] };
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [
      { severity: "critical", op: ">", threshold: 70, forSecs: 6, evaled },
      { severity: "critical", op: ">", threshold: 90, forSecs: 12, evaled },
    ],
  });
  const names = [...out.matchAll(/alert: (\S+)/g)].map((m) => m[1]);
  assert.ok(names.length >= 2, "expected at least two alert lines");
  assert.equal(new Set(names).size, 2, `names collided: ${[...new Set(names)].join(", ")}`);
});

test("the headline names a severity only when there is more than one rule", () => {
  // The docstring claims a single-rule export is byte-identical to what it
  // was before pairs existed. Review #546 W4 measured that false on exactly
  // this line — the one place the `multiple` gate had not been applied — and
  // the test that was supposed to pin it read everything except the headline.
  const evaled = { states: Array(40).fill("firing"), resolves: [] };
  const one = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [{ severity: "critical", op: ">", threshold: 64, forSecs: 12, evaled }],
  });
  assert.doesNotMatch(one.split("\n")[0], /\(critical\)/);
  const two = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [
      { severity: "warning", op: ">", threshold: 60, forSecs: 6, evaled },
      { severity: "critical", op: ">", threshold: 90, forSecs: 12, evaled },
    ],
  });
  assert.match(two, /\(warning\)/);
  assert.match(two, /\(critical\)/);
});

test("a single rule exports exactly what it always did", () => {
  // The pair must not make the common case noisier: no severity suffix on
  // the alert name, and one rule in the group.
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [
      { severity: "critical", op: ">", threshold: 64, forSecs: 12, evaled: { states: Array(40).fill("firing"), resolves: [] } },
    ],
  });
  assert.match(out, /alert: CpuUsageHigh$/m);
  assert.doesNotMatch(out, /CpuUsageHighCritical/);
  assert.equal((out.match(/^#\s+- alert:/gm) || []).length, 1, "one rule in the group");
  assert.equal((out.match(/^    - alert:/gm) || []).length, 1, "one expectation");
  assert.match(out, /# 1\) Alert rule \(vmalert/);
});

test("export comments the expect block when the scenario already has one", () => {
  const out = buildTestExport({
    yaml: "version: 2\nexpect:\n  alerts: []\n",
    entry: ENTRY,
    rules: [{ severity: "critical", op: ">", threshold: 64, forSecs: 12, evaled: { states: Array(40).fill("firing"), resolves: [] } }],
  });
  assert.match(out, /merge the one below by hand/);
  assert.match(out, /^# expect:/m); // our block arrives commented
  assert.match(out, /^#\s+alerts:/m);
});

// --- param scrubbing (numberSpanAt / scrubNumber) -----------------------

test("numberSpanAt finds standalone YAML numbers, with duration suffixes", () => {
  const cases = [
    // [line, column under the pointer, expected matched text]
    ["    generator: { type: sine, amplitude: 30.0, offset: 55.0, period_secs: 20 }", 42, "30.0"],
    ["    generator: { type: sine, amplitude: 30.0, offset: 55.0, period_secs: 20 }", 74, "20"],
    ["  duration: 60s", 13, "60"],
    ["      up_duration: 20s", 20, "20"],
    ["    gaps: { every: 60s, for: 8s }", 29, "8"],
    ["      time_to_ceiling: 1.5h", 24, "1.5"],
    ["      latency_budget: 250ms", 23, "250"],
    ["      offset: -12.5", 16, "-12.5"],
    ["    seed: 42", 11, "42"],
    ["  rate: 4", 8, "4"],
  ];
  for (const [line, column, expected] of cases) {
    const span = numberSpanAt(line, column);
    assert.ok(span, `no span in ${JSON.stringify(line)} at col ${column}`);
    assert.equal(span.text, expected, `wrong span in ${JSON.stringify(line)}`);
    assert.equal(line.slice(span.start, span.end), expected);
  }
});

test("numberSpanAt rejects digits that are not standalone scalar values", () => {
  const cases = [
    // Digits embedded in words and identifiers.
    ["    labels: { host: web-01, region: us-east }", 25],
    ["    labels: { service: checkout, pod: checkout-7d4f9 }", 50],
    // Dotted quads and quoted strings.
    ['    labels: { device: pe-router-1, neighbor: "10.0.0.2" }', 47],
    ['    neighbor: "10.0.0.2"', 16],
    // Comments.
    ["  rate: 4  # was 8 before the incident", 18],
    // The version key is config schema, not signal shape.
    ["version: 2", 9],
    // Scientific notation stays hands-off.
    ["    ceiling: 1e9", 14],
    // Pointer nowhere near a number.
    ["    signal_type: metrics", 8],
  ];
  for (const [line, column] of cases) {
    assert.equal(numberSpanAt(line, column), null, `unexpected span in ${JSON.stringify(line)} at col ${column}`);
  }
});

test("numbers inside quoted strings are prose, not parameters (review #533 W1)", () => {
  // The reviewer's cases: boundary checks alone accept these because the
  // digits have spaces around them INSIDE the string.
  const rejected = [
    ['  - message: "Request took 250 ms"', 28],
    ['  - message: "Disk 85 percent full"', 20],
    ['  labels: { rack: "row 4 rack 12" }', 23],
    ["  - message: 'took 5 ms'", 19],
    ['  msg: "say \\"hi\\" 5 times"', 19], // escaped quotes keep parity
  ];
  for (const [line, column] of rejected) {
    const span = numberSpanAt(line, column);
    assert.equal(span, null, `quoted prose offered as scrubbable: ${JSON.stringify(line)} -> ${span && span.text}`);
  }
  // …but a CLOSED quoted string before the number must not block it.
  const span = numberSpanAt('    labels: { note: "warm", port: 8080 }', 35);
  assert.ok(span, "scalar after a closed quoted string must stay scrubbable");
  assert.equal(span.text, "8080");
});

test("scrubNumber preserves decimal format and scales the step to magnitude", () => {
  assert.equal(scrubNumber("55.0", 5), "60.0"); // step 1
  assert.equal(scrubNumber("30.0", -3), "27.0");
  assert.equal(scrubNumber("120", 1), "130"); // step 10
  assert.equal(scrubNumber("4", -6), "-2"); // step 1, sign crossing allowed
  assert.equal(scrubNumber("0.004", -2), "0.002"); // step 0.001, no float dust
  assert.equal(scrubNumber("0.004", 2), "0.006");
  assert.equal(scrubNumber("-12.5", 4), "-8.5");
  assert.equal(scrubNumber("42", 0), "42");
  assert.equal(scrubNumber("0", 3), "3"); // zero falls back to the fine step
  assert.equal(scrubNumber("0.0", -1), "-0.1");
});

test("scrubNumber never emits a non-literal — huge values pin (review #533 M2)", () => {
  // toFixed goes exponential at >= 1e21; the scrub must return the
  // original text rather than splice `1e+21` into the YAML.
  const huge = "999999999999999999999";
  assert.equal(scrubNumber(huge, 1), huge);
  assert.match(scrubNumber("120", 1), /^\d+$/);
});

test("scrubNumber round-trips through numberSpanAt on a real preset line", () => {
  // The drag gesture replaces the span with the scrubbed text and keeps
  // scrubbing from the ORIGINAL literal — the replacement must stay a
  // valid scrub target so a second gesture works too.
  const line = "    generator: { type: sine, amplitude: 30.0, offset: 55.0, period_secs: 20 }";
  const span = numberSpanAt(line, 42);
  const next = scrubNumber(span.text, 7);
  const patched = line.slice(0, span.start) + next + line.slice(span.end);
  const again = numberSpanAt(patched, span.start + 1);
  assert.equal(again.text, "37.0");
});

// --- label-value escaping (review #532 blocker) ------------------------

test("escapeQuoted handles quotes, backslashes, and control characters", () => {
  assert.equal(escapeQuoted('net"ops'), 'net\\"ops');
  assert.equal(escapeQuoted("a\\b"), "a\\\\b");
  assert.equal(escapeQuoted("a\nb"), "a\\nb");
  assert.equal(escapeQuoted("a\tb"), "a\\tb");
  assert.equal(escapeQuoted("plain"), "plain");
  assert.equal(escapeQuoted(""), "");
});

test("exports stay well-formed for every hostile label value (review #532 table)", () => {
  // The reviewer's case table: each of these, emitted bare, produced a
  // file the export's own instructions reject — or silently-broken PromQL.
  const nasty = {
    quote: 'net"ops',
    backslash: "a\\b",
    colon_space: "a: b",
    numeric: "12345",
    boolish: "true",
    brace: "{x}",
    asterisk: "*ref",
    newline: "a\nb",
    empty: "",
  };
  const states = Array(40).fill("inactive").concat(Array(40).fill("firing"));
  const out = buildTestExport({
    yaml: YAML,
    entry: { ...ENTRY, labels: nasty },
    rules: [{ severity: "critical", op: ">", threshold: 64, forSecs: 12, evaled: { states, resolves: [] } }],
  });
  // Every YAML label line is a double-quoted scalar…
  for (const key of Object.keys(nasty)) {
    assert.match(out, new RegExp(`^\\s+${key}: ".*"$`, "m"), `label ${key} must be quoted`);
  }
  // …with quotes/backslashes escaped, so the quoting cannot be broken out of.
  assert.match(out, /quote: "net\\"ops"/);
  assert.match(out, /backslash: "a\\\\b"/);
  assert.match(out, /newline: "a\\nb"/);
  assert.match(out, /empty: ""/);
  // The PromQL matcher gets the same treatment.
  assert.match(out, /quote="net\\"ops"/);
  assert.match(out, /backslash="a\\\\b"/);
  assert.match(out, /empty=""/);
  // No label value ever lands as a raw unquoted YAML scalar.
  assert.doesNotMatch(out, /^\s+numeric: 12345$/m);
  assert.doesNotMatch(out, /^\s+boolish: true$/m);
});

// --- live-widget presets (review #534 W1/M1) ----------------------------

test("widget controls are well-formed and defaults sit inside their range", () => {
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    // Counted as CONTROLS, not sliders: the encoders widget carries one
    // slider and one <select>, and the invariant being defended is "enough
    // to play with, few enough to take in", not the input element used.
    //
    // The floor is 1, not 2. It was 2 until WP13 added `constant`, which has
    // exactly one parameter — there is no second knob to offer and inventing
    // one would be worse than a short widget. The floor exists to catch a
    // widget shipped with NOTHING to drag, and 1 still catches that; the
    // ceiling is where the real judgement lives.
    const controls = (widget.sliders || []).length + (widget.choices || []).length;
    assert.ok(controls >= 1 && controls <= 3, `${gen}: 1-3 controls, got ${controls}`);
    for (const s of widget.sliders || []) {
      assert.ok(s.step > 0, `${gen}.${s.key}: positive step`);
      assert.ok(s.min < s.max, `${gen}.${s.key}: min < max`);
      assert.ok(s.value >= s.min && s.value <= s.max, `${gen}.${s.key}: default in range`);
    }
  }
});

test("baseline and ceiling ranges are disjoint wherever both exist", () => {
  // Disjoint ranges mean no slider combination can cross them — the
  // compile gate then only has to confirm the engine agrees.
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    const baseline = (widget.sliders || []).find((s) => s.key === "baseline");
    const ceiling = (widget.sliders || []).find((s) => s.key === "ceiling");
    if (baseline && ceiling) {
      assert.ok(baseline.max < ceiling.min, `${gen}: baseline range must sit below ceiling range`);
    }
  }
});

test("duration-coupled slider floors cover the scenario duration (review #534 M1)", () => {
  // The engine rejects a leak that resets mid-run (time_to_ceiling must be
  // >= duration). The floor is derived from durationSecs in the presets;
  // this pins the derivation so retuning the template cannot silently
  // strand the slider below its own scenario length.
  const leak = WIDGETS.leak;
  const ttc = leak.sliders.find((s) => s.key === "time_to_ceiling");
  assert.ok(ttc.min >= leak.durationSecs, "leak: time_to_ceiling floor must cover the duration");
  // And every widget's YAML really carries the duration it declares.
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    const yaml = widget.yaml(defaultParams(widget));
    assert.match(yaml, new RegExp(`duration: ${widget.durationSecs}s`), `${gen}: durationSecs drives the template`);
    assert.match(yaml, new RegExp(`rate: ${widget.rate}`), `${gen}: rate drives the template`);
  }
});

test("every min/max slider pair is non-crossable by range construction", () => {
  // Generalises the baseline/ceiling rule above to the pairs WP13 added.
  // The engine rejects `min >= max`, so a pair whose ranges OVERLAP ships a
  // widget that compiles at rest and throws under the reader's hand — the
  // failure only appears for the readers who actually play with it, which is
  // everyone the widget is for. Disjoint ranges make it unreachable rather
  // than merely untested.
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    const lo = (widget.sliders || []).find((s) => s.key === "min");
    const hi = (widget.sliders || []).find((s) => s.key === "max");
    if (lo && hi) {
      assert.ok(lo.max < hi.min, `${gen}: min range [${lo.min},${lo.max}] must sit below max range [${hi.min},${hi.max}]`);
    }
  }
});

test("the step widget wraps inside the sampled window at every corner", () => {
  // The `step` widget exists to show WRAP-AROUND. A `max` the counter never
  // reaches inside the sampled window draws a plain ramp and teaches the
  // wrong shape — a widget that is wrong in the most confident way, since it
  // renders perfectly.
  //
  // The window is `sampledTicks`, NOT rate * durationSecs (review #549 W1).
  // The sampler clamps to MAX_TICKS, so the product is an upper bound the
  // real window need not reach, and the multiplication grows more permissive
  // exactly where the window stops growing. The two agree for every widget
  // today only because the tick-budget invariant below holds the product at
  // or under MAX_TICKS — which made this check correct on the strength of a
  // NEIGHBOURING assertion rather than its own. Calling sampledTicks makes it
  // self-sufficient; the reviewer's demonstration edit is caught by that
  // neighbour today, so this is a latent coupling closed rather than a live
  // defect fixed.
  //
  // The counter climbs step_size per tick from `start`, so the worst case is
  // the smallest step_size with the largest max and the lowest start.
  const step = WIDGETS.step;
  const ticks = sampledTicks(step);
  const stepSize = step.sliders.find((s) => s.key === "step_size");
  const max = step.sliders.find((s) => s.key === "max");
  const start = step.sliders.find((s) => s.key === "start");
  const worstClimb = stepSize.min * ticks;
  assert.ok(
    worstClimb > max.max - start.min,
    `step: slowest climb ${worstClimb} must exceed the widest span ${max.max - start.min}, ` +
      "or the widget shows a ramp and calls it a wrap"
  );
});

test("the sequence widget's patterns are real, non-empty value lists", () => {
  // `sequence` carries its option data in the preset rather than the markup,
  // which is what puts every option through the compile gate. That only holds
  // if every offered option resolves to a list the engine accepts.
  const sequence = WIDGETS.sequence;
  const pattern = sequence.choices.find((c) => c.key === "pattern");
  for (const option of pattern.options) {
    const values = sequence.patterns[option];
    assert.ok(Array.isArray(values) && values.length > 0, `sequence: "${option}" must name a value list`);
    for (const v of values) assert.ok(Number.isFinite(v), `sequence: "${option}" values must be finite`);
    // And the option must reach the YAML — a pattern the template ignores is
    // a control that does nothing.
    assert.match(
      sequence.yaml({ ...defaultParams(sequence), pattern: option }),
      new RegExp(`values: \\[${values.join(", ").replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\]`),
      `sequence: "${option}" must reach the template`
    );
  }
  assert.ok(pattern.options.includes(pattern.value), "the default pattern must be an offered option");
});

test("preset sampling stays within the playground tick budget", () => {
  // Reads MAX_TICKS from the preset module rather than repeating 240, so this
  // and `sampledTicks` cannot drift apart. This assertion is what makes the
  // product and the real window coincide today — see the wrap guard above,
  // which no longer depends on that.
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    assert.ok(
      widget.rate * widget.durationSecs <= MAX_TICKS,
      `${gen}: rate*duration must fit MAX_TICKS`
    );
  }
});

test("the distribution widgets bound their per-tick observation volume", () => {
  // A metrics entry costs one number per tick. A histogram or summary entry
  // costs `observations_per_tick` DRAWS per tick, and the engine does that
  // work for every tick in the window — so the tick budget above, which
  // bounds ticks alone, says nothing about the quantity these two widgets
  // actually scale. The failure mode is not an error: the page gets heavy,
  // which is invisible to every other gate here and to a reader on a fast
  // machine.
  //
  // This is the half a pure module can check. It CANNOT check the heatmap's
  // cell count, because the bucket ladder is the engine's default for the
  // named distribution and nothing in this file knows it — asserting a
  // remembered number here would be a comment pretending to be a gate. The
  // real rendered row count is measured against the real sampler in the
  // browser suite instead.
  const DRAW_BUDGET = 15_000;
  for (const gen of ["histogram", "summary"]) {
    const widget = WIDGETS[gen];
    const ticks = sampledTicks(widget);
    const observations = (widget.sliders || []).find((s) => s.key === "observations");
    assert.ok(observations, `${gen}: expected an observations slider to bound`);
    const worst = ticks * observations.max;
    assert.ok(
      worst <= DRAW_BUDGET,
      `${gen}: ${ticks} ticks x ${observations.max} observations = ${worst} draws exceeds ${DRAW_BUDGET}`
    );
  }
});

test("a widget may only name encoders the browser's engine carries", () => {
  // The sibling of the logs check below, and the general form of it (review
  // #550 round 3 W1). That one is logs-only, so the widget whose entire
  // subject IS the encoder — `encoders`, offering its choice as a <select> —
  // had nothing saying its options must exist in the wasm build. Adding
  // `otlp` there passed the pure suite, passed 651 compile corners, passed
  // 98 browser checks, and put "configuration error: encoder type 'otlp'
  // requires the 'otlp' feature" in the pane labelled as the engine's bytes.
  //
  // Same root cause as round 2, one widget further out: `sonda-wasm` takes
  // sonda-core with `default-features = false, features = ["config"]`, so
  // `otlp` and `remote_write` — both feature-gated in encoder/mod.rs — are
  // absent, while ci.yml builds the CLI the compile gate uses WITH them.
  //
  // Checked over cornerParams so it sees every <select> option, not just the
  // default: the corner grid enumerates choices, which is what makes an
  // offered-but-broken option reachable here at all.
  //
  // EVERY occurrence, not the first. A widget's YAML names an encoder twice —
  // once in the `defaults:` preamble and once on the entry that overrides it —
  // and `String.match` returns only the first, which is always the preamble's.
  // The first version of this check read that one and passed the very
  // mutation it was written for.
  const WASM_ENCODERS = new Set(["prometheus_text", "influx_lp", "json_lines", "syslog"]);
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    for (const corner of cornerParams(widget)) {
      const named = [...widget.yaml(corner).matchAll(/encoder:\s*\{\s*type:\s*(\w+)/g)].map(
        (m) => m[1]
      );
      assert.ok(named.length > 0, `${gen}: no encoder named in the widget's YAML`);
      for (const encoder of named) {
        assert.ok(
          WASM_ENCODERS.has(encoder),
          `${gen}: encoder "${encoder}" is not in the wasm build — the widget would compile and then fail to sample`
        );
      }
    }
  }
});

test("a logs widget declares an encoder that can encode a log", () => {
  // The compile gate cannot see this. `encoder: { type: prometheus_text }`
  // with `signal_type: logs` COMPILES — `sonda --dry-run run` accepts it —
  // and fails at sampling with "log encoding not supported by this encoder"
  // (encoder/mod.rs). So the widget passes 648 corner compilations and shows
  // every reader an error box. Caught in browser UAT, which is the only gate
  // that runs the sampler, and pinned here so the next logs widget cannot
  // inherit the metrics default silently.
  //
  // The list is an INTERSECTION, and the second half is the half that bites
  // (review #550 round 2 W1). The first version listed the three encoders
  // implementing `encode_log` — json.rs, syslog.rs, otlp.rs — and that is a
  // true statement about sonda-core which is nevertheless the wrong list for
  // a browser widget.
  //
  //   1. implements `encode_log`      json_lines, syslog, otlp
  //   2. present in the wasm build    json_lines, syslog
  //
  // `sonda-wasm` depends on sonda-core with `default-features = false,
  // features = ["config"]` (sonda-wasm/Cargo.toml), and `otlp` pulls in
  // `runtime` plus four crates, so it is absent from the engine these widgets
  // actually run on. `create_encoder` then returns "encoder type 'otlp'
  // requires the 'otlp' feature" — at SAMPLING, exactly the failure this
  // whole invariant exists to prevent.
  //
  // And the other two nets would not have caught it: this suite would pass
  // because otlp was on the list, and the compile gate would pass because
  // ci.yml builds the release binary with `-F otlp` even though the wasm has
  // no such feature. An invariant written to close the compile-vs-sample gap
  // that permits a value reopening it is worse than no invariant, because it
  // is read as coverage.
  //
  // `syslog` is not feature-gated in sonda-core at all, so it stays.
  const LOG_CAPABLE = new Set(["json_lines", "syslog"]);
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    if (widget.signal !== "logs") continue;
    for (const corner of cornerParams(widget)) {
      // matchAll for the same reason as the check above: the preamble and the
      // entry each name an encoder, and only reading the first would miss an
      // entry-level override.
      const named = [...widget.yaml(corner).matchAll(/encoder:\s*\{\s*type:\s*(\w+)/g)].map(
        (m) => m[1]
      );
      assert.ok(named.length > 0, `${gen}: no encoder named in the widget's YAML`);
      for (const encoder of named) {
        assert.ok(
          LOG_CAPABLE.has(encoder),
          `${gen}: encoder "${encoder}" cannot encode a log event — the widget would compile and then fail to sample`
        );
      }
    }
  }
});

test("every control reaches its widget's template (review #549 W2)", () => {
  // The generalisation of the hand-written `sequence` pattern check above.
  // That one covered ONE of sequence's two controls, and an inert `repeat`
  // — `repeat: true` hard-coded in place of `${p.repeat === "on"}` — shipped
  // green through the whole suite AND the browser suite, whose redraw check
  // resolves the FIRST <select> and so never touches it. Measured, not
  // assumed: 191 tests passed with the control dead.
  //
  // Differential rather than textual: render the widget twice, changing one
  // control and nothing else, and require the YAML to differ. That makes no
  // assumption about interpolation syntax, so it keeps working for a control
  // the template consumes rather than pastes — `repeat` maps "on"/"off" to a
  // boolean, and a substring search for the option string would miss it.
  //
  // Only this direction needs a test. The reverse — a template referencing a
  // parameter no control supplies — interpolates `undefined` into the YAML
  // and the compile gate rejects it.
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    const base = defaultParams(widget);
    for (const slider of widget.sliders || []) {
      const lo = widget.yaml({ ...base, [slider.key]: slider.min });
      const hi = widget.yaml({ ...base, [slider.key]: slider.max });
      assert.notEqual(lo, hi, `${gen}.${slider.key}: a slider the template ignores does nothing`);
    }
    for (const choice of widget.choices || []) {
      assert.ok(choice.options.length >= 2, `${gen}.${choice.key}: a one-option <select> is not a control`);
      const first = widget.yaml({ ...base, [choice.key]: choice.options[0] });
      const rest = choice.options
        .slice(1)
        .map((option) => widget.yaml({ ...base, [choice.key]: option }));
      assert.ok(
        rest.some((yaml) => yaml !== first),
        `${gen}.${choice.key}: a <select> the template ignores does nothing`
      );
    }
  }
});

test("corner and sweep enumeration cover what the compile gate feeds the engine", () => {
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    const corners = cornerParams(widget);
    const expected = (widget.choices || []).reduce(
      (n, c) => n * c.options.length,
      (widget.sliders || []).reduce((n, s) => n * new Set([s.min, s.value, s.max]).size, 1)
    );
    assert.equal(corners.length, expected, `${gen}: {min,default,max} grid`);
    for (const corner of corners) {
      for (const s of widget.sliders || []) {
        assert.ok(
          [s.min, s.value, s.max].includes(corner[s.key]),
          `${gen}: corners use range edges or the default`
        );
      }
      // Every corner must produce a single-scenario YAML declaring the
      // widget's own signal type. Hard-coded to `metrics` until WP14, which
      // is the same shape of assumption WP13's slider-less widget broke: an
      // assertion that reads as general and is really about the one case that
      // existed when it was written.
      assert.match(widget.yaml(corner), new RegExp(`signal_type: ${widget.signal || "metrics"}\\b`));
    }
  }
  // Sweeps walk every step of the named slider under min/max neighbors.
  const sweep = sweepParams(WIDGETS.leak, "time_to_ceiling");
  const steps = (600 - WIDGETS.leak.durationSecs) / 10 + 1;
  assert.equal(sweep.length, steps * 4); // 2 other sliders, min/max each
  assert.equal(sweep[0].time_to_ceiling, WIDGETS.leak.durationSecs);
  assert.equal(sweep[sweep.length - 1].time_to_ceiling, 600);
  assert.equal(sweepParams(WIDGETS.leak, "nope").length, 0);
});

// --- runnableScenario (shared case table) ------------------------------

// The browser module and the CI extractor are two implementations of one
// rule. They answer the SAME table, loaded from disk — a case added for one
// side is automatically demanded of the other, so the two cannot drift.
const casesPath = new URL("./runnable-cases.json", import.meta.url);
const { cases } = JSON.parse(readFileSync(casesPath, "utf8"));

test("shared case table is non-trivial and covers both verdicts", () => {
  assert.ok(cases.length >= 20, "table should be a real hostile-case table");
  assert.ok(cases.some((c) => c.expected === true), "needs positive cases");
  assert.ok(cases.some((c) => c.expected === false), "needs negative cases");
});

for (const testCase of cases) {
  test(`runnableScenario: ${testCase.name}`, () => {
    assert.equal(
      runnableScenario(testCase.text),
      testCase.expected,
      `expected ${testCase.expected} for: ${JSON.stringify(testCase.text.slice(0, 60))}`
    );
  });
}

test("normalizeFence dedents uniformly and leaves flush text alone", () => {
  assert.equal(normalizeFence("version: 2\nkind: runnable\n"), "version: 2\nkind: runnable\n");
  assert.equal(normalizeFence("    a\n    b\n"), "a\nb\n");
  // A blank line carries no indentation signal — it must not pin the dedent at 0.
  assert.equal(normalizeFence("    a\n\n    b\n"), "a\n\nb\n");
  // Ragged indentation dedents by the common prefix only.
  assert.equal(normalizeFence("  a\n    b\n"), "a\n  b\n");
  assert.equal(normalizeFence("﻿version: 2"), "version: 2");
  assert.equal(normalizeFence("a\r\nb\rc"), "a\nb\nc");
});

// A fence rendered by Material carries the scenario in `code.textContent`.
// The detector must survive that lens as well as the raw-markdown one.
test("runnableScenario accepts a fence as textContent yields it", () => {
  const rendered = "version: 2\nscenarios:\n  - signal_type: metrics\n    name: cpu_usage\n";
  assert.equal(runnableScenario(rendered), true);
});

// --- hash size cap (§1b hardening) -------------------------------------

test("hash payloads at and under the cap are accepted", () => {
  assert.equal(hashPayloadTooLarge("a".repeat(MAX_HASH_PAYLOAD)), false);
  assert.equal(hashPayloadTooLarge("a".repeat(MAX_HASH_PAYLOAD - 1)), false);
  assert.equal(hashPayloadTooLarge(""), false);
});

test("one byte over the cap is rejected — before any decode happens", () => {
  assert.equal(hashPayloadTooLarge("a".repeat(MAX_HASH_PAYLOAD + 1)), true);
  assert.equal(hashPayloadTooLarge("a".repeat(MAX_HASH_PAYLOAD * 4)), true);
});

test("the cap is measured in payload characters, not decoded bytes", () => {
  // The point of the guard is to refuse work BEFORE decoding, so the check
  // must be a pure length test on the raw hash payload.
  assert.equal(MAX_HASH_PAYLOAD, 32 * 1024);
});

// --- exportFilename ----------------------------------------------------

// An entry name is validated by the engine, not by any filesystem. Every
// shape here is a name the engine accepts; none of them may reach a Save
// dialog intact.
const FILENAME_CASES = [
  { name: "cpu_usage", want: "cpu_usage.yaml" },
  { name: "web-01", want: "web-01.yaml" },
  { name: "CPU_Usage", want: "cpu_usage.yaml", why: "lowercased" },
  { name: "cpu usage %", want: "cpu-usage.yaml", why: "spaces and punctuation collapse, trailing trimmed" },
  { name: "a//b", want: "a-b.yaml", why: "a run of separators becomes ONE hyphen" },
  { name: "a--b", want: "a-b.yaml", why: "pre-existing repeats collapse too" },
  { name: "../x", want: "x.yaml", why: "path traversal cannot survive" },
  { name: "../../etc/passwd", want: "etc-passwd.yaml", why: "deep traversal flattens to a bare name" },
  { name: "/abs/path", want: "abs-path.yaml", why: "absolute path flattens" },
  { name: ".bashrc", want: "bashrc.yaml", why: "never produce a dotfile" },
  { name: "report.tar.gz", want: "report-tar-gz.yaml", why: "exactly one extension, always ours" },
  { name: "東京", want: "scenario.yaml", why: "sanitizes to nothing -> fallback, not '.yaml'" },
  { name: "!!!", want: "scenario.yaml", why: "punctuation-only -> fallback" },
  { name: "", want: "scenario.yaml" },
  { name: "   ", want: "scenario.yaml" },
  { name: "-lead-and-trail-", want: "lead-and-trail.yaml" },
  { name: "café", want: "caf.yaml", why: "accented char is not in the allowed set" },
];

for (const testCase of FILENAME_CASES) {
  const label = `exportFilename: ${JSON.stringify(testCase.name)}${testCase.why ? ` (${testCase.why})` : ""}`;
  test(label, () => {
    assert.equal(exportFilename([{ name: testCase.name }], "yaml"), testCase.want);
  });
}

test("exportFilename never returns a path separator or a leading dot", () => {
  for (const testCase of FILENAME_CASES) {
    const out = exportFilename([{ name: testCase.name }], "yaml");
    assert.ok(!out.includes("/"), `${out} contains /`);
    assert.ok(!out.includes("\\"), `${out} contains backslash`);
    assert.ok(!out.startsWith("."), `${out} is a dotfile`);
    assert.equal(out.split(".").length, 2, `${out} must have exactly one extension`);
  }
});

test("exportFilename falls back when there is nothing to name it after", () => {
  assert.equal(exportFilename([], "yaml"), "scenario.yaml");
  assert.equal(exportFilename(null, "yaml"), "scenario.yaml");
  assert.equal(exportFilename(undefined, "yaml"), "scenario.yaml");
  assert.equal(exportFilename([{}], "yaml"), "scenario.yaml");
  assert.equal(exportFilename([{ name: null }], "yaml"), "scenario.yaml");
});

test("exportFilename caps the stem at 40 chars without a trailing hyphen", () => {
  const long = exportFilename([{ name: "x".repeat(80) }], "yaml");
  assert.equal(long, `${"x".repeat(40)}.yaml`);
  // The cut must land ON a hyphen to exercise the post-cap trim: 39 x's then
  // a space puts the separator at index 39, so slice(0, 40) ends with it.
  const boundary = exportFilename([{ name: `${"x".repeat(39)} tail` }], "png");
  assert.equal(boundary, `${"x".repeat(39)}.png`);
  assert.ok(!boundary.includes("-."), "a capped stem must not end in a hyphen");
});

test("exportFilename takes the extension as given, dotted or bare", () => {
  assert.equal(exportFilename([{ name: "cpu" }], "png"), "cpu.png");
  assert.equal(exportFilename([{ name: "cpu" }], ".png"), "cpu.png");
  assert.equal(exportFilename([{ name: "cpu" }], ""), "cpu");
});

test("exportFilename names the file after the FIRST entry", () => {
  assert.equal(exportFilename([{ name: "first" }, { name: "second" }], "yaml"), "first.yaml");
});

// --- galleryCardState --------------------------------------------------
//
// The examples gallery mounts one widget per example file, and "the engine
// said ok" is NOT the same claim as "there is something to draw". Every case
// below is a shape a real file in examples/ produces — measured through the
// committed wasm bundle, not imagined:
//
//   50 of the 62 cards       entries, a line chart
//    6 cards                 skipped with a reason (all csv_replay + otlp)
//    6 cards                 logs, histograms or summaries — nothing linear
//
// A card that showed a blank canvas for the last twelve would look broken and
// be, technically, a success. That is the failure this function exists to
// stop, so the tests below lean on the boundary between ok-with-nothing and
// ok-with-a-chart rather than on the happy path.

const okWith = (extra) => ({
  ok: true,
  error: null,
  entries: [],
  histograms: [],
  summaries: [],
  logs: [],
  skipped: [],
  ...extra,
});

test("a metrics entry is a chart", () => {
  const state = galleryCardState(okWith({ entries: [{ name: "cpu", values: [1, 2] }] }));
  assert.equal(state.mode, "chart");
  assert.equal(state.extraSeries, 0);
});

test("extra series are counted, not drawn", () => {
  const entries = [{ name: "a" }, { name: "b" }, { name: "c" }];
  assert.equal(galleryCardState(okWith({ entries })).extraSeries, 2);
});

test("ok with nothing at all is not a chart — a kind: composable pack", () => {
  const state = galleryCardState(okWith({}));
  assert.equal(state.mode, "empty");
  assert.match(state.message, /Nothing to sample/);
});

test("a skipped entry surfaces the engine's own reason verbatim", () => {
  // Verbatim means verbatim: no prefix, no rewording. The engine's skip
  // messages are written for a reader and already name the limitation, and a
  // card that introduces them says "browser" twice (review #541 B1).
  const reason = "metric csv_replay reads a file — no filesystem in the browser";
  const state = galleryCardState(okWith({ skipped: [{ id: "cpu_replay", reason }] }));
  assert.equal(state.mode, "skipped");
  assert.equal(state.message, reason);
});

test("a skipped entry with no reason still says it was skipped", () => {
  const state = galleryCardState(okWith({ skipped: [{ id: "x" }] }));
  assert.equal(state.mode, "skipped");
  assert.match(state.message, /Not sampled in the browser/);
});

test("logs, histograms and summaries each get their own note", () => {
  assert.match(galleryCardState(okWith({ logs: [{ lines: [] }] })).message, /Log stream/);
  assert.match(galleryCardState(okWith({ histograms: [{}] })).message, /Histogram/);
  assert.match(galleryCardState(okWith({ summaries: [{}] })).message, /Summary/);
});

test("entries win over logs — a mixed scenario shows its chart", () => {
  const state = galleryCardState(okWith({ entries: [{ name: "cpu" }], logs: [{ lines: [] }] }));
  assert.equal(state.mode, "chart");
});

test("a specific skip reason wins over the generic empty message", () => {
  const state = galleryCardState(okWith({ skipped: [{ reason: "because" }] }));
  assert.equal(state.mode, "skipped");
});

test("ok: false is an error carrying the engine's message", () => {
  const state = galleryCardState({ ok: false, error: "compile_after error" });
  assert.equal(state.mode, "error");
  assert.equal(state.message, "compile_after error");
});

test("ok: false with no message still names the failure", () => {
  for (const bad of [null, "", "   ", 42]) {
    const state = galleryCardState({ ok: false, error: bad });
    assert.equal(state.mode, "error");
    assert.equal(state.message, "compile error");
  }
});

test("hostile and malformed results degrade to an error, never throw", () => {
  for (const bad of [null, undefined, 0, "", "ok", [], { ok: "true" }, { ok: 1 }]) {
    const state = galleryCardState(bad);
    assert.equal(state.mode, "error", `for ${JSON.stringify(bad)}`);
    assert.ok(state.message.length > 0);
  }
});

test("non-array output fields are treated as absent, not iterated", () => {
  const state = galleryCardState({
    ok: true,
    entries: "not an array",
    skipped: { reason: "not an array either" },
    logs: null,
  });
  assert.equal(state.mode, "empty");
});

// --- scheduleWindows ---------------------------------------------------
//
// The shading under both charts. It is worth a case table because every
// input reaching it comes from a slider or from user YAML, and two of the
// shapes a slider can produce used to hang the browser: the loops advance by
// `every`, so `every: 0` spins forever, and it is recomputed on every theme
// flip and every resize.
//
// Engine semantics being pinned here: a burst opens its cycle, a gap closes
// it. Drawing either in the other's place would misreport when the signal
// actually stops.

const withSchedule = (extra) => ({ offset_secs: 0, ...extra });

test("bursts open their cycle, gaps close it", () => {
  const windows = scheduleWindows(
    withSchedule({ burst: { every_secs: 20, for_secs: 5 }, gap: { every_secs: 10, for_secs: 3 } }),
    30
  );
  const bursts = windows.filter((w) => w.kind === "burst");
  const gaps = windows.filter((w) => w.kind === "gap");
  assert.deepEqual(bursts, [
    { kind: "burst", start: 0, end: 5 },
    { kind: "burst", start: 20, end: 25 },
  ]);
  assert.deepEqual(gaps, [
    { kind: "gap", start: 7, end: 10 },
    { kind: "gap", start: 17, end: 20 },
    { kind: "gap", start: 27, end: 30 },
  ]);
});

test("windows shift by the entry's offset", () => {
  const windows = scheduleWindows(
    { offset_secs: 12, burst: { every_secs: 10, for_secs: 2 } },
    32
  );
  assert.deepEqual(windows.map((w) => w.start), [12, 22]);
});

test("every <= 0 yields nothing instead of spinning forever", () => {
  for (const every of [0, -5, NaN, Infinity, undefined, null, "soon"]) {
    const windows = scheduleWindows(withSchedule({ gap: { every_secs: every, for_secs: 3 } }), 60);
    assert.deepEqual(windows, [], `every=${String(every)}`);
  }
});

test("a zero or negative for: shades nothing", () => {
  for (const forSecs of [0, -1, NaN, undefined]) {
    assert.deepEqual(
      scheduleWindows(withSchedule({ burst: { every_secs: 10, for_secs: forSecs } }), 30),
      [],
      `for=${String(forSecs)}`
    );
  }
});

test("for longer than the cycle is clipped per cycle, not run together", () => {
  // every 10s, for 25s: without clipping this is one 25s band that hides the
  // period entirely, and consecutive windows would overlap.
  const windows = scheduleWindows(withSchedule({ burst: { every_secs: 10, for_secs: 25 } }), 30);
  assert.deepEqual(windows, [
    { kind: "burst", start: 0, end: 10 },
    { kind: "burst", start: 10, end: 20 },
    { kind: "burst", start: 20, end: 30 },
  ]);
  for (let i = 1; i < windows.length; i++) {
    assert.ok(windows[i].start >= windows[i - 1].end, "windows must not overlap");
  }
});

test("a gap as long as its cycle starts at the cycle, never before it", () => {
  const windows = scheduleWindows(withSchedule({ gap: { every_secs: 10, for_secs: 30 } }), 20);
  assert.deepEqual(windows.map((w) => w.start), [0, 10]);
  for (const w of windows) assert.ok(w.start >= 0, "no window may precede the series");
});

test("the last window is clipped to the end of the series", () => {
  const windows = scheduleWindows(withSchedule({ burst: { every_secs: 10, for_secs: 8 } }), 25);
  assert.equal(windows.at(-1).end, 25);
  for (const w of windows) assert.ok(w.end <= 25);
});

test("a series that ends before it starts has no windows", () => {
  assert.deepEqual(
    scheduleWindows({ offset_secs: 40, gap: { every_secs: 5, for_secs: 2 } }, 30),
    []
  );
  assert.deepEqual(
    scheduleWindows({ offset_secs: 0, gap: { every_secs: 5, for_secs: 2 } }, 0),
    []
  );
});

test("an entry with no schedule, or no entry at all, yields nothing", () => {
  assert.deepEqual(scheduleWindows({ offset_secs: 0 }, 30), []);
  for (const bad of [null, undefined, 0, "entry", []]) {
    assert.deepEqual(scheduleWindows(bad, 30), [], `entry=${JSON.stringify(bad)}`);
  }
  assert.deepEqual(scheduleWindows(withSchedule({ gap: { every_secs: 5, for_secs: 1 } }), NaN), []);
});

test("a pathological cycle count is capped rather than drawn", () => {
  const windows = scheduleWindows(
    withSchedule({ gap: { every_secs: 0.001, for_secs: 0.0005 } }),
    3600
  );
  assert.equal(windows.length, MAX_SCHEDULE_CYCLES);
});

test("the cap counts cycles per kind, so burst + gap can return twice it", () => {
  // Review #543 N1. The cap is named for cycles because that is what it
  // bounds; the loop runs once per kind, so the RETURNED array is not bounded
  // by it. Pinned rather than fixed: 1024 rects is nothing to draw, and
  // bounding the total would mean one kind silently eating the other's
  // budget depending on which was walked first.
  const windows = scheduleWindows(
    withSchedule({
      burst: { every_secs: 0.001, for_secs: 0.0005 },
      gap: { every_secs: 0.001, for_secs: 0.0005 },
    }),
    3600
  );
  assert.equal(windows.length, 2 * MAX_SCHEDULE_CYCLES);
  assert.equal(windows.filter((w) => w.kind === "burst").length, MAX_SCHEDULE_CYCLES);
  assert.equal(windows.filter((w) => w.kind === "gap").length, MAX_SCHEDULE_CYCLES);
});

// The four shapes below all HUNG before review #543 W1, and none of them was
// caught by the cap above, because that cap counted emitted windows and these
// emit none. Each one is written to fail by timing out rather than by
// asserting, which is why they are worth naming individually: a test that can
// only fail by never finishing needs to be pointed at the exact input.
//
// Verified against the pre-fix implementation with a subprocess timeout — see
// the note in `scheduleWindows`'s comment. All four spun; the two `posInf`
// and `NaN` spellings, which look equally hostile, did not.

test("a non-finite offset yields nothing instead of looping forever", () => {
  // `-Infinity` is truthy, so `Number(x) || 0` passes it straight through and
  // every cycle then starts at -Infinity, forever short of the end. The
  // string spelling arrives from YAML; `-1e309` is the spelling that looks
  // finite in source and overflows on the way in.
  for (const offset of [-Infinity, "-Infinity", -1e309]) {
    assert.deepEqual(
      scheduleWindows({ offset_secs: offset, burst: { every_secs: 10, for_secs: 4 } }, 60),
      [],
      `offset=${String(offset)}`
    );
  }
});

test("a cycle too small to advance a large offset terminates", () => {
  // Entirely finite, entirely positive, no guard rejects it: 1e6 + 1e-13 is
  // 1e6 in double precision, so the cycle start never moves and no window is
  // ever wide enough to emit. Bounding the loop is what ends this; bounding
  // the output cannot.
  assert.deepEqual(
    scheduleWindows({ offset_secs: 1e6, gap: { every_secs: 1e-13, for_secs: 1e-13 } }, 1e6 + 60),
    []
  );
});

// --- burstEmission -----------------------------------------------------
//
// The burst multiplier's only channel (review #543 B1). Before this, dragging
// `multiplier` from 1 to 10 left the mini-chart byte-identical: the trace is
// the metric's VALUE and a burst does not change the value, so there was
// nothing for the eye to catch. The label is what the multiplier does —
// `rate * multiplier` events per second while the band is open, which is the
// engine's `interval = base_interval / multiplier` read the other way round.
//
// What these cases have to pin is that the label is a function of BOTH engine
// numbers. A label that only tracked the multiplier would be the slider's own
// value spelled differently, which is the decoration this finding was about.

const burstEntry = (rate, multiplier) => ({
  rate,
  burst: { every_secs: 15, for_secs: 4, multiplier },
});

test("the burst label reports the rate outside the band and inside it", () => {
  assert.deepEqual(burstEmission(burstEntry(4, 3)), {
    base: 4,
    during: 12,
    multiplier: 3,
    label: "4/s → 12/s",
  });
});

test("the burst label moves with the rate, not only with the multiplier", () => {
  // Same multiplier, different rate: if these two agreed, the label would be
  // reporting the slider rather than the emission rate.
  assert.notEqual(burstEmission(burstEntry(4, 3)).label, burstEmission(burstEntry(2, 3)).label);
  // Same rate, different multiplier — the case the finding was actually about.
  assert.notEqual(burstEmission(burstEntry(4, 3)).label, burstEmission(burstEntry(4, 4)).label);
});

test("a multiplier of 1 says so rather than going blank", () => {
  // "What does x1 do?" is a question the widget should answer, and the answer
  // is "nothing" — stated, not implied by an absent label.
  assert.equal(burstEmission(burstEntry(4, 1)).label, "4/s → 4/s");
});

test("fractional rates and multipliers stay readable", () => {
  assert.equal(burstEmission(burstEntry(4, 3.5)).label, "4/s → 14/s");
  assert.equal(burstEmission(burstEntry(0.5, 3)).label, "0.5/s → 1.5/s");
  // Float dust from rate * multiplier must not reach the canvas.
  assert.equal(burstEmission(burstEntry(0.1, 3)).label, "0.1/s → 0.3/s");
});

test("no burst, or a rate the label would lie about, yields no label", () => {
  assert.equal(burstEmission({ rate: 4 }), null, "no burst window");
  assert.equal(burstEmission({ rate: 4, gap: { every_secs: 5, for_secs: 1 } }), null, "gap only");
  assert.equal(burstEmission({ rate: 4, burst: "soon" }), null, "burst not an object");
  for (const bad of [null, undefined, 0, "entry", []]) {
    assert.equal(burstEmission(bad), null, `entry=${JSON.stringify(bad)}`);
  }
  for (const bad of [0, -4, NaN, Infinity, undefined, null, "fast"]) {
    assert.equal(burstEmission(burstEntry(bad, 3)), null, `rate=${String(bad)}`);
    assert.equal(burstEmission(burstEntry(4, bad)), null, `multiplier=${String(bad)}`);
  }
});

test("every <select> option reaches the compile gate", () => {
  // A choice the widget offers and the gate never compiles is a control that
  // only fails when a reader uses it. The encoders widget is the case that
  // matters: sonda-wasm links sonda-core without the feature-gated encoders,
  // so naming one would produce "requires the 'otlp' feature" in the browser
  // while the CLI-backed gate stayed green.
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    for (const choice of widget.choices || []) {
      assert.ok(choice.options.length >= 2, `${gen}.${choice.key}: a select needs options`);
      assert.ok(
        choice.options.includes(choice.value),
        `${gen}.${choice.key}: the default must be one of the options`
      );
      const covered = new Set(cornerParams(widget).map((c) => c[choice.key]));
      for (const option of choice.options) {
        assert.ok(covered.has(option), `${gen}.${choice.key}: ${option} missing from the grid`);
      }
    }
  }
});

test("a preview widget carries no chart-only assumptions", () => {
  // `preview` widgets render encoded_preview instead of a canvas, so the
  // shading and the tick budget still have to hold, but nothing may depend
  // on there being a line.
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    if (!widget.preview) continue;
    assert.match(widget.yaml(defaultParams(widget)), /encoder: \{ type: /, `${gen}: names an encoder`);
  }
});


// --- the time cursor (WP9) ---------------------------------------------
//
// Three helpers between a pointer and a reading. The cases that matter are
// the ones where the obvious implementation is wrong: the axis gutter is not
// second zero, a chained scenario has no value before it starts, and a log
// line's `secs` is already timeline-absolute.

const GEOM = { padLeft: 48, plotW: 400, spanSecs: 60 };

test("the cursor inverts the chart's own seconds-to-pixels map", () => {
  assert.equal(cursorSecsAt(GEOM, 48), 0);
  assert.equal(cursorSecsAt(GEOM, 448), 60);
  assert.equal(cursorSecsAt(GEOM, 248), 30);
});

test("a pointer outside the plot has no reading, rather than a clamped one", () => {
  // The y-axis gutter is the case this is about: pixels 0..47 are labels, and
  // reporting second zero there would answer a question nobody asked.
  assert.equal(cursorSecsAt(GEOM, 47.9), null);
  assert.equal(cursorSecsAt(GEOM, 0), null);
  assert.equal(cursorSecsAt(GEOM, 448.1), null);
  assert.equal(cursorSecsAt(GEOM, 900), null);
});

test("a degenerate chart geometry yields no cursor instead of NaN seconds", () => {
  for (const bad of [null, undefined, "geom", 42, []]) {
    assert.equal(cursorSecsAt(bad, 100), null, `geom=${JSON.stringify(bad)}`);
  }
  assert.equal(cursorSecsAt({ ...GEOM, plotW: 0 }, 100), null, "zero-width plot");
  assert.equal(cursorSecsAt({ ...GEOM, spanSecs: 0 }, 100), null, "zero-length span");
  assert.equal(cursorSecsAt({ ...GEOM, spanSecs: Infinity }, 100), null);
  assert.equal(cursorSecsAt(GEOM, NaN), null);
  assert.equal(cursorSecsAt(GEOM, "left"), null);
});

const series = (extra) => ({
  id: "cpu",
  name: "cpu_usage",
  tick_secs: 0.5,
  offset_secs: 0,
  values: [10, 20, 30, 40, 50],
  ...extra,
});

test("a reading snaps to the nearest tick and says which tick it read", () => {
  // tick 0.5s: 1.1s is nearer index 2 (1.0s) than index 3 (1.5s).
  assert.deepEqual(cursorSamples([series()], 1.1), [
    { id: "cpu", name: "cpu_usage", value: 30, secs: 1 },
  ]);
  // Exactly between two ticks — Math.round settles it upward, consistently.
  assert.equal(cursorSamples([series()], 1.25)[0].value, 40);
});

test("an entry that has not started yet contributes nothing", () => {
  // The case clamping gets wrong. A chained scenario (`after:`) starting at
  // 60s has no value at 10s, and reporting its first sample would invent
  // data for a scenario the engine had not begun.
  const late = series({ offset_secs: 60 });
  assert.deepEqual(cursorSamples([late], 10), []);
  assert.deepEqual(cursorSamples([late], 59.7), [], "just before its start");
  assert.equal(cursorSamples([late], 60)[0].value, 10, "its own first tick");
  assert.equal(cursorSamples([late], 62)[0].value, 50, "its own last tick");
  assert.deepEqual(cursorSamples([late], 62.3), [], "past its end");
});

test("a series shorter than the span drops out past its end", () => {
  // Two entries on one chart: the short one stops at 2s, the long one runs on.
  const short = series({ id: "short", values: [1, 2, 3] }); // ends at 1.0s
  const long = series({ id: "long", tick_secs: 1, values: [5, 6, 7, 8, 9] });
  assert.deepEqual(cursorSamples([short, long], 0.5).map((r) => r.id), ["short", "long"]);
  assert.deepEqual(cursorSamples([short, long], 3).map((r) => r.id), ["long"]);
  assert.deepEqual(cursorSamples([short, long], 30), []);
});

test("rows come back in entry order, so the readout matches the legend", () => {
  const a = series({ id: "a", tick_secs: 1, values: [1, 1, 1] });
  const b = series({ id: "b", tick_secs: 1, values: [2, 2, 2] });
  assert.deepEqual(cursorSamples([a, b], 1).map((r) => r.id), ["a", "b"]);
  assert.deepEqual(cursorSamples([b, a], 1).map((r) => r.id), ["b", "a"]);
});

test("entries the readout cannot describe are skipped, not guessed at", () => {
  assert.deepEqual(cursorSamples([series({ values: [] })], 1), [], "no samples");
  assert.deepEqual(cursorSamples([series({ values: "10,20" })], 1), [], "values not an array");
  assert.deepEqual(cursorSamples([series({ tick_secs: 0 })], 1), [], "zero tick");
  assert.deepEqual(cursorSamples([series({ tick_secs: -1 })], 1), [], "negative tick");
  assert.deepEqual(cursorSamples([series({ tick_secs: NaN })], 1), [], "non-numeric tick");
  assert.deepEqual(cursorSamples([series({ offset_secs: -Infinity })], 1), [], "infinite offset");
  assert.deepEqual(cursorSamples([series({ values: [NaN, NaN, NaN] })], 1), [], "non-finite value");
  for (const bad of [null, undefined, 0, "entry", []]) {
    assert.deepEqual(cursorSamples([bad], 1), [], `entry=${JSON.stringify(bad)}`);
  }
  for (const bad of [null, undefined, "entries", 5]) {
    assert.deepEqual(cursorSamples(bad, 1), [], `entries=${JSON.stringify(bad)}`);
  }
  assert.deepEqual(cursorSamples([series()], NaN), [], "no cursor");
  assert.deepEqual(cursorSamples([series()], Infinity), []);
});

const logEntry = (extra) => ({
  tick_secs: 1,
  lines: [{ secs: 0 }, { secs: 1 }, { secs: 2 }, { secs: 3 }],
  ...extra,
});

test("a log line is highlighted within half a tick of the cursor", () => {
  assert.deepEqual(logLinesNear(logEntry(), 2), [2]);
  assert.deepEqual(logLinesNear(logEntry(), 2.3), [2]);
  assert.deepEqual(logLinesNear(logEntry(), 1.7), [2]);
  assert.deepEqual(logLinesNear(logEntry(), 10), [], "past the stream");
});

test("a cursor exactly between two lines highlights both", () => {
  // Inclusive on purpose: an exclusive bound flickers between the two as the
  // pointer moves by sub-pixel amounts.
  assert.deepEqual(logLinesNear(logEntry(), 1.5), [1, 2]);
});

test("log correlation reads the shared timeline, not the entry's own clock", () => {
  // sonda-wasm stamps line.secs as offset_secs + tick * tick_secs, so a
  // chained log entry's lines are already absolute. Subtracting the offset
  // again would push the highlight off by the entry's start.
  const chained = logEntry({
    offset_secs: 60,
    lines: [{ secs: 60 }, { secs: 61 }, { secs: 62 }],
  });
  assert.deepEqual(logLinesNear(chained, 61), [1]);
  assert.deepEqual(logLinesNear(chained, 1), [], "no line at the un-offset time");
});

test("a log stream with nothing to correlate yields no highlight", () => {
  assert.deepEqual(logLinesNear(logEntry({ lines: [] }), 1), []);
  assert.deepEqual(logLinesNear(logEntry({ tick_secs: 0 }), 1), []);
  assert.deepEqual(logLinesNear(logEntry({ tick_secs: NaN }), 1), []);
  assert.deepEqual(logLinesNear(logEntry({ lines: [{ secs: "soon" }, { secs: 1 }] }), 1), [1]);
  for (const bad of [null, undefined, 0, "log", []]) {
    assert.deepEqual(logLinesNear(bad, 1), [], `log=${JSON.stringify(bad)}`);
  }
  assert.deepEqual(logLinesNear(logEntry(), NaN), []);
});
// --- parsePromQLRule (WP12) --------------------------------------------
//
// An import feature's whole risk is accepting more than it can represent.
// The lab evaluates one threshold against one sampled series, so the grammar
// is one selector and one scalar — and every richer rule below is valid
// PromQL that must be REFUSED BY NAME rather than half-read.
//
// Law 4: the selector value is the hostile surface. It is a PromQL string
// literal nested inside a YAML scalar, and review #532 caught a `: ` in one
// breaking the layer nobody was looking at.

const ok = (text) => {
  const result = parsePromQLRule(text);
  assert.ok(result.ok, `expected accept, got: ${result.reason}`);
  return result;
};
const no = (text) => {
  const result = parsePromQLRule(text);
  assert.ok(!result.ok, `expected reject, got ${JSON.stringify(result)}`);
  return result.reason;
};

test("a bare threshold expression imports", () => {
  const rule = ok("cpu_usage > 90");
  assert.equal(rule.metric, "cpu_usage");
  assert.equal(rule.op, ">");
  assert.equal(rule.threshold, 90);
  assert.equal(rule.forSecs, 0);
  assert.deepEqual(rule.selectors, {});
});

test("a rules-file snippet imports its name, threshold and for:", () => {
  const rule = ok(`
groups:
  - name: sonda-lab
    rules:
      - alert: CpuUsageHigh
        expr: 'cpu_usage{host="web-01"} > 64'
        for: 12s
`);
  assert.equal(rule.name, "CpuUsageHigh");
  assert.equal(rule.metric, "cpu_usage");
  assert.equal(rule.threshold, 64);
  assert.equal(rule.forSecs, 12);
  assert.deepEqual(rule.selectors, { host: { op: "=", value: "web-01" } });
});

test("the lab's own export round-trips back in", () => {
  // The strongest case available: feed buildTestExport's output to the
  // importer. If these two ever disagree the lab cannot read what it wrote.
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rules: [{ severity: "critical", op: ">", threshold: 64, forSecs: 12, evaled: { states: Array(40).fill("firing"), resolves: [] } }],
  });
  const rule = ok(out);
  assert.equal(rule.metric, "cpu_usage");
  assert.equal(rule.op, ">");
  assert.equal(rule.threshold, 64);
  assert.equal(rule.forSecs, 12);
  assert.deepEqual(rule.selectors, {
    host: { op: "=", value: "web-01" },
    service: { op: "=", value: "api" },
  });
});

test("durations import in every unit Prometheus writes", () => {
  const forOf = (text) => ok(`expr: cpu > 1\nfor: ${text}`).forSecs;
  assert.equal(forOf("30s"), 30);
  assert.equal(forOf("5m"), 300);
  assert.equal(forOf("2h"), 7200);
  assert.equal(forOf("1h30m"), 5400);
  assert.equal(forOf("1d"), 86400);
  assert.equal(forOf("500ms"), 0.5);
  assert.equal(forOf("90"), 90, "a bare number is seconds");
});

test("a duration with trailing junk is refused, not read as its prefix", () => {
  // Review #546 M3. `_durationSecs` rebuilds the string from the parts it
  // matched and compares — without that, every one of these imports as 300s
  // and the reader's `for:` is silently something they did not write.
  // Deleting the guard left all 146 tests green; none of these was a case.
  for (const bad of ["5m junk", "5mx", "5m5", "5 m", "m5", "5m,", "-5m"]) {
    assert.match(
      no(`expr: cpu > 1\nfor: ${bad}`),
      /could not read the duration/,
      `for: ${bad}`
    );
  }
  // And the shapes that ARE complete still parse, so the guard is not simply
  // refusing everything with more than one unit.
  assert.equal(ok("expr: cpu > 1\nfor: 1h30m").forSecs, 5400);
});

test("scientific and signed thresholds import as written", () => {
  assert.equal(ok("errors > 1e3").threshold, 1000);
  assert.equal(ok("errors > 1.5e-2").threshold, 0.015);
  assert.equal(ok("temp < -40").threshold, -40);
  assert.equal(ok("ratio > .5").threshold, 0.5);
  assert.equal(ok("ratio > +0.5").threshold, 0.5);
});

test("whitespace anywhere is not a syntax error", () => {
  const rule = ok('   cpu_usage  {  host = "web-01" , az != "b"  }   >=   90.5   ');
  assert.equal(rule.op, ">=");
  assert.equal(rule.threshold, 90.5);
  assert.deepEqual(rule.selectors, {
    host: { op: "=", value: "web-01" },
    az: { op: "!=", value: "b" },
  });
});

test("hostile selector values survive intact", () => {
  // Each of these is a legal PromQL string literal, and each has broken a
  // layer of this stack before or is one comma away from doing so.
  const cases = [
    ['{msg="a: b"}', "a: b"], //         colon-space — the #532 shape
    ['{msg="a,b"}', "a,b"], //           a comma inside a value, not a separator
    ['{msg="say \\"hi\\""}', 'say "hi"'], // escaped quotes
    ['{msg="C:\\\\tmp"}', "C:\\tmp"], // escaped backslash
    ['{msg="{braces}"}', "{braces}"], //  braces inside the value
    ['{msg="東京"}', "東京"], //           non-ASCII
    ['{msg=""}', ""], //                  empty value
    ['{msg="  "}', "  "], //              whitespace-only value
    // The case that forces the splitter to track escapes. Without it the
    // `\\"` reads as the end of the string, the comma after it looks like a
    // separator, and the matcher is torn in half. Every other value above
    // survives a broken splitter because none of them pairs an escaped quote
    // with a comma — found by mutation, not by inspection.
    ['{msg="a \\" , b"}', 'a " , b'],
  ];
  for (const [selector, expected] of cases) {
    const rule = ok(`cpu${selector} > 1`);
    assert.equal(rule.selectors.msg.value, expected, selector);
  }
});

test("a backslash in one value does not swallow the next matcher", () => {
  // The case that forces the splitter's escape tracking, and the second one
  // mutation testing had to find: `{msg="a \\" , b"}` above does NOT
  // discriminate, because a splitter that stops resetting its escape flag
  // simply never splits — which still yields one correct selector there.
  // Here the comma is a REAL separator sitting after a backslash, so a
  // broken splitter loses the second matcher entirely.
  const rule = ok('cpu{a="x\\\\",b="y"} > 1');
  assert.deepEqual(rule.selectors, {
    a: { op: "=", value: "x\\" },
    b: { op: "=", value: "y" },
  });
});

test("regex matchers are read, not mistaken for equality", () => {
  const rule = ok('cpu{host=~"web-.*",az!~"eu-.*"} > 1');
  assert.deepEqual(rule.selectors, {
    host: { op: "=~", value: "web-.*" },
    az: { op: "!~", value: "eu-.*" },
  });
});

test("rules the lab cannot represent are refused BY NAME", () => {
  // Every one of these is valid PromQL. Half-reading them would put a reader
  // in front of a chart answering a different question than the rule they
  // pasted, which is worse than refusing.
  assert.match(no("rate(http_errors_total[5m]) > 10"), /function call/);
  assert.match(no("sum by (host) (cpu_usage) > 90"), /aggregation/);
  assert.match(no("histogram_quantile(0.99, foo) > 1"), /function call/);
  assert.match(no("up == 1 unless cpu > 90"), /set operator/);
  assert.match(no("cpu > 90 and mem > 90"), /set operator/);
  assert.match(no("cpu_usage[5m] > 90"), /range selector/);
  assert.match(no("cpu_usage offset 5m > 90"), /offset/);
});

test("a threshold the lab cannot evaluate is refused, not silently coerced", () => {
  assert.match(no("cpu_usage == 90"), /`>`.*`==`/);
  assert.match(no("cpu_usage != 90"), /`>`.*`!=`/);
});

test("a comparison against anything but a number is refused", () => {
  // The tail-matching trap: a lenient parser reads `> 90` and ignores the
  // left side entirely.
  no("cpu_usage > other_metric");
  no("cpu_usage > 90 * 2");
  no("cpu_usage + 1 > 90");
  no("cpu_usage");
  no("cpu_usage >");
  no("> 90");
});

test("the expression is read whole, not found inside a larger one", () => {
  // This is the case that forces the leading anchor, and mutation testing is
  // how it got written: unanchoring the regex left every other case in this
  // file green. `100 * cpu_usage > 90` contains a substring that parses
  // perfectly — a parser that scans for one would import a rule about a
  // scaled metric as though it were about the raw one, and the threshold
  // would be wrong by a factor of a hundred with nothing to show for it.
  no("100 * cpu_usage > 90");
  no("(cpu_usage / 2) > 90");
  no("some junk then cpu_usage > 90");
});

test("empty and malformed input says what is wrong", () => {
  assert.match(no(""), /nothing to import/);
  assert.match(no("   \n  "), /nothing to import/);
  assert.match(no(null), /nothing to import/);
  assert.match(no(undefined), /nothing to import/);
  assert.match(no("alert: NoExpr\n  for: 5m"), /no `expr:` line/);
  assert.match(no("expr: cpu > 1\nfor: soon"), /could not read the duration/);
  assert.match(no('cpu{host "web"} > 1'), /label matcher/);
  assert.match(no("cpu{host=web} > 1"), /label matcher/);
});

test("a double-quoted expr is unescaped like YAML, not like PromQL", () => {
  const rule = ok('expr: "cpu_usage{msg=\\"a: b\\"} > 5"');
  assert.equal(rule.selectors.msg.value, "a: b");
  assert.equal(rule.threshold, 5);
});

test("a # inside an expression is not treated as a comment", () => {
  // Guessing wrong here silently truncates the rule to something that still
  // parses, which is the worst available outcome.
  assert.equal(parsePromQLRule('cpu{path="/a#b"} > 1').selectors.path.value, "/a#b");
  // And through the YAML-scalar path, which is where the temptation to strip
  // a trailing comment actually lives. The bare-expression case above does
  // not reach `_unquoteScalar` at all, so it left that behaviour unpinned.
  assert.equal(parsePromQLRule('expr: cpu{path="/a#b"} > 1').selectors.path.value, "/a#b");
  assert.equal(parsePromQLRule("expr: cpu > 1 # a real trailing comment").ok, false);
});

// Review #546 round 2, W2. The grammar runs BEFORE the naming scan, and that
// ordering is the largest behavioural change in #546 — it was documented in
// capitals and pinned by nothing. Reverting it left the whole suite green,
// because every case in the "refused BY NAME" table is genuinely unsupported
// and so refused under either ordering.
//
// What discriminates is the opposite polarity: a rule the anchored grammar
// ACCEPTS whose label value happens to contain a token the scan greps for.
// Scan-first refuses these, and refuses them by asserting a specific false
// fact about the reader's own rule — `{user="alice@example.com"}` reported as
// "an offset or @ modifier". One case per row of the refusal table, so
// removing any single row's protection shows up here.
//
// The general lesson, which is why this table is written this way: vary the
// inputs the code interpolates AND the inputs the code scans.
test("a legal rule is not refused for a token the scan greps for", () => {
  // The nine the round-2 reviewer wrote down, plus one of mine. Two of theirs
  // are a shape my own first table missed entirely: `count{...}` and
  // `sum{...}` put the scanned token in the METRIC NAME, not in a quoted
  // value — and the aggregation pattern matches `count` followed by `{`, so
  // scan-first refuses a metric legitimately named `count`. Varying only
  // label values would never have reached that.
  const cases = [
    ['http_requests_total{user="alice@example.com"} > 100', 100, "an offset or @ modifier"],
    ['log_events{msg="[error] disk"} > 5', 5, "a range selector"],
    ['q{msg="a or b"} > 5', 5, "a set operator"],
    ['q{path="/v1/offset"} > 5', 5, "an offset or @ modifier"],
    ['q{q="rate(x)"} > 5', 5, "a function call"],
    ['q{cmd="unless"} > 5', 5, "a set operator"],
    ['q{msg="sum and count"} > 5', 5, "a set operator"],
    ['count{x="1"} > 5', 5, "an aggregation — metric NAME, not a value"],
    ['sum{x="1"} > 5', 5, "an aggregation — metric NAME, not a value"],
    ['cpu{job="sum by(x)"} > 1', 1, "an aggregation"],
  ];
  for (const [text, threshold, wouldClaim] of cases) {
    const rule = parsePromQLRule(text);
    assert.ok(
      rule.ok,
      `${text} is a legal threshold rule, but it was refused as ${wouldClaim}: ${rule.reason}`
    );
    assert.equal(rule.threshold, threshold, text);
  }
});

// And the other direction, so the pair cannot both be satisfied by a parser
// that simply stopped refusing things: the same tokens OUTSIDE a quoted value
// must still be refused, and still by name.
test("the same tokens outside a label value are still refused by name", () => {
  assert.match(no("rate(cpu[5m]) > 1"), /a function call/);
  assert.match(no("sum by(job) (cpu) > 1"), /an aggregation/);
  assert.match(no("cpu > 1 or mem > 1"), /a set operator/);
  assert.match(no("cpu[5m] > 1"), /a range selector/);
  assert.match(no("cpu offset 5m > 1"), /an offset or @ modifier/);
});


// --- yamlPathAt (WP11 PR3) ---------------------------------------------
//
// This runs on BROKEN YAML by design: a reader asking for completion has
// typed half a line. So the case table is written the way the editor will
// actually call it — cursor marked with `|` in a document that would not
// parse — rather than on tidy complete documents.

/** Split a fixture on the `|` cursor marker and resolve the path there. */
const at = (marked) => {
  const offset = marked.indexOf("|");
  assert.notEqual(offset, -1, "fixture must mark the cursor with |");
  return yamlPathAt(marked.replace("|", ""), offset);
};

test("the path at a top-level key is empty", () => {
  const got = at("version: 2\nkin|");
  assert.deepEqual(got.path, []);
  assert.equal(got.context, "key");
  assert.equal(got.prefix, "kin");
});

test("a value position names its own key", () => {
  const got = at("version: 2\nkind: run|");
  assert.deepEqual(got.path, ["kind"]);
  assert.equal(got.context, "value");
  assert.equal(got.prefix, "run");
});

test("a key inside a sequence item carries the [] step", () => {
  const got = at(
    "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    ra|"
  );
  assert.deepEqual(got.path, ["scenarios", "[]"]);
  assert.equal(got.context, "key");
  assert.equal(got.prefix, "ra");
});

test("a nested mapping inside a sequence item nests under it", () => {
  const got = at(
    "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    generator:\n      ty|"
  );
  assert.deepEqual(got.path, ["scenarios", "[]", "generator"]);
  assert.equal(got.context, "key");
});

test("a value inside a nested mapping names the full chain", () => {
  const got = at(
    "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    generator:\n      type: sin|"
  );
  assert.deepEqual(got.path, ["scenarios", "[]", "generator", "type"]);
  assert.equal(got.context, "value");
  assert.equal(got.prefix, "sin");
});

test("the key on the dash line itself is a sequence step", () => {
  const got = at("version: 2\nkind: runnable\nscenarios:\n  - signal_ty|");
  assert.deepEqual(got.path, ["scenarios", "[]"]);
  assert.equal(got.context, "key");
});

test("a blank indented line inherits its parent, not the line above it", () => {
  // The commonest completion moment: Enter, then Ctrl+Space. There is no
  // text on this line at all, so only the indentation can answer.
  const got = at(
    "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    generator:\n      type: sine\n    |"
  );
  assert.deepEqual(got.path, ["scenarios", "[]"]);
  assert.equal(got.context, "key");
  assert.equal(got.prefix, "");
});

test("a SECOND list item resolves like the first", () => {
  // Review #548 B1. Every fixture in the first version of this table used a
  // one-item list, so nothing could see an item count — and on the second
  // item's dash line the path grew a spurious second `[]`, which matches
  // nothing in the schema. That is the keystroke where a reader starts a new
  // entry, and 60 files in this repo have lists of two or more.
  //
  // The discriminating input is a list with TWO items. One is not enough,
  // and neither is varying the values inside a single item.
  const first = at("version: 2\nscenarios:\n  - sig|");
  const second = at("version: 2\nscenarios:\n  - signal_type: metrics\n  - sig|");
  const third = at("version: 2\nscenarios:\n  - a: 1\n  - b: 2\n  - sig|");
  assert.deepEqual(first.path, ["scenarios", "[]"]);
  assert.deepEqual(second.path, first.path, "a sibling item is not an ancestor");
  assert.deepEqual(third.path, first.path, "and neither are two of them");
});

test("a second item's indented field resolves like the first's", () => {
  const got = at("version: 2\nscenarios:\n  - a: 1\n  - signal_type: logs\n    ra|");
  assert.deepEqual(got.path, ["scenarios", "[]"]);
});

test("a nested sequence contributes one [] per list, not per item", () => {
  const got = at(
    "version: 2\nscenarios:\n  - signal_type: metrics\n    dynamic_labels:\n      - name: a\n      - na|"
  );
  assert.deepEqual(got.path, ["scenarios", "[]", "dynamic_labels", "[]"]);
});

test("a dash at the owning key's own indent keeps the parent", () => {
  // Review #548 W1. YAML lets the sequence sit at the same column as the key
  // that owns it. `want` used to drop to that column and then skip the owner
  // as though it were a sibling, losing `scenarios` entirely — the module
  // docstring claimed block style was handled, and this is block style.
  const field = at("version: 2\nscenarios:\n- signal_type: metrics\n  ra|");
  assert.deepEqual(field.path, ["scenarios", "[]"]);
  const dash = at("version: 2\nscenarios:\n- a: 1\n- sig|");
  assert.deepEqual(dash.path, ["scenarios", "[]"]);
});

test("dedenting out of a nested mapping walks back up", () => {
  const got = at(
    "version: 2\nkind: runnable\ndefaults:\n  encoder:\n    type: prometheus_text\n  ra|"
  );
  assert.deepEqual(got.path, ["defaults"]);
});

test("comments between lines do not become ancestors", () => {
  const got = at(
    "version: 2\nkind: runnable\nscenarios:\n  # the cpu signal\n  - signal_type: metrics\n    # how fast\n    ra|"
  );
  assert.deepEqual(got.path, ["scenarios", "[]"]);
});

test("completion declines inside a comment", () => {
  assert.equal(at("version: 2\n# kin|"), null);
  assert.equal(at("version: 2  # a note her|"), null);
});

test("a # inside a scalar is not a comment", () => {
  // Same trap the PromQL importer hit: `#` only opens a comment after
  // whitespace. `path: /a#b` is one scalar.
  const got = at("version: 2\nscenarios:\n  - path: /a#b|");
  assert.notEqual(got, null);
  assert.equal(got.context, "value");
});

test("completion declines inside a quoted scalar", () => {
  // Key names mean nothing here, and the `: ` inside would otherwise be read
  // as a separator.
  assert.equal(at('scenarios:\n  - msg: "a: b|'), null);
  assert.equal(at("scenarios:\n  - msg: 'hello worl|"), null);
});

test("a closed quote is not inside a scalar", () => {
  const got = at('scenarios:\n  - msg: "a: b"\n    ra|');
  assert.deepEqual(got.path, ["scenarios", "[]"]);
});

test("completion declines on flow style rather than guessing", () => {
  // Indentation says nothing about position inside `{...}`, so an answer
  // here would look confident and be unrelated to the cursor.
  assert.equal(at("scenarios:\n  - generator: { type: sin|"), null);
  assert.equal(at("scenarios:\n  - buckets: [1, 2, |"), null);
});

test("a colon with no space after it is a scalar, not a separator", () => {
  // YAML needs whitespace after the `:` for it to separate a key from a
  // value; `ratio:9` is one plain scalar. A reader mid-word here is still
  // typing a KEY, and telling them otherwise resolves the path one level
  // too deep and offers the wrong list.
  //
  // The first version of this case used `at: 10:3` and could not fail: the
  // line's FIRST colon already had a space after it, so a separator rule
  // that ignored the space entirely returned the same answer. The case has
  // to put the space-less colon where the separator would be found.
  const scalar = at("scenarios:\n  - ratio:9|");
  assert.equal(scalar.context, "key");
  assert.deepEqual(scalar.path, ["scenarios", "[]"]);
  assert.equal(scalar.prefix, "ratio:9");

  // And the ordinary case still reads as a value, colons in the value and
  // all — `url: http://x` has exactly one separator.
  const value = at("scenarios:\n  - url: http://x|");
  assert.equal(value.context, "value");
  assert.deepEqual(value.path, ["scenarios", "[]", "url"]);
  assert.equal(value.prefix, "http://x");
});

test("an empty document offers root keys", () => {
  const got = at("|");
  assert.deepEqual(got.path, []);
  assert.equal(got.context, "key");
  assert.equal(got.prefix, "");
});

test("an out-of-range offset is clamped rather than thrown", () => {
  assert.notEqual(yamlPathAt("version: 2", 9999), null);
  assert.notEqual(yamlPathAt("version: 2", -5), null);
  assert.notEqual(yamlPathAt(null, 0), null);
});

// --- schemaCompletions (WP11 PR3) --------------------------------------
//
// Driven by the REAL committed schema, not a fixture. A hand-written schema
// fixture would let this suite pass while the shipped schema's shape drifted
// away from it — and the shipped schema is a generated artifact that moves
// whenever a config type does.

const SCHEMA = JSON.parse(
  readFileSync(
    new URL("../../docs/schema/sonda-scenario.schema.json", import.meta.url),
    "utf8"
  )
);

const labels = (path, context) =>
  schemaCompletions(SCHEMA, path, context).map((c) => c.label);

test("root keys come from all three top-level shapes", () => {
  const got = labels([], "key");
  for (const key of ["version", "kind", "scenarios", "defaults"]) {
    assert.ok(got.includes(key), `root should offer ${key}, got ${got.join(",")}`);
  }
  // The composable branch contributes too — the root is an anyOf and a pack
  // definition is one of the shapes a reader may be writing.
  assert.ok(got.includes("metrics"));
});

test("kind offers exactly the two values the parser takes", () => {
  assert.deepEqual(labels(["kind"], "value"), ["composable", "runnable"]);
});

test("entry keys come through the sequence step", () => {
  const got = labels(["scenarios", "[]"], "key");
  for (const key of ["signal_type", "generator", "rate", "duration", "encoder", "sink"]) {
    assert.ok(got.includes(key), `entry should offer ${key}`);
  }
});

test("generator type offers every variant the engine has", () => {
  // The single most useful completion in the document, and the reason union
  // branches are unioned rather than picked between.
  const got = labels(["scenarios", "[]", "generator", "type"], "value");
  for (const kind of ["constant", "sine", "sawtooth", "step", "spike", "csv_replay"]) {
    assert.ok(got.includes(kind), `generator types should include ${kind}, got ${got.join(",")}`);
  }
  assert.ok(got.length >= 10, `expected the full variant list, got ${got.length}`);
});

test("a generator's own fields are offered once a type is in view", () => {
  const got = labels(["scenarios", "[]", "generator"], "key");
  assert.ok(got.includes("type"));
  // Unioned across branches: `amplitude` is sine's, `value` is constant's.
  assert.ok(got.includes("amplitude"));
  assert.ok(got.includes("value"));
});

test("sink and encoder types are offered", () => {
  const sinks = labels(["scenarios", "[]", "sink", "type"], "value");
  assert.ok(sinks.includes("stdout"), sinks.join(","));
  assert.ok(sinks.includes("file"));
  const encoders = labels(["scenarios", "[]", "encoder", "type"], "value");
  assert.ok(encoders.includes("prometheus_text"), encoders.join(","));
});

test("while.op offers the operator glyphs, not the variant names", () => {
  // The same wire-shape question #547 M1 was about, asked from the editor's
  // side: a reader completing `op:` must be offered `<` and `>`.
  assert.deepEqual(labels(["scenarios", "[]", "while", "op"], "value"), ["<", ">"]);
});

test("defaults resolves separately from an entry", () => {
  const got = labels(["defaults"], "key");
  assert.ok(got.includes("rate"));
  assert.ok(got.includes("encoder"));
  // `defaults` carries no `signal_type` — that is an entry-level field, and
  // offering it here would be inventing a key the parser rejects.
  assert.ok(!got.includes("signal_type"), got.join(","));
});

test("a free-form mapping descends into its value schema", () => {
  // `labels:` is a map of arbitrary names to strings, so there are no key
  // completions to offer and the walk must not crash looking for them.
  assert.deepEqual(labels(["scenarios", "[]", "labels", "anything"], "key"), []);
});

test("an unknown path yields nothing rather than guessing", () => {
  assert.deepEqual(labels(["nope"], "key"), []);
  assert.deepEqual(labels(["scenarios", "[]", "generator", "nope", "deeper"], "key"), []);
});

test("completions carry the doc comment and a type hint", () => {
  const rate = schemaCompletions(SCHEMA, ["scenarios", "[]"], "key").find(
    (c) => c.label === "rate"
  );
  assert.ok(rate.info && rate.info.length > 0, "the Rust doc comment should reach the reader");
  assert.ok(rate.detail.includes("number"), rate.detail);
  const generator = schemaCompletions(SCHEMA, ["scenarios", "[]"], "key").find(
    (c) => c.label === "generator"
  );
  // "object" would be a useless thing to say about a 14-branch union.
  assert.match(generator.detail, /\d+ types/);
});

test("a union does not claim requirements that contradict each other", () => {
  // Review #548 W2. `required` is a per-branch fact. Flattened across
  // `generator:`'s fourteen branches it marked 14 of 36 keys required, while
  // no single generator requires more than 5 and the sets are mutually
  // exclusive — `amplitude` (sine) and `baseline` (spike) sat adjacent, both
  // marked required, and only one of them can be.
  //
  // The keys are still offered; only the false claim is dropped.
  const generator = schemaCompletions(SCHEMA, ["scenarios", "[]", "generator"], "key");
  assert.ok(generator.length > 20, "the union is still offered in full");
  assert.equal(
    generator.filter((option) => /required/.test(option.detail)).length,
    0,
    "no key may be marked required while the branch is undecided"
  );
});

test("required fields are marked", () => {
  const signalType = schemaCompletions(SCHEMA, ["scenarios", "[]"], "key").find(
    (c) => c.label === "signal_type"
  );
  assert.match(signalType.detail, /required/);
});

test("results are sorted and free of duplicates", () => {
  const got = labels(["scenarios", "[]", "generator"], "key");
  assert.deepEqual(got, [...got].sort((a, b) => a.localeCompare(b)));
  assert.equal(new Set(got).size, got.length);
});

test("a malformed schema yields nothing rather than throwing", () => {
  assert.deepEqual(schemaCompletions(null, [], "key"), []);
  assert.deepEqual(schemaCompletions({}, ["a"], "key"), []);
  assert.deepEqual(schemaCompletions({ $ref: "#/nope" }, [], "key"), []);
});

test("a $ref cycle terminates instead of hanging the editor", () => {
  // A schema is data. The scenario schema is not recursive today, but an
  // upstream change that made it so must not turn a keystroke into a hang.
  const cyclic = { $defs: { a: { $ref: "#/$defs/b" }, b: { $ref: "#/$defs/a" } }, $ref: "#/$defs/a" };
  assert.deepEqual(schemaCompletions(cyclic, [], "key"), []);
});

console.log(`${passed} pure-helper tests passed`);
