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
jitter, and defaults behave exactly like `sonda run`. Underlined numbers are
**draggable** — grab one and slide left or right to scrub the value and watch
the signal reshape live.
</div>

<div id="sonda-playground" class="sonda-playground">
  <div class="sonda-playground__controls">
    <label class="sonda-playground__label" for="sp-preset">Preset</label>
    <select id="sp-preset" class="sonda-playground__select" aria-label="Load a preset scenario"></select>
    <button id="sp-run" type="button" class="md-button md-button--primary sonda-playground__run">Run</button>
    <button id="sp-share" type="button" class="md-button sonda-playground__share">Copy link</button>
    <a id="sp-test-alert" class="md-button sonda-playground__share" href="alert-lab/">Test an alert →</a>
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

[Histogram](../build/generators.md#histogram) entries render as a bucket
heatmap — one row per bucket, cell intensity showing where each tick's
observations landed, so a `mean_shift_per_sec` degradation is visible as
mass drifting into higher buckets. [Summary](../build/generators.md#summary)
entries render as quantile bands (p50 brightest, tail quantiles above). Try
the *Latency histogram + quantiles* preset.

[Log entries](../build/generators.md#log-generators) render as a synthetic
log stream — template messages resolved from their field pools, severity
colored, timed on the scenario timeline — with the `json_lines` output in the
encoded preview. Try the *Synthetic log stream* preset. One thing does not
run in a browser sandbox: `csv_replay` (metric or log) needs the filesystem —
use [`sonda new --from`](../import/from-csv.md) locally.

When the shape looks right, **Test an alert →** carries the scenario into the
[alert lab](alert-lab.md) so you can tune a threshold + `for:` rule against
this exact signal — and the same YAML runs unchanged with
[`sonda run`](../deploy/cli.md) or straight against a
[real backend](../get-started/send-to-a-backend.md).
