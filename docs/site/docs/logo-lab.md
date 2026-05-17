---
title: Logo lab
description: Side-by-side comparison of sine-S logo variants.
hide:
  - navigation
  - toc
---

<style>
.lab-intro {
  margin: 0 0 2rem;
}
.lab-grid {
  display: grid;
  grid-template-columns: 1fr;
  gap: 1.5rem;
  margin: 1.5rem 0;
}
.lab-card {
  border: 1px solid var(--md-default-fg-color--lightest);
  border-radius: 12px;
  overflow: hidden;
  background: var(--md-default-bg-color);
}
.lab-card__header {
  padding: 1rem 1.25rem;
  border-bottom: 1px solid var(--md-default-fg-color--lightest);
  display: flex;
  align-items: baseline;
  gap: 0.75rem;
  flex-wrap: wrap;
}
.lab-card__tag {
  display: inline-block;
  font-family: "JetBrains Mono", ui-monospace, monospace;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  padding: 0.15rem 0.55rem;
  background: #1e3a8a;
  color: #a3e635;
  border-radius: 6px;
}
.lab-card__name {
  font-size: 1.15rem;
  font-weight: 700;
  margin: 0;
}
.lab-card__pitch {
  font-size: 0.95rem;
  color: var(--md-default-fg-color--light);
  margin: 0;
  flex-basis: 100%;
}
.lab-contexts {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 0;
}
.lab-contexts > div {
  padding: 1.25rem;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  min-height: 230px;
  border-right: 1px solid var(--md-default-fg-color--lightest);
}
.lab-contexts > div:last-child { border-right: none; }
.lab-contexts__label {
  font-size: 0.7rem;
  font-weight: 700;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  opacity: 0.7;
}
.lab-sizes {
  display: flex;
  align-items: flex-end;
  gap: 1.25rem;
}
.lab-sizes img {
  display: block;
}
.lab-sizes__caption {
  margin-top: 0.4rem;
  font-size: 0.7rem;
  opacity: 0.55;
  text-align: center;
  font-family: "JetBrains Mono", ui-monospace, monospace;
}
.lab-ctx--navy {
  background: #0f172a;
  color: #a3e635;
}
.lab-ctx--midnight-card {
  background: #1e3a8a;
  color: #a3e635;
}
.lab-ctx--white {
  background: #ffffff;
  color: #1e3a8a;
}
.lab-ctx--white.lab-ctx--brand {
  color: #65a30d;
}
.lab-tradeoffs {
  padding: 1rem 1.25rem;
  border-top: 1px solid var(--md-default-fg-color--lightest);
  background: rgba(30, 64, 175, 0.03);
  font-size: 0.92rem;
}
.lab-tradeoffs dl { margin: 0; display: grid; grid-template-columns: 6rem 1fr; gap: 0.3rem 0.8rem; }
.lab-tradeoffs dt { font-weight: 700; color: var(--md-default-fg-color--light); }
.lab-tradeoffs dd { margin: 0; }
@media (max-width: 900px) {
  .lab-contexts { grid-template-columns: 1fr; }
  .lab-contexts > div { border-right: none; border-bottom: 1px solid var(--md-default-fg-color--lightest); }
  .lab-contexts > div:last-child { border-bottom: none; }
}
.lab-pair {
  display: flex;
  align-items: center;
  gap: 0.6rem;
}
.lab-pair__word {
  font-family: "Inter", system-ui, sans-serif;
  font-weight: 800;
  font-size: 1.4rem;
  letter-spacing: -0.02em;
}
</style>

<div class="sonda-section-hero" markdown>

<span class="sonda-section-hero__eyebrow">Internal · not linked from nav</span>

<h1 class="sonda-section-hero__title">Logo lab</h1>

<p class="sonda-section-hero__subtitle">Six sine-S variants, evaluated in three contexts (navy header, midnight brand card, white body) at three sizes (32 px favicon, 64 px header, 160 px hero). Pick the variant that reads cleanly in all three contexts at the smallest size — that's the one that wins.</p>

</div>

<div class="lab-intro" markdown>

**How to evaluate**

1. **Read** — does the small one (32 px) still look like an S? If you have to squint, it fails.
2. **Pair** — does it sit well next to the `sonda` wordmark? Right side of each row.
3. **Carry** — does the mark hold up alone as a favicon? Left column.
4. **Tradeoffs box** at the bottom of each variant calls out the friction.

The shipping logo (used by the site header and favicon right now) is **A — Baseline**. Anything you pick replaces it.

</div>

<!-- =========================================================================
     A — Baseline
     ========================================================================= -->

