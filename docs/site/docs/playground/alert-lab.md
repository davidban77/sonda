---
title: Alert lab
description: Watch an alert go inactive, pending, firing, and resolved against a live Sonda signal — threshold, comparison, and for-duration semantics in your browser.
hide:
  - navigation
  - toc
---

<div class="sonda-section-hero" markdown>
<span class="sonda-section-hero__eyebrow">Alert lab</span>

# Watch an alert fire — before you page anyone

Pick a failure pattern — or bring your own: the playground's
**Test an alert →** button carries whatever scenario you're editing straight
here. Set a threshold and a `for:` duration, and press play.
The signal comes from the real Sonda engine (the same
[WebAssembly build](index.md) as the playground); the lane below the chart
shows the alert state the way Prometheus would walk it:
<span class="sonda-lab-chip sonda-lab-chip--inactive">inactive</span> →
<span class="sonda-lab-chip sonda-lab-chip--pending">pending</span> →
<span class="sonda-lab-chip sonda-lab-chip--firing">firing</span> → resolved.
</div>

<div id="sonda-alert-lab" class="sonda-playground">
  <div class="sonda-playground__controls">
    <label class="sonda-playground__label" for="al-preset">Scenario</label>
    <select id="al-preset" class="sonda-playground__select" aria-label="Failure pattern preset"></select>
    <label class="sonda-playground__label" for="al-op">Alert when</label>
    <select id="al-op" class="sonda-playground__select" aria-label="Comparison operator">
      <option value=">">value &gt;</option>
      <option value="<">value &lt;</option>
    </select>
    <input id="al-threshold" class="sonda-playground__select sonda-lab-number" type="number"
      step="1" aria-label="Threshold value">
    <label class="sonda-playground__label" for="al-for">for:</label>
    <select id="al-for" class="sonda-playground__select" aria-label="For duration">
      <option value="0">0s</option>
      <option value="6">6s</option>
      <option value="12">12s</option>
      <option value="20">20s</option>
      <option value="30">30s</option>
    </select>
    <button id="al-play" type="button" class="md-button md-button--primary sonda-playground__run">Play</button>
    <button id="al-export" type="button" class="md-button sonda-playground__share">Copy sonda test setup</button>
    <span id="al-state" class="sonda-lab-chip sonda-lab-chip--inactive" role="status" aria-live="polite">inactive</span>
  </div>
  <div id="al-error" class="sonda-playground__error" hidden></div>
  <canvas id="al-chart" class="sonda-playground__chart" height="380"
    aria-label="Signal chart with alert-state lane"></canvas>
  <p id="al-story" class="sonda-lab-story"></p>
  <p class="sonda-lab-open-link"><a id="al-open" href="index.md">Edit this scenario in the playground →</a></p>
  <pre id="al-export-out" class="sonda-playground__output" hidden
    aria-label="Generated sonda test scenario and alert rule"></pre>
  <noscript><p><strong>The alert lab needs JavaScript</strong> — it runs the Sonda engine
  in your browser via WebAssembly.</p></noscript>
</div>

## What the lane is showing

The evaluator walks the sampled series exactly the way a Prometheus alert rule
would walk scrape samples:

- **inactive** — the condition is false.
- **pending** — the condition is true, but hasn't held for the full `for:`
  duration yet. If the signal recovers first, no alert ever fires. This is
  what makes `for:` the standard defense against flapping.
- **firing** — the condition has held continuously for `for:`. This is when
  Alertmanager would notify you.
- **resolved** — the condition went false while firing (marked with a tick on
  the lane).

Try the *Link blips, then a real outage* preset both ways: with `for: 6s` the
two short blips never page, and the real outage still fires. Set `for: 0s`
and every blip becomes a page — that's the on-call experience `for:` exists
to prevent.

## Run it for real

The lab simulates rule evaluation; the real thing is one button away.
**Copy sonda test setup** exports the rule you just tuned as a ready-to-run
package: the scenario YAML with a label-scoped `expect:` block appended
(`firing_within` derived from when the rule actually fired in the preview),
headed by the matching vmalert/Prometheus alert rule. Save it, add the rule
to your rules file, and `sonda test` turns what you watched here into an
exit code. [Alert testing](../test/alert-testing.md) walks the same patterns
against a live Prometheus + Alertmanager stack (the repository ships a
[Compose stack](../deploy/docker.md) with vmalert wired up), and
[CI validation](../test/end-to-end-pipelines.md) covers the pipeline side.
