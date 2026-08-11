/* Sonda docs site — browser smoke suite.
 *
 * The docs site's JS is the only part of this repo with no automated coverage
 * beyond the pure-helper harness: everything that touches wasm, the canvas or
 * the DOM has, until now, been verified by a human driving Chromium once per
 * PR and reporting what they saw. Reviewers cannot reproduce that, so every
 * visual claim in the last several PRs rested on the author's word. This
 * suite moves the load-bearing ones into CI.
 *
 * Run:
 *   node docs/site/tools/browser/smoke.mjs
 *
 * Environment:
 *   SONDA_SITE_URL  base URL of a served build   (default http://localhost:8777)
 *   CHROMIUM_PATH   browser binary to launch     (default: playwright's own;
 *                   locally /opt/pw-browsers/chromium)
 *
 * Conventions that keep this from becoming a flaky tax:
 *
 *   - Every wait is condition-based (waitForFunction / waitForSelector).
 *     There is no bare waitForTimeout anywhere; the one place a debounce has
 *     to settle is expressed as "wait until the value CHANGES", not "wait
 *     500ms and hope".
 *   - Silence is not success. Every check asserts a terminal state, and any
 *     uncaught page error or same-origin request failure fails the run at the
 *     end even if every assertion passed.
 *   - Failures print what they saw, so a red CI log is diagnosable without
 *     re-running locally.
 */
import { chromium } from "playwright";

const BASE = (process.env.SONDA_SITE_URL || "http://localhost:8777").replace(/\/+$/, "");
const CHROMIUM_PATH = process.env.CHROMIUM_PATH || undefined;
const started = Date.now();

const failures = [];
const pageErrors = [];
let checks = 0;

function check(name, condition, detail = "") {
  checks += 1;
  const line = `${name}${detail ? ` — ${detail}` : ""}`;
  if (condition) {
    console.log(`  ok   ${line}`);
  } else {
    console.log(`  FAIL ${line}`);
    failures.push(line);
  }
}

function section(title) {
  console.log(`\n${title}`);
}

/* Same-origin request failures are ours. ERR_ABORTED is not: it means this
 * script navigated away while a request was in flight (the 1.2 MB wasm is the
 * usual victim), which says nothing about the site. */
function watch(page) {
  page.on("pageerror", (err) => pageErrors.push(`${page.url()} :: ${err}`));
  page.on("requestfailed", (req) => {
    const reason = req.failure()?.errorText || "";
    if (req.url().startsWith(BASE) && !reason.includes("ERR_ABORTED")) {
      pageErrors.push(`request failed: ${req.url()} ${reason}`);
    }
  });
  page.on("console", (msg) => {
    // Uncaught errors already arrive via pageerror; this catches explicit
    // console.error calls from the site's own code.
    if (msg.type() === "error" && !/Failed to load resource/.test(msg.text())) {
      pageErrors.push(`console.error: ${msg.text()}`);
    }
  });
  return page;
}

const chartSignature = () =>
  document.querySelector("#sp-chart")?.toDataURL().length ?? 0;

/* The playground has rendered when the canvas holds a non-trivial image. An
 * empty canvas still produces a short data URL, so the threshold is the
 * assertion, not the presence of the element. */
const CHART_DRAWN = 3000;

async function waitForPlayground(page) {
  await page.waitForSelector("#sonda-playground", { timeout: 30000 });
  await page.waitForFunction(
    (min) => {
      const canvas = document.querySelector("#sp-chart");
      return canvas && canvas.style.display !== "none" && canvas.toDataURL().length > min;
    },
    CHART_DRAWN,
    { timeout: 60000 }
  );
}

/* Two ways to find a browser, and they are mutually exclusive in Playwright:
 *
 *   - CHROMIUM_PATH set (dev containers with a preinstalled Chromium): launch
 *     that binary directly.
 *   - Otherwise (CI): ask for the `chromium` channel rather than letting
 *     Playwright default to `chrome-headless-shell`. The shell is a SEPARATE
 *     download from the browser, so a default launch can fail with
 *     "Executable doesn't exist at .../chromium_headless_shell-<rev>" even
 *     though `playwright install chromium` succeeded. Naming the channel
 *     pins us to the full build, which that install always provides.
 */
const browser = await chromium.launch(
  CHROMIUM_PATH ? { executablePath: CHROMIUM_PATH } : { channel: "chromium" }
);
const context = await browser.newContext();

