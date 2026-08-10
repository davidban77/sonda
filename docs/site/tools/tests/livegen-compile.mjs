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

const cases = [];
for (const [gen, widget] of Object.entries(WIDGETS)) {
  for (const params of cornerParams(widget)) cases.push([gen, widget, params]);
}
for (const [gen, key] of SWEEPS) {
  for (const params of sweepParams(WIDGETS[gen], key)) cases.push([gen, WIDGETS[gen], params]);
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
