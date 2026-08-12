// Compile gate for the live generator widgets (review #534 W1/M1).
//
// The presets module claims every slider combination compiles; this script
// proves it against the real engine on every CI run instead of trusting the
// comment. For each widget it feeds every min/max corner combination (the
// documented constraints are linear per parameter, so corners are the
// binding cases) plus a full every-step sweep of the duration-coupled
// sliders (leak.time_to_ceiling, saturation.time_to_saturate) through
// `sonda --dry-run run`.
//
// Run from the repo root after building the binary:
//   cargo build --release -p sonda
//   node docs/site/tools/tests/livegen-compile.mjs
// SONDA_BIN overrides the binary path. Zero npm dependencies.

import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { WIDGETS, cornerParams, sweepParams } from "../../docs/javascripts/livegen-presets.js";

const SONDA = process.env.SONDA_BIN || "./target/release/sonda";
const SWEEPS = [
  ["leak", "time_to_ceiling"],
  ["saturation", "time_to_saturate"],
];

// The corner grid is a product: adding one slider doubles it, and adding one
// `choices` entry multiplies it by the option count. That is fine at today's
// scale and quietly fatal at some larger one — a gate that takes an hour gets
// deleted, and the deletion is how the coverage is lost. The ceiling is well
// above the current count so it is not a tripwire on ordinary growth; it fires
// when someone has multiplied rather than added, and the fix is to say which
// corners matter rather than to raise this number.
const MAX_CASES = 2000;

const cases = [];
for (const [gen, widget] of Object.entries(WIDGETS)) {
  for (const params of cornerParams(widget)) cases.push([gen, widget, params]);
}
for (const [gen, key] of SWEEPS) {
  for (const params of sweepParams(WIDGETS[gen], key)) cases.push([gen, WIDGETS[gen], params]);
}

if (cases.length > MAX_CASES) {
  console.error(
    `${cases.length} slider combinations exceeds the ${MAX_CASES}-case ceiling.\n` +
      `  A widget was added whose corners multiply the grid rather than extend it.\n` +
      `  Narrow the corners for that widget instead of raising this ceiling.`
  );
  process.exit(1);
}

const dir = mkdtempSync(join(tmpdir(), "livegen-"));
let failed = 0;
try {
  cases.forEach(([gen, widget, params], index) => {
    const file = join(dir, `${gen}-${index}.yaml`);
    writeFileSync(file, widget.yaml(params));
    try {
      execFileSync(SONDA, ["--dry-run", "run", file], { stdio: "pipe" });
    } catch (err) {
      failed += 1;
      const stderr = err.stderr ? err.stderr.toString().trim() : String(err);
      console.error(`FAIL ${gen} ${JSON.stringify(params)}\n  ${stderr}`);
    }
  });
} finally {
  rmSync(dir, { recursive: true, force: true });
}

if (failed) {
  console.error(`${failed}/${cases.length} slider combinations failed to compile`);
  process.exit(1);
}
console.log(`${cases.length} slider combinations compile`);
