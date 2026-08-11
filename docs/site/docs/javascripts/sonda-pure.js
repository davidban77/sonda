/* Sonda docs — pure helpers shared by the playground and the alert lab.
 *
 * Nothing in this module touches the DOM, the wasm engine, or the clock:
 * every export is a plain function of its arguments. That is a load-bearing
 * property — docs/site/tools/tests/pure.test.mjs imports this file in node
 * and exercises the case tables in CI, which is the only automated coverage
 * the playground JS has.
 */

/* URL-safe base64 for the #yaml= hash. Encodes through TextEncoder so
 * arbitrary user-typed YAML (accents, CJK) round-trips — a naive btoa()
 * throws on anything outside latin-1. */
export function toBase64Url(text) {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  bytes.forEach((b) => (binary += String.fromCharCode(b)));
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromBase64Url(encoded) {
  const base64 = encoded.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64);
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/* Ceiling on a `#yaml=` hash payload, in characters of the encoded form.
 *
 * The playground and the alert lab will compile anything a link hands them.
 * That is the point — but a link is attacker-supplied input, and an
 * unbounded one buys a decode plus a compile of arbitrary size on page load.
 * 32 KB of base64url is ~24 KB of YAML: far above any real scenario (the
 * largest fence in the docs is under 2 KB) and far below a payload that can
 * wedge the tab.
 */
export const MAX_HASH_PAYLOAD = 32 * 1024;

/* True when a hash payload is too large to be worth decoding.
 *
 * A pure length test on the RAW payload, deliberately: the guard has to run
 * before `fromBase64Url` allocates, so it cannot ask how big the decoded
 * text would be. */
export function hashPayloadTooLarge(payload) {
  return String(payload).length > MAX_HASH_PAYLOAD;
}

/* Normalize a code fence's body before any structural test runs on it.
 *
 * The two callers see the same scenario through different lenses: the browser
 * reads `code.textContent` out of the rendered DOM, while the CI extractor
 * reads raw markdown where an admonition- or tab-nested fence carries four
 * spaces of indentation on every line. Folding both to one shape here is what
 * lets `runnableScenario` be a single rule with a single case table
 * (docs/site/tools/tests/runnable-cases.json) rather than two implementations
 * that drift.
 *
 * Strips a UTF-8 BOM, folds CRLF (a fence pasted from a Windows editor must
 * not read as a different document), and removes the indentation common to
 * every non-blank line.
 */
export function normalizeFence(text) {
  const body = String(text).replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");
  const lines = body.split("\n");
  let common = Infinity;
  for (const line of lines) {
    if (!line.trim()) continue; // blank lines carry no indentation signal
    const indent = line.length - line.replace(/^[ \t]+/, "").length;
    if (indent < common) common = indent;
  }
  if (!Number.isFinite(common) || common === 0) return body;
  return lines.map((line) => (line.trim() ? line.slice(common) : line)).join("\n");
}

/* True when a docs code fence is a complete, runnable Sonda scenario — the
 * one rule behind both the "Run in playground →" buttons and the CI gate that
 * compiles every buttoned fence.
 *
 * A fence qualifies when it declares `version: 2` (the engine rejects a
 * scenario file without it — see ParseError::InvalidVersion, so this is the
 * honest complete-vs-fragment line) AND carries a `scenarios:` list or the
 * `kind:` shorthand header. Fragments — a bare `generator:` block quoted to
 * explain one field — fail naturally and are never offered.
 *
 * A `pack:` reference disqualifies a fence no matter how complete it looks.
 * Packs resolve against a catalog directory, and there is no catalog in the
 * browser — sonda-wasm builds an empty InMemoryPackResolver, so a
 * pack-backed scenario reaches the playground only to report "unknown pack".
 * Offering a button that always lands on an error is worse than offering
 * none, and the CI gate skips them for the same reason (`--catalog <dir>` is
 * a runtime argument a fence cannot carry). The match is indentation-
 * tolerant because `pack:` sits inside an entry; the cost is that a label
 * literally keyed `pack` also opts a fence out, which only ever loses a
 * button.
 *
 * The escape hatch is a `# sonda:static` comment line, visible in both the
 * markdown source and the rendered page: it opts a fence out for cases where
 * running it is not the point (a deliberately broken example whose error
 * message IS the lesson, or a server-only shape the CLI cannot run).
 *
 * Anchors are line-local and use [ \t] rather than \s throughout: \s spans
 * newlines, which would let `version:` on one line and `2` on the next read
 * as a version declaration.
 */
export function runnableScenario(text) {
  const body = normalizeFence(text);
  if (/^#[ \t]*sonda:static\b/m.test(body)) return false;
  if (!/^version:[ \t]*2[ \t]*$/m.test(body)) return false;
  if (/^[ \t]*(?:-[ \t]+)?pack:/m.test(body)) return false;
  return /^scenarios:/m.test(body) || /^kind:/m.test(body);
}

/* Build a download filename from a sampled scenario.
 *
 * The name comes from the first entry, so a downloaded file is recognisable
 * as the thing that was on screen — but an entry name is engine-validated,
 * not filesystem-validated, and the two agree on much less than you would
 * hope. `cpu usage %`, `../etc/passwd`, `.bashrc` and `東京` are all names the
 * engine accepts and none of them may reach a Save dialog intact.
 *
 * So the stem is rebuilt rather than escaped: lowercase, every character
 * outside [a-z0-9_-] becomes a hyphen (runs collapsing to one), repeated
 * hyphens collapse, leading and trailing hyphens go, and the result is capped
 * at 40 characters. Path separators and dots cannot survive that — `../x`
 * reduces to `x` — so the return value is always a single bare filename with
 * exactly one extension, never a path and never a dotfile.
 *
 * A name that sanitizes to nothing (all-CJK, punctuation-only, empty, or no
 * entries at all) falls back to "scenario" rather than producing a file
 * called ".yaml".
 */
export function exportFilename(entries, ext) {
  const first = Array.isArray(entries) && entries.length ? entries[0] : null;
  let stem = String((first && first.name) || "")
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^-+|-+$/g, "");
  if (stem.length > 40) stem = stem.slice(0, 40).replace(/-+$/g, "");
  if (!stem) stem = "scenario";
  const suffix = String(ext || "").replace(/^\.+/, "");
  return suffix ? `${stem}.${suffix}` : stem;
}

/* Round to two leading digits; guards float dust like 60.000000000000004. */
export function tidyNumber(value) {
  const magnitude = Math.pow(10, Math.floor(Math.log10(Math.abs(value))) - 1);
  return Number((Math.round(value / magnitude) * magnitude).toPrecision(3));
}

/* Starting threshold for a scenario the lab has never seen: a value the
 * signal actually crosses (60% up its range), rounded to a tidy number so
 * the input reads like something a human chose. A flat series has no range
 * to cross, so the threshold seats just below the value instead — `>`
 * fires immediately and tuning starts from a live alert rather than one
 * that provably cannot fire (constant scenarios are common bridge
 * traffic). */
export function defaultThreshold(values) {
  const min = Math.min(...values);
  const max = Math.max(...values);
  if (!Number.isFinite(min) || !Number.isFinite(max)) return 1;
  if (max - min < Math.max(1e-9, Math.abs(max) * 1e-9)) {
    if (max === 0) return -1;
    return tidyNumber(max > 0 ? max * 0.9 : max * 1.1);
  }
  const mid = min + (max - min) * 0.6;
  if (mid === 0) return 0;
  return tidyNumber(mid);
}

/* Walk the series the way a Prometheus rule walks scrape samples: pending
 * while the condition holds but hasn't lasted `for:` yet, firing once it
 * has, back to inactive (recording a resolve) when it goes false. */
export function evaluate(values, tickSecs, op, threshold, forSecs) {
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

/* Escape a label value for interpolation into a double-quoted context.
 * PromQL string literals and YAML double-quoted scalars share these rules
 * for the characters that matter here: backslash and quote must be
 * escaped, newlines and tabs become escape sequences. Label KEYS need no
 * treatment — they reached this code through the engine's label
 * validation, so they match the Prometheus name charset; VALUES are
 * arbitrary user strings and always pass through here (review #532:
 * a numeric or empty value emitted bare breaks the generated YAML, and a
 * quote breaks the generated PromQL). */
export function escapeQuoted(value) {
  return String(value)
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .replace(/\t/g, "\\t");
}

/* Find the scrubbable numeric literal at `column` in a line of scenario
 * YAML, or null. Powers the editor's drag-to-scrub gesture, so eligibility
 * is deliberately narrow — a literal is scrubbable only when it stands
 * alone as (part of) a YAML scalar value:
 *
 *   - preceded by start-of-value punctuation (space, `:`, `,`, `{`, `[`)
 *     — rejects digits embedded in words (`web-01`, `checkout-7d4f9`);
 *   - followed by end-of-value punctuation or a bare duration suffix
 *     (`60s`, `1.5h`, `250ms`) — rejects dotted quads (`10.0.0.2`) and
 *     scientific notation;
 *   - not inside a quoted string — a number in a `message:` template is
 *     prose, not a parameter (review #533 W1: the boundary checks alone
 *     accept `"Request took 250 ms"` because the digits have spaces on
 *     both sides INSIDE the string; an odd count of unescaped quotes
 *     before the span means the span is inside one) — and not in a
 *     comment;
 *   - not the `version:` key, where +1 means a different config schema,
 *     not a bigger signal.
 */
export function numberSpanAt(lineText, column) {
  if (/^\s*version:/.test(lineText)) return null;
  const hash = lineText.indexOf("#");
  const insideQuotes = (upto) => {
    let doubles = 0;
    let singles = 0;
    for (let i = 0; i < upto; i++) {
      const c = lineText[i];
      if (c === '"' && lineText[i - 1] !== "\\") doubles += 1;
      else if (c === "'") singles += 1; // YAML escapes '' — pairs keep parity
    }
    return doubles % 2 === 1 || singles % 2 === 1;
  };
  const pattern = /-?\d+(?:\.\d+)?/g;
  let match;
  while ((match = pattern.exec(lineText)) !== null) {
    const start = match.index;
    const end = start + match[0].length;
    if (column < start) return null; // matches advance left-to-right
    if (column > end) continue;
    if (hash !== -1 && start > hash) return null;
    if (insideQuotes(start)) continue;
    const before = start === 0 ? "" : lineText[start - 1];
    if (!/^[ \t:,{[]?$/.test(before)) continue;
    const after = lineText.slice(end);
    if (after !== "" && !/^[ \t,}\]]/.test(after) && !/^(?:ms|[smh])(?:$|[ \t,}\]])/.test(after)) {
      continue;
    }
    return { start, end, text: match[0] };
  }
  return null;
}

