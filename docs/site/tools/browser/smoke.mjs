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

/* Canvas thresholds, every number below MEASURED against this site rather
 * than guessed — review #539 W1 caught the first version of this constant
 * being 1,346 chars BELOW the floor it was supposed to exclude.
 *
 * Corrupting the committed wasm and reading each signal gives:
 *
 *                        healthy    engine dead
 *   #sp-chart             47,754          4,346   <- axes and grid still draw
 *   #sp-output (chars)       355              0   <- nothing without the engine
 *   .sonda-livegen__chart 29,242          2,118
 *
 * The lesson is that the failed state is not a BLANK canvas, it is a
 * chromed-but-empty one: the axes, gridlines and tick labels are drawn by
 * plain canvas code that does not need the engine at all. A threshold picked
 * against "blank" therefore cannot fail for the reason it exists.
 *
 * These sit roughly midway between the two measurements in log terms, so
 * they survive ordinary chart-chrome drift in either direction.
 */
const CHART_WITH_DATA = 12000;
const WIDGET_CHART_WITH_DATA = 8000;

/* Readiness is gated on the ENGINE having produced output, not on the canvas
 * having produced bytes.
 *
 * This predicate is the precondition for three of the nine sections, so it is
 * the last place a check that cannot fail belongs. `#sp-output` holds the
 * encoded events the wasm engine emitted: 355 chars healthy, exactly 0 with a
 * dead engine. It tests the thing we actually care about and cannot be
 * defeated by drawing more axes. The canvas conditions ride along so the
 * chart is known-visible for the assertions that follow. */
