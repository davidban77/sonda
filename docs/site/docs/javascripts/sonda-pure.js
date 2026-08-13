/* Sonda docs — pure helpers shared by the playground and the alert lab.
 *
 * Nothing in this module touches the DOM, the wasm engine, or the clock:
 * every export is a plain function of its arguments. That is a load-bearing
 * property — docs/site/tools/tests/pure.test.mjs imports this file in node
 * and exercises the case tables in CI, which is the only automated coverage
 * the playground JS has.
 *
 * ONE CONSTRAINT THAT IS NOT OBVIOUS FROM HERE: this module is bundled into
 * the CodeMirror editor (docs/site/tools/editor/src imports `numberSpanAt`
 * and `scrubNumber`), and that bundle is committed under a byte-exact drift
 * gate. Exported function declarations tree-shake cleanly, so adding one
 * costs the editor nothing — but a module-level `new RegExp(...)`, `new
 * Set(...)` or any other constructor call does NOT, because esbuild cannot
 * prove it is side-effect-free. Such a value is compiled into a 494 KB bundle
 * that never runs it, and turns the editor's gate red on a change that has
 * nothing to do with the editor. Mark them `/* @__PURE__ *\/` — see the
 * PromQL parser at the end of this file, which is where this was learned.
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
 * `kind: runnable` shorthand header. Fragments — a bare `generator:` block
 * quoted to explain one field — fail naturally and are never offered.
 *
 * The `kind:` match is pinned to `runnable` rather than accepting any value,
 * because the other value the engine takes is `composable`: a metric pack,
 * which declares `version: 2` and `kind:` and looks complete to a looser
 * rule. A pack is a library of metric definitions, not something to run.
 *
 * This is not hypothetical and the compile gate could not have caught it:
 * `sonda --dry-run run` ACCEPTS a pack file and emits nothing, so the pack
 * fence on catalogs-and-packs.md shipped with a "Run in playground →" button
 * that led to an empty chart. Measured through the same engine the button
 * hands the reader to: a pack samples to `ok: true` with every output array
 * empty — no entries, no histograms, no summaries, no logs, nothing skipped.
 * A green gate and a broken promise at the same time, which is the whole
 * reason the detector has to carry this rule rather than lean on the gate.
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
  return /^scenarios:/m.test(body) || /^kind:[ \t]*runnable[ \t]*$/m.test(body);
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

/* The `sonda test` setup for one or more lab rules, as a copyable document.
 *
 * `rules` is an array of `{ severity, op, threshold, forSecs, evaled }` —
 * plural since WP12, because a warning/critical pair is how these are written
 * in practice and exporting only one of them would hand a reader half their
 * alerting policy. A single-rule export is byte-identical to what this
 * produced before the pair existed, so the common case did not get noisier to
 * make room for the uncommon one.
 *
 * Each rule carries its OWN `evaled`, because the deadlines are per-rule: a
 * warning at 60 fires earlier than a critical at 90 on the same series, and
 * asserting the critical's timing against the warning's states would produce
 * an expectation that passes for the wrong reason.
 */
