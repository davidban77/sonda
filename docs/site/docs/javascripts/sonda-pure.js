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
    ? `{${labelKeys.map((k) => `${k}="${labels[k]}"`).join(",")}}`
    : "";
  const forSecs = rule.forSecs;

  const firstFiring = evaled.states.indexOf("firing");
  const fired = firstFiring >= 0;
  const offset = entry.offset_secs || 0;
  const firingWithin = fired
    ? niceDeadlineSecs(offset + firstFiring * entry.tick_secs)
    : null;
  const endsResolved = fired && evaled.states[evaled.states.length - 1] !== "firing";

  const ruleLines = [
    "groups:",
    "  - name: sonda-lab",
    "    interval: 5s",
    "    rules:",
    `      - alert: ${alertName}`,
    `        expr: ${entry.name}${selector} ${rule.op} ${rule.threshold}`,
    `        for: ${forSecs}s`,
    "        labels:",
    "          severity: critical",
  ];

  const expectLines = ["expect:", "  alerts:", `    - alert: ${alertName}`, "      labels:", "        severity: critical"];
  for (const key of labelKeys) {
    expectLines.push(`        ${key}: ${labels[key]}`);
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