<div class="lab-card">
  <div class="lab-card__header">
    <span class="lab-card__tag">A</span>
    <h2 class="lab-card__name">Baseline · thin stroke</h2>
    <p class="lab-card__pitch">What ships today. Elegant single cubic-bezier sine, stroke-width 7.</p>
  </div>
  <div class="lab-contexts">
    <div class="lab-ctx--navy">
      <span class="lab-contexts__label">Header (navy)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/a-baseline.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/a-baseline.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/a-baseline.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/a-baseline.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--midnight-card">
      <span class="lab-contexts__label">Brand card (blue-900)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/a-baseline.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/a-baseline.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/a-baseline.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/a-baseline.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--white">
      <span class="lab-contexts__label">Body (white)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/a-baseline.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/a-baseline.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/a-baseline.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/a-baseline.svg" width="32" height="32" alt=""><span class="lab-pair__word" style="color:#1e3a8a">sonda</span></div>
    </div>
  </div>
  <div class="lab-tradeoffs">
    <dl>
      <dt>Strength</dt><dd>Most elegant proportions; reads as a flowing wave at large sizes.</dd>
      <dt>Risk</dt><dd>Stroke gets fragile at 32 px and below; may disappear on the header next to the wordmark.</dd>
      <dt>Use if</dt><dd>You want the most refined, restrained mark.</dd>
    </dl>
  </div>
</div>

<!-- =========================================================================
     B — Bold
     ========================================================================= -->

<div class="lab-card">
  <div class="lab-card__header">
    <span class="lab-card__tag">B</span>
    <h2 class="lab-card__name">Bold · heavy stroke</h2>
    <p class="lab-card__pitch">Same curve, stroke-width 11. More presence at small sizes; reads from across the room at large sizes.</p>
  </div>
  <div class="lab-contexts">
    <div class="lab-ctx--navy">
      <span class="lab-contexts__label">Header (navy)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/b-bold.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/b-bold.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/b-bold.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/b-bold.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--midnight-card">
      <span class="lab-contexts__label">Brand card (blue-900)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/b-bold.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/b-bold.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/b-bold.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/b-bold.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--white">
      <span class="lab-contexts__label">Body (white)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/b-bold.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/b-bold.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/b-bold.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/b-bold.svg" width="32" height="32" alt=""><span class="lab-pair__word" style="color:#1e3a8a">sonda</span></div>
    </div>
  </div>
  <div class="lab-tradeoffs">
    <dl>
      <dt>Strength</dt><dd>Reads cleanly at every size. Confident, brand-forward.</dd>
      <dt>Risk</dt><dd>At 160 px the loops start to feel slightly chunky — the curves can pinch where the stroke meets itself in the middle diagonal.</dd>
      <dt>Use if</dt><dd>You want the safe, durable pick — works in every context without fuss.</dd>
    </dl>
  </div>
</div>

<!-- =========================================================================
     C — Sine + ping
     ========================================================================= -->

<div class="lab-card">
  <div class="lab-card__header">
    <span class="lab-card__tag">C</span>
    <h2 class="lab-card__name">Sine + ping companion</h2>
    <p class="lab-card__pitch">Baseline sine-S with a lime "pulse" dot to the right (with a faint halo ring). Reads as "probe active, signal returning."</p>
  </div>
  <div class="lab-contexts">
    <div class="lab-ctx--navy">
      <span class="lab-contexts__label">Header (navy)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/c-ping.svg" width="40" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/c-ping.svg" width="80" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/c-ping.svg" width="200" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/c-ping.svg" width="40" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--midnight-card">
      <span class="lab-contexts__label">Brand card (blue-900)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/c-ping.svg" width="40" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/c-ping.svg" width="80" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/c-ping.svg" width="200" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/c-ping.svg" width="40" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--white">
      <span class="lab-contexts__label">Body (white)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/c-ping.svg" width="40" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/c-ping.svg" width="80" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/c-ping.svg" width="200" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/c-ping.svg" width="40" height="32" alt=""><span class="lab-pair__word" style="color:#1e3a8a">sonda</span></div>
    </div>
  </div>
  <div class="lab-tradeoffs">
    <dl>
      <dt>Strength</dt><dd>Most literal to "sonda = probe." Lime dot adds the brand pop you don't otherwise get on light backgrounds.</dd>
      <dt>Risk</dt><dd>Compound mark (S + dot) is harder to scale; the ping halo blurs below 24 px. Wider viewBox means it doesn't slot into a square favicon as cleanly.</dd>
      <dt>Use if</dt><dd>You want a mark that tells the "probe" story without a tagline.</dd>
    </dl>
  </div>
</div>

<!-- =========================================================================
     D — Square wave
     ========================================================================= -->

<div class="lab-card">
  <div class="lab-card__header">
    <span class="lab-card__tag">D</span>
    <h2 class="lab-card__name">Square-wave · digital scope</h2>
    <p class="lab-card__pitch">Same S geometry, all right angles. Reads as a digital signal trace — the "synthetic" reading of Sonda dialed up.</p>
  </div>
  <div class="lab-contexts">
    <div class="lab-ctx--navy">
      <span class="lab-contexts__label">Header (navy)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/d-square.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/d-square.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/d-square.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/d-square.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--midnight-card">
      <span class="lab-contexts__label">Brand card (blue-900)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/d-square.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/d-square.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/d-square.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/d-square.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--white">
      <span class="lab-contexts__label">Body (white)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/d-square.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/d-square.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/d-square.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/d-square.svg" width="32" height="32" alt=""><span class="lab-pair__word" style="color:#1e3a8a">sonda</span></div>
    </div>
  </div>
  <div class="lab-tradeoffs">
    <dl>
      <dt>Strength</dt><dd>Most distinctive of the six. Says "digital signal" louder than any curved variant. Scales perfectly because it's pure horizontals and verticals.</dd>
      <dt>Risk</dt><dd>Reads more like a Z than an S to some viewers. Loses the "wave" softness of the sine.</dd>
      <dt>Use if</dt><dd>You want maximum brand distinctiveness and lean fully into the "scope/synthetic" reading.</dd>
    </dl>
  </div>