export function buildTestExport({ yaml, entry, rules }) {
  const labels = entry.labels || {};
  const labelKeys = Object.keys(labels).sort();
  const selector = labelKeys.length
    ? `{${labelKeys.map((k) => `${k}="${escapeQuoted(labels[k])}"`).join(",")}}`
    : "";
  const offset = entry.offset_secs || 0;
  const multiple = rules.length > 1;

  // Both severity dropdowns offer BOTH severities, so a pair can be two
  // criticals at different `for:` durations — a real pattern, and two clicks
  // away. Suffixing by severity alone then produces two identical names with
  // identical labels in one group, which is exactly the rules file the suffix
  // exists to prevent (review #546 W2). When the severities collide the
  // suffix falls back to the row's position, which cannot.
  const severities = rules.map((rule) => rule.severity || "critical");
  const severityIsUnique = new Set(severities).size === severities.length;

  const prepared = rules.map((rule, index) => {
    const severity = severities[index];
    // The suffix only appears when it has to. With one rule the name is what
    // it always was; with two, two alerts sharing a name in one group is a
    // rules file that does not mean what it looks like.
    const suffix = !multiple
      ? ""
      : severityIsUnique
        ? severity.charAt(0).toUpperCase() + severity.slice(1)
        : `Rule${index + 1}`;
    const alertName = deriveAlertName(entry.name, rule.op) + suffix;
    const states = (rule.evaled && rule.evaled.states) || [];
    const firstFiring = states.indexOf("firing");
    const fired = firstFiring >= 0;
    return {
      severity,
      alertName,
      op: rule.op,
      threshold: rule.threshold,
      forSecs: rule.forSecs,
      fired,
      firingWithin: fired ? niceDeadlineSecs(offset + firstFiring * entry.tick_secs) : null,
      endsResolved: fired && states[states.length - 1] !== "firing",
      expr: `${entry.name}${selector} ${rule.op} ${rule.threshold}`,
    };
  });

  const ruleLines = ["groups:", "  - name: sonda-lab", "    interval: 5s", "    rules:"];
  for (const rule of prepared) {
    ruleLines.push(
      `      - alert: ${rule.alertName}`,
      // The expr is a YAML scalar containing PromQL: single-quote it at the
      // YAML layer (apostrophes doubled, backslashes untouched) so PromQL's
      // own escapes survive and label values with `: ` cannot break the
      // rules file. escapeQuoted already handled the PromQL literal layer.
      `        expr: '${rule.expr.replace(/'/g, "''")}'`,
      `        for: ${rule.forSecs}s`,
      "        labels:",
      `          severity: ${rule.severity}`
    );
  }

  const expectLines = ["expect:", "  alerts:"];
  for (const rule of prepared) {
    expectLines.push(`    - alert: ${rule.alertName}`, "      labels:", `        severity: ${rule.severity}`);
    for (const key of labelKeys) {
      // Always double-quoted: a bare numeric, boolean, `{`, `*`, colon-space
      // or empty value is invalid (or worse, retyped) YAML.
      expectLines.push(`        ${key}: "${escapeQuoted(labels[key])}"`);
    }
    expectLines.push(
      `      firing_within: ${rule.firingWithin !== null ? `${rule.firingWithin}s` : "60s # the rule never fired in the sampled preview — tune it in the lab first"}`
    );
    if (rule.endsResolved) {
      expectLines.push("      resolves_within: 2m");
    } else if (rule.fired) {
      expectLines.push("      # still firing when the scenario ends — resolution not asserted");
    }
  }

  const hasExpect = /^expect:/m.test(yaml);
  const body = hasExpect
    ? `${yaml.trimEnd()}\n\n# NOTE: this scenario already has an expect: block — merge the one below by hand.\n${expectLines.map((l) => `# ${l}`).join("\n")}\n`
    : `${yaml.trimEnd()}\n\n# Scope note: expect.labels pins this scenario's own labels — ALERTS is\n# global, and an unscoped expectation can match alerts other series caused.\n${expectLines.join("\n")}\n`;

  // The severity is named only when there is more than one rule, for the same
  // reason the alert-name suffix is: a single-rule export has to stay
  // byte-identical to what it was before pairs existed. The docstring claims
  // that, and review #546 W4 measured the claim false on exactly this line —
  // the one place `multiple` had not been applied.
  const headline = prepared
    .map(
      (r) =>
        `${entry.name}${selector} ${r.op} ${r.threshold} for ${r.forSecs}s` +
        (multiple ? ` (${r.severity})` : "")
    )
    .join("\n#   ");

  return (
    `# Generated by the Sonda alert lab — ${headline}\n` +
    `#\n` +
    `# 1) Alert rule${multiple ? "s" : ""} (vmalert / Prometheus) — add to your rules file:\n` +
    ruleLines.map((l) => `#    ${l}`).join("\n") +
    `\n#\n` +
    `# 2) This file — save as lab-scenario.yaml and run:\n` +
    `#\n` +
    `#    sonda test lab-scenario.yaml --prometheus-url http://localhost:8428\n` +
    `\n${body}`
  );
}

