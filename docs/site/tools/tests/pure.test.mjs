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
  defaultThreshold,
  deriveAlertName,
  escapeQuoted,
  evaluate,
  exportFilename,
  fromBase64Url,
  hashPayloadTooLarge,
  niceDeadlineSecs,
  normalizeFence,
  numberSpanAt,
  runnableScenario,
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

test("widget sliders are well-formed and defaults sit inside their range", () => {
  for (const [gen, widget] of Object.entries(WIDGETS)) {
    assert.ok(widget.sliders.length >= 2 && widget.sliders.length <= 3, `${gen}: 2-3 sliders`);
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
    const expected = widget.sliders.reduce(
      (n, s) => n * new Set([s.min, s.value, s.max]).size,
      1
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

console.log(`${passed} pure-helper tests passed`);
