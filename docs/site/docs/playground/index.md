---
title: Playground
description: Edit scenario YAML and watch the exact signal Sonda will emit — the real engine, compiled to WebAssembly, running in your browser.
hide:
  - navigation
  - toc
---

<div class="sonda-section-hero" markdown>
<span class="sonda-section-hero__eyebrow">Playground</span>

# See the signal before you send it

Edit scenario YAML on the left; the chart shows the exact values Sonda will emit.
This is not a simulation — it is the real sonda-core engine compiled to WebAssembly,
so compile errors, [operational aliases](../build/generators.md#operational-aliases),
jitter, and defaults behave exactly like `sonda run`.
</div>

<div id="sonda-playground" class="sonda-playground">
  <div class="sonda-playground__controls">
    <label class="sonda-playground__label" for="sp-preset">Preset</label>
    <select id="sp-preset" class="sonda-playground__select" aria-label="Load a preset scenario"></select>
    <button id="sp-run" type="button" class="md-button md-button--primary sonda-playground__run">Run</button>
    <button id="sp-share" type="button" class="md-button sonda-playground__share">Copy link</button>
    <span id="sp-status" class="sonda-playground__status" role="status" aria-live="polite"></span>
  </div>
  <div class="sonda-playground__grid">
    <div class="sonda-playground__editor-pane">
      <label class="sonda-playground__label" for="sp-editor">Scenario YAML</label>
      <div id="sp-error" class="sonda-playground__error" hidden></div>
      <textarea id="sp-editor" class="sonda-playground__editor" spellcheck="false"
        aria-label="Scenario YAML editor"></textarea>
    </div>
    <div class="sonda-playground__result-pane">
      <canvas id="sp-chart" class="sonda-playground__chart" height="320"
        aria-label="Chart of the sampled signal values"></canvas>
      <div id="sp-legend" class="sonda-playground__legend"></div>
      <div id="sp-skipped" class="sonda-playground__skipped"></div>
      <label class="sonda-playground__label" for="sp-output">Encoded output (first events per series)</label>
      <pre class="sonda-playground__output"><code id="sp-output"></code></pre>
    </div>
  </div>
  <noscript><p><strong>The playground needs JavaScript</strong> — it runs the Sonda engine
  in your browser via WebAssembly. All other documentation works without it.</p></noscript>
</div>

## What you can try here

Everything the metrics engine supports works in the playground: the
[core generators](../build/generators.md#metric-generators), the
[operational aliases](../build/generators.md#operational-aliases) (`flap`,
`saturation`, `leak`, `degradation`, `steady`, `spike_event`),
[jitter](../build/generators.md#jitter), multi-scenario files with shared
`defaults:`, labels, and every [encoder](../build/encoders.md) for the output
preview. [Gaps and bursts](../build/scheduling.md) appear as shaded bands on
the chart.

Two things do not run in a browser sandbox: `csv_replay` (no filesystem — use
[`sonda new --from`](../import/from-csv.md) locally) and log/histogram/summary
entries, which are compiled and validated but not visualized yet.

When the shape looks right, the same YAML runs unchanged with
[`sonda run`](../deploy/cli.md) — or straight against a
[real backend](../get-started/send-to-a-backend.md).