async function waitForPlayground(page) {
  await page.waitForSelector("#sonda-playground", { timeout: 30000 });
  try {
    await page.waitForFunction(
      (min) => {
        const canvas = document.querySelector("#sp-chart");
        const output = document.querySelector("#sp-output");
        return (
          canvas &&
          output &&
          output.textContent.trim().length > 0 &&
          canvas.style.display !== "none" &&
          canvas.toDataURL().length > min
        );
      },
      CHART_WITH_DATA,
      { timeout: 60000 }
    );
  } catch (err) {
    // A bare "waitForFunction: Timeout exceeded" says nothing about WHY the
    // playground never became ready, and this gate now guards the whole run.
    // Report what the page actually looked like — with a dead engine the
    // status line and error banner both name the real cause.
    const seen = await page
      .evaluate((min) => {
        const canvas = document.querySelector("#sp-chart");
        const error = document.querySelector("#sp-error");
        return {
          chartChars: canvas ? canvas.toDataURL().length : null,
          floor: min,
          chartDisplay: canvas ? canvas.style.display || "(visible)" : null,
          outputChars: (document.querySelector("#sp-output")?.textContent || "").trim().length,
          status: (document.querySelector("#sp-status")?.textContent || "").trim() || null,
          error: error && !error.hidden ? error.textContent.trim().slice(0, 200) : null,
        };
      }, CHART_WITH_DATA)
      .catch(() => null);
    throw new Error(
      `playground never became ready — ${seen ? JSON.stringify(seen) : "page unreadable"}`
    );
  }
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
  // "Non-blank" was the wrong bar: a dead engine still draws axes and grid.
  // This threshold is above that measured floor, so it fails when the engine
  // does (review #539 W1).
  check("default preset renders a chart with data in it", firstSignature > CHART_WITH_DATA,
    `dataURL ${firstSignature} chars, floor ${CHART_WITH_DATA}`);
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
    secondSignature !== firstSignature && secondSignature > CHART_WITH_DATA,
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
    WIDGET_CHART_WITH_DATA,
    { timeout: 60000 }
  );
  const widgetSignature = await widgets.evaluate(
    () => document.querySelector(".sonda-livegen__chart").toDataURL().length
  );
  check("the widget renders a chart with data in it",
    widgetSignature > WIDGET_CHART_WITH_DATA,
    `dataURL ${widgetSignature} chars, floor ${WIDGET_CHART_WITH_DATA}`);
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

  // --- 10. The examples gallery ------------------------------------------
  //
  // 62 cards built at build time by docs/site/hooks/examples_gallery.py, each
  // carrying one file from examples/. Two things are worth a check and one is
  // easy to get wrong: the cards render (measured through the ENGINE, per the
  // #539 lesson — canvas bytes alone cannot tell a drawn chart from drawn
  // axes), and the cards that CANNOT chart say why instead of showing a blank
  // canvas. Every csv_replay example samples to ok:true with no entries, so
  // the second is the failure mode this whole feature turns on.
  section("[10] the examples gallery mounts and reports honestly");
  const gallery = watch(await context.newPage());
  gallery.setDefaultTimeout(30000);
  await gallery.goto(`${BASE}/test/examples/`, { waitUntil: "domcontentloaded" });

  await gallery.waitForSelector(".sonda-gallery[data-live]", { timeout: 30000 });
  const tables = await gallery.evaluate(
    () => document.querySelectorAll(".md-content table").length
  );
  check("the markdown tables survive — the no-JS floor is untouched", tables > 10, `${tables} tables`);

  // Scroll the page so every lazily-observed card mounts.
  await gallery.evaluate(async () => {
    for (let y = 0; y < document.body.scrollHeight; y += 600) {
      window.scrollTo(0, y);
      await new Promise((r) => requestAnimationFrame(r));
    }
    window.scrollTo(0, document.body.scrollHeight);
  });

  await gallery.waitForFunction(
    () => {
      const cards = [...document.querySelectorAll(".sonda-gallery__live")];
      if (!cards.length) return false;
      return cards.every((card) => {
        const canvas = card.querySelector("canvas");
        const note = card.querySelector(".sonda-livegen__note");
        return (canvas && !canvas.hidden) || (note && !note.hidden);
      });
    },
    null,
    { timeout: 120000 }
  );

  const tally = await gallery.evaluate(() => {
    const out = { cards: 0, charts: 0, skipped: 0, notes: 0, errors: 0, linkless: 0, reason: "" };
    for (const card of document.querySelectorAll(".sonda-gallery__live")) {
      out.cards += 1;
      if (!card.querySelector(".sonda-livegen__open")) out.linkless += 1;
      const canvas = card.querySelector("canvas");
      const note = card.querySelector(".sonda-livegen__note");
      if (canvas && !canvas.hidden) {
        out.charts += 1;
        continue;
      }
      const kind = note ? note.dataset.kind : "";
      if (kind === "skipped") {
        out.skipped += 1;
        if (!out.reason) out.reason = note.textContent.trim();
      } else if (kind === "error") out.errors += 1;
      else out.notes += 1;
    }
    return out;
  });

  check("every table row with a scenario got a card", tally.cards > 40, `${tally.cards} cards`);
  check("most cards chart their scenario", tally.charts > 30, `${tally.charts} charts`);
  check("no card failed to compile", tally.errors === 0, `${tally.errors} errors`);
  check("every card offers the playground", tally.linkless === 0, `${tally.linkless} without a link`);
  check(
    "cards the browser cannot sample say why, rather than showing a blank chart",
    tally.skipped > 0 && /csv_replay|feature|file/.test(tally.reason),
    `${tally.skipped} skipped — ${tally.reason.slice(0, 70)}`
  );

  const galleryLink = await gallery.getAttribute(
    ".sonda-gallery__live .sonda-livegen__open",
    "href"
  );
  await gallery.goto(galleryLink, { waitUntil: "domcontentloaded" });
  await gallery.waitForFunction(
    () => document.querySelector("#sp-output")?.textContent.trim().length > 0,
    null,
    { timeout: 60000 }
  );
  check("a card's link carries its example into the playground", true);
  await gallery.close();

  // --- 11. Scheduling and encoder widgets --------------------------------
  //
  // The gap/burst widgets exist to show WHERE the signal stops, which is the
  // shading and not the trace. That gives an assertion with no ambiguity in
  // it: the sampled values are byte-identical for `for: 1s` and `for: 15s`
  // (measured through the engine — gaps suppress emission at runtime and do
  // not touch the sampled generator), so if the canvas changes when `for`
  // moves, the shading is what changed. Remove the shading and this check
  // goes red while a "chart is non-blank" check would stay green.
  section("[11] scheduling and encoder widgets");
  const widgets2 = watch(await context.newPage());
  widgets2.setDefaultTimeout(30000);
  await widgets2.goto(`${BASE}/build/scheduling/`, { waitUntil: "domcontentloaded" });

  for (const gen of ["gaps", "bursts"]) {
    await widgets2.evaluate((g) => document.querySelector(`[data-gen="${g}"]`)?.scrollIntoView(), gen);
    // The floor is passed in, not closed over: this function body runs in the
    // page, where the module's constants do not exist. A bare reference would
    // throw a ReferenceError that the catch below would swallow, leaving the
    // wait to look like a timeout.
    await widgets2
      .waitForFunction(
        ([g, floor]) => {
          const c = document.querySelector(`[data-gen="${g}"] canvas`);
          return c && c.toDataURL().length > floor;
        },
        [gen, WIDGET_CHART_WITH_DATA],
        { timeout: 90000 }
      )
      .catch(() => {}); // the check below reports what it actually saw
    const drawn = await widgets2.evaluate((g) => {
      const c = document.querySelector(`[data-gen="${g}"] canvas`);
      const err = document.querySelector(`[data-gen="${g}"] .sonda-livegen__error`);
      return {
        chars: c ? c.toDataURL().length : 0,
        controls: document.querySelectorAll(`[data-gen="${g}"] .sonda-livegen__key`).length,
        error: err && !err.hidden ? err.textContent : "",
      };
    }, gen);
    check(`the ${gen} widget renders a chart`, drawn.chars > WIDGET_CHART_WITH_DATA, `dataURL ${drawn.chars}`);
    check(`the ${gen} widget reports no engine error`, drawn.error === "", drawn.error);
    check(`the ${gen} widget offers its controls`, drawn.controls >= 2, `${drawn.controls} controls`);
  }

  const beforeShading = await widgets2.evaluate(
    () => document.querySelector('[data-gen="gaps"] canvas').toDataURL()
  );
  await widgets2.evaluate(() => {
    // The second slider is `for` — the one the values do not depend on.
    const input = document.querySelectorAll('[data-gen="gaps"] input[type="range"]')[1];
    input.value = input.max;
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  let shadingMoved = true;
  await widgets2
    .waitForFunction(
      (prev) => document.querySelector('[data-gen="gaps"] canvas').toDataURL() !== prev,
      beforeShading,
      { timeout: 30000 }
    )
    .catch(() => {
      shadingMoved = false;
    });
  check(
    "widening the gap redraws the chart — the trace is identical, so this is the shading",
    shadingMoved
  );

  await widgets2.goto(`${BASE}/build/encoders/`, { waitUntil: "domcontentloaded" });
  await widgets2.evaluate(() => document.querySelector('[data-gen="encoders"]')?.scrollIntoView());
  await widgets2.waitForFunction(
    () => document.querySelector('[data-gen="encoders"] pre')?.textContent.trim().length > 0,
    null,
    { timeout: 90000 }
  );
  // Each option must produce output SHAPED like the format it names. A
  // widget that offered an encoder the wasm build lacks would show the
  // engine's "requires the 'otlp' feature" here instead.
  const SHAPES = [
    ["prometheus_text", /^cpu_usage\{/],
    ["influx_lp", /^cpu_usage,/],
    ["json_lines", /^\{/],
  ];
  for (const [encoder, shape] of SHAPES) {
    await widgets2.selectOption('[data-gen="encoders"] select', encoder);
    let matched = true;
    await widgets2
      .waitForFunction(
        (source) => {
          const text = document.querySelector('[data-gen="encoders"] pre').textContent.trim();
          return text.length > 0 && new RegExp(source).test(text);
        },
        shape.source,
        { timeout: 30000 }
      )
      .catch(() => {
        matched = false;
      });
    const state = await widgets2.evaluate(() => {
      const err = document.querySelector('[data-gen="encoders"] .sonda-livegen__error');
      return {
        first: document.querySelector('[data-gen="encoders"] pre').textContent.trim().split("\n")[0],
        error: err && !err.hidden ? err.textContent : "",
      };
    });
    check(`${encoder} encodes in its own format`, matched, state.first.slice(0, 60));
    check(`${encoder} reports no engine error`, state.error === "", state.error);
  }
  await widgets2.close();
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
