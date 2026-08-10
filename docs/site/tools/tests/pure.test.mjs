// Node test harness for the playground's pure helpers (sonda-pure.js).
//
// Run: node docs/site/tools/tests/pure.test.mjs  (from the repo root; any
// cwd works — the import is relative to this file). Zero dependencies; uses
// node's built-in assert and exits non-zero on the first failure. Wired
// into the docs CI workflow, this is the only automated coverage the
// playground JS has — keep every function under test free of DOM/wasm.

import assert from "node:assert/strict";
import {
  buildTestExport,
  defaultThreshold,
  deriveAlertName,
  escapeQuoted,
  evaluate,
  fromBase64Url,
  niceDeadlineSecs,
  toBase64Url,
} from "../../docs/javascripts/sonda-pure.js";

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

console.log(`${passed} pure-helper tests passed`);
