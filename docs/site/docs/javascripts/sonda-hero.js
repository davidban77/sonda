/* Sonda docs — ambient hero signal.
 *
 * Draws a slow, continuously scrolling synthetic signal (steady wave with
 * periodic spikes — Sonda's own vocabulary) behind the homepage hero content.
 * Deliberately quiet: low alpha, no interaction. Skipped entirely under
 * prefers-reduced-motion, and paused while the tab is hidden via the
 * browser's own requestAnimationFrame throttling.
 */
(function () {
  "use strict";

  var reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  function init() {
    if (reducedMotion.matches) return; // static gradient hero is the fallback
    var hero = document.querySelector(".sonda-hero");
    if (!hero || hero.dataset.sondaHeroLive) return;
    hero.dataset.sondaHeroLive = "1";

    var canvas = document.createElement("canvas");
    canvas.className = "sonda-hero__canvas";
    canvas.setAttribute("aria-hidden", "true");
    hero.insertBefore(canvas, hero.firstChild);
    var ctx = canvas.getContext("2d");

    /* The signal: a steady oscillation with a spike every few seconds —
     * the same shapes the generators page documents. */
    function sample(t) {
      var base = Math.sin(t * 0.55) * 0.28 + Math.sin(t * 0.13) * 0.1;
      var spikePhase = t % 9;
      if (spikePhase > 7.6 && spikePhase < 8.2) {
        var p = (spikePhase - 7.6) / 0.6;
        base += Math.sin(p * Math.PI) * 0.55;
      }
      return base;
    }

    function draw(nowMs) {
      var dpr = window.devicePixelRatio || 1;
      var w = hero.clientWidth;
      var h = hero.clientHeight;
      if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
        canvas.width = w * dpr;
        canvas.height = h * dpr;
        canvas.style.width = w + "px";
        canvas.style.height = h + "px";
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);

      var t0 = nowMs / 1000;
      var midY = h * 0.88;
      var amp = h * 0.1;
      var windowSecs = 14; // seconds of signal visible across the hero width

      ctx.beginPath();
      for (var px = 0; px <= w; px += 3) {
        var t = t0 - windowSecs + (px / w) * windowSecs;
        var y = midY - sample(t) * amp;
        if (px === 0) ctx.moveTo(px, y);
        else ctx.lineTo(px, y);
      }
      ctx.strokeStyle = "rgba(253, 186, 116, 0.33)";
      ctx.lineWidth = 2;
      ctx.lineJoin = "round";
      ctx.shadowColor = "rgba(253, 186, 116, 0.45)";
      ctx.shadowBlur = 10;
      ctx.stroke();
      ctx.shadowBlur = 0;

      // Emphasized "now" point at the leading edge.
      var yNow = midY - sample(t0) * amp;
      ctx.beginPath();
      ctx.arc(w - 2, yNow, 3, 0, Math.PI * 2);
      ctx.fillStyle = "rgba(253, 186, 116, 0.75)";
      ctx.fill();

      window.requestAnimationFrame(draw);
    }

    window.requestAnimationFrame(draw);
  }

  if (window.document$ && typeof window.document$.subscribe === "function") {
    window.document$.subscribe(init);
  } else if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