</div>

<!-- =========================================================================
     E — Echo
     ========================================================================= -->

<div class="lab-card">
  <div class="lab-card__header">
    <span class="lab-card__tag">E</span>
    <h2 class="lab-card__name">Echo · double-stroke</h2>
    <p class="lab-card__pitch">Primary sine-S in currentColor + a slightly offset, faint lime stroke behind it. Reads as a probe pulse with its return.</p>
  </div>
  <div class="lab-contexts">
    <div class="lab-ctx--navy">
      <span class="lab-contexts__label">Header (navy)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/e-echo.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/e-echo.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/e-echo.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/e-echo.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--midnight-card">
      <span class="lab-contexts__label">Brand card (blue-900)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/e-echo.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/e-echo.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/e-echo.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/e-echo.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--white">
      <span class="lab-contexts__label">Body (white)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/e-echo.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/e-echo.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/e-echo.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/e-echo.svg" width="32" height="32" alt=""><span class="lab-pair__word" style="color:#1e3a8a">sonda</span></div>
    </div>
  </div>
  <div class="lab-tradeoffs">
    <dl>
      <dt>Strength</dt><dd>Carries both brand colors in one mark. The offset stroke adds dimensionality without animation.</dd>
      <dt>Risk</dt><dd>The lime echo gets lost on the brand-card context where lime is already the foreground; offset can look like a print misregistration at small sizes.</dd>
      <dt>Use if</dt><dd>You want both palette colors locked into the logo so it works without surrounding brand chrome.</dd>
    </dl>
  </div>
</div>

<!-- =========================================================================
     F — Gradient
     ========================================================================= -->

<div class="lab-card">
  <div class="lab-card__header">
    <span class="lab-card__tag">F</span>
    <h2 class="lab-card__name">Gradient · navy → lime sweep</h2>
    <p class="lab-card__pitch">Stroke transitions from navy at the top through bright blue to lime at the bottom — probe sweeping from deep ocean to surface ping.</p>
  </div>
  <div class="lab-contexts">
    <div class="lab-ctx--navy">
      <span class="lab-contexts__label">Header (navy)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/f-gradient.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/f-gradient.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/f-gradient.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/f-gradient.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--midnight-card">
      <span class="lab-contexts__label">Brand card (blue-900)</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/f-gradient.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/f-gradient.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/f-gradient.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/f-gradient.svg" width="32" height="32" alt=""><span class="lab-pair__word">sonda</span></div>
    </div>
    <div class="lab-ctx--white">
      <span class="lab-contexts__label">Body (white) — brand color</span>
      <div class="lab-sizes">
        <div><img src="../assets/logo-variants/f-gradient.svg" width="32" height="32" alt=""><div class="lab-sizes__caption">32</div></div>
        <div><img src="../assets/logo-variants/f-gradient.svg" width="64" height="64" alt=""><div class="lab-sizes__caption">64</div></div>
        <div><img src="../assets/logo-variants/f-gradient.svg" width="160" height="160" alt=""><div class="lab-sizes__caption">160</div></div>
      </div>
      <div class="lab-pair"><img src="../assets/logo-variants/f-gradient.svg" width="32" height="32" alt=""><span class="lab-pair__word" style="color:#1e3a8a">sonda</span></div>
    </div>
  </div>
  <div class="lab-tradeoffs">
    <dl>
      <dt>Strength</dt><dd>Self-contained — the mark carries the full palette story without needing a colored background. Most "modern" of the six.</dd>
      <dt>Risk</dt><dd>The top of the gradient (navy) disappears on a navy header. Only really sings on white or midnight-blue backgrounds, not on the slate-900 header.</dd>
      <dt>Use if</dt><dd>You want a mark for README / social cards / external use; pair with a solid-color variant for the header.</dd>
    </dl>
  </div>
</div>

## My read

| Best for | Pick |
|---|---|
| Safe default that just works | **B — Bold** |
| Maximum distinctiveness | **D — Square-wave** |
| Tells the brand story alone | **C — Ping** or **F — Gradient** |
| Refined, restrained | **A — Baseline** (current) |
| Carries both colors | **E — Echo** |

If forced to one: **B for the header + favicon, F for README / social cards.** The header needs to read at 30 px on every monitor; F is too gradient-dependent for that, but it earns its keep on a marketing surface where it can be 200 px+.

Square-wave (D) is the most distinctive of the six and the only one with a real "scope/synthetic" identity — worth a serious look if you want Sonda's mark to do work the wordmark can't.