/* Decide what a gallery card should show for one sampled scenario.
 *
 * The examples gallery (test/examples.md) mounts a widget per example file,
 * and "it charts" is only one of several honest outcomes. The engine reports
 * the rest itself, in the sample result, and this function is the single
 * place that reads it — so a card never has to guess and never shows an empty
 * chart in place of an explanation.
 *
 * The five outcomes, all measured against real files in `examples/`:
 *
 *   chart      `entries` is non-empty — a metrics series to draw. 45 of the
 *              carded examples land here.
 *   note       the scenario samples cleanly but has nothing line-chartable:
 *              logs (`logs`), histograms (`histograms`), summaries
 *              (`summaries`). The playground renders all three; a 150px
 *              sparkline cannot, so the card says which it is and links on.
 *   skipped    the engine could not build an entry and SAID WHY — every
 *              csv_replay example reaches this, because a browser has no
 *              file to replay. `ok` is still true, so a naive card would
 *              show a blank chart and call it success. The engine's own
 *              reason is surfaced verbatim.
 *   empty      `ok` with nothing at all in any output array. A `kind:
 *              composable` pack does this (see runnableScenario above), and
 *              so would a future shape nobody anticipated.
 *   error      `ok` is false, or the result is not the shape we expect.
 *
 * Order is load-bearing: `entries` is checked before everything, because a
 * mixed metrics+logs scenario has both and the chart is the better card. And
 * `skipped` is checked before the empty fallback so the specific reason wins
 * over the generic one.
 */
export function galleryCardState(result) {
  if (!result || typeof result !== "object") {
    return { mode: "error", message: "no result from the engine" };
  }
  if (result.ok !== true) {
    return { mode: "error", message: nonEmptyString(result.error) || "compile error" };
  }

  const entries = arrayOf(result.entries);
  if (entries.length) {
    return { mode: "chart", extraSeries: entries.length - 1 };
  }

  const skipped = arrayOf(result.skipped);
  if (skipped.length) {
    // The engine's sentence stands alone rather than being introduced. Every
    // skip reason it emits already names the limitation and what to do about
    // it ("metric csv_replay reads a file — no filesystem in the browser; run
    // it locally with `sonda run`"), so a "Not sampled in the browser —"
    // prefix only says "browser" a second time. The fallback below is for a
    // reason the engine did not give, which is the one case a card has to
    // speak for itself.
    const reason = nonEmptyString(skipped[0] && skipped[0].reason);
    return {
      mode: "skipped",
      message: reason || "Not sampled in the browser.",
    };
  }

  if (arrayOf(result.logs).length) {
    return { mode: "note", message: "Log stream — open it in the playground to read the lines." };
  }
  if (arrayOf(result.histograms).length) {
    return { mode: "note", message: "Histogram — open it in the playground for the bucket heatmap." };
  }
  if (arrayOf(result.summaries).length) {
    return { mode: "note", message: "Summary — open it in the playground for the quantile bands." };
  }

  return { mode: "empty", message: "Nothing to sample — this file defines metrics for other scenarios to use." };
}

function arrayOf(value) {
  return Array.isArray(value) ? value : [];
}

