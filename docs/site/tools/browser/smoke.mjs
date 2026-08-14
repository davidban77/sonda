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
 *   - Every wait for STATE is condition-based (waitForFunction /
 *     waitForSelector): "wait until the value CHANGES", never "wait 500ms and
 *     hope". The single waitForTimeout in this file is not waiting for state
 *     at all — it paces a synthetic drag across debounce windows, because
 *     spanning several of them is the behaviour section 12 is testing.
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

  // Widgets mount on intersection, so each one has to be scrolled to before it
  // exists. Shared by the WP13 and WP14 loops below rather than written twice.
  const scrollAndMount = (page, sel) =>
    page
      .evaluate((s) => {
        const host = document.querySelector(s);
        if (!host) return { found: false };
        host.scrollIntoView();
        return { found: true };
      }, sel)
      .catch(() => ({ found: false }));

  // WP13 completed the core-generator set, so the page now carries widgets of
  // two SHAPES: slider-driven, and the choice-driven `sequence` whose whole
  // input is a <select>. Section 8 above only ever mounted whichever widget
  // came first in the document, which is a slider one — a choice-only widget
  // could ship broken behind a green suite.
  //
  // Each new kind is asserted by mounting it specifically and reading its own
  // canvas, rather than by counting widgets on the page: a count passes on a
  // page where five placeholders rendered five empty boxes.
  for (const gen of ["constant", "sawtooth", "uniform", "step", "sequence"]) {
    const selector = `.sonda-livegen[data-gen="${gen}"]`;
    const mounted = await scrollAndMount(widgets, selector);

    let drew = false;
    if (mounted.found) {
      drew = await widgets
        .waitForFunction(
          ([sel, min]) => {
            const canvas = document.querySelector(sel)?.querySelector(".sonda-livegen__chart");
            return Boolean(canvas && canvas.toDataURL().length > min);
          },
          [selector, WIDGET_CHART_WITH_DATA],
          { timeout: 60000 }
        )
        .then(() => true)
        .catch(() => false);
    }

    const err = await widgets.evaluate((sel) => {
      const e = document.querySelector(sel)?.querySelector(".sonda-livegen__error");
      return e && !e.hidden ? e.textContent.trim().slice(0, 120) : null;
    }, selector);

    check(
      `the ${gen} widget mounts and draws`,
      mounted.found && drew && err === null,
      mounted.found ? err || (drew ? "" : "no chart data") : "no placeholder on the page"
    );
  }

  // The sequence widget's control is a <select>, and choosing a different
  // pattern must redraw. A widget whose control is inert renders perfectly
  // and teaches nothing.
  // Addressed by key, not by position: `[data-gen="sequence"] select` resolves
  // the FIRST <select>, which is `pattern` only until someone reorders the
  // widget's choices (review #549 M1). `repeat`, the other control, is covered
  // by the control-reaches-template invariant in the pure suite.
  const seqSelector = '.sonda-livegen[data-gen="sequence"]';
  const seqSelectSelector = `${seqSelector} select[data-key="pattern"]`;
  const seqBefore = await widgets.evaluate(
    (sel) => document.querySelector(sel)?.querySelector(".sonda-livegen__chart")?.toDataURL().length ?? 0,
    seqSelector
  );
  const seqSelect = await widgets.$(seqSelectSelector);
  if (seqSelect) await seqSelect.selectOption({ index: 1 });
  const seqChanged = seqSelect
    ? await widgets
        .waitForFunction(
          ([sel, before]) =>
            (document.querySelector(sel)?.querySelector(".sonda-livegen__chart")?.toDataURL().length ?? 0) !== before,
          [seqSelector, seqBefore],
          { timeout: 30000 }
        )
        .then(() => true)
        .catch(() => false)
    : false;
  check("choosing a different sequence pattern redraws the chart", seqChanged,
    seqSelect ? `was ${seqBefore} chars` : 'no <select data-key="pattern"> rendered');

  // --- 8b. WP14: the three sections that are not metrics -----------------
  //
  // These exercise the renderers extracted out of playground.js. The engine
  // error check is not decoration here: a logs widget on the metrics default
  // encoder COMPILES and fails at sampling, so the compile gate is green and
  // only this sees it.
  for (const gen of ["histogram", "summary"]) {
    const sel = `.sonda-livegen[data-gen="${gen}"]`;
    const mounted = await scrollAndMount(widgets, sel);
    // Condition-based: mounting is asynchronous (intersection observer, then
    // the lazily-fetched wasm), so reading the DOM straight after scrolling
    // reads it before the widget exists.
    const shot = await widgets
      .waitForFunction(
        ([s, min]) => {
          const el = document.querySelector(s);
          const err = el?.querySelector(".sonda-livegen__error");
          if (err && !err.hidden) return { pixels: 0, height: 0, error: err.textContent };
          const canvas = el?.querySelector("canvas");
          if (!canvas) return false;
          const pixels = canvas.toDataURL().length;
          return pixels > min ? { pixels, height: canvas.height, error: null } : false;
        },
        [sel, WIDGET_CHART_WITH_DATA],
        { timeout: 60000 }
      )
      .then((h) => h.jsonValue())
      .catch(() => ({ pixels: 0, height: 0, error: "never drew" }));
    check(`the ${gen} widget mounts and draws`,
      mounted.found && shot.pixels > WIDGET_CHART_WITH_DATA && shot.error === null,
      shot.error || (mounted.found ? `dataURL ${shot.pixels}` : "no placeholder on the page"));
  }

  // The half the pure module cannot check: it bounds ticks x observations,
  // but the heatmap's ROW count comes from the engine's default bucket ladder
  // for the named distribution, which no JS in this repo knows. Measured here
  // against the real sampler, at the corner where both sliders are at max.
  const histSel = '.sonda-livegen[data-gen="histogram"]';
  await widgets.evaluate((s) => {
    for (const input of document.querySelectorAll(`${s} input[type="range"]`)) {
      input.value = input.max;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }, histSel);
  const histAtMax = await widgets
    .waitForFunction(
      (s) => {
        const c = document.querySelector(s)?.querySelector("canvas");
        if (!c || !c.height) return false;
        // CSS pixels, not device pixels. `canvas.height` is cssHeight * dpr,
        // and the ceiling below is a CSS-pixel quantity — comparing them
        // directly made the gate's strictness depend on the display of
        // whoever ran it (review #550 W1): identical at CI's dpr 1, twice as
        // strict at dpr 2. `style.height` is what the renderer sets in CSS px.
        return {
          cssHeight: parseFloat(c.style.height),
          devicePixels: c.height,
          pixels: c.toDataURL().length,
        };
      },
      histSel,
      { timeout: 30000 }
    )
    .then((h) => h.jsonValue())
    .catch(() => null);
  // Derived from the arithmetic the renderer can actually produce.
  // `rowHeight = max(12, min(20, floor(240 / rows)))`, so 20px rows happen
  // only while rows <= 12 — the tallest 20px heatmap is 8 + 12*20 + 26 = 274.
  // Every row past that costs the 12px FLOOR, so a ladder of N > 20 rows is
  // 34 + 12N. 40 rows is already more buckets than a reader can scan, and
  // that is the bound: 34 + 40*12 = 514.
  //
  // The first version of this said "40 rows at the 20px maximum row height",
  // a configuration the formula cannot reach, and so admitted 66 rows while
  // claiming to admit 40.
  const HEATMAP_MAX_CSS_HEIGHT = 34 + 40 * 12;
  check("the heatmap stays bounded with both sliders at maximum",
    histAtMax !== null &&
      histAtMax.cssHeight <= HEATMAP_MAX_CSS_HEIGHT &&
      histAtMax.pixels > WIDGET_CHART_WITH_DATA,
    histAtMax
      ? `${histAtMax.cssHeight}css px (${histAtMax.devicePixels} device), ceiling ${HEATMAP_MAX_CSS_HEIGHT}`
      : "never redrew");

  // The logs widget renders ELEMENTS, not pixels — a log line is text, and
  // drawing it into a canvas would make it unselectable and invisible to a
  // screen reader. So it is checked as DOM.
  const logSel = '.sonda-livegen[data-gen="log_template"]';
  const logMounted = await scrollAndMount(widgets, logSel);
  const stream = await widgets
    .waitForFunction(
      (s) => {
        const el = document.querySelector(s);
        const err = el?.querySelector(".sonda-livegen__error");
        if (err && !err.hidden)
          return { lines: 0, severities: [], stamped: 0, error: err.textContent };
        const pane = el?.querySelector(".sonda-livegen__logstream");
        if (!pane || !pane.children.length) return false;
        // The trailing "… N more events" footer is not a log line and must be
        // excluded from every count below, or the stamped check fails on it.
        const footer = pane.querySelector(".sonda-livegen__logmore");
        const rows = [...pane.children].filter((r) => r !== footer);
        return {
          lines: rows.length,
          severities: [...new Set(rows.map((r) => (r.className.match(/logline--(\w+)/) || [])[1]))],
          stamped: rows.filter((r) => /^\+\d/.test(r.textContent)).length,
          withheld: footer ? Number((footer.textContent.match(/(\d+) more/) || [])[1]) : null,
          screens: Math.round((pane.scrollHeight / pane.clientHeight) * 10) / 10,
          error: null,
        };
      },
      logSel,
      { timeout: 60000 }
    )
    .then((h) => h.jsonValue())
    .catch(() => ({ lines: 0, severities: [], stamped: 0, error: "never rendered" }));
  check("the log_template widget renders a severity-coloured stream",
    logMounted.found && stream.lines > 0 && stream.error === null,
    stream.error || `${stream.lines} line(s)`);
  check("every log line is stamped with its offset on the timeline",
    stream.lines > 0 && stream.stamped === stream.lines,
    `${stream.stamped}/${stream.lines} stamped`);
  check("the stream carries more than one severity, so the colouring means something",
    stream.severities.filter(Boolean).length > 1,
    stream.severities.join(" · ") || "none");

  // Review #550 M2. The renderer showed all 240 events inside a 318px pane —
  // seventeen screens of nested scroll in the middle of a reference page,
  // while the heatmap beside it was gated on exactly this kind of reader
  // effort. The cap has to be visible AND honest: a pane that silently stops
  // at 40 tells a reader the scenario produced 40 events.
  check("the log pane is capped rather than becoming a scroll trap",
    stream.lines <= 40 && stream.screens <= 4,
    `${stream.lines} line(s), ${stream.screens} screens of scroll`);
  check("and says how many events it withheld, rather than just stopping",
    Number.isFinite(stream.withheld) && stream.withheld > 0,
    stream.withheld === null ? "no footer" : `${stream.withheld} withheld`);

  // Review #550 round 2 M1. The footer sat 911px down a 318px window, so the
  // view AT REST was 40 lines ending at +9.75s with nothing saying 200 were
  // dropped — the state the cap's own rationale says must not exist. The
  // count now sits above the pane; this asserts it is outside the scroll
  // container rather than merely present in the DOM.
  const logTally = await widgets.evaluate((s) => {
    const el = document.querySelector(s);
    const node = el?.querySelector(".sonda-livegen__logtally");
    const pane = el?.querySelector(".sonda-livegen__logstream");
    if (!node || !pane) return null;
    return {
      text: node.textContent.trim(),
      aboveThePane: node.getBoundingClientRect().bottom <= pane.getBoundingClientRect().top + 1,
      insidePane: pane.contains(node),
    };
  }, logSel);
  check("the withheld count is readable without scrolling the pane",
    logTally !== null &&
      logTally.aboveThePane &&
      !logTally.insidePane &&
      /\b40\b.*\b240\b/.test(logTally.text),
    logTally ? logTally.text : "no tally element");

  // The reviewer's other named corner: error weight at 0 is the ordinary
  // healthy service, and it must still render rather than producing an empty
  // pane or a division by zero in the engine's weighting.
  await widgets.evaluate((s) => {
    for (const input of document.querySelectorAll(`${s} input[type="range"]`)) {
      input.value = input.min;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }, logSel);
  const atZero = await widgets
    .waitForFunction(
      (s) => {
        const el = document.querySelector(s);
        const err = el?.querySelector(".sonda-livegen__error");
        if (err && !err.hidden) return { lines: 0, error: err.textContent };
        const pane = el?.querySelector(".sonda-livegen__logstream");
        if (!pane || !pane.children.length) return false;
        // Excludes the "… N more" footer, same as the counts above, so the
        // two checks report the same quantity rather than differing by one.
        const footer = pane.querySelector(".sonda-livegen__logmore");
        return { lines: pane.children.length - (footer ? 1 : 0), error: null };
      },
      logSel,
      { timeout: 30000 }
    )
    .then((h) => h.jsonValue())
    .catch(() => null);
  check("all severity weights at minimum still produces a stream",
    atZero !== null && atZero.error === null && atZero.lines > 0,
    atZero ? atZero.error || `${atZero.lines} line(s)` : "never settled");

  // Review #550 round 2 W1 named the structural hole: every check above
  // enumerates widgets by NAME, so a widget added later is invisible to all
  // of them — which is how an encoder that compiles and then fails to sample
  // could have reached the page behind a green suite. This one enumerates
  // whatever the PAGE actually carries, so a new placeholder is covered the
  // day it lands rather than the day someone remembers to add it here.
  const everyWidget = await widgets.evaluate(() =>
    [...document.querySelectorAll(".sonda-livegen[data-gen]")].map((el) => el.dataset.gen)
  );
  // One at a time, each scrolled and then WAITED FOR before moving on. The
  // first version scrolled all of them in a loop with no await, so only the
  // last one ever intersected and the rest never mounted — the check timed
  // out on its own harness rather than on the page.
  const complaints = [];
  for (const gen of everyWidget) {
    const sel = `.sonda-livegen[data-gen="${gen}"]`;
    await scrollAndMount(widgets, sel);
    const outcome = await widgets
      .waitForFunction(
        (s) => {
          const el = document.querySelector(s);
          if (!el) return { gen: null, error: "no placeholder" };
          const err = el.querySelector(".sonda-livegen__error");
          if (err && !err.hidden) return { error: err.textContent.trim().slice(0, 80) };
          const canvas = el.querySelector("canvas");
          if (canvas && canvas.height > 0) return { error: null };
          return el.querySelector(".sonda-livegen__logstream, .sonda-livegen__preview")
            ? { error: null }
            : false;
        },
        sel,
        { timeout: 30000 }
      )
      .then((h) => h.jsonValue())
      .catch(() => ({ error: "never settled" }));
    if (outcome.error) complaints.push(`${gen}: ${outcome.error}`);
  }
  check("every widget on the page samples without an engine error",
    complaints.length === 0,
    complaints.length ? complaints.join(" | ") : `${everyWidget.length} widget(s) clean`);

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
  // The waited-for condition is asserted here rather than relied on
  // implicitly (review #544 M2). A `check(..., true)` after a wait is not
  // vacuous today — the wait throws and fails the run — but it READS as an
  // assertion while depending entirely on a line above it, so loosening that
  // wait later would turn it decorative with nothing noticing.
  let fenceArrived = true;
  await fences
    .waitForFunction(
      () =>
        document
          .querySelector("#sonda-playground .cm-content")
          ?.textContent.includes("version: 2"),
      null,
      { timeout: 60000 }
    )
    .catch(() => {
      fenceArrived = false;
    });
  check(
    "the fence's scenario arrives in the editor",
    fenceArrived &&
      (await fences.evaluate(() =>
        document.querySelector("#sonda-playground .cm-content")?.textContent.includes("version: 2")
      )),
    `${(await fences.evaluate(() => document.querySelector("#sonda-playground .cm-content")?.textContent || "")).trim().slice(0, 40)}…`
  );
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
  await gallery
    .waitForFunction(
      () => document.querySelector("#sp-output")?.textContent.trim().length > 0,
      null,
      { timeout: 60000 }
    )
    .catch(() => {}); // the check below reports what it actually saw
  const carried = await gallery.evaluate(
    () => (document.querySelector("#sp-output")?.textContent || "").trim().length
  );
  check(
    "a card's link carries its example into the playground",
    carried > 0,
    `${carried} chars of encoded output`
  );
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

  // The canvas-difference check above proves SOMETHING moved; it cannot say
  // what. `drawMini` stamps the two things the shading is claiming onto the
  // canvas, so they can be asserted exactly (review #543). Sliders were left
  // at `for: max` by the check above, so this reads the widget as it now
  // stands: `every: 15s`, `for: 15s`, gaps therefore filling every cycle of a
  // 60-second sample — four windows, at 0s, 15s, 30s and 45s.
  const gapWindows = await widgets2.evaluate(
    () => document.querySelector('[data-gen="gaps"] canvas').dataset.windows
  );
  check("the gap widget shades one window per cycle", gapWindows === "4", `windows=${gapWindows}`);

  // The burst multiplier. It changes the emission rate, not the value, so the
  // trace cannot show it and neither can a canvas diff — before #543 B1 this
  // slider moved nothing but its own readout. The band now reports the rate
  // outside it and inside it, computed from what the engine returned, and
  // that is a string a test can pin: the widget's rate is 4/s, so the three
  // multiplier positions below have exactly one answer each.
  await widgets2.evaluate(() => document.querySelector('[data-gen="bursts"]')?.scrollIntoView());
  for (const [multiplier, expected] of [["3", "4/s → 12/s"], ["10", "4/s → 40/s"], ["1", "4/s → 4/s"]]) {
    await widgets2.evaluate((value) => {
      // The third slider is `multiplier`; `every` and `for` stay at default.
      const input = document.querySelectorAll('[data-gen="bursts"] input[type="range"]')[2];
      input.value = value;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, multiplier);
    let labelled = true;
    await widgets2
      .waitForFunction(
        (want) => document.querySelector('[data-gen="bursts"] canvas').dataset.burstRate === want,
        expected,
        { timeout: 30000 }
      )
      .catch(() => {
        labelled = false;
      });
    const seen = await widgets2.evaluate(
      () => document.querySelector('[data-gen="bursts"] canvas').dataset.burstRate
    );
    check(`multiplier ${multiplier} reports "${expected}" on the band`, labelled, `saw "${seen}"`);
  }

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

  // --- 12. The time cursor, log correlation and the ghost trace ----------
  //
  // WP9. Three things a canvas diff cannot distinguish, so `drawChart`
  // stamps what it drew and these read the stamps: whether there is a
  // cursor, how many ghost series were matched and drawn, and which log
  // lines the cursor claims.
  section("[12] time cursor, log correlation and ghost trace");
  const cursorPage = watch(await context.newPage());
  cursorPage.setDefaultTimeout(30000);
  await cursorPage.goto(`${BASE}/playground/`, { waitUntil: "domcontentloaded" });
  await waitForPlayground(cursorPage);

  // Scrolled into view before measuring: this suite's viewport is 720 tall and
  // the chart's centre sits below the fold, so a mouse.move to its bounding
  // box would land outside the viewport and never reach the canvas. Measuring
  // before scrolling is the mistake that looks like "the cursor is broken".
  await cursorPage.locator("#sp-chart").scrollIntoViewIfNeeded();
  const chartBox = await cursorPage.locator("#sp-chart").boundingBox();
  // Condition-based like everything else here: the repaint is rAF-driven, so
  // wait for the chart to STAMP a cursor rather than for a duration.
  const hover = async (fraction) => {
    await cursorPage.mouse.move(
      chartBox.x + chartBox.width * fraction,
      chartBox.y + chartBox.height * 0.5
    );
    await cursorPage.waitForFunction(
      () => document.getElementById("sp-chart").dataset.cursor !== "",
      null,
      { timeout: 15000 }
    );
  };

  await hover(0.5);
  const reading = await cursorPage.evaluate(() => {
    const box = document.getElementById("sp-readout");
    return {
      cursor: document.getElementById("sp-chart").dataset.cursor,
      secs: box.dataset.secs,
      rows: box.querySelectorAll(".sonda-playground__readrow").length,
      hidden: box.hidden,
    };
  });
  check(
    "hovering the chart reads out a value at that instant",
    !reading.hidden && reading.rows === 1 && Number(reading.secs) > 0,
    `secs=${reading.secs} rows=${reading.rows}`
  );
  check(
    "the chart and the readout agree on where the cursor is",
    reading.cursor === reading.secs,
    `chart=${reading.cursor} readout=${reading.secs}`
  );

  // The discriminating case. The y-axis gutter is not second zero, and an
  // implementation that clamps the pointer into the plot — the obvious one —
  // reports a reading here that nobody asked for.
  await cursorPage.mouse.move(chartBox.x + 8, chartBox.y + chartBox.height * 0.5);
  await cursorPage
    .waitForFunction(() => document.getElementById("sp-chart").dataset.cursor === "", null, {
      timeout: 15000,
    })
    .catch(() => {}); // the check below reports what it actually saw
  const gutter = await cursorPage.evaluate(() => ({
    cursor: document.getElementById("sp-chart").dataset.cursor,
    hidden: document.getElementById("sp-readout").hidden,
  }));
  check(
    "the axis gutter is not second zero — no reading there",
    gutter.hidden && gutter.cursor === "",
    `cursor="${gutter.cursor}" hidden=${gutter.hidden}`
  );

  await hover(0.5);
  await cursorPage.mouse.move(chartBox.x + chartBox.width * 0.5, chartBox.y - 80);
  let cleared = true;
  await cursorPage
    .waitForFunction(() => document.getElementById("sp-readout").hidden, null, { timeout: 15000 })
    .catch(() => {
      cleared = false;
    });
  check("leaving the chart clears the reading", cleared);

  // The ghost. A scrub drag is what it was built for, so drive that rather
  // than a synthetic edit: the drag spans several debounce windows, and the
  // ghost must stay pinned to the pre-drag curve for all of them instead of
  // following one step behind.
  const beforeDrag = await cursorPage.evaluate(
    () => document.getElementById("sp-chart").dataset.peak
  );

  const scrub = (await cursorPage.$$(".cm-scrub-number"))[2]; // amplitude on the sine preset
  await scrub.scrollIntoViewIfNeeded();
  const scrubBox = await scrub.boundingBox();
  await cursorPage.mouse.move(scrubBox.x + scrubBox.width / 2, scrubBox.y + scrubBox.height / 2);
  await cursorPage.mouse.down();
  const peaksDuring = [];
  for (let step = 1; step <= 5; step++) {
    await cursorPage.mouse.move(
      scrubBox.x + scrubBox.width / 2 + step * 14,
      scrubBox.y + scrubBox.height / 2,
      { steps: 4 }
    );
    // The ONE deliberate dwell in this suite, and it is the behaviour under
    // test rather than a wait for state. The number is bounded on both sides
    // by playground.js and neither bound is arbitrary:
    //
    //   > DEBOUNCE_MS (500)     or no run fires mid-drag at all — every move
    //                           resets the debounce, the whole drag collapses
    //                           into one run after mouse-up, and the two
    //                           possible ghost baselines are indistinguishable
    //                           because neither has anything to be one step
    //                           behind. (This suite asserted exactly that by
    //                           mistake first, and reported a ghost that was
    //                           never drawn.)
    //   < GHOST_IDLE_MS (1500)  or each step is its own burst, which re-pins
    //                           the baseline every time and tests nothing.
    //
    // 700ms sits in that window, so the drag is one burst containing several
    // runs — the only shape in which "pinned" and "one run behind" differ.
    await cursorPage.waitForTimeout(700);
    peaksDuring.push(
      await cursorPage.evaluate(() => document.getElementById("sp-chart").dataset.ghostPeak)
    );
  }
  await cursorPage.mouse.up();
  let ghosted = true;
  await cursorPage
    .waitForFunction(() => document.getElementById("sp-chart").dataset.ghosts === "1", null, {
      timeout: 20000,
    })
    .catch(() => {
      ghosted = false;
    });
  check(
    "a scrub drag leaves the pre-drag curve on the chart as a ghost",
    ghosted,
    `ghosts=${await cursorPage.evaluate(() => document.getElementById("sp-chart").dataset.ghosts)}`
  );
  // The finding this exists for. A count cannot distinguish the two possible
  // baselines — both draw one ghost. The PEAK can: pinned to the pre-drag
  // state it never moves, while a ghost of the previous run creeps toward the
  // live trace on every debounce.
  const settled = peaksDuring.filter((p) => p !== "");
  check(
    "the ghost stays pinned to the pre-drag curve for the whole drag",
    settled.length >= 2 && new Set(settled).size === 1 && settled[0] === beforeDrag,
    `pre-drag peak ${beforeDrag}, ghost peaks seen ${JSON.stringify(peaksDuring)}`
  );

  // Changing preset replaces the whole document, so the old curve is not a
  // comparison — it is an unrelated scenario sharing a pair of axes.
  await cursorPage.selectOption("#sp-preset", { label: "Latency spike + correlated logs" });
  await cursorPage.waitForFunction(
    () =>
      document.querySelectorAll("#sp-logs .sonda-playground__logline").length > 0 &&
      document.querySelector("#sp-chart")?._geom,
    null,
    { timeout: 60000 }
  );
  check(
    "switching preset drops the ghost rather than comparing two scenarios",
    (await cursorPage.evaluate(() => document.getElementById("sp-chart").dataset.ghosts)) === "0"
  );

  // Review #544 M1. The stamps are written inside drawChart, which a hidden
  // chart never reaches — so a logs-only scenario used to keep advertising
  // the cursor from whatever metrics scenario preceded it. The behaviour was
  // already right; the stamp was not, and the stamp is what this suite reads.
  // Hover first, so there is a cursor to go stale.
  await cursorPage.locator("#sp-chart").scrollIntoViewIfNeeded();
  const mixedBox = await cursorPage.locator("#sp-chart").boundingBox();
  await cursorPage.mouse.move(
    mixedBox.x + mixedBox.width * 0.5,
    mixedBox.y + mixedBox.height * 0.5
  );
  await cursorPage
    .waitForFunction(() => document.getElementById("sp-chart").dataset.cursor !== "", null, {
      timeout: 15000,
    })
    .catch(() => {});
  const hadCursor = await cursorPage.evaluate(
    () => document.getElementById("sp-chart").dataset.cursor
  );
  await cursorPage.selectOption("#sp-preset", { label: "Synthetic log stream" });
  await cursorPage.waitForFunction(
    () => document.getElementById("sp-chart").style.display === "none",
    null,
    { timeout: 60000 }
  );
  const hiddenState = await cursorPage.evaluate(() => ({
    cursor: document.getElementById("sp-chart").dataset.cursor,
    readoutHidden: document.getElementById("sp-readout").hidden,
    highlighted: document.querySelectorAll(".sonda-playground__logline--at").length,
  }));
  check(
    "a hidden chart stops advertising a cursor from the scenario before it",
    hadCursor !== "" && hiddenState.cursor === "",
    `was "${hadCursor}", now "${hiddenState.cursor}"`
  );
  check(
    "and nothing is left highlighted or read out on a logs-only scenario",
    hiddenState.readoutHidden && hiddenState.highlighted === 0,
    `readoutHidden=${hiddenState.readoutHidden} highlighted=${hiddenState.highlighted}`
  );
  check(
    "a logs-only scenario says why it has no cursor, rather than leaving a reader hunting",
    (await cursorPage.evaluate(
      () => document.querySelector("#sp-logs .sonda-playground__lognote")?.textContent || ""
    )).includes("no metrics series"),
    await cursorPage.evaluate(
      () => document.querySelector("#sp-logs .sonda-playground__lognote")?.textContent || "(absent)"
    )
  );

  // Back to the mixed preset for the correlation checks below. The block
  // above deliberately leaves the page on a logs-only scenario, which has no
  // visible chart to hover — restoring it here rather than reordering keeps
  // each block reading as one idea.
  await cursorPage.selectOption("#sp-preset", { label: "Latency spike + correlated logs" });
  await cursorPage.waitForFunction(
    () =>
      document.getElementById("sp-chart").style.display !== "none" &&
      document.querySelectorAll("#sp-logs .sonda-playground__logline").length > 0 &&
      document.querySelector("#sp-chart")?._geom,
    null,
    { timeout: 60000 }
  );

  // Log correlation needs a scenario with BOTH signals — the reason this
  // preset exists. Every other one is metrics or logs, never both, so the
  // feature had nothing to run against.
  await cursorPage.locator("#sp-chart").scrollIntoViewIfNeeded();
  const logBox = await cursorPage.locator("#sp-chart").boundingBox();
  // Sampled after the deliberate scroll above, so this measures what the
  // CURSOR does to the page and not what the test itself did.
  const scrollBefore = await cursorPage.evaluate(() => window.scrollY);
  await cursorPage.mouse.move(logBox.x + logBox.width * 0.5, logBox.y + logBox.height * 0.5);
  await cursorPage
    .waitForFunction(
      () => document.querySelectorAll(".sonda-playground__logline--at").length > 0,
      null,
      { timeout: 15000 }
    )
    .catch(() => {}); // the checks below report what they actually saw
  const correlated = await cursorPage.evaluate(() => {
    const hit = document.querySelector(".sonda-playground__logline--at");
    return {
      hits: document.querySelectorAll(".sonda-playground__logline--at").length,
      at: hit ? hit.querySelector(".sonda-playground__logat").textContent : null,
      cursor: Number(document.getElementById("sp-chart").dataset.cursor),
      scrollY: window.scrollY,
    };
  });
  check(
    "the cursor highlights the log lines from that instant",
    correlated.hits >= 1 && correlated.at !== null,
    `${correlated.hits} line(s), first at ${correlated.at}, cursor ${correlated.cursor}s`
  );
  // The highlighted line must be within half a tick of the cursor — the rule
  // `logLinesNear` implements. A highlight on the wrong line would still be
  // "a highlight", which is why this asserts the arithmetic and not presence.
  const stampSecs = Number(String(correlated.at || "").replace(/[+s]/g, ""));
  check(
    "the highlighted line is the one at the cursor, within half a tick",
    Math.abs(stampSecs - correlated.cursor) <= 0.125 + 1e-9,
    `line +${stampSecs}s vs cursor ${correlated.cursor}s`
  );
  // scrollIntoView would have scrolled the page out from under the pointer,
  // cancelling the very cursor that asked for it. It flashed and vanished.
  check(
    "correlating a log line scrolls its pane, never the page",
    correlated.scrollY === scrollBefore,
    `scrollY ${scrollBefore} -> ${correlated.scrollY}`
  );
  await cursorPage.close();

  // --- 13. The alert lab's rule pair and rule import ---------------------
  //
  // WP12. `draw` stamps how many rules it drew and at what thresholds, so
  // these read the chart's own claim rather than diffing pixels — the lesson
  // review #543 taught about the burst label.
  section("[13] alert lab: warning/critical pair and rule import");
  const alPage = watch(await context.newPage());
  alPage.setDefaultTimeout(30000);
  await alPage.goto(`${BASE}/playground/alert-lab/`, { waitUntil: "domcontentloaded" });
  await alPage.waitForFunction(
    () => document.querySelector("#al-chart") && document.querySelector("#al-chart").dataset.rules,
    null,
    { timeout: 90000 }
  );
  check(
    "the lab starts as one rule, exactly as before the pair existed",
    (await alPage.evaluate(() => document.querySelector("#al-chart").dataset.rules)) === "1"
  );
  check(
    "and the second rule's chip is hidden rather than reporting on nothing",
    await alPage.evaluate(() => document.querySelector("#al-state2").hidden)
  );

  await alPage.check("#al-second");
  let alPaired = true;
  await alPage
    .waitForFunction(
      () => document.querySelector("#al-chart").dataset.rules === "2",
      null,
      { timeout: 20000 }
    )
    .catch(() => {
      alPaired = false;
    });
  const alPair = await alPage.evaluate(() => ({
    thresholds: document.querySelector("#al-chart").dataset.thresholds,
    chipHidden: document.querySelector("#al-state2").hidden,
    seeded: document.querySelector("#al-threshold2").value,
  }));
  check("enabling the second rule draws two thresholds", alPaired, alPair.thresholds);
  check(
    "the second rule is seeded with a threshold rather than left empty",
    alPair.seeded !== "" && Number.isFinite(Number(alPair.seeded)),
    `seeded ${alPair.seeded}`
  );
  check("and its state chip appears", !alPair.chipHidden);

  // Review #546 B1: `Number("") === 0`, so a blank threshold used to become a
  // rule at 0 that fired on everything AND exported a warning rule the reader
  // never wrote. The reviewer reached it by racing the checkbox against the
  // wasm load; clearing the box reaches the same place with no timing at all,
  // which is why this is the version that belongs in a suite.
  await alPage.fill("#al-threshold2", "");
  let alCleared = true;
  await alPage
    .waitForFunction(
      () => document.querySelector("#al-chart").dataset.rules === "1",
      null,
      { timeout: 15000 }
    )
    .catch(() => {
      alCleared = false;
    });
  const alBlank = await alPage.evaluate(() => ({
    rules: document.querySelector("#al-chart").dataset.rules,
    thresholds: document.querySelector("#al-chart").dataset.thresholds,
    value: document.querySelector("#al-threshold2").value,
  }));
  check(
    "a blank threshold is not a rule at zero",
    alCleared && !/warning:0\b/.test(alBlank.thresholds),
    `rules=${alBlank.rules} thresholds=${alBlank.thresholds}`
  );
  // And the box stays empty: seeding is an intention, not a reflex on every
  // keystroke, or a reader could never clear the field to type a new number.
  check("and clearing it is not undone by the seeding", alBlank.value === "", `value="${alBlank.value}"`);

  // Review #546 round 2, W1. The check above runs on the ONE timeline where
  // the seeding intention has already been consumed, so it passed while the
  // recheck path was still broken. Toggling the pair off and back on with a
  // number already in the box re-arms the intention; if enabling does not
  // spend it, the next Backspace that empties the field hands it straight
  // back. Three ordinary steps — compare, restore, retype — and the field
  // used to snap back to its seeded value on the last keystroke.
  await alPage.fill("#al-threshold2", "1.3");
  await alPage.uncheck("#al-second");
  await alPage.check("#al-second");
  await alPage.fill("#al-threshold2", "");
  let alRecleared = true;
  await alPage
    .waitForFunction(
      () => document.querySelector("#al-chart").dataset.rules === "1",
      null,
      { timeout: 15000 }
    )
    .catch(() => {
      alRecleared = false;
    });
  const alAfterRecheck = await alPage.evaluate(
    () => document.querySelector("#al-threshold2").value
  );
  check(
    "and clearing it still works after the pair is toggled off and back on",
    alRecleared && alAfterRecheck === "",
    `value="${alAfterRecheck}"`
  );

  // The other half of the same flag: spending the intention must not stop a
  // genuinely empty row from being seeded when it is re-enabled.
  await alPage.uncheck("#al-second");
  await alPage.check("#al-second");
  await alPage.waitForFunction(
    () => document.querySelector("#al-threshold2").value !== "",
    null,
    { timeout: 15000 }
  );
  const alReseeded = await alPage.evaluate(
    () => document.querySelector("#al-threshold2").value
  );
  check(
    "and re-enabling an emptied row still seeds it",
    Number.isFinite(Number(alReseeded)) && alReseeded !== "",
    `reseeded ${alReseeded}`
  );

  await alPage.fill("#al-threshold2", "0.4");
  await alPage
    .waitForFunction(
      () => document.querySelector("#al-chart").dataset.rules === "2",
      null,
      { timeout: 15000 }
    )
    .catch(() => {});

  // Import. The lab evaluates against the LOADED scenario, so a rule about a
  // different metric produces a working demo of the wrong thing — saying so
  // is the assertion worth making.
  await alPage.fill("#al-import", 'expr: some_other_metric{host="web-01"} >= 72\nfor: 15s');
  await alPage.click("#al-import-btn");
  let alImported = true;
  await alPage
    .waitForFunction(
      () => document.querySelector("#al-import-note").dataset.kind === "warn",
      null,
      { timeout: 15000 }
    )
    .catch(() => {
      alImported = false;
    });
  const alNote = await alPage.evaluate(
    () => document.querySelector("#al-import-note").textContent
  );
  // The THRESHOLD, not the note. Review #546 W3: this check waited on the
  // note turning "warn", which happens because of the different-metric
  // notice — computed from the parsed metric name and independent of the
  // threshold ever reaching a control. Removing the assignment line left the
  // whole suite green. The check's name promised this assertion; now it
  // makes it. The import lands in the SECOND row because it is enabled above.
  const alThreshold = await alPage.evaluate(
    () => document.querySelector("#al-threshold2").value
  );
  check(
    "a pasted rule imports its threshold",
    alImported && Number(alThreshold) === 72,
    `note kind ok=${alImported}, threshold control reads "${alThreshold}"`
  );
  check(
    "and says when the rule is about a different series than the chart",
    /but the chart is showing/.test(alNote),
    alNote.slice(0, 120)
  );
  check(
    "and says when a strict comparison was substituted",
    /`>=` shown as `>`/.test(alNote),
    alNote.slice(0, 80)
  );

  // A rule the lab cannot represent must be refused BY NAME, not half-read.
  await alPage.fill("#al-import", "rate(http_errors_total[5m]) > 10");
  await alPage.click("#al-import-btn");
  let alRefused = true;
  await alPage
    .waitForFunction(
      () => document.querySelector("#al-import-note").dataset.kind === "error",
      null,
      { timeout: 15000 }
    )
    .catch(() => {
      alRefused = false;
    });
  const alRefusal = await alPage.evaluate(
    () => document.querySelector("#al-import-note").textContent
  );
  check(
    "a rule the lab cannot represent is refused, naming what it saw",
    alRefused && /function call/.test(alRefusal),
    alRefusal.slice(0, 90)
  );

  await alPage.click("#al-export");
  await alPage.waitForFunction(
    () => !document.querySelector("#al-export-out").hidden,
    null,
    { timeout: 15000 }
  );
  const alExport = await alPage.evaluate(
    () => document.querySelector("#al-export-out").textContent
  );
  const alNames = (alExport.match(/-\s*alert:\s*(\S+)/g) || []).map((line) =>
    line.replace(/-\s*alert:\s*/, "")
  );
  check(
    "the export carries both rules under distinct alert names",
    new Set(alNames).size === 2,
    [...new Set(alNames)].join(", ")
  );
  check(
    "and one expectation per severity",
    /severity: warning/.test(alExport) && /severity: critical/.test(alExport),
    `${(alExport.match(/severity:/g) || []).length} severity lines`
  );
  await alPage.close();

  // -------------------------------------------------------------------
  // 14. schema-driven completion in the editor (WP11 PR3)
  // -------------------------------------------------------------------
  section("[14] schema-driven completion in the editor");
  //
  // The document is loaded through a #yaml= share link rather than typed.
  // Typing multi-line YAML into an auto-indenting editor does NOT reproduce
  // the literal text — CodeMirror adds its own indentation on Enter and the
  // typed leading spaces stack on top of it, so a "type the fixture" harness
  // silently tests a differently-shaped document. That cost the author a
  // false negative before this suite existed; the share link is exact.
  const acShare = (yaml) =>
    `${BASE}/playground/#yaml=${Buffer.from(yaml, "utf8")
      .toString("base64")
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "")}`;

  const acOptions = async (page) =>
    page.$$eval(".cm-tooltip-autocomplete li", (els) =>
      els.map((el) => el.textContent.trim())
    );

  const acPage = watch(await context.newPage());
  acPage.setDefaultTimeout(30000);

  // A value position: the cursor sits after `type:` inside `generator:`,
  // which is the fourteen-branch tagged union and the single most useful
  // completion in the document.
  await acPage.goto(
    acShare(
      "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    name: cpu\n    rate: 1\n    duration: 30s\n    generator:\n      type: "
    ),
    { waitUntil: "domcontentloaded" }
  );
  await acPage.waitForSelector("#sonda-playground .cm-content", { timeout: 60000 });
  await acPage.waitForFunction(
    () => document.querySelector("#sonda-playground .cm-content")?.textContent.includes("generator"),
    null,
    { timeout: 60000 }
  );
  await acPage.click("#sonda-playground .cm-content");
  await acPage.keyboard.press("Control+End");
  await acPage.keyboard.press("Control+Space");

  let acValueShown = true;
  await acPage
    .waitForSelector(".cm-tooltip-autocomplete", { timeout: 20000 })
    .catch(() => {
      acValueShown = false;
    });
  const acValues = acValueShown ? await acOptions(acPage) : [];
  check(
    "the generator type list comes from the schema, not the document",
    acValueShown && acValues.length >= 10,
    `${acValues.length} option(s)`
  );
  // Naming specific variants rather than counting: a count passes on any
  // list, including one scraped out of the words already on screen.
  check(
    "and it names variants that appear nowhere in the document",
    ["sawtooth", "csv_replay", "spike_event"].every((kind) =>
      acValues.some((option) => option.startsWith(kind))
    ),
    acValues.slice(0, 4).join(" · ")
  );

  // A key position, reached by dedenting out of `generator:`. This is the
  // path resolution doing real work: the answer depends on indentation
  // alone, because the line is empty.
  const acKeyPage = watch(await context.newPage());
  acKeyPage.setDefaultTimeout(30000);
  await acKeyPage.goto(
    acShare(
      "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    generator:\n      type: sine\n    r"
    ),
    { waitUntil: "domcontentloaded" }
  );
  await acKeyPage.waitForSelector("#sonda-playground .cm-content", { timeout: 60000 });
  await acKeyPage.waitForFunction(
    () => document.querySelector("#sonda-playground .cm-content")?.textContent.includes("sine"),
    null,
    { timeout: 60000 }
  );
  await acKeyPage.click("#sonda-playground .cm-content");
  await acKeyPage.keyboard.press("Control+End");
  await acKeyPage.keyboard.press("Control+Space");
  let acKeyShown = true;
  await acKeyPage
    .waitForSelector(".cm-tooltip-autocomplete", { timeout: 20000 })
    .catch(() => {
      acKeyShown = false;
    });
  const acKeys = acKeyShown ? await acOptions(acKeyPage) : [];
  check(
    "dedenting out of a nested mapping offers the entry's keys",
    acKeyShown && acKeys.some((option) => option.startsWith("rate")),
    acKeys.slice(0, 4).join(" · ")
  );
  // The entry's keys, NOT the generator's — the discriminating half. `rate`
  // is an entry field; `amplitude` is sine's, one level deeper, and offering
  // it here would mean the dedent was not read.
  check(
    "and not the keys of the block it just left",
    acKeyShown && !acKeys.some((option) => option.startsWith("amplitude")),
    acKeys.slice(0, 6).join(" · ")
  );

  // Deriving the schema from the Rust types is what puts a type next to the
  // name. Asserted against `rate` specifically, because this list is filtered
  // by the typed `r` — an earlier version of this check looked for the
  // "required" marker here and could not pass, since the only required entry
  // field is `signal_type` and the prefix excludes it.
  check(
    "completions carry the type hint from the schema",
    // The label and the detail are adjacent elements, so textContent reads
    // them concatenated — "ratenumber". No word boundary between them.
    acKeys.some((option) => option.startsWith("rate") && option.includes("number")),
    acKeys.find((option) => /^rate/.test(option)) || "(no rate option)"
  );

  // Review #548 B1, end to end. The list has TWO items and the cursor is on
  // the second one's dash line — the keystroke where a reader starts a new
  // entry, and the position where this feature was dead on arrival. Every
  // fixture above uses a one-item list, which is exactly why nothing saw it:
  // a table of single-item lists cannot observe an item count.
  const acSecondPage = watch(await context.newPage());
  acSecondPage.setDefaultTimeout(30000);
  await acSecondPage.goto(
    acShare(
      "version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    name: cpu\n  - sig"
    ),
    { waitUntil: "domcontentloaded" }
  );
  await acSecondPage.waitForSelector("#sonda-playground .cm-content", { timeout: 60000 });
  await acSecondPage.waitForFunction(
    () => document.querySelector("#sonda-playground .cm-content")?.textContent.includes("cpu"),
    null,
    { timeout: 60000 }
  );
  await acSecondPage.click("#sonda-playground .cm-content");
  await acSecondPage.keyboard.press("Control+End");
  await acSecondPage.keyboard.press("Control+Space");
  let acSecondShown = true;
  await acSecondPage
    .waitForSelector(".cm-tooltip-autocomplete", { timeout: 20000 })
    .catch(() => {
      acSecondShown = false;
    });
  const acSecond = acSecondShown ? await acOptions(acSecondPage) : [];
  check(
    "the SECOND list item completes like the first",
    acSecondShown && acSecond.some((option) => option.startsWith("signal_type")),
    acSecondShown ? acSecond.slice(0, 4).join(" · ") : "no list at all"
  );

  // Declining is a feature. Inside a comment an indentation reading is not
  // merely imprecise, it is unrelated to what the reader is writing.
  const acQuietPage = watch(await context.newPage());
  acQuietPage.setDefaultTimeout(30000);
  await acQuietPage.goto(
    acShare("version: 2\nkind: runnable\nscenarios:\n  - signal_type: metrics\n    # ra"),
    { waitUntil: "domcontentloaded" }
  );
  await acQuietPage.waitForSelector("#sonda-playground .cm-content", { timeout: 60000 });
  await acQuietPage.click("#sonda-playground .cm-content");
  await acQuietPage.keyboard.press("Control+End");
  await acQuietPage.keyboard.press("Control+Space");
  await acQuietPage.waitForTimeout(1500);
  const acQuiet = await acQuietPage.$(".cm-tooltip-autocomplete");
  check("no completions inside a comment", acQuiet === null);

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
