/* Sonda docs — animated terminal.
 *
 * Replays a static terminal transcript (`.sonda-term`) as a typed session.
 * The markup ships every line, so with JavaScript disabled or under
 * prefers-reduced-motion the transcript simply renders in full.
 * Dependency-free; re-initializes on Material's instant navigation.
 */
(function () {
  "use strict";

  var TYPE_MS = 26; // per character
  var CMD_START_MS = 420; // pause before a command starts typing
  var CMD_DONE_MS = 380; // pause after a command before its output
  var OUT_MS = 240; // pause between output lines

  var reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  function init() {
    if (reducedMotion.matches) return; // static transcript is the experience
    var terms = document.querySelectorAll("[data-sonda-term]:not([data-sonda-term-ready])");
    Array.prototype.forEach.call(terms, setup);
  }

  function setup(term) {
    term.setAttribute("data-sonda-term-ready", "");
    var lineEls = term.querySelectorAll(".sonda-term__line");
    if (!lineEls.length || !("IntersectionObserver" in window)) return;

    var lines = Array.prototype.map.call(lineEls, function (el) {
      return { el: el, text: el.textContent, type: el.getAttribute("data-t") };
    });

    var observer = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (!entry.isIntersecting) return;
          observer.disconnect();
          play(term, lines);
        });
      },
      { threshold: 0.35 }
    );
    observer.observe(term);
  }

  function play(term, lines) {
    lines.forEach(function (line) {
      line.el.textContent = "";
      line.el.classList.add("is-hidden");
    });
    var replay = term.querySelector(".sonda-term__replay");
    if (replay) replay.remove();

    var index = 0;
    (function next() {
      if (index >= lines.length) {
        addReplayButton(term, lines);
        return;
      }
      var line = lines[index++];
      line.el.classList.remove("is-hidden");
      if (line.type === "cmd") {
        typeLine(line, next);
      } else {
        line.el.textContent = line.text;
        window.setTimeout(next, OUT_MS);
      }
    })();
  }

  function typeLine(line, done) {
    line.el.classList.add("is-typing");
    var pos = 0;
    window.setTimeout(function tick() {
      pos += 1;
      line.el.textContent = line.text.slice(0, pos);
      if (pos < line.text.length) {
        window.setTimeout(tick, TYPE_MS);
      } else {
        line.el.classList.remove("is-typing");
        window.setTimeout(done, CMD_DONE_MS);
      }
    }, CMD_START_MS);
  }

  function addReplayButton(term, lines) {
    var bar = term.querySelector(".sonda-term__bar");
    if (!bar) return;
    var button = document.createElement("button");
    button.type = "button";
    button.className = "sonda-term__replay";
    button.textContent = "replay";
    button.addEventListener("click", function () {
      play(term, lines);
    });
    bar.appendChild(button);
  }

  // Material's instant navigation swaps the page body without a full load;
  // document$ emits on every page change. Fall back to DOMContentLoaded.
  if (window.document$ && typeof window.document$.subscribe === "function") {
    window.document$.subscribe(init);
  } else if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