function nonEmptyString(value) {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

/* Ceiling on how many CYCLES one entry's schedule may be walked.
 *
 * A 30-second scenario with `every: 1s` is 30 cycles; anything approaching
 * this is already unreadable as shading. The bound is on the loop, not on
 * the output, and that distinction is the whole point (review #543 W1): an
 * earlier version capped `windows.length`, which cannot fire when the loop
 * pushes nothing — and a loop that pushes nothing is exactly the shape that
 * hangs. `offset 1e6, every 1e-13, for 1e-13` is entirely finite and
 * positive, but floating point makes `start + for === start`, so no window
 * is ever produced and the counter never advances.
 *
 * Bounding the loop is total over every input shape, including ones nobody
 * has thought of yet; bounding the output only covers the degenerate values
 * someone remembered. This is recomputed on every theme flip and every
 * resize, so a hang here is a wedged tab.
 *
 * Named for CYCLES because that is what it counts, and the returned array is
 * NOT bounded by it (review #543 N1): the loop runs once per kind, so an
 * entry carrying both a burst and a gap can return up to twice this many
 * windows. That needs sub-second periods on both, and 1024 rects is nothing
 * to draw — the point of saying so is that the old name promised an output
 * bound this deliberately does not provide.
 */
export const MAX_SCHEDULE_CYCLES = 512;

/* The gap and burst windows of one sampled entry, in seconds.
 *
 * Shared by the playground's full chart and the docs widgets' mini-chart, so
 * the shading means the same thing in both places. Returns
 * `[{ kind: "gap" | "burst", start, end }]` in draw order, already clipped to
 * `[offset, endSecs]` — a caller only has to map seconds to pixels.
 *
 * The engine's semantics, which this mirrors: windows are relative to each
 * scenario's own start, so they shift by the entry's offset; BURSTS occupy the
 * head of each cycle and GAPS the tail. That asymmetry is not cosmetic — a
 * burst begins when the cycle begins, while a gap is the silence at the end of
 * one, and drawing either in the other's place would misreport when the
 * signal actually stops.
 *
 * Every degenerate input a slider can reach is answered here rather than at
 * the call sites:
 *
 *   every <= 0 or non-finite   no windows. A cycle that never advances is not
 *                              a schedule.
 *   for <= 0                   no windows: a zero-length window shades
 *                              nothing.
 *   for >= every               windows would run into each other; each is
 *                              clipped to its own cycle so the shading stays
 *                              a cycle-by-cycle statement rather than one
 *                              undifferentiated block.
 *   offset non-finite          no windows. `-Infinity` is TRUTHY, so the
 *                              `|| 0` fallback lets it through and every
 *                              cycle then starts at -Infinity, never
 *                              reaching the end. `-1e309` is the nastier
 *                              spelling: nothing about that literal looks
 *                              non-finite, and it overflows on the way in.
 *   offset >= endSecs          no windows — the series ends before it starts.
 *
 * None of those guards is the backstop, though. The loop itself is bounded
 * by MAX_SCHEDULE_CYCLES cycles, which is what makes this function total
 * for inputs no guard anticipates.
 */
export function scheduleWindows(entry, endSecs) {
  if (!entry || typeof entry !== "object") return [];
  const end = Number(endSecs);
  const offset = Number(entry.offset_secs) || 0;
  if (!Number.isFinite(end) || !Number.isFinite(offset) || end <= offset) return [];

  const windows = [];
  for (const [kind, window] of [
    ["burst", entry.burst],
    ["gap", entry.gap],
  ]) {
    if (!window) continue;
    const every = Number(window.every_secs);
    const forSecs = Number(window.for_secs);
    if (!Number.isFinite(every) || every <= 0) continue;
    if (!Number.isFinite(forSecs) || forSecs <= 0) continue;

    // Bounded by CYCLES. See MAX_SCHEDULE_CYCLES above for why the count of
    // emitted windows is the wrong thing to bound.
    for (let cycle = 0; cycle < MAX_SCHEDULE_CYCLES; cycle++) {
      const cycleStart = offset + cycle * every;
      if (cycleStart >= end) break;
      // A burst opens its cycle; a gap closes it.
      const start = kind === "burst" ? cycleStart : cycleStart + Math.max(0, every - forSecs);
      if (start >= end) break;
      // Clipped to the cycle as well as to the series: `for` longer than
      // `every` otherwise paints one continuous band and hides the period.
      const windowEnd = Math.min(start + forSecs, cycleStart + every, end);
      if (windowEnd > start) windows.push({ kind, start, end: windowEnd });
    }
  }
  return windows;
}

/* What a burst does to the emission rate, as a label for the shaded band.
 *
 * A burst is the one schedule setting the trace cannot show (review #543 B1).
 * `every` and `for` move the shading; `multiplier` moves nothing, because the
 * chart plots the metric's VALUE and a burst does not change the value — it
 * changes how often that value is emitted. The engine's rule is
 * `interval = base_interval / multiplier` (sonda-core/src/schedule/core_loop.rs),
 * so the burst emits `rate * multiplier` events per second while the band is
 * open.
 *
 * Both numbers come back from the compiler, not from the slider: `rate` is the
 * entry's resolved rate and `multiplier` is the parsed burst window, so a
 * value the engine rejected never reaches this label — the widget shows the
 * compile error instead. That is the distinction between a readout and a
 * decoration that echoes the input.
 *
 * Returns `null` when there is no burst, or when either number is one the
 * label would be a lie about: a non-positive or non-finite rate or multiplier
 * means the band has no emission rate to report.
 */
export function burstEmission(entry) {
  if (!entry || typeof entry !== "object") return null;
  const burst = entry.burst;
  if (!burst || typeof burst !== "object") return null;
  const base = Number(entry.rate);
  const multiplier = Number(burst.multiplier);
  if (!Number.isFinite(base) || base <= 0) return null;
  if (!Number.isFinite(multiplier) || multiplier <= 0) return null;
  const during = base * multiplier;
  if (!Number.isFinite(during)) return null;
  return {
    base,
    during,
    multiplier,
    // Both ends, so the label carries its own comparison: at multiplier 1 it
    // reads "4/s → 4/s", which is the honest answer to "what does ×1 do?".
    label: `${tidyNumber(base)}/s → ${tidyNumber(during)}/s`,
  };
}

/* ---- the time cursor (WP9) -------------------------------------------
 *
 * Three plain functions between a pointer position and what the page should
 * say about it. The chart's own geometry stays in playground.js; what lives
 * here is every decision that can be wrong: where the cursor lands in
 * scenario seconds, which sample each series was actually emitting then, and
 * which log lines belong to that instant.
 */

/* Where a pointer sits on the chart, in scenario seconds — or null.
 *
 * `geom` is the mapping drawChart already computed: `{ padLeft, plotW,
 * spanSecs }`. The chart's forward map is
 * `secs -> padLeft + (secs / spanSecs) * plotW`, so this is its inverse.
 *
 * Returns null rather than a clamped value when the pointer is outside the
 * plot area — the axis gutter is not second zero, and a cursor pinned to the
 * left edge whenever the pointer strays into the y-axis labels would report a
 * reading the reader never asked for. Callers treat null as "no cursor",
 * which is also what a pointer leaving the canvas produces, so the two paths
 * agree by construction.
 */
export function cursorSecsAt(geom, offsetX) {
  if (!geom || typeof geom !== "object") return null;
  const padLeft = Number(geom.padLeft);
  const plotW = Number(geom.plotW);
  const spanSecs = Number(geom.spanSecs);
  const x = Number(offsetX);
  if (!Number.isFinite(padLeft) || !Number.isFinite(x)) return null;
  if (!Number.isFinite(plotW) || plotW <= 0) return null;
  if (!Number.isFinite(spanSecs) || spanSecs <= 0) return null;
  const fraction = (x - padLeft) / plotW;
  if (fraction < 0 || fraction > 1) return null;
  return fraction * spanSecs;
}

/* What each series was emitting at the cursor, as readout rows.
 *
 * Snapped to the nearest TICK rather than interpolated, because the chart is
 * a sampled signal and an interpolated reading would be a number the engine
 * never produced. The row carries the snapped `secs` as well as the value, so
 * the readout can show where it actually read from — at a coarse rate the
 * difference between the pointer and the sample is visible, and hiding it
 * would make the cursor look more precise than it is.
 *
 * An entry contributes NOTHING when the cursor falls outside its own window.
 * Scenarios chain (`after:`), so a 30-second entry starting at second 60 has
 * no value at second 10 — and reporting its first sample there would invent
 * data for a scenario that had not started. That is the case worth the
 * function existing: clamping the index, which is the obvious implementation,
 * gets it exactly wrong.
 */
export function cursorSamples(entries, cursorSecs) {
  const at = Number(cursorSecs);
  if (!Array.isArray(entries) || !Number.isFinite(at)) return [];
  const rows = [];
  for (const entry of entries) {
    if (!entry || typeof entry !== "object") continue;
    const values = entry.values;
    if (!Array.isArray(values) || !values.length) continue;
    const tick = Number(entry.tick_secs);
    if (!Number.isFinite(tick) || tick <= 0) continue;
    const offset = Number(entry.offset_secs) || 0;
    if (!Number.isFinite(offset)) continue;
    const index = Math.round((at - offset) / tick);
    // Outside this entry's own window — not clamped. See above.
    if (index < 0 || index >= values.length) continue;
    const value = Number(values[index]);
    if (!Number.isFinite(value)) continue;
    rows.push({ id: entry.id, name: entry.name, value, secs: offset + index * tick });
  }
  return rows;
}

/* Indices of the log lines belonging to the cursor's instant.
 *
 * Half a tick either side, so every instant on the timeline belongs to
 * exactly one emission — the same rule the chart's nearest-tick snap uses,
 * which is what keeps the highlighted lines and the highlighted sample
 * describing the same moment.
 *
 * `line.secs` is already on the shared timeline (sonda-wasm stamps it as
 * `offset_secs + tick * tick_secs`), so there is deliberately no offset
 * arithmetic here; adding it again would push the highlight off by the
 * entry's start on any chained scenario.
 *
 * The bound is inclusive, so a cursor exactly between two lines highlights
 * both. That is the honest answer at a boundary and it is stable — an
 * exclusive bound would flicker between the two as the pointer moved by
 * sub-pixel amounts.
 */
export function logLinesNear(log, cursorSecs) {
  const at = Number(cursorSecs);
  if (!log || typeof log !== "object" || !Number.isFinite(at)) return [];
  const lines = log.lines;
  if (!Array.isArray(lines) || !lines.length) return [];
  const tick = Number(log.tick_secs);
  if (!Number.isFinite(tick) || tick <= 0) return [];
  const window = tick / 2;
  const hits = [];
  for (let i = 0; i < lines.length; i++) {
    const secs = Number(lines[i] && lines[i].secs);
    if (!Number.isFinite(secs)) continue;
    if (Math.abs(secs - at) <= window) hits.push(i);
  }
  return hits;
}

/* ---- importing a Prometheus rule (WP12) ------------------------------- */

/* Every module-level value below carries `/* @__PURE__ *\/` because
 * sonda-pure.js is BUNDLED INTO THE EDITOR (docs/site/tools/editor/src imports
 * `numberSpanAt` and `scrubNumber` from it), and esbuild cannot prove a
 * constructor call is side-effect-free on its own. Without the annotation the
 * alert lab's PromQL parser is compiled into a 494 KB CodeMirror bundle that
 * never runs it — and, more to the point, the editor's byte-exact drift gate
 * fails on a change that has nothing to do with the editor. That is how this
 * was found: CI red on #546 with a diff full of CodeMirror internals.
 *
 * Plain function declarations tree-shake without help, which is why WP9's
 * additions to this module never showed up in that bundle.
 */

/* The operators the lab can evaluate. `evaluate` implements `>` and `<`;
 * the rest are accepted here and normalized, because a rule written with
 * `>=` is a rule about the same threshold and refusing it would send a
 * reader away to edit text by hand for no reason. */
const _IMPORTABLE_OPS = /* @__PURE__ */ new Set([">", ">=", "<", "<="]);

/* A PromQL instant-vector selector and a scalar comparison, and nothing else:
 *
 *   metric_name{label="value",other!="x"} >= 12.5e3
 *
 * Anchored end to end on purpose. A partial match is how a parser accepts
 * `rate(cpu[5m]) > 90` by reading only the tail — the class of leniency that
 * makes an import feature lie about what it imported.
 */
const _PROMQL_RULE_RE = /* @__PURE__ */ new RegExp(
  "^\\s*(?<metric>[a-zA-Z_:][a-zA-Z0-9_:]*)\\s*" +
    // The selector block: anything but a brace or quote, OR a complete
    // string literal — which MAY contain braces. `[^{}]*` is the obvious
    // version and it rejects `{msg="{braces}"}`, a legal matcher, because a
    // brace inside a value is indistinguishable from the closing one to a
    // rule that cannot see quoting.
    '(?:\\{(?<selectors>(?:[^{}"]|"(?:[^"\\\\]|\\\\.)*")*)\\})?\\s*' +
    "(?<op>>=|<=|==|!=|>|<)\\s*" +
    "(?<value>[+-]?(?:\\d+\\.?\\d*|\\.\\d+)(?:[eE][+-]?\\d+)?)\\s*$"
);

/* One label matcher inside the braces. The value is a double-quoted PromQL
 * string literal, so it may contain escaped quotes and backslashes — and
 * `: ` , which is what broke the YAML layer in review #532. */
const _SELECTOR_RE = /* @__PURE__ */ new RegExp(
  '^\\s*(?<label>[a-zA-Z_][a-zA-Z0-9_]*)\\s*(?<match>=~|!~|!=|=)\\s*"(?<value>(?:[^"\\\\]|\\\\.)*)"\\s*$'
);

/* Duration suffixes Prometheus accepts on `for:`, in seconds. */
const _DURATION_UNITS = { ms: 0.001, s: 1, m: 60, h: 3600, d: 86400, w: 604800, y: 31536000 };

/* Parse a Prometheus duration (`5m`, `1h30m`, `90s`) into seconds, or null. */
function _durationSecs(text) {
  const raw = String(text).trim();
  if (!raw) return null;
  if (/^\d+(?:\.\d+)?$/.test(raw)) return Number(raw); // bare number = seconds
  const parts = raw.match(/\d+(?:\.\d+)?(?:ms|[smhdwy])/g);
  if (!parts || parts.join("") !== raw) return null;
  let total = 0;
  for (const part of parts) {
    const match = part.match(/^(\d+(?:\.\d+)?)(ms|[smhdwy])$/);
    if (!match) return null;
    total += Number(match[1]) * _DURATION_UNITS[match[2]];
  }
  return total;
}

/* Split the inside of `{...}` on commas that are not inside a string. */
function _splitSelectors(text) {
  const out = [];
  let current = "";
  let inString = false;
  let escaped = false;
  for (const char of text) {
    if (escaped) {
      current += char;
      escaped = false;
      continue;
    }
    if (char === "\\") {
      current += char;
      escaped = true;
      continue;
    }
    if (char === '"') inString = !inString;
    if (char === "," && !inString) {
      out.push(current);
      current = "";
      continue;
    }
    current += char;
  }
  out.push(current);
  return out;
}

/* Import one Prometheus alerting rule into the lab's controls.
 *
 * Accepts either a rules-file snippet — `alert:` / `expr:` / `for:`, with or
 * without the `groups:` scaffolding around it — or a bare expression. Returns
 * `{ ok: true, name, metric, selectors, op, threshold, forSecs }`, or
 * `{ ok: false, reason }` where the reason names what was actually seen.
 *
 * THE GRAMMAR IS DELIBERATELY TINY: one instant-vector selector compared
 * against one scalar. The lab evaluates a threshold against a single sampled
 * series, so that is the whole of what it can honestly represent. A rule
 * carrying `rate(...)`, `sum by (...)`, `unless`, an arithmetic expression or
 * a comparison between two series is not a rule this lab could show — and
 * importing "most of it" would put a reader in front of a chart that answers
 * a different question than the rule they pasted. Those are refused by name.
 *
 * Selectors are parsed but do NOT drive evaluation: the lab always evaluates
 * against the scenario currently loaded. They are validated so a well-formed
 * rule is not rejected for having them, and returned so the caller can say
 * which series the rule was written about.
 */
export function parsePromQLRule(text) {
  const source = String(text == null ? "" : text);
  if (!source.trim()) return { ok: false, reason: "nothing to import — paste a rule first" };

  // Pull the fields out of a rules-file snippet if that is what this is.
  // A bare expression has no `expr:` key and is used whole.
  //
  // The line prefix allows `#` as well as indentation and the list dash,
  // because THIS LAB'S OWN EXPORT writes its rule inside a comment block —
  // a reader who copies what the lab gave them and pastes it back would
  // otherwise be told there is no `expr:` line. Found by round-tripping
  // `buildTestExport` through this parser, which is now a test: if the two
  // ever disagree again, the lab cannot read what it wrote.
  let expr = source;
  let name = null;
  let forText = null;
  const exprLine = source.match(/^[ \t#-]*expr:[ \t]*(?<value>.+?)[ \t]*$/m);
  if (exprLine) {
    expr = _unquoteScalar(exprLine.groups.value);
    const nameLine = source.match(/^[ \t#-]*alert:[ \t]*(?<value>.+?)[ \t]*$/m);
    if (nameLine) name = _unquoteScalar(nameLine.groups.value);
    const forLine = source.match(/^[ \t#-]*for:[ \t]*(?<value>.+?)[ \t]*$/m);
    if (forLine) forText = _unquoteScalar(forLine.groups.value);
  } else if (/^[ \t#-]*(?:record|alert):/m.test(source)) {
    // A rule block with no expr: is a rules file we failed to read, not a
    // bare expression — saying so beats reporting a PromQL syntax error
    // about YAML.
    return { ok: false, reason: "found a rule block but no `expr:` line" };
  }

  expr = expr.trim();
  if (!expr) return { ok: false, reason: "the rule has an empty `expr:`" };

  // THE GRAMMAR RUNS FIRST, and the naming scan only when it fails.
  //
  // The scan is a set of substring patterns over the raw expression, so it
  // cannot tell a PromQL token from the inside of a string literal. Running
  // it first refused rules this lab represents perfectly and, worse, refused
  // them by asserting a specific false fact about the reader's own rule —
  // `{user="alice@example.com"}` was "an offset or @ modifier",
  // `{msg="[error] disk"}` was "a range selector", `{msg="a or b"}` was "a
  // set operator" (review #546 W1, which measured nine such rules).
  //
  // Anything the anchored grammar accepts is representable by construction,
  // so trying it first cannot let an unsupported rule through — the scan
  // still names every construct it named before, just for expressions that
  // failed the grammar rather than for every expression.
  const match = _PROMQL_RULE_RE.exec(expr);
  if (!match) return { ok: false, reason: _whyUnsupported(expr) };
  const { metric, selectors: selectorText, op, value } = match.groups;
  if (!_IMPORTABLE_OPS.has(op)) {
    return {
      ok: false,
      reason: `the lab evaluates \`>\`, \`>=\`, \`<\` and \`<=\`; this rule uses \`${op}\``,
    };
  }

  const threshold = Number(value);
  if (!Number.isFinite(threshold)) {
    return { ok: false, reason: `\`${value}\` is not a finite number` };
  }

  const selectors = {};
  if (selectorText !== undefined && selectorText.trim()) {
    for (const part of _splitSelectors(selectorText)) {
      const selector = _SELECTOR_RE.exec(part);
      if (!selector) {
        return { ok: false, reason: `could not read the label matcher \`${part.trim()}\`` };
      }
      const { label, match: matcher, value: raw } = selector.groups;
      // Unescape the PromQL string literal so the value round-trips through
      // `escapeQuoted` on the way back out to a rules file.
      selectors[label] = { op: matcher, value: raw.replace(/\\(.)/g, "$1") };
    }
  }

  let forSecs = 0;
  if (forText !== null) {
    const parsed = _durationSecs(forText);
    if (parsed === null) {
      return { ok: false, reason: `could not read the duration \`${forText}\`` };
    }
    forSecs = parsed;
  }

  return { ok: true, name, metric, selectors, op, threshold, forSecs };
}

/* Why an expression the grammar rejected could not be imported.
 *
 * Substring patterns, so this is only sound on expressions the anchored
 * grammar has ALREADY refused — see the note at the call site. Naming the
 * construct beats "does not match", which is a useless thing to tell someone
 * holding a rule that is perfectly valid PromQL.
 */
function _whyUnsupported(expr) {
  const unsupported = [
    [/\b(?:rate|irate|increase|avg_over_time|max_over_time|min_over_time|sum_over_time|histogram_quantile|absent|delta|deriv|predict_linear)\s*\(/, "a function call"],
    [/\b(?:sum|avg|min|max|count|topk|bottomk|quantile|stddev|group)\s*(?:by|without)?\s*[({]/, "an aggregation"],
    [/\b(?:unless|and|or)\b/, "a set operator"],
    [/\[[^\]]*\]/, "a range selector"],
    [/\boffset\b|@/, "an offset or @ modifier"],
  ];
  for (const [pattern, what] of unsupported) {
    if (pattern.test(expr)) {
      return `only simple threshold rules import; this one uses ${what}. Edit it by hand.`;
    }
  }
  return "only simple threshold rules import — expected `metric{labels} > number`. Edit it by hand.";
}

/* Strip one layer of YAML scalar quoting from a value read off a line.
 *
 * `expr: 'cpu > 90'` and `expr: "cpu > 90"` are the same expression; the
 * single-quoted form doubles its own apostrophes, which is how
 * `buildTestExport` writes them. Trailing `#` comments are NOT stripped: a
 * `#` inside an unquoted PromQL expression is not a comment, and guessing
 * wrong would silently truncate the rule.
 */
function _unquoteScalar(value) {
  const text = String(value).trim();
  if (text.length >= 2 && text.startsWith("'") && text.endsWith("'")) {
    return text.slice(1, -1).replace(/''/g, "'");
  }
  if (text.length >= 2 && text.startsWith('"') && text.endsWith('"')) {
    return text.slice(1, -1).replace(/\\(.)/g, "$1");
  }
  return text;
}