/* Move a numeric literal by `steps` scrub increments, preserving its
 * decimal format. The step is fixed by the ORIGINAL text for the whole
 * gesture (no acceleration): one decimal place for floats, whole numbers
 * for integers, scaled up one order below the value's own magnitude so
 * `120` steps by 10 while `4` steps by 1 and `0.004` by 0.001. */
export function scrubNumber(text, steps) {
  const value = Number(text);
  const dot = text.indexOf(".");
  const decimals = dot === -1 ? 0 : text.length - dot - 1;
  const fine = Math.pow(10, -decimals);
  const magnitude = Math.abs(value);
  const coarse = magnitude > 0 ? Math.pow(10, Math.floor(Math.log10(magnitude)) - 1) : fine;
  const step = Math.max(fine, coarse);
  const out = (value + steps * step).toFixed(decimals);
  // toFixed goes exponential at >= 1e21 (review #533 M2) — a scrub must
  // never replace a plain literal with something that isn't one, so past
  // that point the gesture pins rather than corrupts.
  return /^-?\d+(?:\.\d+)?$/.test(out) ? out : text;
}

/* PascalCase alert name from a metric name and a direction:
 * cpu_usage + ">" → CpuUsageHigh. */
export function deriveAlertName(metricName, op) {
  const words = String(metricName)
    .split(/[^a-zA-Z0-9]+/)
    .filter(Boolean)
    .map((w) => w[0].toUpperCase() + w.slice(1));
  const base = words.join("") || "LabAlert";
  const prefixed = /^[a-zA-Z]/.test(base) ? base : `Lab${base}`;
  return prefixed + (op === ">" ? "High" : "Low");
}

