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
  defaultThreshold,
  deriveAlertName,
  escapeQuoted,
  evaluate,
  exportFilename,
  fromBase64Url,
  galleryCardState,
  hashPayloadTooLarge,
  MAX_SCHEDULE_WINDOWS,
  niceDeadlineSecs,
  normalizeFence,
  numberSpanAt,
  runnableScenario,
  scheduleWindows,
  scrubNumber,
  toBase64Url,
} from "../../docs/javascripts/sonda-pure.js";
import { WIDGETS, cornerParams, defaultParams, sweepParams } from "../../docs/javascripts/livegen-presets.js";

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
    rule: { op: ">", threshold: 64, forSecs: 12 },
    evaled: { states, resolves: [200] },
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
    rule: { op: ">", threshold: 64, forSecs: 12 },
    evaled: { states, resolves: [] },
  });
  assert.doesNotMatch(out, /resolves_within:/);
  assert.match(out, /resolution not asserted/);
});

test("export flags a rule that never fired instead of inventing a deadline", () => {
  const out = buildTestExport({
    yaml: YAML,
    entry: ENTRY,
    rule: { op: ">", threshold: 9999, forSecs: 12 },
    evaled: { states: Array(80).fill("inactive"), resolves: [] },
  });
  assert.match(out, /never fired in the sampled preview/);
});

test("export comments the expect block when the scenario already has one", () => {
  const out = buildTestExport({
    yaml: "version: 2\nexpect:\n  alerts: []\n",
    entry: ENTRY,
    rule: { op: ">", threshold: 64, forSecs: 12 },
    evaled: { states: Array(40).fill("firing"), resolves: [] },
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
    rule: { op: ">", threshold: 64, forSecs: 12 },
    evaled: { states, resolves: [] },
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
    const controls = widget.sliders.length + (widget.choices || []).length;
    assert.ok(controls >= 2 && controls <= 3, `${gen}: 2-3 controls, got ${controls}`);
    for (const s of widget.sliders) {
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
    const baseline = widget.sliders.find((s) => s.key === "baseline");
    const ceiling = widget.sliders.find((s) => s.key === "ceiling");
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

test("preset sampling stays within the playground tick budget", () => {
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    assert.ok(widget.rate * widget.durationSecs <= 240, `${gen}: rate*duration must fit MAX_TICKS`);
  }
});

test("corner and sweep enumeration cover what the compile gate feeds the engine", () => {
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    const corners = cornerParams(widget);
    const expected = (widget.choices || []).reduce(
      (n, c) => n * c.options.length,
      widget.sliders.reduce((n, s) => n * new Set([s.min, s.value, s.max]).size, 1)
    );
    assert.equal(corners.length, expected, `${gen}: {min,default,max} grid`);
    for (const corner of corners) {
      for (const s of widget.sliders) {
        assert.ok(
          [s.min, s.value, s.max].includes(corner[s.key]),
          `${gen}: corners use range edges or the default`
        );
      }
      // Every corner must produce a single-scenario YAML mentioning the type.
      assert.match(widget.yaml(corner), /signal_type: metrics/);
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
  assert.equal(windows.length, MAX_SCHEDULE_WINDOWS);
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

console.log(`${passed} pure-helper tests passed`);