try {
  // --- 1. The playground boots and draws the default preset --------------
  section("[1] playground boots and renders the default preset");
  const page = watch(await context.newPage());
  page.setDefaultTimeout(30000);
  await page.goto(`${BASE}/playground/`, { waitUntil: "domcontentloaded" });
  await waitForPlayground(page);

  const firstSignature = await page.evaluate(chartSignature);
  check("default preset renders a non-blank chart", firstSignature > CHART_DRAWN,
    `dataURL ${firstSignature} chars`);
  const output = await page.textContent("#sp-output");
  check("encoded output pane is populated", output.trim().length > 0,
    `${output.trim().length} chars`);

  // --- 2. Switching preset redraws ---------------------------------------
  section("[2] switching preset redraws the chart");
  const presetCount = await page.evaluate(
    () => document.querySelector("#sp-preset").options.length
  );
  check("presets are populated", presetCount > 5, `${presetCount} presets`);
  await page.selectOption("#sp-preset", "1");
  // Condition-based: wait for the image to actually differ, not for a delay.
  await page.waitForFunction(
    (previous) => {
      const canvas = document.querySelector("#sp-chart");
      return canvas && canvas.toDataURL().length !== previous;
    },
    firstSignature,
    { timeout: 30000 }
  );
  const secondSignature = await page.evaluate(chartSignature);
  check("a different preset produces a different chart",
    secondSignature !== firstSignature && secondSignature > CHART_DRAWN,
    `${firstSignature} -> ${secondSignature}`);

  // --- 3. Scrub decoration exists and dragging edits the document --------
  section("[3] numeric literals are scrubbable");
  await page.selectOption("#sp-preset", "0");
  await waitForPlayground(page);
  await page.waitForSelector(".cm-scrub-number", { timeout: 30000 });
  const scrubCount = await page.evaluate(
    () => document.querySelectorAll(".cm-scrub-number").length
  );
  check("scrub targets are decorated", scrubCount > 0, `${scrubCount} decorated literals`);

  const docBefore = await page.evaluate(
    () => document.querySelector("#sonda-playground .cm-content").textContent
  );
  const target = await page.$(".cm-scrub-number");
  const box = await target.boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 40, box.y + box.height / 2, { steps: 8 });
  await page.mouse.up();
  await page.waitForFunction(
    (before) =>
      document.querySelector("#sonda-playground .cm-content").textContent !== before,
    docBefore,
    { timeout: 15000 }
  );
  const docAfter = await page.evaluate(
    () => document.querySelector("#sonda-playground .cm-content").textContent
  );
  check("dragging a literal rewrites the document", docAfter !== docBefore);

  // --- 4. Logs preset renders a stream and hides the chart ---------------
  section("[4] logs preset renders a stream and hides the chart");
  const logPreset = await page.evaluate(() => {
    const select = document.querySelector("#sp-preset");
    const option = [...select.options].find((o) => /log/i.test(o.textContent));
    return option ? option.value : null;
  });
  check("a logs preset is present", logPreset !== null);
  await page.selectOption("#sp-preset", logPreset);
  await page.waitForSelector("#sp-logs", { timeout: 30000 });
  await page.waitForFunction(
    () => document.querySelectorAll("#sp-logs .sonda-playground__logline").length > 0,
    null,
    { timeout: 30000 }
  );
  const logLines = await page.evaluate(
    () => document.querySelectorAll("#sp-logs .sonda-playground__logline").length
  );
  check("log lines render", logLines > 0, `${logLines} lines`);
  check("the line chart is hidden for a logs-only scenario",
    await page.evaluate(() => document.querySelector("#sp-chart").style.display === "none"));

  // --- 5. Export buttons produce real files ------------------------------
  section("[5] exports produce real files");
  await page.selectOption("#sp-preset", "0");
  await waitForPlayground(page);
  const [yamlDownload] = await Promise.all([
    page.waitForEvent("download", { timeout: 30000 }),
    page.click("#sp-download"),
  ]);
  check("Download YAML yields a sanitized bare filename",
    /^[a-z0-9_-]+\.yaml$/.test(yamlDownload.suggestedFilename()),
    yamlDownload.suggestedFilename());
  const [pngDownload] = await Promise.all([
    page.waitForEvent("download", { timeout: 30000 }),
    page.click("#sp-png"),
  ]);
  check("Chart PNG yields a sanitized bare filename",
    /^[a-z0-9_-]+\.png$/.test(pngDownload.suggestedFilename()),
    pngDownload.suggestedFilename());

  // --- 6. Hostile share links are inert ----------------------------------
  section("[6] hostile share links are inert");
  const scriptYaml = "version: 2\nkind: runnable\n# <script>window.__pwned = 1</script>\n";
  const encoded = Buffer.from(scriptYaml, "utf8")
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  const hostile = watch(await context.newPage());
  hostile.setDefaultTimeout(30000);
  await hostile.goto(`${BASE}/playground/#yaml=${encoded}`, { waitUntil: "domcontentloaded" });
  await hostile.waitForFunction(
    () => document.querySelector("#sonda-playground .cm-content")?.textContent.includes("script"),
    null,
    { timeout: 60000 }
  );
  check("a script tag in shared YAML does not execute",
    (await hostile.evaluate(() => window.__pwned)) === undefined);
  check("it is shown as inert editor text",
    await hostile.evaluate(() =>
      document.querySelector("#sonda-playground .cm-content").textContent.includes("<script>")
    ));

  const oversized = "A".repeat(32 * 1024 + 1);
  await hostile.goto(`${BASE}/playground/#yaml=${oversized}`, { waitUntil: "domcontentloaded" });
  await hostile.waitForFunction(
    () => document.querySelector("#sp-status")?.textContent.includes("too large"),
    null,
    { timeout: 60000 }
  );
  check("an oversized hash is refused with a status, not a hang",
    (await hostile.textContent("#sp-status")).includes("too large"));
  check("the page still works after refusing it",
    await hostile.evaluate(() =>
      document.querySelector("#sonda-playground .cm-content").textContent.includes("version: 2")
    ));
  await hostile.close();

  // --- 7. Alert lab evaluates and exports --------------------------------
  section("[7] alert lab evaluates and exports");
  // The lab animates a playback sweep and, while it runs, the chip shows the
  // state AT the sweep position — so asserting "the chip says something" would
  // pass on a transient frame and prove nothing. The lab already skips the
  // sweep under prefers-reduced-motion, so emulating that gives the settled
  // verdict deterministically, and lets us assert the useful thing: the
  // default preset is tuned to fire, so it must reach "firing".
  const labContext = await browser.newContext({ reducedMotion: "reduce" });
  const lab = watch(await labContext.newPage());
  lab.setDefaultTimeout(30000);
  await lab.goto(`${BASE}/playground/alert-lab/`, { waitUntil: "domcontentloaded" });
  await lab.waitForSelector("#al-state", { timeout: 30000 });
  await lab.waitForFunction(
    () => document.querySelector("#al-state")?.textContent.trim() === "firing",
    null,
    { timeout: 60000 }
  );
  const chipText = (await lab.textContent("#al-state")).trim();
  check("the default preset evaluates to a firing rule", chipText === "firing", chipText);
  check("the lab reports no error banner",
    await lab.evaluate(() => document.querySelector("#al-error").hidden));

  await lab.click("#al-export");
  await lab.waitForFunction(
    () => {
      const out = document.querySelector("#al-export-out");
      return out && !out.hidden && out.textContent.includes("expect:");
    },
    null,
    { timeout: 30000 }
  );
  const exported = await lab.textContent("#al-export-out");
  check("export emits a scenario with an expect: block", exported.includes("expect:"),
    `${exported.length} chars`);
  await lab.close();
  await labContext.close();

  // --- 8. A generators.md widget mounts on scroll ------------------------
  section("[8] a live generator widget mounts on scroll");
  const widgets = watch(await context.newPage());
  widgets.setDefaultTimeout(30000);
  await widgets.goto(`${BASE}/build/generators/`, { waitUntil: "domcontentloaded" });
  await widgets.waitForSelector(".sonda-livegen", { timeout: 30000 });
  await widgets.evaluate(() => document.querySelector(".sonda-livegen").scrollIntoView());
  await widgets.waitForFunction(
    (min) => {
      const canvas = document.querySelector(".sonda-livegen__chart");
      return canvas && canvas.toDataURL().length > min;
    },
    CHART_DRAWN,
    { timeout: 60000 }
  );
  check("the widget renders a live chart", true);
  const widgetError = await widgets.evaluate(() => {
    const err = document.querySelector(".sonda-livegen__error");
    return err && !err.hidden ? err.textContent : null;
  });
  check("the widget reports no engine error", widgetError === null, widgetError || "");
  await widgets.close();

  // --- 9. A runnable-fence button carries its scenario -------------------
  section("[9] a runnable-fence button carries its scenario to the playground");
  const fences = watch(await context.newPage());
  fences.setDefaultTimeout(30000);
  await fences.goto(`${BASE}/build/scheduling/`, { waitUntil: "domcontentloaded" });
  await fences.waitForSelector("a.sonda-runnable", { timeout: 30000 });
  const buttonCount = await fences.evaluate(
    () => document.querySelectorAll("a.sonda-runnable").length
  );
  check("fence buttons are present", buttonCount > 0, `${buttonCount} buttons`);
  await fences.click("a.sonda-runnable");
  await fences.waitForSelector("#sonda-playground", { timeout: 30000 });
  await fences.waitForFunction(
    () =>
      document
        .querySelector("#sonda-playground .cm-content")
        ?.textContent.includes("version: 2"),
    null,
    { timeout: 60000 }
  );
  check("the fence's scenario arrives in the editor", true);
  await fences.close();
} catch (err) {
  failures.push(`threw: ${err && err.message ? err.message : err}`);
  console.log(`\n  FAIL threw: ${err && err.stack ? err.stack.split("\n")[0] : err}`);
} finally {
  await browser.close();
}

const elapsed = ((Date.now() - started) / 1000).toFixed(1);
console.log(`\n${checks} checks in ${elapsed}s`);

if (pageErrors.length) {
  console.log(`\n${pageErrors.length} page error(s):`);
  for (const err of pageErrors) console.log(`   ${err}`);
}

const bad = failures.length + pageErrors.length;
if (bad) {
  console.log(`\nSMOKE FAILED — ${failures.length} assertion(s), ${pageErrors.length} page error(s)`);
  process.exit(1);
}
console.log("\nSMOKE PASSED");