/* Round a deadline up to a human-looking number of seconds. */
export function niceDeadlineSecs(secs) {
  const padded = secs * 1.25 + 10;
  const steps = [15, 30, 45, 60, 90, 120, 180, 300, 600];
  for (const step of steps) if (padded <= step) return step;
  return Math.ceil(padded / 60) * 60;
}

/* Build the "run this for real" export: one clipboard-ready file — the
 * scenario YAML with an appended, label-scoped `expect:` block, headed by a
 * comment carrying the matching vmalert/Prometheus rule. Deadlines come
 * from the evaluated preview timeline, so the exported expectation is one
 * the tuned rule demonstrably meets. */
export function buildTestExport({ yaml, entry, rule, evaled }) {
  const alertName = deriveAlertName(entry.name, rule.op);
  const labels = entry.labels || {};
  const labelKeys = Object.keys(labels).sort();
  const selector = labelKeys.length
    ? `{${labelKeys.map((k) => `${k}="${escapeQuoted(labels[k])}"`).join(",")}}`
    : "";
  const forSecs = rule.forSecs;

  const firstFiring = evaled.states.indexOf("firing");
  const fired = firstFiring >= 0;
  const offset = entry.offset_secs || 0;
  const firingWithin = fired
    ? niceDeadlineSecs(offset + firstFiring * entry.tick_secs)
    : null;
  const endsResolved = fired && evaled.states[evaled.states.length - 1] !== "firing";

  // The expr is a YAML scalar containing PromQL: single-quote it at the
  // YAML layer (apostrophes doubled, backslashes untouched) so PromQL's
  // own escapes survive and label values with `: ` cannot break the rules
  // file. escapeQuoted already handled the PromQL string-literal layer.
  const expr = `${entry.name}${selector} ${rule.op} ${rule.threshold}`;
  const ruleLines = [
    "groups:",
    "  - name: sonda-lab",
    "    interval: 5s",
    "    rules:",
    `      - alert: ${alertName}`,
    `        expr: '${expr.replace(/'/g, "''")}'`,
    `        for: ${forSecs}s`,
    "        labels:",
    "          severity: critical",
  ];

  const expectLines = ["expect:", "  alerts:", `    - alert: ${alertName}`, "      labels:", "        severity: critical"];
  for (const key of labelKeys) {
    // Always double-quoted: a bare numeric, boolean, `{`, `*`, colon-space
    // or empty value is invalid (or worse, retyped) YAML.
    expectLines.push(`        ${key}: "${escapeQuoted(labels[key])}"`);
  }
  expectLines.push(
    `      firing_within: ${firingWithin !== null ? `${firingWithin}s` : "60s # the rule never fired in the sampled preview — tune it in the lab first"}`
  );
  if (endsResolved) {
    expectLines.push("      resolves_within: 2m");
  } else if (fired) {
    expectLines.push("      # still firing when the scenario ends — resolution not asserted");
  }

  const hasExpect = /^expect:/m.test(yaml);
  const body = hasExpect
    ? `${yaml.trimEnd()}\n\n# NOTE: this scenario already has an expect: block — merge the one below by hand.\n${expectLines.map((l) => `# ${l}`).join("\n")}\n`
    : `${yaml.trimEnd()}\n\n# Scope note: expect.labels pins this scenario's own labels — ALERTS is\n# global, and an unscoped expectation can match alerts other series caused.\n${expectLines.join("\n")}\n`;

  return (
    `# Generated by the Sonda alert lab — ${entry.name}${selector} ${rule.op} ${rule.threshold} for ${forSecs}s\n` +
    `#\n` +
    `# 1) Alert rule (vmalert / Prometheus) — add to your rules file:\n` +
    ruleLines.map((l) => `#    ${l}`).join("\n") +
    `\n#\n` +
    `# 2) This file — save as lab-scenario.yaml and run:\n` +
    `#\n` +
    `#    sonda test lab-scenario.yaml --prometheus-url http://localhost:8428\n` +
    `\n${body}`
  );
}
